# 路线图

分五个阶段。每个阶段都有明确的"做完怎么算做完"，避免堆半成品。

---

## 阶段二：用户态（先把 EL0 跑起来）

现在全部代码在 EL1。任何应用都要先有用户态。

必须做的：

| # | 事项 | 状态 | 说明 |
| --- | --- | --- | --- |
| 1 | **EL0 切换**：`SPSR_EL1` 设 `M=EL0t`，`ELR_EL1` 设用户入口，`ERET` | ✅ 已完成 | `aster_enter_user`，进 EL0 前清空 x0–x30 |
| 2 | **系统调用**：`SVC #0`，走向量表 8 号槽按 `x8` 分发 | ✅ 已完成 | write/exit/getpid，用 Linux 号 64/93/172 |
| 3 | **页表级隔离**：内核页 `AP=EL1-only`，用户页 `AP=EL0RW` | ✅ 已完成 | L1→L2→L3 拆开，`runf` 实测越权被硬件拦下 |
| 4 | **每个进程独立地址空间**：每任务一套 TTBR0 页表 + 切 ASID | ⬜ 未做 | 现在是同一张表靠 AP 位隔离 |
| 5 | **物理页分配**：位图式页帧分配器（从 DTB memory node 切） | ⬜ 未做 | 现在的用户页是静态写死的 |
| 6 | **进程与调度**：`task struct` + 定时器中断 + round-robin | ✅ 基本完成 | GICv2 PPI 30 + CNTP 2 ms 时间片；`runp` 证明纯计算任务不会饿死别人。缺优先级/阻塞/每任务内核栈 |
| 7 | **最小 syscall 集合**：`write/read/exit/brk/mmap/open/close` | 🔶 部分 | 已有 write/exit/getpid，缺 read/brk/mmap/open |

验收：能跑一个 EL0 的 `hello` 程序，从用户态 `write(1, ...)` 输出到串口。

**这个验收已经达成**（`make test` 里断言 EL0 输出、退出码、越权拦截、双任务轮转、抢占五项）。但阶段二整体不能算完：还差 4/5/7，以及"FP 上下文保存""每任务内核栈"。

下一步优先级：**4（每任务独立页表）** 最要紧 —— 现在两个任务能互读对方的页，这是隔离上的真缺口；然后 5（页帧分配）跟上，最后补 syscall。

---

## 阶段三：能装东西

7. **驱动框架**：先 virtio（QEMU 上最好验证）
   - `virtio-blk`：块设备 → 文件系统
   - `virtio-net`：网卡 → lwIP 或无栈的裸以太网
8. **文件系统**：先做只读的 simplefs（自制，inode + 直接块），够用再上 FAT32
9. **ELF 加载**：AArch64 ELF64 静态可执行文件加载到用户空间
10. **用户态 shell + coreutils 精简版**：`ls/cat/echo/ps/kill`

验收：用户态 shell 里 `ls /` 能列出 simplefs 的文件，`cat` 能读出来。

---

## 阶段四：APK 与 EXE（重点，也是最难的部分）

### APK 到底需要什么

APK 是一个 ZIP，里面：

```
AndroidManifest.xml   （二进制 AXML）
classes.dex           （Dalvik 字节码，可能多个 dex）
resources.arsc        （资源表）
lib/arm64-v8a/*.so    （native 库，可选）
META-INF/             （签名）
```

要让它在 Aster 上跑，缺的东西按依赖顺序排：

| 依赖 | 是否必需 | 难度 |
| --- | --- | --- |
| ZIP 解包 + AXML 解析 | 必需 | 低，自己做 |
| DEX 解析（格式、常量池、方法体） | 必需 | 中，格式规范公开 |
| Dalvik 字节码解释器 | 必需 | 中高，约 200 条指令 |
| 一个最小 `libc`（bionic 兼容子集） | native 库才需要 | 中 |
| ART / JNI / Binder / SurfaceFlinger | 取决于应用 | 极高 |

所以有两条路：

**路线 A：自研轻量 DEX 解释器（推荐先走）**

- 只支持"无资源、纯 dex、不依赖 native so"的应用
- 自己实现 `java.lang` 最小子集（String/Object/System.out）
- 目标：能跑一个手写的 `HelloAster.apk`
- 工作量：约 3000–5000 行，2–3 个月量级

**路线 B：移植 ART**

- 需要先把 bionic libc 跑起来，再搬 ART、JNI、`libhwui`……
- 等于把 Android 运行时整个搬过来，工作量以人年计
- 只有真要跑普通商店 APK 才值得

建议先做 A 路线的 POC，把"能解析 dex、能解释执行、能调到我们的 syscall"这条链跑通。跑通之后再决定是否往 B 走。

### EXE 到底需要什么

Windows PE 可执行文件 + x86-64 指令。ARM64 机器上跑它，必须过两关：

1. **指令关**：x86-64 → AArch64 的翻译
2. **系统调用关**：Windows API（`kernel32.dll` 等）→ 我们的 syscall

现实世界的解法是 `Wine + FEX-Emu`（x86 翻译到 ARM64，Wine 提供 PE 加载和 Win32 实现）。

自研的话，合理的拆法是：

- **自己做 PE 加载器**（约 1000 行）：解析 DOS/PE 头、节表、重定位、导入表，把镜像铺到用户空间
- **指令翻译交给成熟后端**：预留 `translator` 接口，默认接 FEX-Emu；不自己做 JIT（那是另一个团队的工作量）
- **Win32 API 层**：只实现目标程序用到的 DLL 导出（`kernel32` 的 `CreateFile/ReadFile/WriteConsole` 等），按程序逐个补齐

验收：能跑一个用 MinGW 编译的 `hello.exe`（只依赖 kernel32 的 `WriteConsoleA` 和 `ExitProcess`）。

> 不要相信"一键跑 EXE"。能跑 `hello.exe` 和能跑 Photoshop 之间差着几个数量级。

---

## 阶段五：真机与硬件信任根

11. 树莓派 4/5 真机启动（现在是 QEMU 验证 + 地址适配）
12. UEFI 引导（替代裸机 `-kernel` 路径，为通用 ARM64 设备做准备）
13. SPI Flash / eMMC 真实刷写（现在是 RAM 暂存区）
14. **引导器自身签名 + BootROM 信任根**：解决"引导器可被替换"这个根本问题
15. Ed25519 非对称签名，替换 HMAC 共享密钥
16. 单调计数器（eFuse / OTP）防回滚

---

## 优先级建议

```
阶段二（用户态）  ──►  阶段三（驱动+文件系统）
                          │
                          ├─► APK 路线 A（自研 DEX 解释器）← 投入产出比最高
                          └─► EXE（PE 加载器 + FEX 后端）← 依赖外部转译器

阶段五（真机+信任根）随时可以插进来，因为校验链的地基已经打好了
```
