// Aster Kernel — GICv2 中断控制器（QEMU virt：GICD 0x0800_0000 / GICC 0x0801_0000）
//
// 只需要两个能力：把 PPI 30（EL1 物理定时器）打开、取中断号、回 EOIR。
#![allow(dead_code)]

const GICD_BASE: u64 = 0x0800_0000;
const GICC_BASE: u64 = 0x0801_0000;

const GICD_CTLR: u64 = 0x0000;
const GICD_ISENABLER0: u64 = 0x0100;

const GICC_CTLR: u64 = 0x0000;
const GICC_PMR: u64 = 0x0004;
const GICC_IAR: u64 = 0x000C;
const GICC_EOIR: u64 = 0x0010;

const IRQ_TIMER: u32 = 30; // PPI 30 = EL1 physical timer

#[inline]
fn w32(addr: u64, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

#[inline]
fn r32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

pub fn init() {
    unsafe {
        // 只打开定时器这一路 PPI，其他保持关闭，避免不明中断把内核打停
        w32(GICD_BASE + GICD_ISENABLER0, 1 << IRQ_TIMER);
        w32(GICD_BASE + GICD_CTLR, 1);
        // CPU interface：PMR 放到最低阈值，使能
        w32(GICC_BASE + GICC_PMR, 0xFF);
        w32(GICC_BASE + GICC_CTLR, 1);
    }
}

/// 取当前最高优先级中断号；1023 = spurious，直接丢弃
pub fn ack() -> u32 {
    r32(GICC_BASE + GICC_IAR) & 0x3FF
}

pub fn eoi(id: u32) {
    w32(GICC_BASE + GICC_EOIR, id)
}

pub fn irq_id() -> u32 {
    IRQ_TIMER
}
