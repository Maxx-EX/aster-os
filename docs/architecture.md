# 架构说明

## 1. 为什么分成引导器和内核两级

单级内核（把校验写进内核自己）有个死结：校验代码和被校验对象在同一块内存里，改镜像的人可以顺手把校验逻辑一起改掉。

所以采用两级：

- **引导器**只做四件事：初始化串口、校验、加载、跳转。代码量压到 12 KiB，容易审计。
- **内核**不持有密钥，只能"复算并比对"，不能"自己签发"。

密钥 K 只存在于 `boot/manifest.h`（构建时由 `tools/aster-sign.py` 从 `boot/keys/boot.key` 注入），编译进引导器的只读段。内核链接的 `libaster_c.a` 里没有 K。

## 2. 启动时序

| 步骤 | 位置 | 关键点 |
| --- | --- | --- |
| 0 | QEMU `-kernel` | 载入 ELF，从 `_start` 进 EL1，MMU/cache 关闭，x0 = DTB |
| 1 | `boot/start.S` | 只留 0 核；设栈；清 bss；`CPACR_EL1.FPEN=0b11` |
| 2 | `boot/main.c` | `uart_init` → banner → `avp_check` |
| 3 | `boot/verify.c` | 三级校验，详见 verify-chain.md |
| 4 | `boot/main.c` | 可信 → 3 秒菜单；不可信 → 直接 `shell_run` |
| 5 | `boot/image.c` | 逐字节拷到 `0x4020_0000`，`dc cvau` + `ic ialluis` |
| 6 | `boot/image.c` | `br x1`，x0 指向 `struct aster_bootinfo` |
| 7 | `kernel/src/boot.S` | 设内核栈、放行 FP、清内核 bss |
| 8 | `kernel_main` | 校验 magic → 装异常向量 → 复算摘要 → **比对 EX3** → 开 MMU → shell |

### 为什么 FP/SIMD 要显式放行

`CPACR_EL1` 默认把 EL1 的 FP/SIMD 访问 trap 掉。Rust 编译器生成的代码会用到 NEON（复制、清零都会），一旦没放行，第一条 NEON 指令就触发 `EC=0x07`。

第一次跑内核就是死在这里，异常现场打出来是 `ESR=0x1fe00000 / ELR=0x40201180`。修复就是入口处 `msr cpacr_el1, #0x300000`。

## 3. 内存与 MMU

QEMU virt 的 RAM 从 `0x4000_0000` 开始。引导器把自己放在 `0x4008_0000`，内核放 `0x4020_0000`，中间留出 1.5 MiB 给引导器自身（含内嵌镜像）和栈。

MMU 用 4 KiB 粒度、39 位 VA、L1 的 1 GiB 块映射，全部恒等映射：

```
L1[0]  0 - 1 GiB    Device-nGnRnE   （UART 0x0900_0000、GIC）
L1[1]  1 - 2 GiB    Normal WBWA      （RAM，内核在 0x4020_0000）
L1[2]  2 - 3 GiB    Normal WBWA      （预留）
L1[3]  3 - 4 GiB    Normal WBWA      （预留）
```

描述符：`bit[1:0]=0b01`（有效 + 块）、`AttrIndx`、`SH=0b11`、`AF=1`。

> 踩过的坑：一开始把 `bit[1:0]` 写成 `0b11`，那是**表描述符**，硬件会把高 48 位当成下一级表地址去翻译，立刻挂死且没有异常输出。块描述符必须是 `0b01`。

`MAIR_EL1 = 0xFF | (0x04 << 8)`：attr0 普通内存、attr1 Device-nGnRnE。

## 4. 异常

`exceptions.rs` 用 `global_asm!` 造 16 项向量表（每项 128 字节），全部跳到一个公共入口，读出 `ESR_EL1 / ELR_EL1 / FAR_EL1` 后打印停机。DIY 阶段先保证"出错能看见现场"，比"能恢复"重要。

`VBAR_EL1` 在 `chain_check` 之前就装好，所以后面任何一步崩都有输出。

## 5. 内核里的 Rust / C 分工

- Rust：内存布局、MMU 页表、异常、命令行、字符串处理 —— 这些地方值得用类型系统兜住
- C：`sha256.c`（与引导器**同一份源码**，保证两侧摘要一致）、`aster_c_hash_image` / `aster_c_ct_eq` 两个薄封装

同一个 SHA-256 实现被编译两次（引导器和内核），这是故意的：链式校验的意义就在于"两边独立算、结果必须相同"。如果共用同一份运行时状态，就等于自己验自己。

`build.rs` 在构建时调 `aarch64-linux-gnu-gcc` 编 C 再 `ar` 成 `libaster_c.a`，由 `cargo:rustc-link-lib=static=aster_c` 交给 rust-lld。

## 6. EX3 落到代码里的位置

`EX3` 不是一个手写常量，而是构建产物：

| 环节 | 文件 | 说明 |
| --- | --- | --- |
| 生成 | `tools/aster-sign.py gen-ex3` | `HMAC(K, BOOT_ID‖"EX3"‖build_id‖nonce)` |
| C 侧载体 | `boot/ex3.h`（生成） | `ASTER_EX3[32]`、`ASTER_EX3_TAG` |
| Rust 侧载体 | `kernel/src/ex3.rs`（生成） | `EX3: [u8;32]`、`EX3_TAG` |
| 传递 | `bootinfo.ex3` | 引导器校验通过后填进 bootinfo 交给内核 |
| 比对 | `kernel_main` → `ex3_check` | 32 字节常量时间比较，不一致就 wfe 停机 |

两个生成文件都在 `.gitignore` 里：克隆下来的人第一次 `make` 会用自己的 K 和 nonce 生成属于自己的 EX3，不会和仓库里某人构建出的值撞车。

## 7. 串口协议之外的东西

引导器里还有个 `AFP`（Aster Flash Protocol），跑在串口上：握手 → 长度 → 期望 MAC → 数据。收到数据后用同一把 K 算 HMAC 比对，通过了才标记为"可信暂存镜像"，`boot` 命令就会优先从暂存区启动。

协议细节见 `boot/flash.c` 顶部注释，主机端实现 `tools/afp-flash.py`。
