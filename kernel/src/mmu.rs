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
static mut L3_USER: PageTable = PageTable([0; 512]);

const ATTR_NORMAL: u64 = 0;
const ATTR_DEVICE: u64 = 1;

// AP[7:6]：0b00 = EL1 读写、EL0 无权限；0b01 = EL0/EL1 都可读写
const AP_EL1_RW: u64 = 0b00;
const AP_ALL_RW: u64 = 0b01;

/// 用户段布局（虚拟地址 = 物理地址，恒等映射）
pub const USER_CODE: u64 = 0x4440_0000;
pub const USER_GUARD: u64 = 0x4440_1000;
pub const USER_STACK_BASE: u64 = 0x4440_2000;
pub const USER_STACK_TOP: u64 = 0x4440_3000;
pub const USER_REGION_END: u64 = 0x4440_3000;

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
        let l3 = &mut L3_USER.0;
        for e in l1.iter_mut() {
            *e = 0;
        }
        for e in l2.iter_mut() {
            *e = 0;
        }
        for e in l3.iter_mut() {
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

        // 0x4440_0000 落在 [1GiB,2GiB) 的第 34 个 2 MiB 块
        let user_l2_idx = ((USER_CODE - (1u64 << 30)) / (2 << 20)) as usize; // = 34
        l2[user_l2_idx] = table_desc(l3.as_ptr() as u64);

        // L3 里第 0 个 4 KiB 页 = 用户代码页：EL0 可执行
        l3[0] = desc(USER_CODE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 0);
        // l3[1] 保持 0（无效）→ guard 页，用户越界访问立刻触发异常
        // L3 第 2 页 = 用户栈：EL0 可读写、不可执行
        l3[2] = desc(USER_STACK_BASE, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 1);

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
        (USER_CODE, USER_CODE + 0x1000),
        (USER_STACK_BASE, USER_STACK_TOP),
    ];
    for (lo, hi) in pages {
        if ptr >= lo && ptr < hi {
            // 允许恰好到页尾
            return ptr.saturating_add(len) <= hi;
        }
    }
    false
}

/// 把用户程序（扁平二进制）拷进用户代码页
pub fn load_user_image(bytes: &[u8]) -> bool {
    if bytes.len() > 0x1000 {
        return false;
    }
    unsafe {
        let dst = USER_CODE as *mut u8;
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
    }
    // 拷贝完成后让指令缓存看到新内容
    unsafe {
        asm!("dc cvau, {}", in(reg) USER_CODE);
        asm!("dsb ish");
        asm!("ic ivau, {}", in(reg) USER_CODE);
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
        crate::println!(
            "  L2[34] = {:#x}  → L3（用户段 0x4440_0000）",
            L2_RAM.0[34]
        );
        crate::println!("  L3[0] = {:#x}   用户代码页, EL0 可执行", L3_USER.0[0]);
        crate::println!("  L3[1] = {:#x}   guard 页（无效）", L3_USER.0[1]);
        crate::println!("  L3[2] = {:#x}   用户栈页, EL0 可读写不可执行", L3_USER.0[2]);
    }
}
