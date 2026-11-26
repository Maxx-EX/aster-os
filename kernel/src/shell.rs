// Aster Kernel — 内核命令行（用户看到的"系统"入口）
use crate::{fb, print, println, sched, timer, uart};
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
        let mut c;
        loop {
            crate::net::poll();
            crate::input::poll();
            crate::ui::tick();
            if let Some(ch) = crate::input::read_char() {
                c = ch;
                break;
            }
            if uart::rx_ready() {
                c = uart::getc().unwrap_or(0);
                break;
            }
            core::hint::spin_loop();
        }
        match c {
            b'\r' | b'\n' => {
                uart::putc(b'\n');
                return n;
            }
            0x7F | 0x08 if n > 0 => {
                n -= 1;
                uart::puts("\x08 \x08");
            }
            0x20..=0x7E | 0x80..=0xFF if n < buf.len() - 1 => {
                buf[n] = c;
                n += 1;
                uart::putc(c);
            }
            _ => {}
        }
    }
}


static mut EXTREME: bool = false;

fn parse_hex(s: &str) -> Option<u64> {
    let t = s.trim();
    let t = t.strip_prefix("0x").unwrap_or(t);
    u64::from_str_radix(t, 16).ok()
}

fn in_ram(addr: u64) -> bool {
    (0x4000_0000..0x5000_0000).contains(&addr)
}

fn cmd_peek(a: &str) {
    if !unsafe { EXTREME } {
        println!("  需要 Extreme 权限：先输入 ex");
        return;
    }
    match parse_hex(a) {
        Some(addr) if in_ram(addr) => {
            let v = unsafe { core::ptr::read_volatile(addr as *const u64) };
            println!("  [EX] {:#012x} = {:#018x}", addr, v);
        }
        Some(_) => println!("  [EX] 拒绝：地址不在 0x4000_0000-0x5000_0000"),
        None => println!("  用法: peek 0x40200000"),
    }
}

fn cmd_poke(a: &str) {
    if !unsafe { EXTREME } {
        println!("  需要 Extreme 权限：先输入 ex");
        return;
    }
    let mut it = a.split_whitespace();
    let (addr_s, val_s) = match (it.next(), it.next()) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            println!("  用法: poke 0x地址 0x值");
            return;
        }
    };
    match (parse_hex(addr_s), parse_hex(val_s)) {
        (Some(addr), Some(v)) if in_ram(addr) && addr % 8 == 0 => {
            unsafe { core::ptr::write_volatile(addr as *mut u64, v) };
            println!("  [EX] {:#012x} <- {:#018x}", addr, v);
        }
        (Some(_), _) => println!("  [EX] 拒绝：地址须 8 字节对齐且在 RAM 范围内"),
        _ => println!("  用法: poke 0x地址 0x值"),
    }
}

fn cmd_disk() {
    if !crate::blk::available() {
        println!("  没有 virtio-blk 设备");
        return;
    }
    println!("  设备   : virtio-blk（{} 扇区，{} MiB）",
        crate::blk::sectors(),
        crate::blk::sectors() * 512 / (1024 * 1024));
    println!("  请求数 : {}", crate::blk::requests());
    match crate::disk::probe() {
        Some(info) => {
            println!("  卷名   : {}", crate::disk::name_str(&info.name));
            println!("  格式   : AsterDisk v{}   {} 扇区", info.version, info.total);
            let mut any = false;
            for i in 0..crate::disk::MAX_PARTS {
                if let Some(p) = &info.parts[i] {
                    any = true;
                    let nm = core::str::from_utf8(&p.name).unwrap_or("?").trim_end_matches('\0');
                    println!(
                        "  分区 {} : {}  LBA {}+{}  类型 {}",
                        i, nm, p.start, p.sectors, p.kind
                    );
                }
            }
            if !any {
                println!("  分区   : 空");
            }
        }
        None => println!("  状态   : 未格式化（magic 不是 ADSK 或 CRC 不符）"),
    }
}

fn cmd_fmt() {
    if !unsafe { EXTREME } {
        println!("  格式化会清掉整盘，需要 Extreme 权限：先输入 ex");
        return;
    }
    if !crate::blk::available() {
        println!("  没有 virtio-blk 设备");
        return;
    }
    let ok = crate::disk::format("aster");
    println!("  [EX] AsterDisk 格式化：{}", if ok { "完成" } else { "失败" });
    if ok {
        cmd_disk();
    }
}

struct CatCell(core::cell::UnsafeCell<[u8; crate::afs::BS]>);
unsafe impl Sync for CatCell {}
static CAT_OUT: CatCell = CatCell(core::cell::UnsafeCell::new([0; crate::afs::BS]));

fn cat_out() -> &'static mut [u8; crate::afs::BS] {
    unsafe { &mut *CAT_OUT.0.get() }
}

fn cmd_mkfs() {
    if !unsafe { EXTREME } {
        println!("  需要 Extreme 权限：先输入 ex");
        return;
    }
    if !crate::blk::available() {
        println!("  没有 virtio-blk 设备");
        return;
    }
    if crate::disk::probe().is_none() {
        println!("  磁盘没有 AsterDisk 头，先 fmt");
        return;
    }
    let ok = crate::afs::format("aster");
    println!("  [EX] AsterFS v0 格式化：{}", if ok { "完成" } else { "失败" });
    if ok {
        cmd_df();
    }
}

fn cmd_ls() {
    let mut n = 0u32;
    let r = crate::afs::list(&mut |name: &str, ino: u32, typ: u16, size: u32| {
        let kind = if typ == crate::afs::T_DIR { "dir " } else { "file" };
        println!("  {}  {:>5}  {:>4}  {}", kind, size, ino, name);
        n += 1;
    });
    match r {
        Ok(()) => {
            if n == 0 {
                println!("  （空目录）");
            }
        }
        Err(e) => println!("  {}", e),
    }
}

fn cmd_cat(a: &str) {
    let name = a.split_whitespace().next().unwrap_or("");
    if name.is_empty() {
        println!("  用法: cat <文件>");
        return;
    }
    let out = cat_out();
    match crate::afs::read_file(name, out) {
        Ok(n) => {
            let mut i = 0usize;
            while i < n {
                let mut end = (i + 64).min(n);
                while end < n && (out[end] & 0xc0) == 0x80 {
                    end += 1;
                }
                match core::str::from_utf8(&out[i..end]) {
                    Ok(s) => println!("  {}", s),
                    Err(_) => println!("  <非 UTF-8 数据，{} 字节>", n),
                }
                i = end;
            }
        }
        Err(e) => println!("  {}", e),
    }
}

fn cmd_wr(a: &str) {
    let t = a.trim();
    if t.is_empty() {
        println!("  用法: wr <文件> <内容>");
        return;
    }
    let (name, text) = match t.find(|c: char| c.is_whitespace()) {
        Some(p) => (&t[..p], t[p..].trim()),
        None => (t, ""),
    };
    match crate::afs::write_file(name, text.as_bytes()) {
        Ok(n) => println!("  {}  {} 字节", name, n),
        Err(e) => println!("  {}", e),
    }
}

fn cmd_df() {
    match crate::afs::stat() {
        Some(st) => {
            println!(
                "  AsterFS  {} 块 × 4 KiB = {} KiB，空闲 {} 块",
                st.blocks,
                st.blocks * 4,
                st.free_blocks
            );
            println!("  inode   {}/{} 用", st.used_inodes, st.inodes);
        }
        None => println!("  磁盘未格式化（先 mkfs）"),
    }
}

fn cmd_help() {
    println!("可得命令:");
    println!("  help      显示帮助");
    println!("  info      系统与镜像信息");
    println!("  ex3       校验原创标识值 EX3");
    println!("  hash      重新计算内核镜像摘要（链式校验自检）");
    println!("  mmu       查看 MMU / 页表状态");
    println!("  run       切到 EL0 执行内嵌用户程序（演示系统调用）");
    println!("  runf      从 EL0 越权读内核内存（验证页表隔离）");
    println!("  runp      抢占验证（纯计算任务 vs 打印任务）");
    println!("  sched     任务调度统计");
    println!("  gfx       重绘图形界面（ramfb / VNC 远程可见）");
    println!("  net       网络状态（virtio-net / IP / 收发计数）");
    println!("  uptime    运行时长");
    println!("  mem       内存布局");
    println!("  disk      磁盘信息（virtio-blk + AsterDisk 头/分区表）");
    println!("  fmt       格式化磁盘（需 Extreme）");
    println!("  mkfs      格式化 AsterFS 文件系统（需 Extreme）");
    println!("  ls        列根目录");
    println!("  cat <文件> 显示文件内容");
    println!("  wr <文件> <内容>  写入文件（覆盖或新建）");
    println!("  df        文件系统用量");
    println!("  ex        Extreme 权限（等同安卓 root，提示符变 #）");
    println!("  ex off    关闭 Extreme 权限");
    println!("  peek A    读 8 字节（仅 Extreme）");
    println!("  poke A V  写 8 字节（仅 Extreme）");
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
    println!("  Aster Kernel 0.5  (Rust + C 混编)");
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
        if unsafe { EXTREME } {
            print!("# ");
        } else {
            print!("$ ");
        }
        let n = read_line(&mut buf);
        if n == 0 {
            continue;
        }
        let line = core::str::from_utf8(&buf[..n]).unwrap_or("").trim();

        if line == "ex" || line == "ex on" {
            unsafe { EXTREME = true };
            println!("[EX] Extreme 已开启，提示符变为 #，输入 ex off 退出");
            continue;
        }
        if line == "ex off" || line == "rex" {
            unsafe { EXTREME = false };
            println!("[EX] Extreme 已关闭");
            continue;
        }
        if let Some(a) = line.strip_prefix("peek ") {
            cmd_peek(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("poke ") {
            cmd_poke(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("cat ") {
            cmd_cat(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("wr ") {
            cmd_wr(a);
            continue;
        }

        match line {
            "help" | "?" => cmd_help(),
            "disk" => cmd_disk(),
            "fmt" => cmd_fmt(),
            "mkfs" => cmd_mkfs(),
            "ls" => cmd_ls(),
            "cat" => cmd_cat(""),
            "wr" => cmd_wr(""),
            "df" => cmd_df(),
            "info" => cmd_info(),
            "ex3" => cmd_ex3(),
            "hash" => cmd_hash(),
            "mmu" => {
                println!("  MMU: {}", if crate::mmu::enabled() { "已开启" } else { "关闭" });
                crate::mmu::dump();
            }
            "yield" => match crate::task::run_yield() {
                Ok(code) => println!("[YIELD] 任务结束，退出码 {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "wx" => match crate::task::run_wx() {
                Ok(code) => println!("[WX] 任务结束，退出码 {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "fp" => match crate::task::run_fp() {
                Ok(code) => println!("[FP] 任务结束，最后退出码 {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "iso" => match crate::task::run_iso() {
                Ok(code) => println!("[ISO] 任务结束，最后退出码 {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "run" => match crate::task::run_two() {
                Ok(code) => println!("[TASK] 全部任务结束，最后退出码 = {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "runf" => match crate::task::run_fault() {
                Ok(code) => println!("[TASK] 全部任务结束，最后退出码 = {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "runp" => match crate::task::run_preempt_test() {
                Ok(code) => println!("[TASK] 全部任务结束，最后退出码 = {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "sched" => println!("  {}", crate::sched::stats()),
            "gfx" => {
                // 显式重绘才回一行提示；界面自动刷新时不能打印，
                // 否则打印会再次把终端弄脏，形成无限重绘
                crate::ui::redraw();
                println!("[GFX] 界面已重绘（{}x{}）", fb::width(), fb::height());
            }
            "net" => cmd_net(),
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

/// 把当前系统状态画到 framebuffer 上
pub fn render_dashboard() {
    if !fb::available() {
        println!("[GFX] 没有 framebuffer（QEMU 启动时是否带了 -device ramfb？）");
        return;
    }
    let i = info();
    let short = &crate::ex3::EX3_TAG[..13];
    fb::draw_desktop(
        short,
        uptime_ms(),
        crate::input::available(),
        crate::net::available(),
        sched::count() as u32,
        i.payload_size / 1024,
    );
    crate::term::mark_clean(uptime_ms());
}

fn cmd_net() {
    if !crate::net::available() {
        println!("  网络未启用");
        return;
    }
    let m = crate::net::mac();
    let (rx, tx, conns) = crate::net::stats();
    println!("  设备      : virtio-net (virtio-mmio)");
    println!("  MAC       : {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
    println!("  IP        : 10.0.2.15/24   网关 10.0.2.2");
    println!("  服务      : TCP :80（宿主用 hostfwd 映射即可打开）");
    println!("  收/发帧   : {}/{}", rx, tx);
    println!("  建立连接  : {}", conns);
    let (ver, st, av, us, is) = crate::net::debug_info();
    println!("  设备版本  : {}   状态: {:#x}   RX avail={} used={}  intstatus={:#x}", ver, st, av, us, is);
    println!("  网关 MAC  : {}", if crate::net::gw_known() { "已学习" } else { "未知（ARP 未回）" });
}
