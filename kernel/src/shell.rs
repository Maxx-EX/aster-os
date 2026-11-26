// Aster Kernel — 内核命令行（用户看到的"系统"入口）
use crate::{print, println, uart};
use core::arch::asm;

pub struct SysInfo {
    pub payload_base: u64,
    pub payload_size: u64,
    pub dtb: u64,
    pub hash_ok: bool,
    pub ex3_ok: bool,
    pub avp_us: u32,
    pub boot_trusted: bool,
}

static mut SYS: Option<SysInfo> = None;

pub fn set_info(info: SysInfo) {
    unsafe { SYS = Some(info) };
}

fn info<'a>() -> &'a SysInfo {
    unsafe { SYS.as_ref().expect("SYS 未初始化") }
}

pub fn ticks() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, cntpct_el0", out(reg) v) };
    v
}

pub fn freq() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) v) };
    v
}

fn uptime_ms() -> u64 {
    let f = freq();
    if f == 0 {
        return 0;
    }
    (ticks() as u128 * 1000 / f as u128) as u64
}

fn read_line(buf: &mut [u8]) -> usize {
    let mut n = 0;
    loop {
        while !uart::rx_ready() {
            core::hint::spin_loop();
        }
        let c = uart::getc().unwrap_or(0);
        match c {
            b'\r' | b'\n' => {
                uart::putc(b'\n');
                return n;
            }
            0x7F | 0x08 if n > 0 => {
                n -= 1;
                uart::puts("\x08 \x08");
            }
            0x20..=0x7E if n < buf.len() - 1 => {
                buf[n] = c;
                n += 1;
                uart::putc(c);
            }
            _ => {}
        }
    }
}


fn cmd_help() {
    println!("可得命令:");
    println!("  help      显示帮助");
    println!("  info      系统与镜像信息");
    println!("  ex3       校验原创标识值 EX3");
    println!("  hash      重新计算内核镜像摘要（链式校验自检）");
    println!("  mmu       查看 MMU 状态");
    println!("  uptime    运行时长");
    println!("  mem       内存布局");
    println!("  clear     清屏");
    println!("  reboot    复位整机 (PSCI)");
    println!("  halt      停机");
}

fn cmd_ex3() {
    let i = info();
    println!("  EX3 —— Aster 原创校验值");
    println!("    指纹     : {}", crate::ex3::EX3_TAG);
    print!("    完整值   : ");
    for b in crate::ex3::EX3.iter() {
        print!("{:02x}", b);
    }
    println!();
    println!(
        "    一致性   : {}",
        if i.ex3_ok {
            "与引导器一致（内核与该引导器同源）"
        } else {
            "不一致 —— 镜像被混搭过"
        }
    );
    println!("    派生方式 : HMAC-SHA256(K, BOOT_ID ‖ \"EX3\" ‖ build_id ‖ nonce)");
    println!("    密钥位置 : 仅引导器只读区与构建机，内核拿不到，因此无法自行伪造");
}

fn cmd_info() {
    let i = info();
    println!("  Aster Kernel 0.1  (Rust + C 混编)");
    println!("  内核基址   : {:#x}", i.payload_base);
    println!("  内核大小   : {} bytes", i.payload_size);
    println!("  DTB        : {:#x}", i.dtb);
    println!("  链式校验   : {}", if i.hash_ok { "PASS" } else { "FAIL" });
    println!("  EX3        : {} ({})", crate::ex3::EX3_TAG, if i.ex3_ok { "PASS" } else { "FAIL" });
    println!("  引导器 AVP : {} us, trusted={}", i.avp_us, i.boot_trusted);
    println!("  C 侧状态   : {}", unsafe {
        let p = crate::aster_c_tag();
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(p, 9))
    });
    println!("  计时器     : {} Hz", freq());
}

fn cmd_mem() {
    println!("  0x0000_0000 - 0x3FFF_FFFF  Device / 外设");
    println!("  0x4000_0000 - 0x5FFF_FFFF  RAM（内核在 0x4020_0000）");
    println!("  0x4410_0000 - ...          AFP 刷写暂存区");
}

fn cmd_hash() {
    let i = info();
    let mut h = [0u8; 32];
    unsafe {
        crate::aster_c_hash_image(i.payload_base, i.payload_size, h.as_mut_ptr());
    }
    print!("  SHA-256 = ");
    for b in h.iter() {
        print!("{:02x}", b);
    }
    println!();
}

fn psci_reboot() -> ! {
    unsafe {
        asm!("mov x0, #0x84000000", "add x0, x0, #9", "hvc #0", out("x0") _);
    }
    loop {
        unsafe { asm!("wfe") };
    }
}

pub fn run() -> ! {
    println!();
    println!("Aster 内核已接管。输入 help 查看命令。");
    let mut buf = [0u8; 128];
    loop {
        print!("aster> ");
        let n = read_line(&mut buf);
        if n == 0 {
            continue;
        }
        let line = core::str::from_utf8(&buf[..n]).unwrap_or("").trim();

        match line {
            "help" | "?" => cmd_help(),
            "info" => cmd_info(),
            "ex3" => cmd_ex3(),
            "hash" => cmd_hash(),
            "mmu" => println!("  MMU: {}", if crate::mmu::enabled() { "已开启" } else { "关闭" }),
            "uptime" => println!("  已运行 {} ms", uptime_ms()),
            "mem" => cmd_mem(),
            "clear" => {
                for _ in 0..40 { println!() }
            }
            "reboot" => psci_reboot(),
            "halt" => loop {
                unsafe { asm!("wfe") };
            },
            "" => {}
            other => {
                println!("未知命令: {}", other);
            }
        }
    }
}
