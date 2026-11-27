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
static mut L2_LOW: PageTable = PageTable([0; 512]);
static mut L3_USER_A: PageTable = PageTable([0; 512]);
static mut L3_USER_B: PageTable = PageTable([0; 512]);

const ATTR_NORMAL: u64 = 0;
const ATTR_DEVICE: u64 = 1;

// AP[7:6]：0b00 = EL1 读写、EL0 无权限；0b01 = EL0/EL1 都可读写
const AP_EL1_RW: u64 = 0b00;
const AP_ALL_RW: u64 = 0b01;
/// EL1 与 EL0 都只读 —— 用户代码页用它：能读能执行、谁都写不进去。
/// 注意 AP 编码：bit2=1 表示只读，bit1=0 表示 EL0 无访问权，所以 0b10
/// 是"EL1 只读 + EL0 完全不可访问"，不是"EL0 只读"。这里要的是 0b11。
const AP_RO_ALL: u64 = 0b11;

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
/// 每任务统一使用的用户虚拟地址布局（VA 固定、PA 各自独立）
/// 这样用户程序链接地址不用变，任务之间却互不可见 —— 切换页表就够了。
pub mod user_va {
    pub const CODE: u64 = 0x4440_0000;
    pub const GUARD: u64 = 0x4440_1000;
    pub const STACK_BASE: u64 = 0x4440_2000;
    pub const STACK_TOP: u64 = 0x4440_3000;
}

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

        // [0, 1 GiB)
        // QEMU virt：这一段全是 MMIO（fw_cfg、virtio、GIC、ramfb），按 device 映射。
        // 树莓派：内核自己就在 0 起的 RAM 里，必须按 normal 映射才能取指，
        //         只把外设窗口（0x3F00_0000 起 16 MiB）单独标成 device。
        #[cfg(not(any(feature = "rpi3", feature = "rpi4")))]
        {
            l1[0] = desc(0, 0b01, ATTR_DEVICE, AP_EL1_RW, 0, 1);
        }
        #[cfg(any(feature = "rpi3", feature = "rpi4"))]
        {
            let low = &mut L2_LOW.0;
            l1[0] = table_desc(low.as_ptr() as u64);
            for i in 0..512usize {
                let a = (i as u64) << 21;
                if a >= 0x3F00_0000 {
                    low[i] = desc(a, 0b01, ATTR_DEVICE, AP_EL1_RW, 1, 1);
                } else {
                    low[i] = desc(a, 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 0);
                }
            }
        }
        // [1, 2 GiB) 走 L2，为了能细拆用户段
        l1[1] = table_desc(l2.as_ptr() as u64);
        // [2, 4 GiB) 预留 RAM（树莓派没有这段，映射了也不会去碰）
        #[cfg(not(any(feature = "rpi3", feature = "rpi4")))]
        {
            l1[2] = desc(2 << 30, 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 1);
            l1[3] = desc(3 << 30, 0b01, ATTR_NORMAL, AP_EL1_RW, 0, 1);
        }

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

// ---------------- 每任务地址空间 ----------------
//
// 一个地址空间 = 5 页物理内存：
//   L1（512 项，复制内核 + 自己的 L1[1] 指向自己的 L2）
//   L2（512 个 2 MiB 项，复制内核，只有覆盖用户段的那一项改指自己的 L3）
//   L3（code / guard / stack 三项）
//   code 页、stack 页
//
// 所有任务用同一段 VA（0x4440_0000），但 L3 指向各自的物理页。切换任务时
// 换 TTBR0 就是换地址空间，硬件保证任务 A 的指针在任务 B 里打不开。

#[derive(Clone, Copy)]
pub struct Space {
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
    pub code: u64,
    pub stack: u64,
}

impl Space {
    pub const EMPTY: Space = Space {
        l1: 0,
        l2: 0,
        l3: 0,
        code: 0,
        stack: 0,
    };

    pub fn valid(&self) -> bool {
        self.l1 != 0
    }
}

fn pt_at(pa: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(pa as *mut [u64; 512]) }
}

/// 新建一个地址空间（从页帧分配器拿 5 页）。池空返回 None。
pub fn new_space() -> Option<Space> {
    let l1 = crate::frame::alloc()?;
    let l2 = crate::frame::alloc()?;
    let l3 = crate::frame::alloc()?;
    let code = crate::frame::alloc()?;
    let stack = crate::frame::alloc()?;

    // 内核页表的镜像：内核代码在 0x4020_0000 必须在新页表里也可见，
    // 否则切完 TTBR0 执行的下一行指令就取不到了。
    let kl1 = unsafe { &L1_TABLE.0 };
    let kl2 = unsafe { &L2_RAM.0 };
    let nl1 = pt_at(l1);
    let nl2 = pt_at(l2);
    let nl3 = pt_at(l3);

    for i in 0..512 {
        nl1[i] = kl1[i];
        nl2[i] = kl2[i];
    }
    nl1[1] = table_desc(l2);

    let idx = ((user_va::CODE - (1u64 << 30)) / (2 << 20)) as usize; // 34
    nl2[idx] = table_desc(l3);

    // 代码：EL0/EL1 都只读但可执行（AP=0b11 + UXN=0）。内核加载镜像时
    // 走的是内核自己那张表的 2 MiB 块映射（EL1 RW），照样写得进去。
    // 这样用户程序改不了自己的代码 —— W^X 的 X 那一半。
    nl3[0] = desc(code, 0b11, ATTR_NORMAL, AP_RO_ALL, 1, 0);
    nl3[1] = 0; // guard：不映射，越界立刻 fault
    nl3[2] = desc(stack, 0b11, ATTR_NORMAL, AP_ALL_RW, 1, 1); // 栈：可写不可执行

    Some(Space {
        l1,
        l2,
        l3,
        code,
        stack,
    })
}

/// 释放一个地址空间的所有页
pub fn drop_space(sp: &Space) {
    for pa in [sp.l1, sp.l2, sp.l3, sp.code, sp.stack] {
        if pa != 0 {
            crate::frame::free(pa);
        }
    }
}

/// 把扁平用户程序装进该空间的代码页
pub fn load_image(sp: &Space, bytes: &[u8]) -> Result<(), &'static str> {
    if !sp.valid() {
        return Err("地址空间无效");
    }
    if bytes.is_empty() {
        return Err("内核构建时没有内嵌该用户程序（先 make user）");
    }
    if bytes.len() > 0x1000 {
        return Err("用户程序大于一页（4 KiB），拒绝装载");
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), sp.code as *mut u8, bytes.len());
        asm!("dc cvau, {}", in(reg) sp.code);
        asm!("dsb ish");
        asm!("ic ivau, {}", in(reg) sp.code);
        asm!("dsb ish");
        asm!("isb");
    }
    Ok(())
}

/// 切到某个地址空间（不启用 ASID，直接刷全 TLB；任务数少，够用）
pub fn activate(sp: &Space) {
    if !sp.valid() {
        return;
    }
    unsafe {
        asm!("msr ttbr0_el1, {}", in(reg) sp.l1);
        asm!("dsb sy");
        asm!("tlbi vmalle1is");
        asm!("dsb sy");
        asm!("isb");
    }
}

/// 切回内核自己的页表
pub fn activate_kernel() {
    unsafe {
        asm!("msr ttbr0_el1, {}", in(reg) L1_TABLE.0.as_ptr() as u64);
        asm!("dsb sy");
        asm!("tlbi vmalle1is");
        asm!("dsb sy");
        asm!("isb");
    }
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
