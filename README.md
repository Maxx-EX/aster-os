# Aster · 星芒

从零写起的 ARM64 系统：**原创引导器 + 原创内核 + 原创三级镜像校验**，不基于任何现有内核（没有 Linux、没有 Zircon、没有 *BSD 的任何代码）。

当前是 **v0.3 / 阶段二**：在阶段一（上电、校验、进内核、命令行、串口刷写）之上，加进了**用户态 + 抢占调度**——内核把程序装进只属于它的 4 KiB 代码页和栈页，切到 EL0 执行，程序只能通过 `svc` 请求内核做事；每 2 ms 定时器中断打断一次，调度器在两个任务之间轮转。一个只做纯计算、一次系统调用都不发的程序，也饿不死别的任务。

图形、文件系统、抢占式调度、APK/EXE 兼容层仍是后续阶段，见 [docs/roadmap.md](docs/roadmap.md)。

---

## 已经验证能跑的部分

| 能力 | 状态 | 验证方式 |
| --- | --- | --- |
| 引导器上电接管（EL1，MMU off） | 通过 | `make run`，串口打印 ASTER BOOT |
| 三级校验 AVP（完整性 / 真实性 / 链绑定） | 通过 | 启动日志三级 PASS |
| 镜像被改一个字节 → 拒绝启动 | 通过 | 篡改 offset 0x131f4 后进入底层模式 |
| 底层模式 Recovery Shell | 通过 | 3 秒窗口按 `B` 进入 |
| AFP 串口刷写（长度 + MAC + 数据） | 通过 | 刷写后 `FLASH OK trusted=1`，可从暂存区启动 |
| 内核接管、banner、命令行 | 通过 | `aster>` 提示符，`help/info/hash/mmu` 可用 |
| MMU 开启（恒等映射，1 GiB 块） | 通过 | `mmu` 命令显示"已开启"，开启后系统不崩 |
| 异常向量表 + 现场打印 | 通过 | 故意触发 FP 陷阱时能打印 ESR/ELR/FAR |
| 内核侧独立复算摘要（链式校验） | 通过 | `hash` 命令输出与引导器一致 |
| **EX3 原创校验值**（内核与引导器各持一份） | 通过 | `ex3` 命令；混搭镜像时内核自己拒绝接管 |
| **用户态 EL0**：独立代码页 + 栈，切特权级执行 | 通过 | `run` 命令，串口打印 "Aster 用户态已启动 (EL0)" |
| **SVC 系统调用**：write / exit / getpid | 通过 | 用户程序打印 3 段文本、退出码 7 正确带回内核 |
| **页表级隔离**：EL0 打不开内核与外设 | 通过 | `runf` 故意读 0x4020_0000 → Data Abort(低 EL) 被捕获 |
| **用户指针校验**：syscall 参数不能反手打穿内核 | 通过 | `write` 的 buf 必须落在用户页内，否则 EFAULT |
| **寄存器清零**：进 EL0 前不留内核残留 | 通过 | `enter_user` 里 `x0..x30` 全清 |
| **定时器抢占调度**：2 ms 时间片、双任务轮转 | 通过 | `run` 两个任务交替输出；`runp` 纯计算任务期间另一任务照常跑完 |
| **GICv2 + PPI 30** 中断通路 | 通过 | `run` 的切换计数随运行时长增长 |
| 树莓派 4/5 镜像 | 仅构建通过 | `make rpi4` 产出 `kernel8.img`；**内存属性映射与 GIC 地址未适配，真机不可用** |

## 明确还没做的

- **抢占调度的粒度**：只有"每 2 ms 轮转"，没有优先级、没有阻塞/唤醒、没有每任务独立内核栈
- **浮点/NEON 上下文不保存**：陷阱帧只存 x0-x30，任务里一旦用 Q 寄存器，被抢占后会串
- **任务之间能互读对方的用户页**：共用一张 TTBR0，只靠不同虚拟区间分开（见下一条）
- **每个任务独立的地址空间**：现在所有任务共用同一张 TTBR0 页表，隔离靠页表 AP 位（硬件强制），不是靠切换页表
- **文件系统 / 网络 / 驱动框架 / 图形**：都没有
- **APK 与 EXE 兼容层**：只有路线设计，没有实现（见路线图）
- 树莓派真机：只做了板级地址适配和镜像产出，没在硬件上跑过

这一版的目标是把"系统骨架 + 可信启动"这条线打通，而不是假装已经是个能日常使用的操作系统。

---

## 架构

```
  上电 / 复位
      │
      ▼
 ┌─────────────────────────────────────────────────────────┐
 │  ASTER BOOT（C + AArch64 汇编，约 12 KiB）               │
 │                                                         │
 │  1. start.S：设栈、清 bss、放行 FP/SIMD、保存 DTB         │
 │  2. 串口初始化（PL011）                                   │
 │  3. AVP 三级校验内嵌内核镜像                              │
 │       L1 SHA-256      完整性                             │
 │       L2 HMAC-SHA256  真实性（密钥只在这里）               │
 │       L3 HMAC(BOOT_ID‖"|CHAIN|"‖EX3‖L2)  链绑定          │
 │  4. 校验通过 → 3 秒菜单（按 B 进底层模式）                 │
 │     校验失败 → 强制进底层模式，绝不放行                     │
 │  5. 逐字节载入内核到 0x4020_0000，清 cache，跳 EL1          │
 └─────────────────────────────────────────────────────────┘
      │  x0 = struct aster_bootinfo（含引导器算出的摘要）
      ▼
 ┌─────────────────────────────────────────────────────────┐
 │  ASTER KERNEL（Rust no_std + C，约 28 KiB）              │
 │                                                         │
 │  boot.S  设栈、放行 FP、清 bss                            │
 │  chain_check  用 C 侧 SHA-256 独立复算，与 bootinfo 比对   │
 │  mmu.rs  L1/L2/L3 三级页表，内核页 EL1-only、用户页 EL0    │
 │  exceptions.rs  16 项向量表 + SVC 入口 + EL0 故障处理      │
 │  task.rs  装载用户程序、切 EL0、回收退出码                │
 │  shell.rs  命令行（run / runf 触发用户态）                 │
 └─────────────────────────────────────────────────────────┘
```

内存布局（QEMU virt，512 MiB）：

```
0x0000_0000  外设区（PL011 在 0x0900_0000，GIC 在 0x0800_0000）
0x4000_0000  RAM 起始
0x4008_0000  引导器载入地址
0x4008_3000  内嵌内核镜像（.payload 段）
0x4020_0000  内核运行地址
0x4410_0000  AFP 刷写暂存区（8 MiB 上限）
0x4440_0000  用户代码页（4 KiB，EL0 可执行，内核 EL1 可读写）
0x4440_1000  用户 guard 页（不映射，越界立刻 fault）
0x4440_2000  用户栈页（4 KiB，EL0 可读写、不可执行）
0x4440_3000  用户栈顶
```

树莓派 4/5（`make rpi4`）：引导器 0x80000、内核 0x200000、PL011 0xFE20_1000。

---

## 目录

```
aster-os/
├── boot/            引导器：start.S、uart、sha256、verify(AVP)、flash(AFP)、shell、main
├── kernel/          内核：Rust 主体 + c/ 下 C 侧支持代码
│   ├── src/          main.rs / uart.rs / mmu.rs / exceptions.rs / task.rs / shell.rs / boot.S
│   │                 ex3.rs（构建时生成，内嵌 EX3）
│   └── c/            aster_c.c、sha256.c（与引导器同一份实现）
├── user/            用户态程序（独立扁平二进制，链接到 0x4440_0000）
│   ├── hello.S      系统调用演示：write / getpid / 一次非法调用 / exit(7)
│   └── fault.S      越权演示：从 EL0 读内核地址，验证 MMU 隔离
├── tools/            aster-sign.py（密钥/EX3/签名）、afp-flash.py（主机端刷写）、test-afp.py（刷写回归）
├── docs/             架构、校验链、路线图
└── Makefile          一条命令走完 内核→签名→引导器→运行
```

---

## 构建与运行

依赖：`aarch64-linux-gnu-gcc`、`qemu-system-arm`（含 aarch64）、Rust + `aarch64-unknown-none` 目标。

```bash
# 一次性准备
rustup target add aarch64-unknown-none
sudo apt install gcc-aarch64-linux-gnu qemu-system-arm

# 构建 + 启动（会打开串口交互）
make run

# 只做冒烟测试（不交互，逐项断言：内核接管 / EX3 / EL0 输出 / 退出码 / 越权被拦）
make test

# 树莓派镜像
make rpi4      # 产出 build/kernel8.img，拷到 SD 卡 boot 分区
```

首次构建会自动生成引导密钥 `boot/keys/boot.key` 和 EX3 构建随机数 `boot/keys/ex3.nonce`（都**不会**提交到仓库），并据此生成 `boot/ex3.h` 与 `kernel/src/ex3.rs`。

内核产物用自定义后缀 **`.ast`**（`build/aster-kernel.ast`），是 Aster 自己的裸机镜像格式，不会和常见的 `.bin`/`.img` 混淆。

一次构建的顺序是固定的，`make` 已经排好：

```
user/*.S（交叉汇编 + 链接到 0x44400000）→ 扁平 bin → 被 build.rs 嵌进内核
gen-ex3（生成 EX3）→ cargo 编内核(.ast) → aster-sign（出 manifest.h）
                  → 交叉 gcc 编引导器 → 把 .ast 当 .payload 内嵌 → 链接
```

## 怎么调试

```bash
# 引导器打印全部串口内容
qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic -kernel build/aster-boot.elf

# 串口挂 unix socket（刷写、自动化测试用）
make run-sock
python3 tools/afp-flash.py build/aster-kernel.ast --sock /tmp/aster.sock
python3 tools/test-afp.py                       # 完整刷写回归（需先 make run-sock）
```

---

## 校验链设计

三层都通过才放行，任一层失败都会停在底层模式：

1. **L1 完整性** `SHA-256(payload)`，防止镜像被截断或改字节
2. **L2 真实性** `HMAC-SHA256(K, payload)`，K 只存在于引导器只读区和构建机
3. **L3 链绑定** `HMAC-SHA256(K, "ASTER-BOOT-ARM64-V1" ‖ "|CHAIN|" ‖ EX3 ‖ L2)`，把引导器版本、EX3 和镜像指纹绑死

内核再独立算一遍 `SHA-256` 并与引导器传来的摘要比对，形成第二道关。

### EX3 —— 原创校验值

EX3（Aster Exclusive Identifier v3）是本系统给每次构建打的原创身份值：

```
EX3 = HMAC-SHA256(K, BOOT_ID ‖ "EX3" ‖ build_id ‖ nonce)
```

- 32 字节，显示为 `EX3-8F2A1C7D-3B9E4421` 这种指纹
- 由只有本机持有的 K 派生，内核**拿不到 K，只能比对、无法伪造**
- 引导器在 AVP 里算一份，内核在自己的 `.aster` 镜像里嵌一份，两边必须完全一致才允许内核接管
- 换了内核（来自另一次构建）而引导器不变 → **AVP 三层全过，内核照样自己拒绝**

这就是 EX3 的独立价值：它不依赖引导器的判断，内核用自己嵌的那份做最后确认。实测记录见 [docs/verify-chain.md](docs/verify-chain.md)。

> 注意：这是**共享密钥**（HMAC）方案，适合"设备自己可信"的场景。要防的是"外部改镜像"。如果要做成"第三方可以验证但无法伪造"的发布签名，需要换成非对称签名（Ed25519），那是阶段三的事。

细节见 [docs/verify-chain.md](docs/verify-chain.md)。

---

## 许可

MIT，见 [LICENSE](LICENSE)。所有代码为本项目原创，密码学实现（SHA-256 / HMAC-SHA256）也是自己实现，没有引用外部库。
