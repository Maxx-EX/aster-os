// Aster Kernel — 星芒
// 主语言 Rust（内存安全部分：内存管理、命令行、异常处理）
// C 侧负责密码学常量与哈希复算（见 c/ 目录）
#![no_std]
#![no_main]
#![allow(static_mut_refs)]

mod cursor;
mod exceptions;
mod ex3;
mod fb;
mod frame;
mod font;
mod gic;
mod input;
mod mmu;
mod blk;
mod disk;
mod afs;
mod net;
mod rtc;
mod sched;
mod shell;
mod task;
mod term;
mod ui;
mod timer;
mod uart;
mod virtio;

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
    pub ex3: [u8; 32],
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
    match crate::rtc::local() {
        Some((y, mo, d, h, mi, _s)) => {
            println!("  Aster 0.6   ARM64   {:04}-{:02}-{:02} {:02}:{:02} CST", y, mo, d, h, mi)
        }
        None => println!("  Aster 0.6   ARM64"),
    }
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

/// EX3 校验：内核自己内嵌的那份 EX3，必须和引导器校验通过后传下来的完全一致。
/// EX3 由只有构建机持有的 K 派生，内核无法自己生成，只能比对 ——
/// 所以它是"防镜像混搭"的第三道关：换了内核，EX3 就对不上。
fn ex3_check(info: &BootInfo) -> bool {
    unsafe { aster_c_ct_eq(ex3::EX3.as_ptr(), info.ex3.as_ptr(), 32) == 1 }
}

fn hex32(v: &[u8; 32]) -> &'static str {
    // 只在出错路径用，简单拼成静态缓冲即可
    unsafe {
        static mut BUF: [u8; 65] = [0; 65];
        const H: &[u8; 16] = b"0123456789abcdef";
        for i in 0..32 {
            BUF[i * 2] = H[(v[i] >> 4) as usize];
            BUF[i * 2 + 1] = H[(v[i] & 0xF) as usize];
        }
        BUF[64] = 0;
        core::str::from_utf8_unchecked(&BUF[..64])
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

    // EX3：原创校验值，必须与引导器传下来的一致
    let ex3_ok = ex3_check(info);
    print!("[KERNEL] EX3 原创校验值 {} : ", ex3::EX3_TAG);
    println!("{}", if ex3_ok { "PASS" } else { "FAIL" });
    if !ex3_ok {
        println!("[KERNEL] EX3 不一致 —— 内核与引导器不是同一次构建，拒绝接管。");
        println!("[KERNEL] 内核 EX3 = {}", hex32(&ex3::EX3));
        println!("[KERNEL] 引导器 EX3 = {}", hex32(&info.ex3));
        loop {
            unsafe { core::arch::asm!("wfe") };
        }
    }

    mmu::init();
    println!("[KERNEL] MMU 已开启（L1/L2/L3 三级页表，用户段 4KiB 页）");

    gic::init();
    timer::allow_el0_counter();
    println!("[KERNEL] GICv2 已初始化，PPI 30（EL1 物理定时器）作为调度时基");

    if input::init() {
        println!("[IN] virtio-input 键盘已就绪（事件缓冲 8 个，字符环 128 字节）");
    } else {
        println!("[IN] 未找到 virtio-keyboard 设备（可用串口输入）");
    }
    if input::mouse_available() {
        let (mx, my) = input::mouse_pos();
        println!("[IN] virtio-input 鼠标已就绪，光标在 ({}, {})", mx, my);
    }

    if net::init() {
        let m = net::mac();
        println!(
            "[NET] virtio-net 已就绪  MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  IP 10.0.2.15/24  GW 10.0.2.2  HTTP :80",
            m[0], m[1], m[2], m[3], m[4], m[5]
        );
    } else {
        println!("[NET] 未找到 virtio-net 设备（启动时是否带了 -device virtio-net-device？）");
        // 诊断槽位表放到 `net` 命令里按需查看，启动时不要刷屏
        // virtio::dump_slots();
    }

    if blk::init() {
        println!(
            "[BLK] virtio-blk 已就绪  {} 扇区 / {} MiB",
            blk::sectors(),
            blk::sectors() * 512 / (1024 * 1024)
        );
    } else {
        println!("[BLK] 未找到 virtio-blk 设备（需要 -device virtio-blk-device）");
    }

    frame::init();
    timer::mark_boot();

    if fb::init() {
        println!("[KERNEL] 图形输出已就绪：ramfb {}x{} XRGB8888 @ {:#x}", fb::width(), fb::height(), fb::base_addr());
    } else {
        println!("[KERNEL] 未检测到 ramfb 设备，本次仅串口输出");
        fb::dump_fwcfg();
    }

    shell::set_info(shell::SysInfo {
        payload_base: info.payload_base,
        payload_size: info.payload_size,
        dtb: info.dtb,
        hash_ok: ok,
        ex3_ok,
        avp_us: info.avp_elapsed_us,
        boot_trusted: info.trusted == 1,
    });

    fb::cache_wall(&ex3::EX3_TAG[..13]);
    shell::render_dashboard();
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
