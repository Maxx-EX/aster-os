// Aster Kernel — 异常向量表、IRQ 入口、SVC 系统调用、EL0 故障处理
//
// 16 项向量表按 AArch64 规定排布：
//   0-3   Current EL with SP_EL0      （兜底，内核不用 SP_EL0）
//   4-7   Current EL with SP_ELx      （内核态崩了 → 打印现场停机）
//   8-11  Lower EL using AArch64      （8 = 同步、9 = IRQ，其余兜底）
//   12-15 Lower EL using AArch32      （兜底）
//
// 上下文统一用 272 字节的"陷阱帧"（34 个 u64）：
//   [0..30] x0..x30 | [31] SP_EL0 | [32] ELR_EL1 | [33] SPSR_EL1
// 调度器直接在这个帧上做保存/替换，所以 IRQ 抢占和 SVC 不需要各写一套保存逻辑。
//
// 已知限制：帧里不含 Q0-Q31 / FPCR / FPSR。当前的用户程序都是纯整数代码，
// 一旦有任务用浮点或 NEON，被抢占后寄存器组会串。要做 FP 支持，得在
// SAVE_FRAME/LOAD_FRAME 里加 512 字节 + 2 个系统寄存器（见 roadmap）。
#![allow(dead_code)]

use crate::{gic, mmu, println, sched, timer};
use core::arch::{asm, global_asm};

/// 进入 EL0 前的内核上下文（全部任务结束后用来 ret 回 shell）
///   [0] 内核 SP | [1..12] x19..x30 | [13] 最后的退出码
#[no_mangle]
pub static mut ASTER_USER_CTX: [u64; 14] = [0; 14];

global_asm!(
    r#"
    .section .text
    .align 11
    .global vector_table
vector_table:
    .rept 4
    .align 7
    b aster_unexpected
    .endr

    .rept 4
    .align 7
    b aster_unexpected
    .endr

    .align 7
    b aster_lower_sync
    .align 7
    b aster_lower_irq
    .align 7
    b aster_unexpected
    .align 7
    b aster_unexpected

    .rept 4
    .align 7
    b aster_unexpected
    .endr

    /* ---------- 陷阱帧 800 字节：通用 272 + FP 528 ---------- */
    .macro SAVE_FRAME
    sub     sp, sp, #800
    stp     x0, x1,   [sp, #0]
    stp     x2, x3,   [sp, #16]
    stp     x4, x5,   [sp, #32]
    stp     x6, x7,   [sp, #48]
    stp     x8, x9,   [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]
    mrs     x0, sp_el0
    str     x0,       [sp, #248]
    mrs     x0, elr_el1
    str     x0,       [sp, #256]
    mrs     x0, spsr_el1
    str     x0,       [sp, #264]
    /* FP：q0..q31 = 512 字节（偏移 272..784），再 fpsr/fpcr。
       基址先挪到 sp+272，因为 stp q 的立即数上限只有 ±512 字节。 */
    add     x0, sp, #272
    stp     q0,  q1,  [x0, #0]
    stp     q2,  q3,  [x0, #32]
    stp     q4,  q5,  [x0, #64]
    stp     q6,  q7,  [x0, #96]
    stp     q8,  q9,  [x0, #128]
    stp     q10, q11, [x0, #160]
    stp     q12, q13, [x0, #192]
    stp     q14, q15, [x0, #224]
    stp     q16, q17, [x0, #256]
    stp     q18, q19, [x0, #288]
    stp     q20, q21, [x0, #320]
    stp     q22, q23, [x0, #352]
    stp     q24, q25, [x0, #384]
    stp     q26, q27, [x0, #416]
    stp     q28, q29, [x0, #448]
    stp     q30, q31, [x0, #480]
    mrs     x0, fpsr
    str     x0,       [sp, #784]
    mrs     x0, fpcr
    str     x0,       [sp, #792]
    .endm

    /* 先装 ELR/SPSR/SP_EL0（借 x1 当临时），再恢复 FP，最后通用寄存器 */
    .macro LOAD_FRAME
    ldr     x1, [sp, #248]
    msr     sp_el0, x1
    ldr     x1, [sp, #256]
    msr     elr_el1, x1
    ldr     x1, [sp, #264]
    msr     spsr_el1, x1
    add     x1, sp, #272
    ldp     q0,  q1,  [x1, #0]
    ldp     q2,  q3,  [x1, #32]
    ldp     q4,  q5,  [x1, #64]
    ldp     q6,  q7,  [x1, #96]
    ldp     q8,  q9,  [x1, #128]
    ldp     q10, q11, [x1, #160]
    ldp     q12, q13, [x1, #192]
    ldp     q14, q15, [x1, #224]
    ldp     q16, q17, [x1, #256]
    ldp     q18, q19, [x1, #288]
    ldp     q20, q21, [x1, #320]
    ldp     q22, q23, [x1, #352]
    ldp     q24, q25, [x1, #384]
    ldp     q26, q27, [x1, #416]
    ldp     q28, q29, [x1, #448]
    ldp     q30, q31, [x1, #480]
    ldr     x1, [sp, #784]
    msr     fpsr, x1
    ldr     x1, [sp, #792]
    msr     fpcr, x1
    ldp     x0, x1,   [sp, #0]
    ldp     x2, x3,   [sp, #16]
    ldp     x4, x5,   [sp, #32]
    ldp     x6, x7,   [sp, #48]
    ldp     x8, x9,   [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]
    add     sp, sp, #800
    eret
    .endm

    /* ---------- 内核态异常：打印现场停机 ---------- */
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

    /* ---------- EL0 同步异常：SVC / 用户态故障 ---------- */
aster_lower_sync:
    SAVE_FRAME
    mrs     x0, esr_el1
    mov     x1, sp
    bl      svc_handler
    cmp     x0, #0
    b.eq    1f
    add     sp, sp, #800
    b       aster_task_end
1:  LOAD_FRAME

    /* ---------- EL0 IRQ：定时器抢占 ---------- */
aster_lower_irq:
    SAVE_FRAME
    mov     x0, sp
    bl      irq_handler
    LOAD_FRAME

    /* ---------- 所有任务结束：回内核 shell ---------- */
aster_task_end:
    adr     x0, ASTER_USER_CTX
    ldr     x1, [x0, #0]
    mov     sp, x1
    ldp     x19, x20, [x0, #8]
    ldp     x21, x22, [x0, #24]
    ldp     x23, x24, [x0, #40]
    ldp     x25, x26, [x0, #56]
    ldp     x27, x28, [x0, #72]
    ldp     x29, x30, [x0, #88]
    ldr     x0, [x0, #104]
    ret

    /* ---------- 进入第一个任务：x0 = 入口，x1 = 用户栈顶 ---------- */
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
    msr     spsr_el1, xzr
    isb

    /* 进 EL0 前清空通用寄存器，不把内核残留值带出去 */
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

/// 定时器中断：清掉本次、装下一次，然后让调度器在帧上换人
#[no_mangle]
pub extern "C" fn irq_handler(frame: *mut u64) {
    let id = gic::ack();
    if id == 1023 {
        return; // spurious，不写 EOIR
    }
    if id == gic::irq_id() {
        timer::disarm();
        timer::arm();
        sched::tick(frame);
    }
    gic::eoi(id);
}

/// 返回 0 = 继续（帧可能已被换成另一个任务）；1 = 没有任务了，回内核
#[no_mangle]
pub extern "C" fn svc_handler(esr: u64, frame: *mut u64) -> u64 {
    let ec = (esr >> 26) & 0x3F;

    if ec != 0x15 {
        let far: u64;
        unsafe { asm!("mrs {}, far_el1", out(reg) far) };
        let elr = unsafe { core::ptr::read_volatile((frame as *const u64).add(32)) };
        println!();
        println!("!!! 用户态故障（Aster 捕获，内核继续运行）!!!");
        println!("  EC      = {:#04x} ({})", ec, ec_name(ec));
        println!("  ELR     = {:#018x}  (出错指令)", elr);
        println!("  FAR     = {:#018x}  (访问地址)", far);
        println!("  ESR     = {:#018x}", esr);
        println!("  → 任务 {} 被终止", sched::current_id());
        set_return_code(1);
        return kill_current(frame);
    }

    unsafe {
        let f = core::slice::from_raw_parts_mut(frame, sched::FRAME_WORDS);
        let nr = f[8];
        let a0 = f[0];
        let a1 = f[1];
        let a2 = f[2];

        let ret: i64 = match nr {
            64 => {
                let fd = a0;
                if fd != 1 && fd != 2 {
                    println!("[SVC] write(fd={}) → EBADF", fd);
                    -9
                } else if !mmu::user_ptr_ok(a1, a2) {
                    println!(
                        "[SVC] write(fd={}, buf={:#x}, len={}) → EFAULT（指针不在用户段）",
                        fd, a1, a2
                    );
                    -14
                } else {
                    let s = core::slice::from_raw_parts(a1 as *const u8, a2 as usize);
                    for &b in s {
                        crate::uart::putc(b);
                    }
                    a2 as i64
                }
            }
            93 => {
                println!("[SVC] exit({})  ← 任务 {}", a0 as i64, sched::current_id());
                set_return_code(a0);
                return kill_current(frame);
            }
            172 => sched::current_id() as i64,
            _ => {
                println!("[SVC] 未知系统调用号 {} → ENOSYS", nr);
                -38
            }
        };
        f[0] = ret as u64;
    }
    0
}

/// 杀掉当前任务：还有别的任务就把帧换成下一个（返回 0），全没了返回 1
fn kill_current(frame: *mut u64) -> u64 {
    if sched::exit_current(frame) {
        0
    } else {
        timer::disarm();
        1
    }
}

fn set_return_code(code: u64) {
    unsafe {
        ASTER_USER_CTX[13] = code;
    }
}

/// 切回内核页表（任务全部退出、或从用户态被异常赶回来时用）
pub fn activate_kernel_space() {
    crate::mmu::activate_kernel();
}

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
