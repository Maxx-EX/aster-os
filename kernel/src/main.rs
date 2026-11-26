// Aster Kernel — 星芒
// 主语言 Rust（内存安全部分：内存管理、命令行、异常处理）
// C 侧负责密码学常量与哈希复算（见 c/ 目录）
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

mod exceptions;
mod mmu;
mod shell;
mod uart;

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(include_str!("boot.S"));

#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub dtb: u64,
    pub payload_base: u64,
    pub payload_size: u64,
    pub image_hash: [u8; 32],
    pub image_mac: [u8; 32],
    pub avp_elapsed_us: u32,
    pub trusted: u32,
}

extern "C" {
    fn aster_c_hash_image(base: u64, len: u64, out: *mut u8);
    fn aster_c_ct_eq(a: *const u8, b: *const u8, n: u64) -> i32;
    fn aster_c_tag() -> *const u8;
}

const ASTER_BOOT_MAGIC: u64 = 0x4153_5445_524B_4552; // "ASTERKER"

fn banner() {
    println!();
    println!("        _    ____ _____ _____ ____  ");
    println!("       / \\  / ___|_   _| ____|  _ \\ ");
    println!("      / _ \\ \\___ \\ | | |  _| | |_) |");
    println!("     / ___ \\ ___) || | | |___|  _ < ");
    println!("    /_/   \\_\\____/ |_| |_____|_| \\_\\");
    println!();
    println!("    A S T E R   星芒   v0.1   ARM64");
    println!("    原创内核 · Rust + C · 无任何第三方内核依赖");
    println!();
}

/// 链式校验：引导器算过一遍摘要，内核再独立算一遍，两者必须一致。
/// 这样即使引导器被削弱，内核也会发现镜像在跳转后被改过。
fn chain_check(info: &BootInfo) -> bool {
    let mut h = [0u8; 32];
    unsafe {
        aster_c_hash_image(info.payload_base, info.payload_size, h.as_mut_ptr());
        aster_c_ct_eq(h.as_ptr(), info.image_hash.as_ptr(), 32) == 1
    }
}

#[no_mangle]
pub extern "C" fn kernel_main(bi: *const BootInfo) -> ! {
    uart::init();
    banner();

    if bi.is_null() {
        println!("[KERNEL] 引导器没有传 bootinfo，停机。");
        loop {
            unsafe { core::arch::asm!("wfe") };
        }
    }

    let info = unsafe { &*bi };

    if info.magic != ASTER_BOOT_MAGIC {
        println!("[KERNEL] bootinfo magic 不正确: {:#x}", info.magic);
        loop {
            unsafe { core::arch::asm!("wfe") };
        }
    }

    println!("[KERNEL] bootinfo magic  OK");
    println!("[KERNEL] 内核载入地址 {:#x}，大小 {} bytes", info.payload_base, info.payload_size);

    // 先装异常向量，后面任何一步出错都能看到现场
    exceptions::init();
    println!("[KERNEL] 异常向量表 已装载");

    println!("[KERNEL] 开始链式校验（C 侧 SHA-256）...");
    let ok = chain_check(info);
    println!("[KERNEL] 链式校验返回: ok={}", ok);
    println!(
        "[KERNEL] 链式校验（内核侧独立复算）: {}",
        if ok { "PASS" } else { "FAIL" }
    );

    mmu::init();
    println!("[KERNEL] MMU 已开启（恒等映射，4KiB 粒度 1GiB 块）");

    shell::set_info(shell::SysInfo {
        payload_base: info.payload_base,
        payload_size: info.payload_size,
        dtb: info.dtb,
        hash_ok: ok,
        avp_us: info.avp_elapsed_us,
        boot_trusted: info.trusted == 1,
    });

    shell::run()
}

#[panic_handler]
fn panic(p: &PanicInfo) -> ! {
    println!();
    println!("!!! ASTER 内核 panic !!!");
    if let Some(loc) = p.location() {
        println!("  位置: {}:{}", loc.file(), loc.line());
    }
    if let Some(msg) = p.message().as_str() {
        println!("  信息: {}", msg);
    }
    println!("  系统停机。");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
