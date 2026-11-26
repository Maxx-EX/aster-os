// Aster Kernel — 异常向量表与同步异常现场打印
#![allow(dead_code)]

use crate::println;
use core::arch::{asm, global_asm};

global_asm!(
    r#"
    .section .text
    .align 11
    .global vector_table
vector_table:
    .rept 16
    .align 7
    b aster_common_exception
    .endr

    .align 2
aster_common_exception:
    mrs x3, esr_el1
    mrs x4, elr_el1
    mrs x5, far_el1
    mov x0, x3
    mov x1, x4
    mov x2, x5
    bl exception_report
0:  wfe
    b 0b
    "#
);

extern "C" {
    static vector_table: u8;
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

fn ec_name(ec: u64) -> &'static str {
    match ec {
        0x00 => "未知原因",
        0x0E => "非法指令",
        0x15 => "SVC 系统调用",
        0x20 => "指令中止",
        0x21 => "数据中止(同 EL)",
        0x24 => "数据中止(低 EL)",
        0x18 => "MSR/MRS 访问陷阱",
        _ => "其他",
    }
}
