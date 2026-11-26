// Aster Kernel — 异常向量表、SVC 系统调用入口、EL0 故障处理
//
// 向量表 16 项按 AArch64 规定的位置排布：
//   0-3   Current EL with SP_EL0      （内核自己不会用 SP_EL0，留作兜底）
//   4-7   Current EL with SP_ELx      （内核态异常 → 打印现场并停机）
//   8-11  Lower EL using AArch64      （EL0 → 8 号槽统一进 svc_entry）
//   12-15 Lower EL using AArch32      （留作兜底）
//
// EL0 的 data abort / 非法指令也落在 8 号槽，所以 svc_entry 里先看 ESR 的 EC，
// 只有 EC=0x15(SVC) 才走系统调用，其余按用户态故障处理：打印 + 杀任务 + 回到 shell。
#![allow(dead_code)]

use crate::{mmu, println};
use core::arch::{asm, global_asm};

/// 用户任务的内核上下文保存区（汇编侧 adr 引用同一个符号）
///
///   [0]    进入用户态前的内核 SP
///   [1..12] x19..x30
///   [13]   任务退出码
#[no_mangle]
pub static mut ASTER_USER_CTX: [u64; 14] = [0; 14];

global_asm!(
    r#"
    .section .text
    .align 11
    .global vector_table
vector_table:
    /* ---- 0-3: Current EL with SP_EL0 ---- */
    .rept 4
    .align 7
    b aster_unexpected
    .endr

    /* ---- 4-7: Current EL with SP_ELx（内核态异常） ---- */
    .rept 4
    .align 7
    b aster_unexpected
    .endr

    /* ---- 8-11: Lower EL using AArch64 ---- */
    .align 7
    b aster_svc_entry            /* 8: 同步异常（含 EL0 的 svc） */
    .align 7
    b aster_unexpected           /* 9: IRQ from EL0（阶段二还没开抢占） */
    .align 7
    b aster_unexpected           /* 10: FIQ */
    .align 7
    b aster_unexpected           /* 11: SError */

    /* ---- 12-15: Lower EL using AArch32 ---- */
    .rept 4
    .align 7
    b aster_unexpected
    .endr

    /* ============================================================
     * 内核态异常：打印现场后停机
     * ============================================================ */
aster_unexpected:
    mrs     x3, esr_el1
    mrs     x4, elr_el1
    mrs     x5, far_el1
    mov     x0, x3
    mov     x1, x4
    mov     x2, x5
    bl      exception_report
0:  wfe
    b       0b

    /* ============================================================
     * EL0 同步异常入口（svc 系统调用 / 用户态故障）
     * 陷阱栈 160 字节：18 个 x 寄存器 + x30
     * ============================================================ */
aster_svc_entry:
    sub     sp, sp, #160
    stp     x0, x1, [sp, #0]
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x30, [sp, #144]

    mrs     x0, esr_el1
    /* ELR_EL1 对 SVC 来说已经是"返回地址"（svc 的下一条指令），不要再加 4。
       踩过的坑：误以为它指向 svc 本身而 +4，用户程序每次被跳过一条指令，
       mov x8,#9999 / mov x0,#7 全没执行，参数一路滞后一轮。 */
    mrs     x1, elr_el1
    mov     x2, sp
    bl      svc_handler
    cmp     x0, #0
    b.ne    aster_task_exit

    /* 返回值已经由 svc_handler 写回 [sp]，恢复现场后 eret 回 EL0 */
    ldp     x0, x1, [sp, #0]
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x30, [sp, #144]
    add     sp, sp, #160
    eret

    /* ============================================================
     * 任务结束：放弃陷阱栈，恢复进入用户态前的内核上下文，ret 回 Rust
     * ============================================================ */
aster_task_exit:
    add     sp, sp, #160
    adr     x0, ASTER_USER_CTX
    ldr     x1, [x0, #0]          /* 内核 SP */
    mov     sp, x1
    ldp     x19, x20, [x0, #8]
    ldp     x21, x22, [x0, #24]
    ldp     x23, x24, [x0, #40]
    ldp     x25, x26, [x0, #56]
    ldp     x27, x28, [x0, #72]
    ldp     x29, x30, [x0, #88]
    ldr     x0, [x0, #104]        /* 退出码（索引 13） */
    ret

    /* ============================================================
     * 进入用户态：x0 = 入口地址，x1 = 用户栈顶
     * 返回时 x0 = 退出码
     * ============================================================ */
    .global aster_enter_user
aster_enter_user:
    adr     x2, ASTER_USER_CTX
    mov     x3, sp
    str     x3, [x2, #0]
    stp     x19, x20, [x2, #8]
    stp     x21, x22, [x2, #24]
    stp     x23, x24, [x2, #40]
    stp     x25, x26, [x2, #56]
    stp     x27, x28, [x2, #72]
    stp     x29, x30, [x2, #88]

    msr     sp_el0, x1
    msr     elr_el1, x0
    msr     spsr_el1, xzr         /* EL0t，DAIF 全开 */
    isb

    /* 进 EL0 前把通用寄存器全部清零：不让用户态看到内核残留的
       x0(入口地址)/x1/……（内核值已经存在 ASTER_USER_CTX 里，退出时恢复） */
    .irp r, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30
    mov     x\r, xzr
    .endr
    eret
    "#
);

extern "C" {
    static vector_table: u8;
    fn aster_enter_user(entry: u64, user_sp: u64) -> u64;
}

pub fn init() {
    unsafe {
        let addr = &vector_table as *const u8 as u64;
        asm!("msr vbar_el1, {}", in(reg) addr);
        asm!("isb");
    }
}

#[no_mangle]
pub extern "C" fn exception_report(esr: u64, elr: u64, far: u64) -> ! {
    println!();
    println!("!!! ASTER 内核异常 !!!");
    println!("  ESR_EL1 = {:#018x}", esr);
    println!("  ELR_EL1 = {:#018x}", elr);
    println!("  FAR_EL1 = {:#018x}", far);
    let ec = (esr >> 26) & 0x3F;
    println!("  EC      = {:#04x} ({})", ec, ec_name(ec));
    println!("  系统已停机，需要复位。");
    loop {
        unsafe { asm!("wfe") };
    }
}

/// 返回 0 = 继续回用户态；返回 1 = 任务结束（exit 或故障）
#[no_mangle]
pub extern "C" fn svc_handler(esr: u64, elr: u64, frame: *mut u64) -> u64 {
    let ec = (esr >> 26) & 0x3F;

    if ec != 0x15 {
        // 不是 svc —— EL0 撞了别的异常（段错误、非法指令、越权访问……）
        let far: u64;
        unsafe { asm!("mrs {}, far_el1", out(reg) far) };
        println!();
        println!("!!! 用户态故障（Aster 捕获，内核继续运行）!!!");
        println!("  EC      = {:#04x} ({})", ec, ec_name(ec));
        println!("  ELR     = {:#018x}  (出错指令)", elr);
        println!("  FAR     = {:#018x}  (访问地址)", far);
        println!("  ESR     = {:#018x}", esr);
        println!("  → 该任务被终止，返回内核 shell。");
        set_return_code(1);
        return 1;
    }

    unsafe {
        let f = core::slice::from_raw_parts_mut(frame, 20);
        let nr = f[8];
        let a0 = f[0];
        let a1 = f[1];
        let a2 = f[2];

        // ELR 已在 svc_entry 汇编里前移 4 字节，这里只读参数

        let ret: i64 = match nr {
            64 => {
                let fd = a0;
                if fd != 1 && fd != 2 {
                    println!("[SVC] write(fd={}) → EBADF", fd);
                    -9
                } else if !mmu::user_ptr_ok(a1, a2) {
                    println!("[SVC] write(fd={}, buf={:#x}, len={}) → EFAULT（指针不在用户段）", fd, a1, a2);
                    -14
                } else {
                    println!("[SVC] write(fd={}, len={})", fd, a2);
                    let s = core::slice::from_raw_parts(a1 as *const u8, a2 as usize);
                    for &b in s {
                        crate::uart::putc(b);
                    }
                    a2 as i64
                }
            }
            93 => {
                // exit(code)
                println!("[SVC] exit({})", a0 as i64);
                set_return_code(a0);
                return 1;
            }
            172 => {
                // getpid
                println!("[SVC] getpid() → 1");
                1
            }
            _ => {
                println!("[SVC] 未知系统调用号 {} → ENOSYS", nr);
                -38
            }
        };
        f[0] = ret as u64;
    }
    0
}

fn set_return_code(code: u64) {
    unsafe {
        ASTER_USER_CTX[13] = code;
    }
}

/// 切到 EL0 执行用户程序，返回退出码。调用前必须已经把程序装载进用户页。
pub fn enter_user_state(entry: u64, user_sp: u64) -> u64 {
    unsafe { aster_enter_user(entry, user_sp) }
}

fn ec_name(ec: u64) -> &'static str {
    match ec {
        0x00 => "未知原因",
        0x0E => "非法指令",
        0x15 => "SVC 系统调用",
        0x18 => "MSR/MRS 访问陷阱",
        0x20 => "指令中止",
        0x21 => "数据中止(同 EL)",
        0x24 => "数据中止(低 EL)",
        0x3C => "断点",
        _ => "其他",
    }
}
