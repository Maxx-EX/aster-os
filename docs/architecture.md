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

MMU 用 4 KiB 粒度、39 位 VA、恒等映射。阶段一只有 L1 的 1 GiB 块，阶段二为了能单独给用户段开权限，把 RAM 那一段拆成了三级：

```
L1[0]       0 - 1 GiB    Device-nGnRnE   AP=EL1RW  UXN=1
L1[1]   →   L2（1 - 2 GiB，QEMU 的 RAM 在这里）
  L2[i]     每项 2 MiB  Normal WBWA      AP=EL1RW  UXN=1    ← 内核
  L2[34] →  L3（0x4440_0000 起的 2 MiB）
    L3[0]   用户代码页 4 KiB             AP=EL0RW  UXN=0    ← EL0 可执行
    L3[1]   guard 页（不映射）           ——                 ← 越界立刻 fault
    L3[2]   用户栈页 4 KiB               AP=EL0RW  UXN=1    ← 可读写、不可执行
L1[2], L1[3] 2 - 4 GiB   Normal         AP=EL1RW  UXN=1    （预留）
```

描述符字段：`bit[1:0]=0b01` 块 / `0b11` 页（L3）/ `0b11` 表（指向下一级）；`AP[7:6]`；`SH[9:8]=0b11`；`AF=1`；`PXN[53]`、`UXN[54]`。

> 关键在于 `AP=0b00` 的内核页：EL0 就算拿着 `0x4020_0000` 也读不到任何一个字节 —— 这是页表硬件拦的，不是软件 `if` 拦的。`runf` 命令就是专门验证这一点。

> 另一个坑：`SCTLR_EL1.SPAN(bit23)` 默认是 1，异常进入 EL1 时会自动置 `PSTATE.PAN`，那样内核在 syscall 里读用户 buf 会直接 fault。cortex-a72 属于 ARMv8.0，PAN 是 v8.1 的可选特性，这一版把 SPAN 清 0，用户内存的访问控制交给页表 AP 位 + `user_ptr_ok` 软件校验。

> 踩过的坑：一开始把 `bit[1:0]` 写成 `0b11`，那是**表描述符**，硬件会把高 48 位当成下一级表地址去翻译，立刻挂死且没有异常输出。块描述符必须是 `0b01`。

`MAIR_EL1 = 0xFF | (0x04 << 8)`：attr0 普通内存、attr1 Device-nGnRnE。

## 4. 异常

`exceptions.rs` 用 `global_asm!` 造 16 项向量表（每项 128 字节），按来源分流：

```
0-3    Current EL with SP_EL0    → aster_unexpected（内核自己不用 SP_EL0，兜底）
4-7    Current EL with SP_ELx    → aster_unexpected（内核态崩了：打印现场后停机）
8-11   Lower EL using AArch64    → 8 号槽 = aster_svc_entry，其余兜底
12-15  Lower EL using AArch32    → aster_unexpected
```

EL0 的 `svc`、data abort、非法指令**都落在 8 号槽**，所以 `svc_entry` 里先看 `ESR.EC`：

- `EC=0x15` → 系统调用，按 `x8` 分发
- 其它 → 用户态故障：打印 EC/ELR/FAR，杀掉这个任务，**内核继续活着**

`VBAR_EL1` 在 `chain_check` 之前就装好，所以后面任何一步崩都有输出。

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

## 8. 用户态与系统调用

### 用户程序不是内核的一部分

`user/hello.S` 用交叉汇编编出来，链接到 `0x4440_0000`，`objcopy -O binary` 得到一个几百字节的扁平镜像，再由 `kernel/build.rs` 编成 Rust 静态数组嵌进内核。内核启动时它只是一段数据，`run` 命令才把它拷进用户代码页。

这样做的原因：阶段二还没有文件系统，也没有 ELF 加载器。等有了块设备驱动，同样的 `load_user_image(binary)` 接口可以换成"从盘上读 ELF、按段映射"，内核内部逻辑不用动。

### 切到 EL0 的三步

```asm
aster_enter_user:            // x0 = 入口, x1 = 用户栈顶
    adr  x2, ASTER_USER_CTX  // 先把内核自己的现场存起来
    ...                      // [0]=sp, [1..12]=x19..x30, [13]=退出码
    msr  sp_el0, x1          // 用户态用它自己的栈
    msr  elr_el1, x0         // 返回地址 = 用户入口
    msr  spsr_el1, xzr       // M=EL0t、DAIF 全开
    .irp r, 0,1,...,30
    mov  x\r, xzr            // 清空所有通用寄存器，不留内核残留
    .endr
    eret
```

`eret` 之后 CPU 就在 EL0 跑用户代码，任何 `msr/mrs` 特权指令、任何对外设的访问都会 trap。

### 系统调用的现场

`svc #0` 进 8 号槽，`svc_entry` 先在内核栈上压一个 160 字节的陷阱帧：

```
[sp,#0]    x0 x1     ← 系统调用参数；返回值也写回这里
[sp,#16]   x2 x3
...
[sp,#64]   x8 x9     ← x8 = 系统调用号
...
[sp,#144]  x18 x30
```

`svc_handler(esr, elr, frame)` 读 `frame[8]` 拿调用号，处理完把返回值写进 `frame[0]`；汇编恢复现场后 `eret`，用户就在 `x0` 里拿到结果。

退出（`exit` / 用户态故障）走另一条路：`svc_handler` 返回 1，`svc_entry` 跳到 `aster_task_exit`，丢掉陷阱帧、从 `ASTER_USER_CTX` 恢复内核的 sp 与 x19–x30，`ret` 回 Rust 调用点，`x0` 是退出码。shell 于是打印 "用户任务已退出，退出码 = 7"。

### 系统调用号

沿用 AArch64 Linux 的编号，为以后能直接跑现成的用户态程序（APK 的 bionic 层也认这套）留路：

| 号 | 名称 | 参数 | 返回 |
| --- | --- | --- | --- |
| 64 | write | fd, buf, len | 写入字节数 / `-EBADF` / `-EFAULT` |
| 93 | exit | code | 不返回 |
| 172 | getpid | — | 1 |

`buf` 必须落在用户代码页或用户栈页内（`mmu::user_ptr_ok` 检查起止），否则返回 `-EFAULT`。哪怕页表权限配错，用户也没法借 `write` 把内核内存读出来。

### 踩过的两个坑

1. **ELR 不需要 +4**。AArch64 的 `SVC` 保存的 `ELR_EL1` 已经是"下一条指令"的地址（和 x86 的 `int` 不同）。一开始按 x86 直觉加了 4，结果用户程序每次被跳过一条指令：`mov x8,#9999`、`mov x0,#7` 全没执行，参数一路滞后一轮，表现为"第 4 次调用打印的却是第 3 次的参数"。
2. **退出码槽和 x30 槽撞了**。`ASTER_USER_CTX` 里 `x19..x30` 是 12 个寄存器（索引 1..12），退出码必须放索引 13。一开始放 12，把 x30 覆盖成返回值，`ret` 直接跳到 `-9` 地址，报 `Data Abort`。

### 隔离验证

`runf` 装载 `user/fault.S`，它执行 `ldr x1, [0x40200000]`（内核镜像基址）。实测输出：

```
!!! 用户态故障（Aster 捕获，内核继续运行）!!!
  EC      = 0x24 (数据中止(低 EL))
  ELR     = 0x0000000044400004  (出错指令)
  FAR     = 0x0000000040200000  (访问地址)
  → 该任务被终止，返回内核 shell。
```

`EC=0x24` 就是 "Data Abort from a lower Exception level"，也就是硬件认定 EL0 越权。内核没有停机，回到 `aster>` 继续可用。
