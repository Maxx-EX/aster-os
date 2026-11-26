// Aster Kernel — EL1 物理定时器（CNTP），用作调度时间片
//
// CNTP_TVAL_EL0 写入 N 表示"再过 N 个计数触发中断"，触发后需要重新装填。
// 频率从 CNTFRQ_EL0 读（QEMU virt 上不是固定值，不能写死）。
#![allow(dead_code)]

use core::arch::asm;

fn cntfrq() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) v) };
    v
}

/// 每个时间片多长（毫秒）。给用户任务的是 2 ms，切换足够频繁、又不会把
/// 串口输出切得稀碎。
pub const SLICE_MS: u64 = 2;

fn slice_ticks() -> u64 {
    let f = cntfrq();
    if f == 0 {
        return 1_000_000;
    }
    (f / 1000) * SLICE_MS
}

/// 装填下一次中断并使能
pub fn arm() {
    let t = slice_ticks();
    unsafe {
        asm!("msr cntp_tval_el0, {}", in(reg) t);
        asm!("msr cntp_ctl_el0, {}", in(reg) 1u64); // ENABLE=1, IMASK=0
        asm!("isb");
    }
}

pub fn disarm() {
    unsafe {
        asm!("msr cntp_ctl_el0, {}", in(reg) 0u64);
        asm!("isb");
    }
}

pub fn freq_hz() -> u64 {
    cntfrq()
}

/// 从开机算起的毫秒数（kernel 启动时记一次基准）
static mut BASE: u64 = 0;

pub fn mark_boot() {
    unsafe { BASE = read_cnt() };
}

fn read_cnt() -> u64 {
    let v: u64;
    unsafe { asm!("isb; mrs {}, cntpct_el0", out(reg) v) };
    v
}

pub fn now_ms() -> u64 {
    let f = cntfrq();
    if f == 0 {
        return 0;
    }
    let base = unsafe { BASE };
    (read_cnt().saturating_sub(base) * 1000 / f) as u64
}

/// 允许 EL0 读 CNTPCT_EL0（用户程序要用它做忙等/计时）
pub fn allow_el0_counter() {
    unsafe {
        asm!("msr cntkctl_el1, {}", in(reg) 1u64); // EL0PCTEN=1
        asm!("isb");
    }
}
