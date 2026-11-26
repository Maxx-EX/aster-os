// Aster Kernel — 物理页帧分配器
//
// 池子放在 RAM 的高半段：0x4800_0000 .. 0x5000_0000（128 MiB / 32768 页），
// 正好避开三样东西：
//   · 内核镜像      0x4008_0000 起（引导器）与 0x4020_0000（内核）
//   · 用户旧槽      0x4440_0000 / 0x4460_0000（保留给兼容路径）
//   · framebuffer   0x5000_0000 起 3 MiB（ramfb 直接映射，绝不能借出去）
//
// 位图 1 = 已占用。分配出来的页一律清零 —— 新任务的栈和代码页不能带
// 上一个任务的残留，否则隔离就名存实亡。
#![allow(static_mut_refs)]

use core::arch::asm;

pub const PAGE_SIZE: u64 = 4096;

const POOL_BASE: u64 = 0x4800_0000;
const POOL_END: u64 = 0x5000_0000;
const POOL_BYTES: u64 = POOL_END - POOL_BASE;
const NPAGES: usize = (POOL_BYTES / PAGE_SIZE) as usize; // 32768
const WORDS: usize = NPAGES / 64;

static mut BITMAP: [u64; WORDS] = [0; WORDS];
static mut USED: usize = 0;

pub fn init() {
    unsafe {
        for w in BITMAP.iter_mut() {
            *w = 0;
        }
        USED = 0;
    }
}

fn set_bit(i: usize, busy: bool) {
    unsafe {
        let w = i / 64;
        let b = i % 64;
        if busy {
            BITMAP[w] |= 1 << b;
        } else {
            BITMAP[w] &= !(1 << b);
        }
    }
}

fn is_busy(i: usize) -> bool {
    unsafe { BITMAP[i / 64] & (1 << (i % 64)) != 0 }
}

fn zero_page(pa: u64) {
    unsafe {
        let p = pa as *mut u64;
        for i in 0..(PAGE_SIZE / 8) as usize {
            core::ptr::write_volatile(p.add(i), 0);
        }
        asm!("dsb ish");
    }
}

/// 分配一页，返回物理地址（恒等映射下与虚拟地址相同）。池空时返回 None。
pub fn alloc() -> Option<u64> {
    unsafe {
        for w in 0..WORDS {
            if BITMAP[w] == u64::MAX {
                continue;
            }
            for b in 0..64 {
                let i = w * 64 + b;
                if !is_busy(i) {
                    set_bit(i, true);
                    USED += 1;
                    let pa = POOL_BASE + (i as u64) * PAGE_SIZE;
                    zero_page(pa);
                    return Some(pa);
                }
            }
        }
    }
    None
}

/// 连续分配 n 页（返回首页地址）。用于将来做大块 DMA 缓冲。
pub fn alloc_pages(n: usize) -> Option<u64> {
    unsafe {
        let mut run = 0usize;
        for i in 0..NPAGES {
            if is_busy(i) {
                run = 0;
            } else {
                run += 1;
                if run == n {
                    let start = i + 1 - n;
                    for k in start..=i {
                        set_bit(k, true);
                        zero_page(POOL_BASE + (k as u64) * PAGE_SIZE);
                    }
                    USED += n;
                    return Some(POOL_BASE + (start as u64) * PAGE_SIZE);
                }
            }
        }
    }
    None
}

pub fn free(pa: u64) {
    if pa < POOL_BASE || pa >= POOL_END || (pa - POOL_BASE) % PAGE_SIZE != 0 {
        return;
    }
    let i = ((pa - POOL_BASE) / PAGE_SIZE) as usize;
    unsafe {
        if is_busy(i) {
            set_bit(i, false);
            USED = USED.saturating_sub(1);
        }
    }
}

/// (已用页, 总页, 已用字节)
pub fn stats() -> (usize, usize, u64) {
    unsafe { (USED, NPAGES, USED as u64 * PAGE_SIZE) }
}

/// 物理地址是否落在池内且已分配（给调试用）
pub fn owns(pa: u64) -> bool {
    pa >= POOL_BASE && pa < POOL_END && is_busy(((pa - POOL_BASE) / PAGE_SIZE) as usize)
}
