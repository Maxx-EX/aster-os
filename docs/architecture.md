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

## 9. 中断、定时器与抢占

### 一条完整的中断链路

```
CNTP_TVAL 到期 → PPI 30 → GICv2（GICD 0x0800_0000 / GICC 0x0801_0000）
   → CPU 在 EL0 时 IRQ 不被屏蔽 → 向量表 9 号槽 aster_lower_irq
   → SAVE_FRAME（272 字节） → irq_handler → sched::tick(frame) → LOAD_FRAME → eret
```

`irq_handler` 的顺序是固定的：`ack()` 取中断号 → `timer::disarm()` 清掉本次 → `timer::arm()` 装下一次 → `tick()` 换人 → `eoi()`。先 disarm 再 arm 是必须的，否则定时器的中断线还拉高，`eoi` 之后会立刻再进一次，变成中断风暴。

`ack()` 返回 1023 是 spurious，直接返回且**不写 EOIR** —— 对伪中断写 EOIR 在 GICv2 上是未定义行为。

### 为什么所有异常共用一个陷阱帧

如果 IRQ 抢占另写一套保存/恢复，SVC 和 IRQ 就会有两份寄存器布局，改一处忘一处。这里统一成 34 个 u64：

```
[0..30]  x0..x30        [31] SP_EL0        [32] ELR_EL1        [33] SPSR_EL1
```

调度器的工作因此简化成一句话：**把当前帧存进 `Task.ctx`，把下一个任务的 `ctx` 覆盖到同一帧上**。`eret` 出去时 CPU 已经在跑另一个任务，不需要额外的"任务切换汇编"。任务第一次运行也是同一个帧——只是 `ctx` 由 `spawn()` 预先填好。

### 抢占的证据

`runp` 把任务 A 设成纯计算（1.3 亿次减法，中间一次 `svc` 都不发），任务 B 是打印任务：

```
BUSY start
TASK 2
TASK 2
...（30 行）
[SVC] exit(0)  ← 任务 2
BUSY end
[SVC] exit(0)  ← 任务 1
```

B 的 30 行全部落在 A 的 `BUSY start` 和 `BUSY end` 之间，而且 B 还正常退出了。这说明 A 在长时间独占 CPU 意图时，定时器照样把 CPU 抢给了 B。如果换成没有抢占的系统，这 30 行会全部排在 `BUSY end` 之后 —— 也就是"一个失控程序锁死整机"。

`make test` 里用 awk 断言了这个行序关系，不是靠肉眼看日志。

### 这一版调度器的边界

| 有 | 没有 |
| --- | --- |
| 2 ms 时间片、round-robin | 优先级、阻塞/唤醒、睡眠 |
| 每任务独立用户代码页与栈（不同虚拟区间） | 每任务独立 TTBR0 页表（任务间能互读对方页） |
| 任务越权 → 杀任务、内核继续 | 每任务独立内核栈 |
| 退出码回传、切换计数 | 浮点/NEON 上下文保存（帧里没有 Q0-Q31） |

最后一条是实际隐患：用户程序只要用一次 NEON，被抢占后寄存器组就会串。当前 `hello` / `busy` / `fault` 都是纯整数代码，所以跑得对；但这是一个早晚要补的洞，已写进 `docs/roadmap.md`。

### 中断期间不能做的事

- 内核态（EL1）**全程屏蔽 IRQ**（进入异常时硬件自动置 DAIF）。所以 `svc_handler`、`println`、UART 输出都不会被抢占打断，这也意味着这些路径里不能写死循环。
- `irq_handler` 里不做任何打印：UART 输出要花时间，在中断里做会拖长关中断窗口。

---

## 10. 图形输出与远程访问

### 10.1 为什么是 ramfb

QEMU 的 `virt` 机器上没有 PCI 显卡（除非手动挂 virtio-gpu），但有一个更简单的设备：**ramfb**。它不是显卡，只是一块由固件接口描述的线性帧缓冲——guest 把自己的内存地址、分辨率、像素格式写进 `fw_cfg` 的 `etc/ramfb` 条目，QEMU 就把那块内存当作屏幕内容显示出来。对内核来说，画图就是往一段内存里写 `u32`。

代价是要先会跟 `fw_cfg` 打交道，而这一层在 6.2 版本的 QEMU 上坑不少（见 10.3）。

### 10.2 三层接口

```
内核 fb 模块
  │
  ├── fw_cfg 目录：找 "etc/ramfb" → 拿到它的 selector
  ├── fw_cfg 读：DMA 读 28 字节配置
  └── fw_cfg 写：DMA 写回 28 字节配置（addr / fourcc / flags / w / h / stride）
```

`fw_cfg` 的 MMIO 地址在 `0x0902_0000`（DTB 里写作 `fw-cfg@9020000`）：

- `+0x00` data 寄存器，8 字节，只接受 8 字节对齐访问
- `+0x08` selector 寄存器，2 字节
- `+0x10` DMA 地址寄存器，8 字节

DMA 描述符 16 字节：`{ u32 control; u32 length; u64 address; }`，字段**大端**。`control` 位定义：`bit0`=error、`bit1`=read、`bit2`=skip、`bit3`=select、`bit4`=write。写完描述符后把它的物理地址写进 `+0x10`，QEMU 自己填缓冲区，`bit0` 清零表示完成。

### 10.3 四个踩过的坑（都真实发生过）

1. **selector 要预反转**。设备按"大端设备"建模，AArch64 侧是小端访问，QEMU 会把写入值再按大端反转一次。直接写 `0x0022` 到设备那侧变成 `0x2200`，选不中任何条目。正确做法是 `key.to_be()` 先反转，抵消设备的反转。
2. **文件目录的 key 是 `0x0019`，不是 `0x0022`**。`0x0022` 是目录里某个文件项的 selector，选它会读到别的东西（当初读到一段 `RSD TPR ` 的垃圾）。用 `-trace "fw_cfg*"` 能看到 QEMU 自己打印的 `fw_cfg_add_bytes key 0x0019 'file_dir'`。
3. **8 字节 MMIO 读和多字段结构会错位**。目录是 `u32 count` + 每项 64 字节（`u32 size | u16 select | u16 reserved | char name[56]`），用 8 字节块一块块读，很容易和内部游标对不上。改成 DMA 整块搬 2052 字节后一次就对。
4. **ramfb 配置字段要大端**。fw_cfg 的写回调交给设备的值按大端解释，配置里 `addr=0x5000_0000` 若按小端写，设备收到的是 `0x0000005000000000`，屏幕就一直停在 "Guest has not initialized the display (yet)"。

另外 `fw_cfg` 目录里的字段本身也是大端（QEMU 用 `cpu_to_be32` 写进去的），只有一个缓冲里混着"大端字段"和"原样字节流"两种东西，读的时候要分开对待。

### 10.4 framebuffer 与绘图

- 物理地址 `0x5000_0000`（512 MiB RAM 的高段，避开内核、用户槽和 AFP 暂存区）
- 1024×768，`XRGB8888`，stride 4096 字节，共 3 MiB

绘图只有四个原语：`fill_rect`、`frame_rect`、`clear`、`draw_glyph`。上层 `draw_text` 按字符分流：ASCII 走 8×16 点阵，汉字走 16×16 点阵，宽度按 8/16 递进，所以中英文能混排在同一行。

字模不是手写的，是 `tools/genfont.py` 用系统字体渲染出来的：

```bash
python3 tools/genfont.py     # 生成 kernel/src/font.rs（ASCII 95 + 汉字 74）
```

只生成界面上真正用到的汉字（`CJK_CHARS` 列表），一个汉字 32 字节，74 个才 2.4 KiB。想加字就往那个字符串里加一个字，重跑脚本、重编内核。

### 10.5 远程链路

```
                    ┌─────────────┐
   手机 VNC 客户端 ──►│  QEMU :5901 │──► ramfb（内核写的帧缓冲）
                    │             │
   手机/电脑 nc    ──►│  QEMU :4444 │──► PL011 UART（引导器/内核命令行）
                    └─────────────┘
```

要说清楚边界：**网络协议栈不在 Aster 内核里**。现在的"远程"是 QEMU 在宿主侧把串口和显示转成 TCP / VNC，内核自己仍然只会写 UART 和帧缓冲。内核里跑 TCP/IP、跑 SSH，那是后面阶段的事。

`make run-gfx` 用 `-display vnc=0.0.0.0:1` 和 `-serial tcp:0.0.0.0:4444,server,nowait` 启动；`make shot` 则用 monitor 的 `screendump` 抓 PPM 再转 PNG，用于无头验证。

### 10.6 这一块还没做的

- 鼠标：键盘已经有了（见 §12），鼠标还没有，窗口拖不动、点不了
- 多窗口 / 合成器：现在是"单窗口桌面"，没有真正的窗口管理器
- 面板不是"实时"的：`run` 期间 shell 被阻塞在任务循环里，界面要等命令回来再按 `gfx` 重绘，所以任务表反映的是"上一次运行之后"的状态，不是运行中。EL1 期间的中断走 5 号槽，不触发调度（见 §9）


---

## 11. 网络

### 11.1 为什么又选了 virtio-mmio

上一节为了图形绕开 PCI，这次也一样。`virt` 机器把 32 个 virtio-mmio 槽摆在 `0x0a000000` 起、每槽 `0x200` 字节，设备倒序分配（第一个挂上的在第 31 槽，即 `0x0a003e00`）。走 mmio 的好处是不用 ECAM 枚举、不用 BAR 映射、不用 MSI-X，坏处是**没有 PCI 的 capability 描述**，版本行为完全依赖设备。

### 11.2 那个让人误判一整晚的 legacy 坑

QEMU 6.2 的 virtio-mmio 默认 `force-legacy=1`，于是设备自报 `Version=1`。按 legacy 规范写：

```
QueueSel=0 → QueueNum=8 → QueueAlign=4096 → QueuePFN=mem>>12 → Status|=DRIVER_OK → QueueNotify=0
```

全部写进去，寄存器读回也全对，`Status=0x7`（ACK|DRIVER|DRIVER_OK），`InterruptStatus=0`，**但 `used.idx` 永远是 0**。表现为：

- 驱动初始化"成功"，MAC 从 config 空间读得到（`52:54:00:12:34:56`）
- RX 挂了 8 个缓冲（`avail.idx=8`）
- 收发计数 `0/0`，宿主 `curl` 12 秒超时 `HTTP 000`
- `-trace "virtio*"` 显示 690 万次读、16 次写，`0x50` notify 确实到达了设备

也就是说：设备收了 notify、认了 DRIVER_OK，却从不消费描述符。试过把 `QueuePFN` 先清 0 再设、把 QSIZE 换成 64、确认 desc 页对齐与物理可达——都没用。

**解法**：加 `-global virtio-mmio.force-legacy=false`，设备变成 `Version=2`，走 modern 队列寄存器（`QueueDescLow/High`、`QueueDriverLow/High`、`QueueDeviceLow/High` + `QueueReady=1`）。同一份驱动逻辑立刻正常：`curl` 拿到 `200`，收发计数立刻动起来。

代码里两条路径都保留了（`virtio::setup_queue_legacy` / `setup_queue`），但**默认启动一律 modern**，`make run-net` / `tools/nettest.py` 都带这个参数。

### 11.3 队列与内存布局

RX 与 TX 各一块 8 KiB 的页对齐静态区（`#[repr(C, align(4096))]`），内部切成 desc（`8×16=128`）、avail（紧跟其后）、used（固定在偏移 4096）。QSIZE=8，每帧缓冲 2048 字节，RX 用 `DESC_F_WRITE`，TX 用 `DESC_F_NEXT` 串两段（virtio-net 头 + 帧体）。

`VIRTIO_NET_F_MRG_RXBUF` 与 `VIRTIO_NET_F_MAC` 参与协商；`HDR_LEN=12`（协商了 MRG 时头是 12 字节，没协商是 10）。

### 11.4 协议栈是自己写的

按依赖顺序实现，每步都能在串口里看到证据：

1. **ARP**：开机先广播三次 `who has 10.0.2.2`。这一步不是可选的——slirp 只有在收到 guest 的帧之后才学到"这个 IP 对应这个 MAC"，在此之前它拿到宿主侧连接也没有目的地址可填，SYN 根本送不下来。少了这三次喊话，现象同样是"收帧 0"。
2. **ICMP**：应答 echo，验证链路。
3. **TCP**：三次握手、序号/确认号、ACK、FIN、单连接。校验和按 RFC 1071 反码求和。
4. **HTTP/1.1**：解析请求行与方法，返回编译进镜像的中文页面（`HTTP_PAGE` 用 `r#""#` 原样串，因为 HTML 里有 `#`）。

驱动层与协议层之间只有两个函数：`send_frame(&[u8])` 和轮询 `pop_used()`。`net::poll()` 挂在 shell 的空闲循环里，所以只要系统在等输入，网络就在收包。

### 11.5 边界

- 没有重传、没有滑动窗口、没有乱序处理；丢了就丢了
- 没有 UDP / DNS / DHCP（地址是写死的 `10.0.2.15`，slirp 的固定网段）
- 没有 TLS
- 一次只维护一条连接，`建立连接` 计数只增不减（不复用端口）
- 端口从宿主进 guest 仍需 QEMU `hostfwd`，因为 guest 在 NAT 后面

---

## 12. 输入设备与桌面

### 12.1 事件模型

`virtio-keyboard-device`（DeviceID 18）只有一条队列：queue 0 = eventq，方向"设备→驱动"。每个事件 8 字节、全部小端：

```
u16 type  0=EV_SYN  1=EV_KEY  2=EV_REL
u16 code  Linux 键码（30='a'、42=LShift、58=CapsLock…）
u32 value 1=按下 0=抬起 2=自动重复
```

挂 8 个 8 字节缓冲，消费一个立刻归还并 `notify(0)`——**不归还就会在敲十几个键之后彻底卡住**，因为队列只有 8 格。

### 12.2 键码到字符

`key_to_char` 是一张 `Linux code → (无 Shift, 有 Shift)` 的表：字母走大小写映射，符号（`1`↔`!`、`[`↔`{`）走 Shift 分支。`CapsLock` 只影响字母，`Shift` 与 `CapsLock` 用异或合成。`Enter/Backspace/Tab/Esc` 翻译成 `\n / 0x08 / \t / 0x1B`，其余丢弃。

字符进一个 128 字节的环形缓冲。`shell::read_line` 的循环变成三路：**先取键盘环，再取串口，都没有就自旋**。这样键盘和串口可以同时用，谁先到算谁的。

### 12.3 屏幕终端与重绘节流

串口输出与屏幕共用一份内容：`uart::puts()` 在写 PL011 之前先 `term::write(s)`。终端缓冲是 `26×88` 个 `u16`：

- `0` = 空
- `<0x80` = ASCII
- 其它 = Unicode 码位
- `0xFFFF` = "宽字符第二格"——汉字占两格，保证中英混排行列对齐

全屏重绘一次约几十毫秒（1024×768 的填充 + 2288 个格子的字模），所以不按帧刷：`term` 写入时置脏标记，`ui::tick()` 在 shell 等待输入的空闲里检查，**脏 + 距上次 ≥60 ms** 才重画。

### 12.4 布局与配色

1024×768，深色系：

| 区域 | 位置 | 内容 |
|---|---|---|
| 顶栏 | y=0..28 | `ASTER 星芒 v0.5 ARM64 原创内核` + 右 `EX3-…` 短指纹 |
| 图标列 | x=24，y=76 起 | 终端 / 系统 / 网络 / 关于（入口占位） |
| 主窗口 | 140,60 848×600 | 标题栏（终端 TERMINAL + 三键）+ 终端内容 |
| 任务栏 | y=720..768 | 开始按钮、运行中程序（下方亮条）、键盘/网络状态灯、运行时长 |

壁纸用 48 条横带做的纵向渐变（`lerp(0x101a26 → 0x1d3149)`），右上两块微亮矩形打破平板感。强调色 `#4FC3F7`（青）、状态绿 `#7EE787`、警示橙 `#FFB86B`。

字模来自 `tools/genfont.py`：ASCII 8×16（DejaVuSansMono-Bold），汉字 16×16（arphic `uming.ttc`）。**字表按源码自动提取**——脚本扫 `kernel/src/*.rs` 里所有非 ASCII 字符，去重后写进 `CJK_CHARS`，当前 641 个汉字，随文案更新自动增减。

### 12.5 还没做的

- 鼠标与光标
- 窗口管理与多窗口
- 多终端会话（现在串口与屏幕是同一份流）
