# Aster 移植指南

给愿意把 Aster 搬到自己板子上的人。目标很明确：**不基于任何现成内核**，所以移植工作是"接上驱动"，而不是"改设备树解析"。

## 1. 两条已经存在的目标

| 目标 | 状态 | 命令 |
| --- | --- | --- |
| QEMU virt（cortex-a72） | 可跑，有图形与磁盘 | `make run-gfx` / `make run-disk` |
| 树莓派 4（BCM2711） | 能编译出镜像，未在真机验证 | `make BOARD=rpi4 image` |

板级事实集中在 `kernel/src/board.rs`。加一块新板子就是加一条 `const XXX: Board`，然后用一个 cargo feature 选中它，`ACTIVE` 会自动指向新板。

## 2. 内存布局

| 项 | QEMU virt | 树莓派 4 |
| --- | --- | --- |
| 引导器加载地址 | `0x4008_0000` | `0x0008_0000`（固件固定） |
| 内核链接基址 | `0x4020_0000` | `0x0020_0000` |
| 页帧池 | `0x4800_0000..0x5000_0000` | 需要按板子实际 RAM 重算 |
| framebuffer | `0x5000_0000`（ramfb 给的） | mailbox 返回的物理地址 |

`_start` 在 `boot/start.S`，只做一件事：把 `x0`（固件给的 DTB 指针）存下来，设栈，跳 `main`。树莓派固件会传 DTB，QEMU 的 `-kernel` 也会；两条路径都保留。

## 3. 需要接的驱动（按优先级）

1. **串口**：`kernel/src/uart.rs`。QEMU 用 PL011 `0x0900_0000`；树莓派 4 的 PL011 在 `0xFE20_1000`，但默认 GPIO14/15 被蓝牙占了，`config.txt` 里要加 `dtoverlay=disable-bt`，否则串口没输出。
2. **framebuffer**：`kernel/src/fb.rs`。QEMU 走 fw_cfg 的 ramfb；树莓派要走 VideoCore mailbox（属性通道 `0xFE00B810`），先 `SET_PHYS_WH` 再 `SET_VIRT_WH` 再 `ALLOCATE_BUFFER`，拿回总线地址后要减 `0xC000_0000` 才是 ARM 视角的物理地址。
3. **输入**：`kernel/src/input.rs`。QEMU 是 virtio-input（事件 8 字节 evdev 格式）；树莓派要接 USB，最省事的是 xHCI + HID boot protocol 键盘鼠标。事件格式不用改，往里灌 `(type, code, value)` 就行。
4. **块设备**：`kernel/src/blk.rs` + `disk.rs`。QEMU 是 virtio-blk；树莓派是 EMMC（`0xFE34_0000`）。只要实现"读一个 512 字节扇区、写一个扇区"，上层 `disk`/`afs`/`cache` 全不用动。
5. **中断与定时器**：`kernel/src/gic.rs` + `timer.rs`。BCM2711 带 GIC-400（`0xFF84_1000`），PPI 30 的 generic timer 与 QEMU 一致，`cntfrq_el0` 运行时读，不要写死。

## 4. 校验链怎么降级

QEMU 上引导器跑三级 AVP（固件 → 引导器 → 内核），第三级由 QEMU 的 `-kernel` 加载路径配合。真机固件不给我们这个链路，所以 `board.has_avp = false`：

- 引导器仍然算 SHA-256 + HMAC，EX3 校验值照旧生成并打印；
- 第三级退化成"引导器自己校验内核镜像"（同一份 `verify.c`，只是没有外部签名者）；
- 内核启动日志会写 `AVP=无`，系统窗的"信任"一行显示 `EX3 自校验`。

这条降级是刻意设计的：**校验逻辑不因为换板子而消失**，只是信任根从固件变成引导器自己。

## 5. 构建与烧写

```sh
make BOARD=rpi4 image
aarch64-linux-gnu-objcopy -O binary build/aster-boot.elf build/kernel8.img
```

`kernel8.img` 放到 FAT32 的 boot 分区，`config.txt` 至少要有：

```
arm_64bit=1
kernel=kernel8.img
enable_uart=1
dtoverlay=disable-bt
uart_2ndstage=1
```

串口 115200 8N1，GPIO14(TX)/GPIO15(RX)。

## 6. 已知的坑

- 树莓派 4 的 RAM 不是从 0 开始连续可用，高地址段要读 mailbox 的 `GET_ARM_MEMORY`；先用 1 GiB 保守跑通再往上加。
- BCM2711 的外设寄存器在 `0xFC00_0000` 以上，和 QEMU 的 `0x0800_0000` 不冲突，但页表要额外映射一段 device 内存。
- 树莓派没有 PL031，`rtc.rs` 要换成本地 RTC 或从网络校时，别直接照抄 QEMU 的地址。
- `-display none` 下 QEMU 不建 console，输入事件无法路由（这是 QEMU 的行为，不是驱动问题）；调 UI 请用 `sim` 注入或带显示的 `-display gtk`。
