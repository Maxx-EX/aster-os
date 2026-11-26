// Aster Kernel — MMU：AArch64 4 KiB 粒度，39 位虚拟地址
//
// 阶段二把"粗粒度恒等映射"升级成带权限隔离的三级页表：
//
//   L1[0]        [0, 1 GiB)    Device        AP=EL1RW  UXN=1
//   L1[1]   →    L2 表（覆盖 [1, 2 GiB)，QEMU virt 的 RAM 在这里）
//     L2[i]      每项 2 MiB Normal 块       AP=EL1RW  UXN=1   ← 内核用
//     L2[34] →   L3 表（覆盖 0x4440_0000..0x4460_0000，用户段）
//       L3[0]    code  0x4440_0000          AP=EL0RW  UXN=0   ← EL0 可执行
//       L3[1]    留空 —— 一道 guard 页，越界立刻 fault
//       L3[2]    stack 0x4440_2000          AP=EL0RW  UXN=1   ← EL0 可读写、不可执行
//   L1[2], L1[3] [2, 4 GiB)    Normal        AP=EL1RW  UXN=1
//
// 关键点：内核自己的页 AP=0b00（只有 EL1 能访问），所以 EL0 的代码
// 就算拿着 0x4020_0000 这种地址也读不到内核 —— 这是硬件强制的隔离，
// 不是靠软件检查。
#![allow(dead_code)]

use core::arch::asm;

#[repr(C, align(4096))]
struct PageTable([u64; 512]);

static mut L1_TABLE: PageTable = PageTable([0; 512]);
static mut L2_RAM: PageTable = PageTable([0; 512]);
static mut L3_USER_A: PageTable = PageTable([0; 512]);
static mut L3_USER_B: PageTable = PageTable([0; 512]);

const ATTR_NORMAL: u64 = 0;
const ATTR_DEVICE: u64 = 1;

// AP[7:6]：0b00 = EL1 读写、EL0 无权限；0b01 = EL0/EL1 都可读写
const AP_EL1_RW: u64 = 0b00;
const AP_ALL_RW: u64 = 0b01;

/// 用户段布局（虚拟地址 = 物理地址，恒等映射）
/// 任务 A 占第一个 2 MiB 块，任务 B 占第二个 —— 目前共用一张 TTBR0 页表，
/// 靠不同虚拟区间分开，真正的地址空间隔离留到"每任务一张页表"。
pub mod slot_a {
    pub const CODE: u64 = 0x4440_0000;
    pub const STACK_BASE: u64 = 0x4440_2000;
    pub const STACK_TOP: u64 = 0x4440_3000;
}

pub mod slot_b {
    pub const CODE: u64 = 0x4460_0000;
    pub const STACK_BASE: u64 = 0x4460_2000;
    pub const STACK_TOP: u64 = 0x4460_3000;
}

/// 兼容旧名字（第一个槽）
pub const USER_CODE: u64 = slot_a::CODE;
pub const USER_STACK_BASE: u64 = slot_a::STACK_BASE;
pub const USER_STACK_TOP: u64 = slot_a::STACK_TOP;

/// 描述符拼装。kind: 0b01 = 块（L1/L2），0b11 = 页（L3）
#[inline]
fn desc(pa: u64, kind: u64, attr: u64, ap: u64, pxn: u64, uxn: u64) -> u64 {
    pa | kind | (attr << 2) | (ap << 6) | (0b11 << 8) | (1 << 10) | (pxn << 53) | (uxn << 54)
}

/// 表描述符：有效 + 表，指向下一级
#[inline]
fn table_desc(pa: u64) -> u64 {
    pa | 0b11
}

pub fn init() {
    unsafe {
        let l1 = &mut L1_TABLE.0;
        let l2 = &mut L2_RAM.0;
        let l3a = &mut L3_USER_A.0;
        let l3b = &mut L3_USER_B.0;
        for e in l1.iter_mut() {
            *e = 0;
        }
        for e in l2.iter_mut() {
            *e = 0;
        }
        for e in l3a.iter_mut() {
            *e = 0;
        }
        for e in l3b.iter_mut() {
            *e = 0;
        }

        // [0, 1 GiB) 设备：只有 EL1 能碰，EL0 连读都不行
        l1[0] = desc(0, 0b01, ATTR_DEVICE, AP_EL1_RW, 0, 1);
        // [1, 2 GiB) 走 L2，为了能细拆用户段
        l1[1] = table_desc(l2.as_ptr() as u64);
        // [2, 4 GiB) 预留 RAM
        l1[2] = desc(2 << 30, 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 1);
        l1[3] = desc(3 << 30, 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 1);

        // RAM 段：512 个 2 MiB 块，全程 EL1-only
        for i in 0..512 {
            l2[i] = desc((1u64 << 30) + (i as u64) * (2 << 20), 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 1);
        }

        // 0x4440_0000 与 0x4460_0000 分别落在第 34、35 个 2 MiB 块
        let idx_a = ((slot_a::CODE - (1u64 << 30)) / (2 << 20)) as usize; // 34
        let idx_b = ((slot_b::CODE - (1u64 << 30)) / (2 << 20)) as usize; // 35
        l2[idx_a] = table_desc(l3a.as_ptr() as u64);
        l2[idx_b] = table_desc(l3b.as_ptr() as u64);

        // 每个槽：第 0 页代码（EL0 可执行）、第 1 页留空（guard）、第 2 页栈
        l3a[0] = desc(slot_a::CODE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 0);
        l3a[2] = desc(slot_a::STACK_BASE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 1);

        l3b[0] = desc(slot_b::CODE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 0);
        l3b[2] = desc(slot_b::STACK_BASE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 1);

        let table_addr = l1.as_ptr() as u64;

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
        // SPAN(bit23)=0：异常进入 EL1 时不自动置 PSTATE.PAN。
        // cortex-a72 属 ARMv8.0，PAN 是 v8.1 的可选特性，这里不用它，
        // 用户内存的访问控制交给页表 AP 位 + user_ptr_ok 软件校验。
        sctlr &= !(1 << 23);
        asm!("msr sctlr_el1, {}", in(reg) sctlr);
        asm!("isb");
    }
}

pub fn enabled() -> bool {
    let sctlr: u64;
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr) };
    sctlr & 1 != 0
}

/// 用户指针合法性检查：只允许落在用户代码页或用户栈页内，
/// 且整段不跨页越界。目的是让 syscall 的 buf/len 参数不能反手打穿内核。
pub fn user_ptr_ok(ptr: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    let pages = [
        (slot_a::CODE, slot_a::CODE + 0x1000),
        (slot_a::STACK_BASE, slot_a::STACK_TOP),
        (slot_b::CODE, slot_b::CODE + 0x1000),
        (slot_b::STACK_BASE, slot_b::STACK_TOP),
    ];
    for (lo, hi) in pages {
        if ptr >= lo && ptr < hi {
            // 允许恰好到页尾
            return ptr.saturating_add(len) <= hi;
        }
    }
    false
}

/// 把用户程序（扁平二进制）拷进指定槽的代码页，并清掉对应 I-cache
pub fn load_user_image(bytes: &[u8], code_page: u64) -> bool {
    if bytes.len() > 0x1000 || code_page != slot_a::CODE && code_page != slot_b::CODE {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), code_page as *mut u8, bytes.len());
        asm!("dc cvau, {}", in(reg) code_page);
        asm!("dsb ish");
        asm!("ic ivau, {}", in(reg) code_page);
        asm!("dsb ish");
        asm!("isb");
    }
    true
}

/// 打印当前页表关键项（给 mmu 命令用）
pub fn dump() {
    unsafe {
        crate::println!(
            "  L1[0] = {:#x}   Device, EL1-only",
            L1_TABLE.0[0]
        );
        crate::println!("  L1[1] = {:#x}   → L2（RAM 2MiB 块）", L1_TABLE.0[1]);
        crate::println!("  L2[34] = {:#x}  → 槽 A（0x4440_0000）", L2_RAM.0[34]);
        crate::println!("  L2[35] = {:#x}  → 槽 B（0x4460_0000）", L2_RAM.0[35]);
        crate::println!("  A: code={:#x} guard={:#x} stack={:#x}",
            L3_USER_A.0[0], L3_USER_A.0[1], L3_USER_A.0[2]);
        crate::println!("  B: code={:#x} guard={:#x} stack={:#x}",
            L3_USER_B.0[0], L3_USER_B.0[1], L3_USER_B.0[2]);
    }
}
