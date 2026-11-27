# Aster 星芒 — 完整路线图

从零到 v1.0 的全部阶段。每个阶段写清 **目标 / 任务 / 验收 / 风险**，任务带编号，可直接当 issue 用。

> 记号：`✅` 已完成　`🔄` 进行中　`🔶` 部分完成　`⬜` 未开始
> 版本号只表示"阶段内的一个可用快照"，不表示功能完整度。

---

## 总览

| 阶段 | 主题 | 版本 | 状态 | 产出 |
|---|---|---|---|---|
| 一 | 启动与信任根 | v0.1–v0.2 | ✅ | `boot/`、AVP 三级校验、EX3、AFP 刷写、底层 shell |
| 二 | 内核基础 | v0.3–v0.5 | 🔄 | 内核、MMU/EL0/调度、图形、键盘、网络 |
| 2.5 | 界面精修 | v0.6 | 🔄 | 抗锯齿渲染、圆角与投影、鼠标、窗口管理 |
| 三 | 存储与文件系统 | v0.6.5–v0.7 | ⬜ | virtio-blk、AsterFS、VFS、从盘加载内核与程序 |
| 四 | 进程与用户态生态 | v0.7 | ⬜ | ELF 加载器、fork/exec、AsterLibc、动态链接 |
| 五 | 桌面成熟 | v0.8 | ⬜ | 鼠标、窗口管理、应用框架、内置应用 |
| 六 | 网络应用 | v0.8.5 | ⬜ | DNS、TLS、HTTP、浏览器、包管理 |
| 七 | 跑 APK | v0.9 | ⬜ | DEX 解释器、JNI、资源、装包、沙箱 |
| 八 | 转译跑 EXE | v0.9.5 | ⬜ | PE 加载、Win32 翻译层、x86→ARM64 转译 |
| 九 | 真机与发布 | v1.0 | ⬜ | 树莓派 4、安装镜像、硬件信任根、CI、发行 |

---

## 阶段一：启动与信任根 ✅

已交付（`8225c5f` → `7032840`）。

- **P1-1** ✅ 两级启动：`boot/`（C + AArch64 汇编，约 12 KiB）与内核分离，引导器先行、内核后接
- **P1-2** ✅ 自研 SHA-256 / HMAC-SHA256（`boot/sha256.c`），零第三方依赖
- **P1-3** ✅ AVP 三级校验：L1 完整性、L2 真实性、L3 链绑定（含 EX3）
- **P1-4** ✅ EX3 原创校验值：`HMAC(K, BOOT_ID ‖ "EX3" ‖ build_id ‖ nonce)`，内核常量时间复算，不一致即 `wfe` 停机
- **P1-5** ✅ 底层 Recovery 模式（`boot/shell.c`）：`verify / hash / ex3 / flash / boot / dump`
- **P1-6** ✅ AFP 串口刷写（`boot/flash.c` + `tools/afp-flash.py`），分块校验 + 重传
- **P1-7** ✅ 混搭实测：换内核保引导器 → L1/L2/L3 全 PASS，内核 `EX3 ... FAIL` 拒绝接管

**验收**：`make test` 中 L1/L2/L3/EX3 断言全 PASS；`tools/test-afp.py` 刷写往返一致。

---

## 阶段二：内核基础 ✅（v0.3–v0.5，已收口）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 1 | EL0 用户态 | ✅ | 用户程序链接 `0x4440_0000` / `0x4460_0000`，由 `build.rs` 内嵌 |
| 2 | SVC 系统调用 | ✅ | 64 write / 93 exit / 172 getpid，`ESR.EC=0x15` 分流 |
| 3 | 页表权限隔离 | ✅ | 内核页 `AP=00`，用户代码 `AP=01 UXN=0`、栈 `AP=01 UXN=1`，guard 页不映射 |
| 4 | 每任务独立 TTBR0 | ✅ | 一任务 5 页（L1/L2/L3 + code + stack），同 VA 不同 PA，`iso` 证明不串 |
| 5 | 页帧分配器 | ✅ | 位图管 128 MiB / 32768 页，池 0x4800_0000..0x5000_0000，分配即清零 |
| 6 | 抢占调度 | ✅ | GICv2 PPI 30 + CNTP，2 ms 片，34 字陷阱帧，round-robin |
| 7 | 系统调用扩展 | 🔶 | 缺 `read / brk / mmap / open / close / clock / yield` |
| 8 | 图形输出 | ✅ | fw_cfg DMA + ramfb，1024×768 XRGB8888，自研绘图原语与字模 |
| 9 | 远程访问 | ✅ | VNC :1 + 串口 TCP :4444 + monitor socket 截屏 |
| 10 | 输入设备（键盘） | ✅ | virtio-input，eventq 8 缓冲，Linux 键码→ASCII，Shift/CapsLock |
| 11 | 网络栈 | ✅ | virtio-net（modern 队列）+ 自研 ARP/ICMP/TCP + HTTP，宿主 `curl` 200 |
| 12 | FP/NEON 上下文 | ✅ | 陷阱帧 272 → 800 字节，存 q0–q31 + FPSR/FPCR；`fp` 命令证明不串 |
| 13 | W^X（用户侧） | ✅ | 代码页 `AP=0b11 + UXN=0` 只读可执行；`wx` 命令证明写不进去 |
| 14 | 每任务内核栈 | ⬜ | 现在共用内核栈；因每次中断都整帧拷进 Task.ctx 才没出事，等 syscall 支持阻塞就必须补 |
| 15 | **多核启动（SMP）** | ⬜ | PSCI `CPU_ON` 拉起其余核，每核独立栈与 GIC 重定向 |
| 16 | 内核 panic 屏 | ⬜ | 内核异常别直接 `wfe`，落到可读的崩溃页并能重启 |

**验收（阶段二收口）**
- `make test` 扩到 10 项：原 7 项 + 页帧分配 + 地址空间隔离 + 浮点上下文不串
- 两个任务各跑浮点，结果互不污染
- 任务写自己的代码页被 MMU 拦下

**风险**：#4 与 #5 必须一起做；#12 要加宽陷阱帧（272 → 784 字节），会牵动调度器与用户态入口。


---

## 阶段 2.5：界面精修（v0.6）🔄

目标：从"能看"到"耐看"。全程软件渲染，没有 GPU、没有合成器，所有效果都要靠逐个像素算出来。

| # | 任务 | 状态 | 做法与验收 |
|---|---|---|---|
| UI-1 | 灰度字模 | ✅ | 4-bit alpha 字模（ASCII 8×16=64 B/字，汉字 16×16=128 B/字），绘制时按 alpha 混合。663 汉字共 ~90 KiB |
| UI-2 | 圆角与抗锯齿 | ✅ | 角落按覆盖率混合，`isqrt` 整数开方（`f32::sqrt` 在 no_std 拿不到） |
| UI-3 | 柔和投影 | ✅ | 三层递减 alpha，只在窗口边缘外露出 |
| UI-4 | 壁纸与质感 | ✅ | 非线性竖直渐变 + 两团柔光；图标卡内高光/内暗边；标题栏高光带 |
| UI-5 | 配色与信息层级 | ✅ | 统一色板（底 `#0a1220` / 面板 `#121d29` / 强调 `#5cc8f5` / 成功 `#7ee787`），窗口底部状态行 |
| UI-6 | **鼠标** | ✅ | virtio-mouse（DeviceID 18 第二实例，EV_REL + EV_KEY），16×16 光标图层（存底-贴图-还原，移动不重绘桌面），点击环形队列，拖拽标题栏 |
| UI-7 | **窗口管理** | 🔄 | 拖拽标题栏 + 脏矩形（静态层缓存 3 MiB，拖动只擦并集区域）、壁纸离屏缓存、重绘入口统一；多窗口/z 序/焦点/三个圆点按钮仍未做 |
| UI-9 | 时钟与权限提示 | ✅ | RTC(PL031) 读 UTC+8 显示到分钟、横幅一行化、提示符按 Extreme 权限在 `$`/`#` 切换 |
| UI-8 | 过渡与反馈 | ✅ 部分 | 窗口淡入淡出（alpha 逐帧）、悬停变亮、按钮按下态、光标闪烁、托盘图标动效 |

**验收**
- 目视：桌面截图与上一版对比，文字边缘无锯齿、窗口有圆角与投影
- 性能：1024×768 全屏重绘 < 30 ms（QEMU TCG 下），VNC 观看不明显卡顿
- `make test-gfx` 三项不变；新增 `make test-ui`：monitor `sendkey` 注入鼠标点击，断言焦点切换

**风险**
- 软件渲染的填充率是真瓶颈：圆角+投影+文字都是 alpha 混合，像素写次数比原来高 3–5 倍。缓解：脏矩形重绘（只重画变化区域）、壁纸缓存成离屏位图、窗口内容双缓冲
- 鼠标事件队列只有 8 个缓冲，快速拖动会丢事件；要改成环形队列 + 每帧批量取出

---

## 阶段三：存储与文件系统（v0.6.5–v0.7）

目标两件事，按风险拆成 A、B 两个里程碑：

- **A（内核侧，低风险）**：内核从磁盘读文件，用户程序不再内嵌，`load` 命令直接从盘加载并运行
- **B（引导器侧，高风险）**：引导器自己从磁盘读内核镜像，`kernel.ast` 不再由 `-kernel` 塞进去

### A. 内核侧的磁盘栈

| # | 任务 | 要点 |
|---|---|---|
| AF-1 ✅ | 磁盘镜像格式 `AsterDisk` | 512 B 扇区；扇区 0 放磁盘头（magic `ADSK` + 版本 + 分区数 + CRC32），扇区 1 放分区表（4 项，每项 起始 LBA / 扇区数 / 类型 / 名字 16 B）。分区 0 固定为 AsterFS。生成工具 `tools/mkdisk.py` |
| AF-2 ✅ | virtio-blk 驱动（Rust） | DeviceID 2，复用 `virtio.rs` 的 modern 队列；一次请求三段：请求头（16 B：类型/保留/sector/数据长度）+ 数据 + 状态字节。读走只读请求（类型 0），写走类型 1 |
| AF-3 ✅ | **AsterFS v0** | 4 KiB 块；块 0 超级块（`AFS1` magic + 块大小 + 块数 + inode 数 + 根 inode + CRC32）；块 1..N inode 表（每 inode 64 B：类型/大小/直接块指针 12 个/时间）；位图块；数据块。**目录项就是 inode 里的 `名字→inode` 列表**，不做 B 树 |
| AF-4 | 块缓存 | 32 个 4 KiB 缓冲 + LRU，读命中直接返回，写标脏，`sync` 刷盘。内存从页帧池要（`alloc_pages`） |
| AF-5 | VFS 薄层 | `mount / lookup / open / read / write / close / readdir / stat`，fd 表每任务一份（放在 `Space` 旁边），路径解析最多 8 层 |
| AF-6 | syscall 接上 | 56 openat / 57 close / 62 lseek / 63 read / 80 fstat，编号与 Linux 一致 |
| AF-7 | shell 命令 | `ls / cat / load / sync / df`。`load <名字>` 把盘上程序读进新地址空间并跑起来 |
| AF-8 | 用户程序脱离内嵌 | 删掉 `build.rs` 里的 `include_bytes!`，六个测试程序改为 `mkdisk.py` 打包进镜像；`iso / fp / wx / yield` 全部改成从盘加载，**这一条是"从盘加载"的真正验收点** |

### B. 引导器从盘加载内核

| # | 任务 | 要点 |
|---|---|---|
| AF-9 | boot 的 virtio-blk（C） | `boot/` 里独立实现一遍 MMIO 探测 + modern 队列（不复用内核代码，引导器必须自足），只读 |
| AF-10 | boot 的 AsterFS 只读解析（C） | 与内核同一磁盘布局，按名字找 `kernel.ast`，读进 `0x40200000` |
| AF-11 | 信任根演进 | 原来 hash 编译期内嵌（`manifest.h`）；改成构建时 `aster-sign.py` 把 `{hash, build_id, nonce, EX3}` 写进镜像的元数据区。引导器读盘算 SHA-256 比对，再用内置 K 验 EX3；内核启动后自己复算一遍 EX3 —— 链路一环不少，只是数据来源从 `.rodata` 变成磁盘 |
| AF-12 ✅ | `make run-disk` | `-drive if=none,file=build/aster.img,format=raw,id=d0 -device virtio-blk-device,drive=d0`，去掉 `-kernel`，用 `-device loader,file=...` 只装引导器 |
| AF-13 | 挂载校验与只读修复 | 超级块 CRC 不符 → 只读模式 + 打印差异，不自动"修复"（先保证不写坏） |

**验收**
- A：`make test-fs` —— 建文件 → 写 1 MiB → 卸载重挂 → 逐字节一致；中断 QEMU 后重挂不崩
- A：启动日志里 `kernel` 与六个用户程序全部来自磁盘（`load` 出来的程序能跑）
- B：`make run-disk` 屏幕上仍显示同一 EX3 指纹，`AVP L1/L2/L3` 全 PASS，但内核来源是磁盘
- B：把 `kernel.ast` 换成一个改过的镜像，引导器必须拒绝（`hash mismatch`）

**为什么分 A、B**
A 只碰内核，出错最多是"读不出来"，回退一行代码就能恢复内嵌；B 碰的是引导器和信任根，一旦校验逻辑写错就是"整机起不来"，必须放在 A 稳定之后。

**时间**：A 约 2 周（每天 1–2 小时），B 约 2 周。

**风险**
- virtio-blk 和 virtio-net 共用 MMIO 扫描逻辑，容易和设备排序打架（现在设备是倒序挂到 slot 31），要按 DeviceID 而不是槽位找设备
- AsterFS 一旦写进镜像就是"事实标准"，布局必须在 `docs/asterfs.md` 写死再动手，中途改布局要重新生成所有镜像
- 从盘加载内核后，`build.rs` 里那套内嵌流程要保留成 `--features embedded` 备选，否则树莓派（SD 卡启动）路径会断

---

## 阶段四：进程与用户态生态（v0.7）

目标：用户程序从文件系统加载；有真正的进程、地址空间、C 库。

| # | 任务 | 要点 |
|---|---|---|
| P4-1 | ELF64 加载器 | 解析 PT_LOAD，按 p_flags 设页属性，bss 清零，跳入口 |
| P4-2 | 地址空间对象 | `AddressSpace`：独立 TTBR0、VMA 列表、按需映射（缺页 `EC=0x24`） |
| P4-3 | `fork / exec / wait` | COW 页、写时复制、僵尸回收 |
| P4-4 | 线程与 `clone` | 共享地址空间、独立栈、TLS（`TPIDR_EL0`） |
| P4-5 | **AsterLibc** | 原创 C 库：slab `malloc`、`printf`、`string`、`stdio`、`pthread` 基础；静态链接先行 |
| P4-6 | 动态链接器 | `ld-aster.so`，`.dynamic` 解析 + `R_AARCH64_RELATIVE/GLOB_DAT/JUMP_SLOT` |
| P4-7 | 信号 | `kill`、默认处置，`SIGSEGV` 由权限/缺页异常映射 |
| P4-8 | 时钟与随机 | `clock_gettime`（CNTPCT）、`getrandom`（CNTPCT 抖动 + EX3 nonce 混合） |
| P4-9 | 用户态工具 | `ash`（shell）、`ls`、`cat`、`echo`、`init`，全用 AsterLibc 编 |

**验收**
- `make test-proc`：`fork` 后父子各打印、`exec` 换程序、`wait` 拿到正确退出码
- 用户态 `ash` 里 `ls /` 并执行 `/bin/echo hello`
- 动态链接能加载带 `.so` 依赖的测试程序

**风险**：全项目最容易失控的一段。**顺序锁死**：静态链接单进程 → fork → 动态链接，任何一步不稳不往下走。

---

## 阶段五：桌面成熟（v0.8）

目标：从"一张图"变成能用的桌面——鼠标、多窗口、应用框架、内置应用。

| # | 任务 | 要点 |
|---|---|---|
| P5-1 | virtio-mouse | DeviceID 18 第二个实例，`EV_REL` 位移 + `EV_KEY` 左键，软件光标 + 加速曲线 |
| P5-2 | 窗口管理 | 窗口树、Z 序、拖拽、焦点、最小化/最大化/关闭、Alt-Tab |
| P5-3 | 合成器 | 双缓冲 + 脏矩形，替代整屏重画 |
| P5-4 | 控件库 | 按钮、输入框、列表、滚动条、对话框，统一主题 token |
| P5-5 | 应用框架 | `aster-app`：事件循环、窗口创建、绘制上下文；一个 app 一个 ELF |
| P5-6 | 内置应用 | 终端（真 shell）、文件管理器、文本编辑器、系统监视、设置 |
| P5-7 | 字体与图标 | 灰度抗锯齿、图标集、中文字库按需加载（不塞进内核） |
| P5-8 | 桌面服务 | 壁纸、任务栏配置、托盘、通知 |

**验收**
- 鼠标能拖窗口、点按钮触发动作
- 两窗口叠加遮挡区重绘正确（脏矩形测试）
- 终端 `ls` 与文件管理器显示一致

**风险**：光标与拖拽需要"输入事件队列 + 主循环"，会和现在阻塞式 `read_line` 冲突——P5-2 前必须先把 shell 改成非阻塞行编辑。

---

## 阶段六：网络应用（v0.8.5）

| # | 任务 | 要点 |
|---|---|---|
| P6-1 | 协议栈加固 | 重传、滑动窗口、乱序、校验和、`SOCK_STREAM` 抽象 |
| P6-2 | UDP + DNS | `A`/`AAAA` 查询、缓存、超时重试 |
| P6-3 | **TLS** | 引入可审计实现（BearSSL / mbedTLS 精简）或自研，证书链校验 |
| P6-4 | HTTP 客户端 | GET/POST、分块、重定向、最小 Cookie |
| P6-5 | 浏览器 | HTML 子集解析 + 布局 + 文本/图片渲染，无 JS；够看文档与本站页面 |
| P6-6 | 包管理雏形 | `aster-pkg`：网络拉 `.apkg`，校验签名后落盘安装 |

**验收**：DNS 解析正确；`https://` 页面能拉下来；TLS 拒绝过期证书。

**风险**：TLS 是安全敏感件，必须在 `docs/licenses.md` 写清引入来源与许可证。

---

## 阶段七：跑 APK（v0.9）

### APK 到底需要什么

APK 是 ZIP，里面：

```
AndroidManifest.xml   （二进制 AXML）
classes.dex           （Dalvik 字节码，可能多个）
resources.arsc        （资源表）
lib/arm64-v8a/*.so    （native 库，可选）
META-INF/             （签名）
```

| 依赖 | 是否必需 | 难度 |
|---|---|---|
| ZIP 解包 + AXML 解析 | 必需 | 低，自己做 |
| DEX 解析 | 必需 | 中，格式公开 |
| Dalvik 解释器 | 必需 | 中高，约 200 条指令 |
| 最小 bionic 兼容 libc | native 库才要 | 中 |
| ART / JNI / Binder / SurfaceFlinger | 看应用 | 极高 |

**路线 A（推荐）— 自研 `AsterDex` 解释器**

| # | 任务 | 要点 |
|---|---|---|
| P7-1 | DEX 解析 | 头、string/type/proto/method/class_def 表、`CodeItem` |
| P7-2 | 解释器 | Dalvik 指令子集（mov/invoke/if/field/new-instance/return） |
| P7-3 | 对象模型 | `Object/String/Class` 最小实现、字段布局、vtable |
| P7-4 | JNI 桥 | Java 方法 ↔ AsterLibc / syscall |
| P7-5 | 资源系统 | `resources.arsc` 解析 + 简版 layout（能画基本控件） |
| P7-6 | 安装 | 解包、校验、dex 落到 `/system/app/<pkg>/`、AXML 简版解析 |
| P7-7 | 沙箱与权限 | 按 manifest 授予文件/网络，越权拒绝 |
| P7-8 | 图形桥 | `Canvas`/`SurfaceView` 映射到 Aster 窗口 |

**路线 B（重）— 移植 ART**：需要 bionic、`libart`、`libc++`、`libziparchive`、`libunwind`……约为 A 的 5–10 倍工作量，且大量 Google 代码要逐项审许可证。第一版不追。

**路线 C（最轻）— 只跑无 UI 的 APK**：跳过资源与 Canvas，只支持纯计算/网络类小工具。

**验收（A 第一里程碑）**：装一个自制 30 行 Java 小 APK（一个按钮 + Toast），点击后弹窗；`make test-apk` 断言窗口出现。

**风险**：Android 生态重度依赖 framework 类，真机 APK 大概率跑不起来。**诚实目标**是"能跑我们自制的、指令子集内的 APK"，文档写清兼容边界。

---

## 阶段八：转译跑 EXE（v0.9.5）

### EXE 到底需要什么

Windows PE + x86-64 指令，在 ARM64 上必须过两关：

1. **指令关**：x86-64 → AArch64 翻译
2. **系统调用关**：Win32 API → 我们的 syscall

现实解法是 `Wine + FEX-Emu`。自研的合理拆法：

| # | 任务 | 要点 |
|---|---|---|
| P8-1 | PE/COFF 加载器 | DOS 头 → NT 头 → 节表，重定位、导入表、TLS 回调 |
| P8-2 | **x86-64 → ARM64 转译** | 静态二进制翻译（基本块缓存）+ 解释回退；先覆盖整数与 SSE2 子集 |
| P8-3 | Win32 翻译层 | `kernel32`/`user32`/`gdi32` 最小集：文件、进程、窗口、消息泵、GDI |
| P8-4 | 注册表与路径虚拟化 | 私有目录树，API 打到虚拟注册表 |
| P8-5 | 控制台程序先行 | 先让 CLI 工具跑起来 |
| P8-6 | GUI 程序 | 窗口类、消息泵映射到 Aster 窗口管理器 |
| P8-7 | 兼容性矩阵 | 每支持一个 API 记一行，做成表，不吹 |

**验收**：自制 `hello.exe`（MinGW/`WriteConsoleA`+`ExitProcess`）能打印并退出；自制 Win32 窗口程序能弹窗、按钮有响应；转译缓存命中率与速度记进 `docs/translation.md`。

**风险**：x86 指令集无穷，转译是长期工程。里程碑定为"能跑我们编的几个 exe"，不是"兼容 Office"。

---

## 阶段九：真机与发布（v1.0）

| # | 任务 | 要点 |
|---|---|---|
| P9-1 | 树莓派 4 启动 | 固件 `config.txt` + `kernel8.img`，串口 UART0 引脚 |
| P9-2 | BCM2711 适配 | GIC-400 地址、EMMC/SD 驱动、`mailbox` framebuffer |
| P9-3 | 安装镜像 | `aster-install.img`：引导器 + 内核 + AsterFS，可 `dd` 写 SD 卡 |
| P9-4 | 首次启动向导 | 语言、网络、本地账户（无云） |
| P9-5 | A/B 更新 | 双分区 + EX3 校验，失败自动回滚 |
| P9-6 | 硬件信任根 | 引导器自身签名、Ed25519 替换 HMAC 共享密钥、OTP 单调计数器防回滚 |
| P9-7 | CI | GitHub Actions：交叉编译 + `make test`/`test-gfx`/`test-net` + 产物归档 |
| P9-8 | 文档收口与发布 | 架构、AsterFS、ABI、贡献指南、许可证审计表；tag `v1.0` 附镜像与校验和 |

**验收**：树莓派 4 从 SD 卡启动进桌面，键鼠可用、能上网、能装一个自制 APK，`make test` 全绿。

---

## 时间线（单人、每周 10–15 小时）

| 周 | 内容 |
|---|---|
| 1–2 | 阶段二收口：页帧分配器 + 每任务页表 + FP 上下文 + W^X |
| 3–5 | 阶段三：virtio-blk、AsterFS、VFS、fsck |
| 6–10 | 阶段四：ELF 加载、fork/exec、AsterLibc、动态链接、ash |
| 11–14 | 阶段五：鼠标、窗口管理、控件、内置应用 |
| 15–17 | 阶段六：DNS、TLS、HTTP、浏览器子集 |
| 18–26 | 阶段七：DEX 解析、解释器、JNI、资源、装包 |
| 27–36 | 阶段八：PE 加载、转译、Win32 子集 |
| 37–42 | 阶段九：树莓派、镜像、CI、发布 |

> "一个人业余时间"的量级。有团队可压缩，但阶段七、八的本质工作量压不下去。

---

## 三条底线

1. **不基于任何现有内核**：内核代码全自写；用户态引入第三方（TLS、字体、压缩）必须在 `docs/licenses.md` 逐项登记。
2. **每步可验证**：阶段结束必须有 `make test-*`，没有断言的功能不算完成。
3. **诚实边界**：文档永远保留"明确还没做的"一节，不把"能跑一个自制 APK"写成"兼容安卓"。

---

## 当前位置

- 已完成：阶段一全部；阶段二 1/2/3/4/5/6/8/9/10/11/12/13；sched_yield(124)
- 当前 v0.6，正在做阶段 2.5：
  1. ✅ UI-1 灰度字模 / UI-2 圆角抗锯齿 / UI-3 投影 / UI-4 壁纸质感 / UI-5 配色与状态行
  2. UI-6 鼠标已落地，下一步 UI-7 窗口管理（拖拽要配脏矩形）→ UI-8 过渡动效
  3. 之后进阶段三 A（virtio-blk + AsterFS + 从盘加载用户程序），再进 B（引导器从盘加载内核）
- 阶段二遗留（不急，等需要时补）：#7 其余 syscall、#14 每任务内核栈、#15 SMP、#16 panic 屏
- 每次提交前跑：`make test && make test-gfx && make test-net`
