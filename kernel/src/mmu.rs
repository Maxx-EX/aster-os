// Aster Kernel — MMU：AArch64 4 KiB 粒度，39 位虚拟地址，1 GiB 块映射
//
//   [0, 1 GiB)   → Device 属性（UART / GIC 在这里）
//   [1, 2 GiB)   → Normal 属性（QEMU virt 的 RAM 0x4000_0000 在这里）
//   [2, 4 GiB)   → Normal 属性（预留，真机 RAM 更大时用得上）
//
// 全部是恒等映射，所以使能 MMU 后 PC 不用跳转。
#![allow(dead_code)]

use core::arch::asm;

#[repr(C, align(4096))]
struct PageTable([u64; 512]);

static mut L1_TABLE: PageTable = PageTable([0; 512]);

const ATTR_NORMAL: u64 = 0;
const ATTR_DEVICE: u64 = 1;

/// L1 块描述符：bit[1:0]=0b01（有效 + 块），AttrIndx[4:2]，
/// SH[9:8]=内共享，AF[10]=1
#[inline]
fn block_desc(gi_b: u64, attr: u64) -> u64 {
    (gi_b << 30) | 0b01 | (attr << 2) | (0b11 << 8) | (1 << 10)
}

pub fn init() {
    unsafe {
        let t = &mut L1_TABLE.0;
        for e in t.iter_mut() {
            *e = 0;
        }
        t[0] = block_desc(0, ATTR_DEVICE);
        t[1] = block_desc(1, ATTR_NORMAL);
        t[2] = block_desc(2, ATTR_NORMAL);
        t[3] = block_desc(3, ATTR_NORMAL);

        let table_addr = t.as_ptr() as u64;

        // MAIR：attr0 = 0xFF 普通可缓存内存；attr1 = 0x04 Device-nGnRnE
        let mair: u64 = 0xFF | (0x04 << 8);

        // TCR：T0SZ=25(39bit VA)，IRGN0/ORGN0=WBWA，SH0=inner，TG0=4KiB，IPS=40bit
        let tcr: u64 = 25 | (1 << 8) | (1 << 10) | (3 << 12) | (2 << 32);

        asm!("msr mair_el1, {}", in(reg) mair);
        asm!("msr tcr_el1, {}", in(reg) tcr);
        asm!("msr ttbr0_el1, {}", in(reg) table_addr);
        asm!("dsb sy");
        asm!("isb");
        asm!("tlbi vmalle1is");
        asm!("dsb sy");
        asm!("isb");

        let mut sctlr: u64;
        asm!("mrs {}, sctlr_el1", out(reg) sctlr);
        sctlr |= (1 << 0) | (1 << 2) | (1 << 12); // M, C, I
        asm!("msr sctlr_el1, {}", in(reg) sctlr);
        asm!("isb");
    }
}

pub fn enabled() -> bool {
    let sctlr: u64;
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr) };
    sctlr & 1 != 0
}
