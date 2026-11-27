// Aster Kernel — 板级描述（board）
//
// 把"这块板子和那块板子不一样"的事实集中在一处：内存布局、外设地址、
// 外设属于哪一类驱动、有没有 AVP 校验链。移植到新板子时先在这里加一条
// `const XXX: Board`，再实现对应类型的驱动，最后用 feature 选中它。
#![allow(dead_code)]

#[derive(Clone, Copy, PartialEq)]
pub enum FbKind {
    /// QEMU 的 fw_cfg 里给出 framebuffer（ramfb）
    Ramfb,
    /// VideoCore mailbox 申请一块 framebuffer
    Mailbox,
}

#[derive(Clone, Copy, PartialEq)]
pub enum InputKind {
    /// virtio-mmio 的 input 设备（QEMU）
    VirtioMmio,
    /// USB HID（真机，需要 xHCI 或 dwc2）
    UsbHid,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BlkKind {
    /// virtio-mmio 的 block 设备（QEMU）
    VirtioMmio,
    /// SD/EMMC 控制器（真机）
    SdHost,
}

pub struct Board {
    pub name: &'static str,
    pub ram_base: u64,
    pub ram_size: u64,
    /// 固件把引导器放到哪（树莓派固件固定 0x80000）
    pub boot_load: u64,
    /// 内核 ELF 的链接基址
    pub kernel_load: u64,
    /// 调试串口
    pub uart: u64,
    pub gicd: Option<u64>,
    pub gicc: Option<u64>,
    /// virtio-mmio 窗口基址，真机没有就是 None
    pub virtio_mmio: Option<u64>,
    /// PL031 RTC，真机没有就是 None（时间从 RTC 芯片或网络取）
    pub rtc: Option<u64>,
    /// VideoCore mailbox 基址（树莓派才有）
    pub mailbox: Option<u64>,
    pub usb: Option<u64>,
    /// ARM 物理地址 → 总线地址（VC DMA 看到的地址）的偏移
    pub bus_offset: u64,
    /// 页帧池区间
    pub pool_base: u64,
    pub pool_end: u64,
    pub fb: FbKind,
    pub input: InputKind,
    pub blk: BlkKind,
    /// 有没有"引导器 → 内核"的固件级校验链（AVP）；没有就只做镜像自校验
    pub has_avp: bool,
    pub notes: &'static str,
}

pub const QEMU_VIRT: Board = Board {
    name: "QEMU virt (cortex-a72)",
    ram_base: 0x4000_0000,
    ram_size: 0x2000_0000,
    boot_load: 0x4008_0000,
    kernel_load: 0x4020_0000,
    uart: 0x0900_0000,
    gicd: Some(0x0800_0000),
    gicc: Some(0x0801_0000),
    virtio_mmio: Some(0x0a00_0000),
    rtc: Some(0x0901_0000),
    mailbox: None,
    usb: None,
    bus_offset: 0,
    pool_base: 0x4800_0000,
    pool_end: 0x5000_0000,
    fb: FbKind::Ramfb,
    input: InputKind::VirtioMmio,
    blk: BlkKind::VirtioMmio,
    has_avp: true,
    notes: "全部外设走 virtio-mmio；framebuffer 由 fw_cfg 给",
};

pub const RPI4: Board = Board {
    name: "Raspberry Pi 4 Model B (BCM2711)",
    ram_base: 0x0000_0000,
    // 4 GiB 版的低 1 GiB 一定可用；再往上要读 mailbox 的 ARM memory 属性
    ram_size: 0x3B00_0000,
    boot_load: 0x0008_0000,
    kernel_load: 0x0080_0000,
    // PL011（GPIO14/15）；miniUART 在 0xFE21_5000，早期固件默认用它
    uart: 0xFE20_1000,
    // BCM2711 带 GIC-400
    gicd: Some(0xFF84_1000),
    gicc: Some(0xFF84_2000),
    virtio_mmio: None,
    rtc: None,
    mailbox: Some(0xFE00_B880),
    usb: Some(0xFE98_0000),
    bus_offset: 0xC000_0000,
    pool_base: 0x3000_0000,
    pool_end: 0x3800_0000,
    fb: FbKind::Mailbox,
    input: InputKind::UsbHid,
    blk: BlkKind::SdHost,
    has_avp: false,
    notes: "无 virtio、无 PL031；显示走 mailbox，键鼠走 USB，磁盘走 EMMC",
};

pub const RPI3: Board = Board {
    name: "Raspberry Pi 3B (BCM2837)",
    ram_base: 0x0000_0000,
    ram_size: 0x3E00_0000,
    boot_load: 0x0008_0000,
    kernel_load: 0x0080_0000,
    uart: 0x3F20_1000,
    gicd: Some(0x3F00_4000),
    gicc: Some(0x3F00_5000),
    virtio_mmio: None,
    rtc: None,
    mailbox: Some(0x3F00_B880),
    usb: Some(0x3F98_0000),
    bus_offset: 0x4000_0000,
    pool_base: 0x3000_0000,
    pool_end: 0x3800_0000,
    fb: FbKind::Mailbox,
    input: InputKind::UsbHid,
    blk: BlkKind::SdHost,
    has_avp: false,
    notes: "外设窗口 0x3F000000；QEMU 有 raspi3b 机型，可先在这里跑通同一套驱动",
};

#[cfg(feature = "rpi3")]
pub const ACTIVE: &Board = &RPI3;
#[cfg(feature = "rpi4")]
pub const ACTIVE: &Board = &RPI4;
#[cfg(not(any(feature = "rpi4", feature = "rpi3")))]
pub const ACTIVE: &Board = &QEMU_VIRT;

/// 当前板子缺哪些能力（给 shell / 系统窗显示用）
pub fn missing() -> &'static str {
    if ACTIVE.virtio_mmio.is_none() {
        "无 virtio（需 USB / EMMC 驱动）"
    } else {
        ""
    }
}
