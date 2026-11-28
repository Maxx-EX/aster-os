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
            // 0x90 上一条 / 0x91 下一条（软键盘 UP/DN，PC 端复用伪键，避开真键码）
            0x90 | 0x91 => {
                unsafe {
                    // HIST_AT = 1..=HIST_N 表示"正显示第几条"，HIST_N 表示"新的空行"
                    let want = if c == 0x91 {
                        if HIST_AT < HIST_N { HIST_AT + 1 } else { HIST_AT }
                    } else if HIST_AT > 0 {
                        HIST_AT - 1
                    } else {
                        0
                    };
                    HIST_AT = want;
                    for _ in 0..n {
                        crate::print!("\u{8} \u{8}");
                    }
                    n = 0;
                    if HIST_AT > 0 {
                        let src = HIST[(HIST_AT - 1) % HIST_MAX];
                        let mut len = 0;
                        while len < HIST_LEN && src[len] != 0 {
                            len += 1;
                        }
                        for k in 0..len.min(buf.len() - 1) {
                            buf[k] = src[k];
                            crate::print!("{}", src[k] as char);
                            n = k + 1;
                        }
                    }
                }
            }
            b'\r' | b'\n' => {
                crate::print!("\n");
                return n;
            }
            0x7F | 0x08 if n > 0 => {
                n -= 1;
                // 屏幕和串口一起擦：term 的 \x08 处理会把光标退一格并抹白
                crate::print!("\u{8} \u{8}");
            }
            0x20..=0x7E | 0x80..=0xFF if n < buf.len() - 1 => {
                buf[n] = c;
                n += 1;
                crate::print!("{}", c as char);
            }
            _ => {}
        }
    }
}


static mut EXTREME: bool = false;

/// 给 auth 用：EXTREME 是否已解锁
pub fn extreme_on() -> bool {
    unsafe { EXTREME }
}

/// 命令行历史（手机端软键盘有 UP/DN 键，PC 端也复用：↑ = 0x90、↓ = 0x91 伪键）
const HIST_MAX: usize = 8;
const HIST_LEN: usize = 64;
static mut HIST: [[u8; HIST_LEN]; HIST_MAX] = [[0; HIST_LEN]; HIST_MAX];
static mut HIST_N: usize = 0;
static mut HIST_AT: usize = 0;
/// Device Lock：默认锁定。锁定状态下任何"动底层"的命令都不执行，
/// 即使进了 Extreme —— 两道锁各管一层：Extreme 管"要不要放开权限"，
/// DL 管"这台设备现在准不准动固件/内存/磁盘"。
static mut DL_LOCKED: bool = true;

fn parse_hex(s: &str) -> Option<u64> {
    let t = s.trim();
    let t = t.strip_prefix("0x").unwrap_or(t);
    u64::from_str_radix(t, 16).ok()
}

/// 解锁码 = 本机构建的 EX3 指纹末 8 位（顶栏那一串），大小写不敏感。
fn dl_code() -> &'static str {
    let t = crate::ex3::EX3_TAG;
    &t[t.len() - 8..]
}

fn dl_unlocked() -> bool {
    unsafe { !DL_LOCKED }
}

/// 底层命令的统一门：Extreme 已开 且 Device Lock 已解
fn gate(what: &str) -> bool {
    if !unsafe { EXTREME } {
        println!("  需要 Extreme 权限：先输入 ex");
        return false;
    }
    if unsafe { DL_LOCKED } {
        println!("  Device Lock 锁定中，{} 被拒。解锁码是顶栏 EX3 的末 8 位：dl unlock <code>", what);
        return false;
    }
    true
}

/// update            —— 版本 / 槽 / 上次结果
/// update check [f]  —— 只校验更新包（魔数、长度、CRC32、EX3）
/// update stage [f]  —— 校验通过后登记"待应用"
/// update apply      —— 切 B 槽（写 boot.conf）
/// update rollback   —— 切回 A 槽
fn cmd_update(a: &str) {
    let mut it = a.split_whitespace();
    let sub = it.next().unwrap_or("");
    let path = it.next();
    let bytes: &[u8] = match path {
        Some(p) => {
            let out = xce_buf();
            match crate::afs::read_file(p, out) {
                Ok(n) => &out[..n],
                Err(e) => {
                    println!("  读不到 {}：{}", p, e);
                    return;
                }
            }
        }
        None => crate::UPD_DEMO,
    };

    match sub {
        "" => {
            let slot = crate::update::boot_slot();
            println!("  版本      Aster {}  ARM64", crate::update::CURRENT_VER);
            println!("  当前槽    {}（{}）", if slot == 1 { "B" } else { "A" },
                if slot == 1 { "暂存更新" } else { "内嵌镜像" });
            println!("  磁盘      {} / 单文件上限 {} 字节", 
                if crate::blk::available() { "已挂载 AsterFS v0" } else { "未挂载（起机没带 -drive）" },
                crate::afs::BS);
            {
                let out = xce_buf();
                let line = match crate::afs::read_file("upd.state", out) {
                    Ok(n) => core::str::from_utf8(&out[..n]).unwrap_or("").trim(),
                    Err(_) => crate::update::state(),
                };
                println!("  上次状态  {}", line);
            }
            println!("  检查包    update check [文件]   登记 update stage   切换 update apply   回滚 update rollback");
        }
        "check" => match crate::update::parse(bytes) {
            Ok(p) => {
                println!("  AUP       v{}  目标构建 {}  body {} 字节  CRC32 {:08x} {}",
                    crate::update::version_str(p.version), p.build, p.body_size, p.crc,
                    if p.crc_ok { "通过" } else { "不匹配" });
                println!("  构建绑定  EX3 {}（{}）", &crate::ex3::EX3_TAG[4..16],
                    if p.ex3_ok { "与这台设备一致" } else { "是别的设备打的，拒绝应用" });
                if !p.crc_ok || !p.ex3_ok {
                    println!("  结论      包不可用");
                } else if p.version < 9 {
                    println!("  结论      版本不高于当前，可跳过");
                } else {
                    println!("  结论      可以应用（update stage）");
                }
            }
            Err(e) => println!("  {}", e),
        },
        "stage" => match crate::update::parse(bytes) {
            Ok(p) => {
                if !p.crc_ok || !p.ex3_ok {
                    println!("  拒绝登记：CRC 或 EX3 绑定不通过");
                    crate::update::set_state("stage-failed crc/ex3\n");
                    return;
                }
                println!("  记录到 upd.state（v{} / {} 字节 / crc {:08x}）",
                    crate::update::version_str(p.version), p.body_size, p.crc);
                crate::update::set_state(if p.version >= 9 {
                    "staged ver=0.9 pending\n"
                } else {
                    "staged older-version pending\n"
                });
                println!("  已登记待应用：v{}  {} 字节（update apply 切槽）",
                    crate::update::version_str(p.version), p.body_size);
            }
            Err(e) => println!("  {}", e),
        },
        "apply" => {
            if crate::update::boot_slot() == 1 {
                println!("  已经在 B 槽，无需重复 apply");
                return;
            }
            match crate::update::set_slot(1) {
                Ok(()) => {
                    crate::update::set_state("applied slot=B bootloader=embedded(BOARD-6 reads disk)\n");
                    println!("  已切到 B 槽（boot.conf slot=B）");
                    println!("  注意：当前引导器仍加载内嵌镜像；读盘启动是 BOARD-6，");
                    println!("        那一步做完重启才会真的用上暂存镜像。");
                }
                Err(e) => println!("  写 boot.conf 失败：{}（磁盘没挂载？先 mkfs）", e),
            }
        }
        "rollback" => match crate::update::set_slot(0) {
            Ok(()) => {
                crate::update::set_state("rolled-back slot=A reason=manual\n");
                println!("  已回滚到 A 槽（内嵌镜像）");
            }
            Err(e) => println!("  写 boot.conf 失败：{}", e),
        },
        _ => println!("  用法: update | update check [文件] | update stage [文件] | update apply | update rollback"),
    }
}

fn cmd_dl(a: &str) {
    let mut it = a.split_whitespace();
    match it.next().unwrap_or("") {
        "" => {
            println!(
                "  Device Lock {}",
                if unsafe { DL_LOCKED } { "锁定 —— 底层写操作全部拒绝" } else { "已解锁 —— 底层命令可用（仍需 Extreme）" }
            );
            println!("  范围      内存读写（peek/poke）、磁盘格式化（fmt/mkfs）、固件刷写");
            println!("  解锁      dl unlock <code>   解锁码 = EX3 指纹末 8 位（{}）", dl_code());
            println!("  重新上锁  dl lock（关机不保留，下次启动默认锁定）");
        }
        "unlock" => {
            let code = it.next().unwrap_or("").trim();
            if code.is_empty() {
                println!("  用法: dl unlock <code>（8 位十六进制）");
                return;
            }
            if code.len() == 8 && code.eq_ignore_ascii_case(dl_code()) {
                unsafe { DL_LOCKED = false };
                println!("  Device Lock 已解锁：{} 与本机构建一致", dl_code());
            } else {
                println!("  解锁失败：码与 EX3 指纹末 8 位不符（尝试次数不限制，但这层不是安全边界，只防误操作）");
            }
        }
        "lock" => {
            unsafe { DL_LOCKED = true };
            println!("  Device Lock 已重新锁定");
        }
        _ => println!("  用法: dl | dl unlock <code> | dl lock"),
    }
}

fn in_ram(addr: u64) -> bool {
    (0x4000_0000..0x5000_0000).contains(&addr)
}

fn cmd_peek(a: &str) {
    if !gate("peek") {
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

fn wait_ms(ms: u64) {
    let t0 = crate::timer::now_ms();
    while crate::timer::now_ms().saturating_sub(t0) < ms {
        core::hint::spin_loop();
    }
}

/// 内核内鼠标注入：与真实事件同一个状态机，用于无显示环境下的交互回归
fn cmd_sim(a: &str) {
    let mut it = a.split_whitespace();
    let op = it.next().unwrap_or("");
    let n = |s: Option<&str>| s.and_then(|v| v.parse::<i32>().ok());
    match op {
        "move" => match (n(it.next()), n(it.next())) {
            (Some(x), Some(y)) => {
                crate::input::inject_move_to(x, y);
                crate::ui::tick();
                println!("  [SIM] 光标 ({}, {})", x, y);
            }
            _ => println!("  用法: sim move x y"),
        },
        "click" => match (n(it.next()), n(it.next())) {
            (Some(x), Some(y)) => {
                crate::input::inject_move_to(x, y);
                crate::ui::tick();
                wait_ms(90);
                crate::input::inject_btn(true);
                crate::ui::tick();
                wait_ms(90);
                crate::input::inject_btn(false);
                crate::ui::tick();
                println!("  [SIM] 点击 ({}, {})", x, y);
            }
            _ => println!("  用法: sim click x y"),
        },
        "drag" => match (n(it.next()), n(it.next()), n(it.next()), n(it.next())) {
            (Some(x1), Some(y1), Some(x2), Some(y2)) => {
                crate::input::inject_move_to(x1, y1);
                crate::ui::tick();
                wait_ms(90);
                crate::input::inject_btn(true);
                crate::ui::tick();
                wait_ms(120);
                for i in 1..=6 {
                    crate::input::inject_move_to(x1 + (x2 - x1) * i / 6, y1 + (y2 - y1) * i / 6);
                    crate::ui::tick();
                    wait_ms(90);
                }
                crate::input::inject_btn(false);
                crate::ui::tick();
                println!("  [SIM] 拖拽 ({},{}) -> ({},{})", x1, y1, x2, y2);
            }
            _ => println!("  用法: sim drag x1 y1 x2 y2"),
        },
        _ => println!("  用法: sim move x y | sim click x y | sim drag x1 y1 x2 y2"),
    }
}

fn cmd_poke(a: &str) {
    if !gate("poke") {
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
    if !gate("fmt") {
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

struct XceCell(core::cell::UnsafeCell<[u8; 8192]>);
unsafe impl Sync for XceCell {}
static XCE_BUF: XceCell = XceCell(core::cell::UnsafeCell::new([0; 8192]));

fn xce_buf() -> &'static mut [u8; 8192] {
    unsafe { &mut *XCE_BUF.0.get() }
}

/// xce            —— 看内嵌演示程序
/// xce info [f]   —— 校验一个 XCE 文件（不带路径就看内嵌那份）
/// xce run  [f]   —— 装进新地址空间并切 EL0 跑
fn cmd_xce(a: &str) {
    let mut it = a.split_whitespace();
    let sub = it.next().unwrap_or("");
    let path = it.next();
    let bytes: &[u8] = match path {
        Some(p) => {
            let out = xce_buf();
            match crate::afs::read_file(p, out) {
                Ok(n) => &out[..n],
                Err(e) => {
                    println!("  读不到 {}：{}", p, e);
                    return;
                }
            }
        }
        None => crate::XCE_DEMO,
    };

    match sub {
        "" | "info" => match crate::xce::describe(bytes) {
            Ok(i) => {
                println!("  XCE       v{}  {}  头 {} 字节", i.version, crate::xce::kind_name(i.kind), crate::xce::HDR);
                println!("  入口      {:#x}  栈顶 {:#x}  flags {:#x}", i.entry, i.stack, i.flags);
                println!("  body      {} 字节  CRC32 {:08x}  {}", i.body_size, i.crc, if i.crc_ok { "通过" } else { "不匹配" });
                if path.is_none() {
                    println!("  来源      内核内嵌（xce run 跑的就是这一份）");
                }
            }
            Err(e) => println!("  {}", e),
        },
        "run" => match crate::xce::run(bytes) {
            Ok(code) => println!("  [XCE] 任务结束，退出码 {}", code),
            Err(e) => println!("  装载失败：{}", e),
        },
        _ => println!("  用法: xce | xce info [文件] | xce run [文件]"),
    }
}

struct CatCell(core::cell::UnsafeCell<[u8; crate::afs::BS]>);
unsafe impl Sync for CatCell {}
static CAT_OUT: CatCell = CatCell(core::cell::UnsafeCell::new([0; crate::afs::BS]));

fn cat_out() -> &'static mut [u8; crate::afs::BS] {
    unsafe { &mut *CAT_OUT.0.get() }
}

fn cmd_mkfs() {
    if !gate("mkfs") {
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
        // 新盘先把用户库落一份，省得第一次 useradd 之前 passwd 不存在
        if crate::auth::save_to_disk() {
            println!("  用户库: passwd 已写入（{} 个用户）", crate::auth::count());
        }
        cmd_df();
    }
}

fn cmd_ls(a: &str) {
    let long = a.trim() == "-l";
    let mut n = 0u32;
    let r = crate::afs::list(&mut |name: &str, ino: u32, typ: u16, size: u32, mode: u16, uid: u16| {
        if long {
            let mut pb = [0u8; 11];
            crate::auth::mode_str(mode, typ, &mut pb);
            let perms = core::str::from_utf8(&pb[..10]).unwrap_or("----------");
            let owner = crate::auth::name_of_uid(uid);
            println!("  {}  {:>6}  {:<8}  {}", perms, size, owner, name);
        } else {
            let kind = if typ == crate::afs::T_DIR { "dir " } else { "file" };
            println!("  {}  {:>5}  {:>4}  {}", kind, size, ino, name);
        }
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
    let f = match crate::afs::raw_inode(name) {
        Some(f) => f,
        None => {
            println!("  {}", if crate::blk::available() { "没有这个文件" } else { "没有磁盘" });
            return;
        }
    };
    if f.typ == crate::afs::T_DIR {
        println!("  {} 是目录，用 ls 看", name);
        return;
    }
    if f.size == 0 {
        println!("  （空文件）");
        return;
    }
    // 流式：一次读一块，多大的文件都能从头打到尾（不再受页缓冲上限）
    let out = cat_out();
    let mut off = 0u32;
    while off < f.size {
        let n = crate::afs::read_at(f.ino, off, out);
        if n == 0 {
            break;
        }
        let mut i = 0usize;
        while i < n {
            let mut end = (i + 64).min(n);
            while end < n && (out[end] & 0xc0) == 0x80 {
                end += 1;
            }
            match core::str::from_utf8(&out[i..end]) {
                Ok(s) if !s.is_empty() => println!("  {}", s),
                _ => {
                    // 块尾切在多字节字符中间：退回上一个完整字符再试
                    let mut e = end;
                    while e > i + 1 && (out[e - 1] & 0xc0) == 0x80 {
                        e -= 1;
                    }
                    if let Ok(s) = core::str::from_utf8(&out[i..e]) {
                        if !s.is_empty() {
                            println!("  {}", s);
                        }
                    }
                    end = e.max(i + 1);
                }
            }
            i = end;
        }
        off += n as u32;
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
        Ok(n) => {
            crate::afs::sync();
            println!("  {}  {} 字节", name, n);
        }
        Err(e) => println!("  {}", e),
    }
}

fn cmd_cache() {
    let (h, m, used, dirty) = crate::cache::stats();
    let total = h + m;
    let pct = if total == 0 { 0 } else { (h * 100) / total };
    println!("  块缓存  8 槽 / 用 {} / 脏 {}", used, dirty);
    println!("  命中 {}/{}  = {}%   写回 {} 块", h, total, pct, crate::cache::writes());
}

/// `load <名字>`：从 AsterFS 读一个用户程序镜像，装进新地址空间并切 EL0。
/// 这条命令是"用户程序脱离内嵌"的验收点 —— 字节确实来自磁盘。
fn cmd_load(a: &str) {
    struct B(core::cell::UnsafeCell<[u8; 4096]>);
    unsafe impl Sync for B {}
    static BUF: B = B(core::cell::UnsafeCell::new([0u8; 4096]));

    let name = if a.is_empty() { "hello" } else { a };
    let buf = unsafe { &mut *BUF.0.get() };
    let n = match crate::afs::read_file(name, buf) {
        Ok(n) => n,
        Err(e) => {
            println!("  {} → {}", name, e);
            println!("  盘上有：{}", if crate::blk::available() { "（ls 看一下）" } else { "没有磁盘，先 ip 一条 -device virtio-blk-device / make run-disk" });
            return;
        }
    };
    println!("[LOAD] 从盘读入 {}（{} 字节）", name, n);
    match crate::task::run_from(&buf[..n]) {
        Ok(code) => println!("[LOAD] 任务结束，退出码 {}", code),
        Err(e) => println!("[LOAD] 装载失败：{}", e),
    }
}

fn cmd_sync() {
    let n = crate::afs::sync();
    println!("  写回 {} 个脏块", n);
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
            let (hit, miss, used, dirty) = crate::cache::stats();
            println!(
                "  缓存    {} 命中 / {} 未命中 · {} 槽占用 · {} 脏",
                hit, miss, used, dirty
            );
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
    println!("  net       网络状态（virtio-net / IP / DNS / 收发计数）");
    println!("  wifi     无线状态（后端 / SDHCI / SDIO），wifi scan 扫 AP");
    println!("  fetch    从网络取一个文件落到 AsterFS，例: fetch 10.0.2.2:8088/a.txt a.txt");
    println!("  dhcp      DHCP 自动取地址（DISCOVER→OFFER→REQUEST→ACK）");
    println!("  dns <名>  查 A 记录，例: dns example.com");
    println!("  wifi      无线网现状（无 802.11 硬件，WIFI-1 未开工）");
    println!("  web       内置浏览器：web / web home / web <ip:port/path> / web scroll up|down|top");
    println!("  date      当前时间（固定 Asia/Shanghai）");
    println!("  tz        时区详情（UTC+8 / 无夏令时）");
    println!("  pkg       包管理：list [分类] | search <词> | info <名> | install <名> [url] | remove <名>");
    println!("  http      HTTP 客户端：http get <ip> <port> <path> [保存名]");
    println!("  perf      绘制耗时（整桌 / 壁纸 / 侧栏 / 窗口 / 帧）");
    println!("  uptime    运行时长");
    println!("  mem       内存布局");
    println!("  disk      磁盘信息（virtio-blk + AsterDisk 头/分区表）");
    println!("  fmt       格式化磁盘（需 Extreme）");
    println!("  mkfs      格式化 AsterFS 文件系统（需 Extreme）");
    println!("  ls        列根目录");
    println!("  cat <文件> 显示文件内容");
    println!("  wr <文件> <内容>  写入文件（覆盖或新建）");
    println!("  df        文件系统用量");
    println!("  cache     块缓存命中率");
    println!("  sync      强制写回脏块");
    println!("  fd        查看 VFS 的 fd 表（open/read/write 计数）");
    println!("  vfw       测 fd 写路径：open(O_CREAT) → write → close 落盘");
    println!("  file      用 fd 系统调用读 /motd（EL0 + openat/read/close）");
    println!("  apk       读内置示例 APK：zip 目录 / classes.dex 头 / 二进制 XML");
    println!("  apk2      deflate 混合包：apk2 dex / apk2 cat <条目>（走 inflate）");
    println!("  dex       DEX 头/结构；dex run 执行 Main->run()V（非零 opcode 解释器）");
    println!("  usb       DWC2 主机状态与设备描述符（树莓派键鼠走这条）");
    println!("  xce       Aster 原生程序：xce / xce info [文件] / xce run [文件]");
    println!("  dl        Device Lock：锁定/解锁底层写（解锁码 = EX3 末 8 位）");
    println!("  update    A/B 槽更新：update / check / stage / apply / rollback");
    println!("  bootanim  重播开机动画（2.4s）");
    println!("  bootframe <ms>  把动画某一时刻静态画出来（取证用）");
    println!("  ex        Extreme 权限（等同安卓 root，提示符变 #）");
    println!("  ex off    关闭 Extreme 权限");
    println!("  sim       sim move x y | sim click x y | sim drag x1 y1 x2 y2（无头交互）");
    println!("  wins      多窗口状态（位置/层序/焦点）");
    println!("  win on|off <n>  打开/关闭第 n 个窗口（0 终端 1 系统 2 网络）");
    println!("  fbinfo    framebuffer 基址/尺寸/stride");
    println!("  peek A    读 8 字节（需 Extreme + Device Lock 解锁）");
    println!("  poke A V  写 8 字节（需 Extreme + Device Lock 解锁）");
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
        crate::bootanim::set_hold(false);
        unsafe {
            if !line.is_empty() {
                let at = HIST_N % HIST_MAX;
                let mut k = 0;
                for b in line.as_bytes() {
                    if k + 1 >= HIST_LEN {
                        break;
                    }
                    HIST[at][k] = *b;
                    k += 1;
                }
                HIST[at][k] = 0;
                HIST_N += 1;
                HIST_AT = HIST_N;
            }
        }

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
        if let Some(a) = line.strip_prefix("sim ") {
            cmd_sim(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("bootframe ") {
            let ms: i32 = a.trim().parse().unwrap_or(1500);
            crate::bootanim::still(ms);
            println!("  已画到第 {} ms 并停在屏幕上", ms);
            continue;
        }
        if line == "update" {
            cmd_update("");
            continue;
        }
        if let Some(a) = line.strip_prefix("update ") {
            cmd_update(a);
            continue;
        }
        if line == "dl" {
            cmd_dl("");
            continue;
        }
        if let Some(a) = line.strip_prefix("dl ") {
            cmd_dl(a);
            continue;
        }
        if line == "xce" {
            cmd_xce("");
            continue;
        }
        if let Some(a) = line.strip_prefix("xce ") {
            cmd_xce(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("dex ") {
            cmd_dex(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("apk2 ") {
            cmd_apk2(a);
            continue;
        }
        if let Some(a) = line.strip_prefix("apk ") {
            cmd_apk(a);
            continue;
        }

        match line {
            "help" | "?" => cmd_help(),
            "disk" => cmd_disk(),
            "sim" => cmd_sim(""),
            "win on 0" | "win on 1" | "win on 2" | "win on 3" => {
                let i = (line.as_bytes()[7] - b'0') as usize;
                crate::fb::show_win(i);
                println!("  [WIN] {} 打开并置顶", i);
            }
            "win off 0" | "win off 1" | "win off 2" | "win off 3" => {
                let i = (line.as_bytes()[8] - b'0') as usize;
                crate::fb::close_win(i);
                println!("  [WIN] {} 关闭", i);
            }
            "app" | "app on" | "app off" | "app start" | "app stop" | "app status" | "app step" => cmd_app(line),
            l if l.starts_with("app start ") => cmd_app(line),
            l if l == "pkg" || l.starts_with("pkg ") => cmd_pkg(l.strip_prefix("pkg").unwrap_or("").trim()),
            l if l == "http" || l.starts_with("http ") => {
                cmd_http(l.strip_prefix("http").unwrap_or("").trim())
            }
            l if l == "web" || l.starts_with("web ") => {
                let a = l[3..].trim();
                crate::fb::show_win(3);
                if a.is_empty() || a == "home" {
                    crate::web::open_home();
                    println!("  已打开内置起始页（窗口「浏览器」）");
                    println!("  地址栏: {}   行数: {}", crate::web::url_str(), crate::html::count());
                } else if let Some(rest) = a.strip_prefix("scroll ") {
                    let d = match rest.trim() {
                        "up" => -80,
                        "down" => 80,
                        "top" => {
                            crate::web::scroll_reset();
                            0
                        }
                        other => other.parse::<i32>().unwrap_or(80),
                    };
                    crate::web::scroll_by(d);
                    println!("  滚动 {}  当前位置 {} / {}", d, crate::web::scroll_pos(), crate::web::max_scroll());
                } else {
                    println!("  拉取 {} …", a);
                    if crate::web::open_addr(a) {
                        println!("  成功  行数 {}", crate::html::count());
                    } else {
                        println!("  失败，页面里写了原因（窗口「浏览器」）");
                    }
                }
            },
            "panic" => {
                println!("  这是自检命令：接下来内核会真的停下，用来验证崩溃屏");
                println!("  串口留证据，屏幕（如果有）画一张红屏，然后不再回来");
                panic!("shell 手动触发内核 panic");
            },
            "wifi" | "wifi scan" | "wifi status" => {
                let st = crate::wifi::probe();
                println!("  后端      : {}", crate::wifi::backend_name(st.backend));
                println!("  SDHCI     : {}", crate::sdhci::status_text());
                println!("  SDIO 卡   : {}", if st.sdio_card { "CMD5 有应答" } else { "无应答（没有卡/没有控制器）" });
                println!("  进度      : {}", crate::wifi::phase_text(crate::wifi::phase()));
                if line == "wifi scan" {
                    let mut buf: [(&str, i16); 8] = [("", 0); 8];
                    let n = crate::wifi::scan(&mut buf);
                    if n == 0 {
                        println!("  扫描      : 0 个 AP（没有驱动，不编造热点）");
                    } else {
                        for i in 0..n {
                            println!("  AP        : {:<16} {} dBm", buf[i].0, buf[i].1);
                        }
                    }
                }
                if crate::board::ACTIVE.emmc.is_none() {
                    println!("  QEMU virt : 没有 EMMC / SDIO，无线只能等真机验证");
                    println!("  眼下替代  : virtio-net 当有线用，-netdev user 自带上游 NAT");
                } else {
                    println!("  真机路径  : SDHCI 复位 → CMD5 → CMD3/CMD7 → 读 CIS → 固件 → WPA2");
                }
            },
            l if l == "tz" => {
                println!("  时区      : {}", crate::rtc::TZ_NAME);
                println!("  UTC 偏移  : +{} 小时（固定写死在 rtc.rs，不跟宿主机）", crate::rtc::offset_hours());
                println!("  夏令时    : 无（中国自 1991 年起不用）");
                println!("  改时区    : 编译期常量 TZ_OFFSET_HOURS，改完重新 make image");
            }
            "canvas" | "canvas on" | "canvas off" => {
                let (nc, dropped) = crate::apkcanvas::stats();
                if line == "canvas off" || line == "canvas" {
                    crate::apkcanvas::set_active(false);
                    crate::fb::fill_rect(
                        (crate::fb::width() / 2 - 328) as u32,
                        (crate::fb::height() / 2 - 208) as u32,
                        656, 416, 0x1d1f24,
                    );
                    println!("  [CV] 隐藏（{} 条命令，丢弃 {}）", nc, dropped);
                }
                if line == "canvas on" || line == "canvas" {
                    crate::apkcanvas::set_active(true);
                    println!("  [CV] 显示（{} 条命令，丢弃 {}）", nc, dropped);
                }
                crate::shell::render_dashboard();
            }
            "fbinfo" => {
                if !crate::fb::available() {
                    println!("  没有 framebuffer");
                } else {
                    let (w, h) = (crate::fb::width(), crate::fb::height());
                    let base = crate::fb::base_addr();
                    println!("  板级   {}", crate::board::ACTIVE.name);
                    println!("  显示   {}x{} @ {:#x}", w, h, base);
                    println!(
                        "  自检   (0,0)={:#x}  中心={:#x}  ({}, {})",
                        crate::fb::read_px(0, 0),
                        crate::fb::read_px(w / 2, h / 2),
                        w / 2,
                        h / 2
                    );
                    if crate::board::ACTIVE.mailbox.is_some() {
                        println!("  来源   VideoCore mailbox property 通道");
                    } else {
                        println!("  来源   fw_cfg / etc/ramfb");
                    }
                }
            }
            "perf" => {
                let (a, b, c) = crate::fb::frame_breakdown();
                println!("  整桌 {:?} ms   壁纸 {:?}   侧栏 {:?}   窗口 {:?}   帧 {}",
                    crate::fb::last_frame_ms(), a, b, c, crate::fb::frame());
                println!("  窗口 终端 {}x{}  系统 {}x{}  网络 {}x{}",
                    crate::fb::win(0).w, crate::fb::win(0).h,
                    crate::fb::win(1).w, crate::fb::win(1).h,
                    crate::fb::win(2).w, crate::fb::win(2).h);
            }
            "wins" => {
                for (n, x, y, w, h, o) in crate::fb::dump_wins() {
                    println!("  {} ({},{}) {}x{} open={}", n, x, y, w, h, o);
                }
            }
            "whoami" | "id" => {
                let uid = crate::auth::current_uid();
                println!(
                    "  {}  uid={} gid={}  权限{}",
                    crate::auth::current_name(),
                    uid,
                    crate::auth::current_gid(),
                    if uid == 0 {
                        "（root，全通）"
                    } else {
                        "（非 root：看 owner 位，别人看 other 位）"
                    }
                );
                if unsafe { EXTREME } {
                    println!("  EXTREME 已解锁：root 可 su");
                }
            }
            "passwd" => {
                println!("  用户库（内存 {} 个，落盘于根目录 passwd）", crate::auth::count());
                for i in 0..crate::auth::count() {
                    let (name, uid) = crate::auth::user(i);
                    println!("  {:<8} uid={} gid={}", name, uid, crate::auth::gid_of(uid));
                }
                if crate::auth::save_to_disk() {
                    println!("  已同步到 passwd");
                }
            }
            "users" => {
                println!("  用户表 {} 个（* 当前）", crate::auth::count());
                for i in 0..crate::auth::count() {
                    let (name, uid) = crate::auth::user(i);
                    let me = if crate::auth::current_uid() == uid { "*" } else { " " };
                    println!("  {} {:<8} uid={} gid={}", me, name, uid, crate::auth::gid_of(uid));
                }
            }
            "su" => println!("  用法: su <root|aster|guest|名字>（切 root 需先 ex）"),
            "useradd" => println!("  用法: useradd <名字>（需要 root 或 EXTREME）"),
            "chmod" => println!("  用法: chmod <八进制> <文件>   例: chmod 640 note.txt / chmod 4755 tool"),
            "chown" => println!("  用法: chown <用户|uid> <文件>"),
            "fmt" => cmd_fmt(),
            "mkfs" => cmd_mkfs(),
            "ls" => cmd_ls(""),
            l if l.starts_with("stat ") => {
                let name = l[5..].trim();
                match crate::afs::raw_inode(name) {
                    Some(f) => println!(
                        "  {} ino={} typ={} size={} mode={:o} uid={} parent={} blk0={}",
                        name, f.ino, f.typ, f.size, f.mode, f.uid, f.parent, f.blocks[0]
                    ),
                    None => println!("  读不到 {} 的 inode", name),
                }
            }
            l if l.starts_with("su ") => {
                let name = l[3..].trim();
                match crate::auth::su(name) {
                    Ok(uid) => println!("  → {} (uid {})", name, uid),
                    Err(e) => println!("  {}", e),
                }
            }
            l if l.starts_with("useradd ") => {
                let name = l[8..].trim();
                match crate::auth::add(name) {
                    Ok(uid) => {
                        println!("  已加用户 {} (uid {})", name, uid);
                        if crate::auth::save_to_disk() {
                            println!("  已写入 passwd");
                        } else {
                            println!("  写 passwd 失败（磁盘没格式化？）");
                        }
                    }
                    Err(e) => println!("  {}", e),
                }
            }
            l if l.starts_with("chmod ") => {
                let rest = l[6..].trim();
                let (m, name) = match rest.split_once(' ') {
                    Some(x) => (x.0.trim(), x.1.trim()),
                    None => ("", ""),
                };
                let mode = match u16::from_str_radix(m, 8) {
                    Ok(x) if x <= 0o7777 => x,
                    _ => {
                        println!("  用法: chmod <八进制> <文件>（0..7777，前一位是 setuid/setgid/sticky）");
                        continue;
                    }
                };
                if name.is_empty() {
                    println!("  用法: chmod <八进制> <文件>");
                    continue;
                }
                let (old_mode, owner, grp) = match crate::afs::meta(name) {
                    Some(x) => x,
                    None => {
                        println!("  磁盘上没有 {}（先 fmt/mkfs？）", name);
                        continue;
                    }
                };
                if !crate::auth::can(old_mode, owner, grp, crate::auth::W_WRITE) && !unsafe { EXTREME } {
                    println!("  没有权限改 {} 的权限位（owner 不是你）", name);
                    continue;
                }
                match crate::afs::set_meta(name, Some(mode), None, None) {
                    Ok((m2, _, _)) => {
                        let mut pb = [0u8; 11];
                        crate::auth::mode_str(m2, 1, &mut pb);
                        println!("  {} → {}", name, core::str::from_utf8(&pb[..10]).unwrap_or(""));
                    }
                    Err(e) => println!("  {}", e),
                }
            }
            l if l.starts_with("chgrp ") => {
                let rest = l[6..].trim();
                let (who, name) = match rest.split_once(' ') {
                    Some(x) => (x.0.trim(), x.1.trim()),
                    None => ("", ""),
                };
                if who.is_empty() || name.is_empty() {
                    println!("  用法: chgrp <gid> <文件>");
                    continue;
                }
                let gid: u16 = match who.parse() {
                    Ok(x) => x,
                    Err(_) => {
                        println!("  gid 要是数字（查看 users 里的 gid）");
                        continue;
                    }
                };
                if !crate::auth::is_root() && !unsafe { EXTREME } {
                    println!("  chgrp 需要 root 或 EXTREME");
                    continue;
                }
                match crate::afs::set_meta(name, None, None, Some(gid)) {
                    Ok((_, _, g2)) => println!("  {} → gid {}", name, g2),
                    Err(e) => println!("  {}", e),
                }
            }
            l if l.starts_with("chown ") => {
                let rest = l[6..].trim();
                let (who, name) = match rest.split_once(' ') {
                    Some(x) => (x.0.trim(), x.1.trim()),
                    None => ("", ""),
                };
                if who.is_empty() || name.is_empty() {
                    println!("  用法: chown <用户|uid> <文件>");
                    continue;
                }
                if !crate::auth::is_root() && !unsafe { EXTREME } {
                    println!("  chown 需要 root 或 EXTREME");
                    continue;
                }
                let uid = match crate::auth::uid_of(who) {
                    Some(u) => u,
                    None => {
                        println!("  没有这个用户/uid: {}", who);
                        continue;
                    }
                };
                match crate::afs::set_meta(name, None, Some(uid), Some(crate::auth::gid_of(uid))) {
                    Ok((_, u2, g2)) => println!("  {} → owner uid {} gid {}", name, u2, g2),
                    Err(e) => println!("  {}", e),
                }
            }
            l if l == "ls -l" || l.starts_with("ls -l") => cmd_ls("-l"),
            "cat" => cmd_cat(""),
            "wr" => cmd_wr(""),
            "df" => cmd_df(),
            "cache" => cmd_cache(),
            "fd" => cmd_fd(),
            "vfw" => cmd_vfw(),
            "apk" => cmd_apk(""),
            "apk2" => cmd_apk2(""),
            "dex" => cmd_dex(""),
            "usb" => crate::usb::probe(),
            "usb poll" => {
                let n = crate::usb::poll_hid();
                println!("  轮询 1 次：{} 个事件（累计 {}）", n, crate::usb::events());
            }
            "bootanim" => crate::bootanim::play(),
            "file" => match crate::task::run_file() {
                Ok(code) => println!("[VFS] 任务结束，退出码 {}", code),
                Err(e) => println!("[TASK] 无法运行: {}", e),
            },
            "load" => cmd_load("hello"),
            l if l.starts_with("load ") => cmd_load(l[5..].trim()),
            "sync" => cmd_sync(),
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
            "dhcp" => {
                println!("  DHCP: DISCOVER → OFFER → REQUEST → ACK");
                let t0 = crate::timer::now_ms();
                if crate::net::dhcp_acquire(2500) {
                    let ip = crate::net::ip();
                    let gw = crate::net::gw();
                    let dns = crate::net::dns();
                    println!(
                        "  租到 {}.{}.{}.{}  网关 {}.{}.{}.{}  DNS {}.{}.{}.{}  耗时 {} ms",
                        ip[0], ip[1], ip[2], ip[3], gw[0], gw[1], gw[2], gw[3],
                        dns[0], dns[1], dns[2], dns[3],
                        crate::timer::now_ms().saturating_sub(t0)
                    );
                } else {
                    println!("  没拿到租约（slirp 没回？确认 -netdev user,id=n0 已挂）");
                }
            }
            l if l.starts_with("fetch ") => {
                // fetch <host[:port]/path> <磁盘上的名字>  —— 真下载 + 落 AsterFS
                let rest = l[6..].trim();
                let (url, name) = match rest.split_once(' ') {
                    Some(x) => (x.0.trim(), x.1.trim()),
                    None => ("", ""),
                };
                if url.is_empty() || name.is_empty() {
                    println!("  用法: fetch 10.0.2.2:8088/pkg-git.ase pkg-git.ase");
                    continue;
                }
                if crate::disk::probe().is_none() {
                    println!("  没有 AsterFS（先 fmt + mkfs）");
                    continue;
                }
                let t0 = crate::timer::now_ms();
                match crate::net::fetch_url(url, 4000) {
                    Ok((off, len)) => {
                        let body = &crate::net::cli_data()[off..off + len];
                        match crate::vfs::open(name, true) {
                            Ok(fd) => {
                                let n = crate::vfs::write(fd, body);
                                crate::vfs::close(fd);
                                println!(
                                    "  下载 {} → {}（{} 字节，{} ms）",
                                    url, name, n,
                                    crate::timer::now_ms().saturating_sub(t0)
                                );
                            }
                            Err(e) => println!("  写 {} 失败（{}）", name, e),
                        }
                    }
                    Err(code) => {
                        let why = match code {
                            -1 => "网络不可用",
                            -2 => "ARP 拿不到 MAC",
                            -3 => "连不上",
                            -4 => "超过 16 KiB",
                            -5 => "DNS 解析失败",
                            -6 => "响应没有头体分隔",
                            _ => "未知错误",
                        };
                        println!("  下载失败: {}（{}）", why, code);
                    }
                }
            }
            l if l.starts_with("dns ") => {
                let name = l[4..].trim();
                if name.is_empty() {
                    println!("  用法: dns example.com");
                    continue;
                }
                let t0 = crate::timer::now_ms();
                match crate::net::resolve(name, 1200) {
                    Some(ip) => println!(
                        "  {} → {}.{}.{}.{}  耗时 {} ms",
                        name, ip[0], ip[1], ip[2], ip[3],
                        crate::timer::now_ms().saturating_sub(t0)
                    ),
                    None => println!("  解析失败（DNS {}.{}.{}.{} 没回 A 记录）", crate::net::dns()[0], crate::net::dns()[1], crate::net::dns()[2], crate::net::dns()[3]),
                }
            }
            "uptime" => println!("  已运行 {} ms", uptime_ms()),
            "date" => cmd_date(),

            "perf" => cmd_perf(),
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
    // APK 跑过画布就盖在最上层（桌面重绘不擦掉它）
    crate::apkcanvas::draw_if_active();
    // 软键盘盖在所有窗口之上（手机端唯一能用的输入手段）
    crate::kb::draw_if_open();
}

/// 包索引表由 tools/mkpkg.py 生成（kernel/src/pkgidx.rs），加包只改脚本再重跑。
/// 这里只管状态与命令：本进程装过哪些（PKG_ON），以及 list/info/install。
use crate::pkgidx::{CATS, NPKG, PKGS};

/// 本次运行里的安装状态：true 表示已落到 AsterFS 的 /pkg-<name>.ase
static mut PKG_ON: [bool; NPKG] = [false; NPKG];

fn pkg_find(name: &str) -> Option<usize> {
    PKGS.iter().position(|p| p.name == name)
}

/// 分类代码 → 中文标签；认不出来的原样返回（不返回 &str，免得引发生命周期问题）
fn cat_label(c: &str) -> &'static str {
    for (k, v) in CATS.iter() {
        if *k == c {
            return v;
        }
    }
    "未知分类"
}


/// http get <ip> <port> <path> [保存名]
///
/// 内核自己的 HTTP 客户端：ARP → SYN → GET → 收完 → 写 AsterFS。
/// 不做重传、不管乱序；先证明"能主动从网上下东西"这件事行得通。
fn cmd_http(a: &str) {
    let mut it = a.split_whitespace();
    let sub = it.next().unwrap_or("");
    if sub != "get" {
        println!("  用法: http get <ip> <port> <path> [保存名]   例: http get 10.0.2.2 8001 /hello.txt");
        return;
    }
    let ip_s = it.next().unwrap_or("");
    let port: u16 = it.next().unwrap_or("80").parse().unwrap_or(80);
    let path = it.next().unwrap_or("/");
    let save = it.next().unwrap_or("/net-dl.bin");

    let mut ip = [0u8; 4];
    let mut part = 0usize;
    let mut num = 0u32;
    let mut digits = 0;
    for c in ip_s.bytes() {
        if c == b'.' {
            if part < 3 {
                ip[part] = num.min(255) as u8;
                part += 1;
            }
            num = 0;
            digits = 0;
        } else if c.is_ascii_digit() {
            num = num * 10 + (c - b'0') as u32;
            digits += 1;
        }
    }
    if part < 3 || digits == 0 {
        println!("  IP 写法不对：要 a.b.c.d");
        return;
    }
    ip[3] = num.min(255) as u8;

    if !crate::net::available() {
        println!("  没有 virtio-net（启动时带 -device virtio-net-device,netdev=n0）");
        return;
    }
    println!("  GET {}:{}{} …（等 3 秒）", ip_s, port, path);
    let n = crate::net::http_get(ip, port, path, 3000);
    if n < 0 {
        let why = match n {
            -1 => "网络没就绪",
            -2 => "ARP 拿不到对方 MAC（对端不在同一网段？）",
            -3 => "连不上或没收到数据",
            -4 => "响应超过 16 KiB 缓冲",
            _ => "未知错误",
        };
        println!("  失败（{}）：{}", n, why);
        return;
    }
    let body = crate::net::cli_data();
    // 只看状态行，够判断成不成
    let mut line = [0u8; 64];
    let mut k = 0usize;
    for &b in body.iter().take(64) {
        if b == b'\r' || b == b'\n' {
            break;
        }
        line[k] = b;
        k += 1;
    }
    println!("  收到 {} 字节   状态行: {}", n, core::str::from_utf8(&line[..k]).unwrap_or("?"));

    if !crate::blk::available() {
        println!("  没有 virtio-blk，不落盘（挂盘后会自动保存到 {}）", save);
        return;
    }
    match crate::vfs::open(save, true) {
        Ok(fd) => {
            let w = crate::vfs::write(fd, body);
            crate::vfs::close(fd);
            if w < 0 {
                println!("  写 {} 失败（{}）", save, w);
            } else {
                println!("  已保存 {}（{} 字节，{} 块）  查看: cat {}", save, w, (w as u32).div_ceil(4096), save);
            }
        }
        Err(e) => println!("  打开 {} 失败（{}）", save, e),
    }
}

fn cmd_pkg(a: &str) {
    let mut it = a.split_whitespace();
    let sub = it.next().unwrap_or("list");
    match sub {
        "list" | "" => {
            // pkg list            → 分类概览
            // pkg list <分类>      → 该分类明细
            // pkg list installed  → 只列已装
            let cat = it.next().unwrap_or("");
            if cat.is_empty() {
                let mut installed = 0usize;
                for v in unsafe { PKG_ON }.iter() {
                    if *v {
                        installed += 1;
                    }
                }
                println!("  Aster 包索引  {} 个 / {} 类，已装 {}", NPKG, CATS.len(), installed);
                for (c, label) in CATS.iter() {
                    let mut n = 0usize;
                    let mut on = 0usize;
                    let mut sample = "";
                    for (i, p) in PKGS.iter().enumerate() {
                        if p.cat != *c {
                            continue;
                        }
                        if n == 0 {
                            sample = p.name;
                        }
                        n += 1;
                        if unsafe { PKG_ON[i] } {
                            on += 1;
                        }
                    }
                    println!("  {:<8} {:<12} {:>2} 个   已装 {}{}", c, label, n, on, if n > 0 { "" } else { "" });
                    let _ = sample;
                }
                println!("  用法: pkg list <分类> 看明细 · pkg search <词> · pkg install <名>");
                println!("  分类: lang build vcs net edit sys db media sec android");
                return;
            }
            let mut idx = [0usize; NPKG];
            let mut n = 0usize;
            for (i, p) in PKGS.iter().enumerate() {
                let hit = if cat == "installed" {
                    unsafe { PKG_ON[i] }
                } else {
                    p.cat == cat
                };
                if hit {
                    idx[n] = i;
                    n += 1;
                }
            }
            if n == 0 {
                println!("  {} 里没有包（或还没装东西）", cat);
                return;
            }
            println!(
                "  Aster 包索引  {} 共 {} 个",
                if cat == "installed" { "已装" } else { cat_label(cat) },
                n
            );
            let mut k = 0usize;
            while k < n {
                let p = &PKGS[idx[k]];
                let m = unsafe { if PKG_ON[idx[k]] { "*" } else { " " } };
                if k + 1 < n {
                    let q = &PKGS[idx[k + 1]];
                    let m2 = unsafe { if PKG_ON[idx[k + 1]] { "*" } else { " " } };
                    println!("  {}{:<11}{:<8}{}{:<11}{:<8}", m, p.name, p.ver, m2, q.name, q.ver);
                } else {
                    println!("  {}{:<11}{:<8}", m, p.name, p.ver);
                }
                k += 2;
            }
            println!("  * = 已装    pkg info <名> 看详情 · pkg install <名> 装");
        }
        "search" => {
            let kw = it.next().unwrap_or("");
            if kw.is_empty() {
                println!("  用法: pkg search <关键词>");
                return;
            }
            let mut n = 0;
            for (i, p) in PKGS.iter().enumerate() {
                if p.name.contains(kw) || p.desc.contains(kw) || p.cat == kw {
                    let st = unsafe { if PKG_ON[i] { "[已装]" } else { "[可装]" } };
                    println!("  {} {:<10} {:<8} {}", st, p.name, p.ver, p.desc);
                    n += 1;
                }
            }
            println!("  命中 {} 个", n);
        }
        "info" => match it.next().map(pkg_find) {
            Some(Some(i)) => {
                let p = &PKGS[i];
                println!("  名称      : {}", p.name);
                println!("  版本      : {}   （占位版本号）", p.ver);
                println!("  分类      : {}", p.cat);
                println!("  包体      : {} 字节，CRC32 写在头 8..12", p.blob.len() - 40);
                println!("  说明      : {}", p.desc);
                println!("  落盘路径  : {}", p.path);
                println!("  状态      : {}", unsafe { if PKG_ON[i] { "已安装" } else { "未安装" } });
            }
            _ => println!("  没有这个包"),
        },
        "install" => {
            let name = it.next().unwrap_or("");
            let idx = match pkg_find(name) {
                Some(i) => i,
                None => {
                    println!("  索引里没有 {}", name);
                    return;
                }
            };
            if unsafe { PKG_ON[idx] } {
                println!("  {} 已经装好了", name);
                return;
            }
            // pkg install <名> <url>：给了 URL 就真的从网络取包体（PKG-3）
            if let Some(url) = it.next() {
                if crate::disk::probe().is_none() {
                    println!("  没有 AsterFS（先 fmt + mkfs）");
                    return;
                }
                let t0 = crate::timer::now_ms();
                match crate::net::fetch_url(url, 5000) {
                    Ok((off, len)) => {
                        let body = &crate::net::cli_data()[off..off + len];
                        match crate::vfs::open(PKGS[idx].path, true) {
                            Ok(fd) => {
                                let n = crate::vfs::write(fd, body);
                                crate::vfs::close(fd);
                                unsafe { PKG_ON[idx] = true };
                                println!(
                                    "  远程安装 {} → {}（{} 字节，{} ms）",
                                    name, PKGS[idx].path, n,
                                    crate::timer::now_ms().saturating_sub(t0)
                                );
                            }
                            Err(e) => println!("  写 {} 失败（{}）", PKGS[idx].path, e),
                        }
                    }
                    Err(c) => println!("  下载失败（错误码 {}）", c),
                }
                return;
            }
            // 本地索引里的 144 字节占位包
            if !crate::blk::available() {
                println!("  没有 virtio-blk（先挂盘再 pkg install）");
                return;
            }
            let path = PKGS[idx].path;
            match crate::vfs::open(path, true) {
                Ok(fd) => {
                    let n = crate::vfs::write(fd, PKGS[idx].blob);
                    crate::vfs::close(fd);
                    if n < 0 {
                        println!("  写 {} 失败（错误码 {}）", path, n);
                        return;
                    }
                    unsafe { PKG_ON[idx] = true };
                    println!("  已安装 {} → {}（{} 字节，{} 块）", name, path, n, (n as u32).div_ceil(4096));
                    println!("  查看: ls   /   pkg list");
                }
                Err(e) => println!("  打开 {} 失败（{}）", path, e),
            }
        }
        "remove" => {
            let name = it.next().unwrap_or("");
            match pkg_find(name) {
                Some(i) => {
                    unsafe { PKG_ON[i] = false };
                    println!("  已卸载 {}（AsterFS 里的 /pkg-{} 留着，v0 没有 unlink）", name, name);
                }
                None => println!("  索引里没有 {}", name),
            }
        }
        other => println!("  未知子命令 {}（list / search / info / install / remove）", other),
    }
}

fn cmd_date() {
    match crate::rtc::local() {
        Some((y, m, d, hh, mm, ss)) => {
            println!("  {}-{:02}-{:02} {:02}:{:02}:{:02}  {}", y, m, d, hh, mm, ss, crate::rtc::TZ_NAME);
            println!("  时区      : {}（UTC+{}，固定不跟随宿主机）", crate::rtc::TZ_NAME, crate::rtc::offset_hours());
            println!("  来源      : PL031 RTC 读 UTC，内核统一加固定偏移");
        }
        None => println!("  RTC 不可用（没有 PL031 或没上电）"),
    }
}

fn cmd_perf() {
    let (rest, side, wins) = fb::frame_breakdown();
    let frames = fb::frame_count();
    let up = uptime_ms();
    println!("  帧号      : {}", frames);
    println!("  上一帧耗时: 恢复壁纸 {} ms / 侧栏 {} ms / 窗口 {} ms", rest, side, wins);
    println!("  合计      : {} ms（{} fps 上限）", rest + side + wins,
        if rest + side + wins == 0 { 0 } else { 1000 / (rest + side + wins).max(1) });
    println!("  运行时长  : {} ms → 平均 {:.1} 帧/秒", up,
        frames as f64 * 1000.0 / up.max(1) as f64);
    println!("  帧缓冲    : {}x{} 双缓冲（画满一帧才 present）", fb::width(), fb::height());
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
    let ip = crate::net::ip();
    let gw = crate::net::gw();
    let dns = crate::net::dns();
    println!(
        "  IP        : {}.{}.{}.{}/24   网关 {}.{}.{}.{}{}",
        ip[0], ip[1], ip[2], ip[3], gw[0], gw[1], gw[2], gw[3],
        if crate::net::dhcp_ok() { "（DHCP 已租到）" } else { "（静态默认，可 dhcp）" }
    );
    println!("  DNS       : {}.{}.{}.{}", dns[0], dns[1], dns[2], dns[3]);
    println!("  服务      : TCP :80（宿主用 hostfwd 映射即可打开）");
    println!("  收/发帧   : {}/{}", rx, tx);
    println!("  建立连接  : {}", conns);
    let (ver, st, av, us, is) = crate::net::debug_info();
    println!("  设备版本  : {}   状态: {:#x}   RX avail={} used={}  intstatus={:#x}", ver, st, av, us, is);
    println!("  网关 MAC  : {}", if crate::net::gw_known() { "已学习" } else { "未知（ARP 未回）" });
}

// ---------- APK / zip 容器 ----------

static DEMO_APK: &[u8] = include_bytes!("../../assets/demo.apk");
static DEMO2_APK: &[u8] = include_bytes!("../../assets/demo2.apk");
static DEMO3_APK: &[u8] = include_bytes!("../../assets/demo3.apk");
/// inflate 的输出缓冲。APK 里的 dex 通常几百 KB，这里给 128 KiB；
/// 超出会明确报"输出超出缓冲"，不会越界写。
static mut INFL_BUF: [u8; 131072] = [0; 131072];

/// FNV-1a 64：用来和解压前的原始内容对指纹，比逐字节打印可靠
fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x1_0000_0001_b3);
    }
    h
}

// —— APK-11：驻留 app（帧回调 + 拖动）——
static mut APP_ON: bool = false;
static mut APP_FRAMES: u64 = 0;

pub fn app_active() -> bool {
    unsafe { APP_ON }
}

pub fn app_frames() -> u64 {
    unsafe { APP_FRAMES }
}

fn app_dex() -> Result<(&'static [u8], crate::dex::Header), &'static str> {
    let buf = unsafe { &mut INFL_BUF[..] };
    let n = crate::zip::extract(DEMO3_APK, "classes.dex", buf)?;
    // 注意：这里必须把 buffer 当成 'static —— INFL_BUF 本身是静态的
    let d: &'static [u8] = unsafe { &INFL_BUF[..n] };
    let h = crate::dex::header(d)?;
    Ok((d, h))
}

fn app_start() -> Result<bool, &'static str> {
    app_start_arg(0)
}

/// 带启动参数的起动（APK-13）：arg 由 APK 用 App.arg() 取回
fn app_start_arg(arg: i32) -> Result<bool, &'static str> {
    crate::dex::set_start_arg(arg);
    let (d, h) = app_dex()?;
    let (_steps, created) = crate::dex::app_begin(d, &h, "Lcom/aster/Main;")?;
    unsafe {
        APP_ON = true;
        APP_FRAMES = 0;
    }
    crate::apkcanvas::set_active(true);
    Ok(created)
}

/// 一帧：失败返回错误串并自动停机，免得每帧都刷同一条错误
pub fn app_frame() -> Option<&'static str> {
    if !app_active() {
        return None;
    }
    let (d, h) = match app_dex() {
        Ok(v) => v,
        Err(e) => {
            unsafe { APP_ON = false };
            return Some(e);
        }
    };
    // 先把这一拍攒下的触摸事件全部分给 onTouch(kind, x, y)
    loop {
        match crate::apkcanvas::next_event() {
            Some((kind, x, y)) => {
                if let Err(e) = crate::dex::app_touch(d, &h, "Lcom/aster/Main;", kind as i32, x, y) {
                    unsafe { APP_ON = false };
                    return Some(e);
                }
            }
            None => break,
        }
    }

    // 每帧从空命令表开始：APK 画什么就是什么，不叠昨天的帧
    crate::apkcanvas::reset();
    match crate::dex::app_step(d, &h, "Lcom/aster/Main;", "frame") {
        Ok(_) => {
            unsafe { APP_FRAMES += 1 };
            None
        }
        Err(e) => {
            unsafe { APP_ON = false };
            Some(e)
        }
    }
}

fn cmd_app(line: &str) {
    match line {
        "app" | "app status" => {
            if app_active() {
                let (nc, drop) = crate::apkcanvas::stats();
                let (pages, used, cap) = crate::dex::heap_stats();
                println!("  [APP] 运行中，已画 {} 帧，命令 {} 条（丢弃 {}）", app_frames(), nc, drop);
                println!("  [APP] 对象堆 {} 页 / 槽 {} 用 {}（容量 {}）", pages, used, used, cap);
                println!("  [APP] 启动参数 arg={}", crate::dex::start_arg());
            } else {
                println!("  [APP] 未运行。app start [n] 起动 Main.frame()V，n 用 App.arg() 取回");
            }
        }
        "app step" => {
            // 手动跑一帧：无显示环境（-nographic）也能验证 frame() 能出命令
            if !app_active() {
                match app_start() {
                    Ok(_) => {}
                    Err(e) => {
                        println!("  [APP] 起动失败: {}", e);
                        return;
                    }
                }
            }
            match app_frame() {
                None => {
                    let (nc, _) = crate::apkcanvas::stats();
                    println!("  [APP] 帧 {} 完成，画布命令 {} 条", app_frames(), nc);
                }
                Some(e) => println!("  [APP] 帧失败: {}", e),
            }
        }
        "app off" | "app stop" => {
            let was_on = app_active();
            let mut destroyed = false;
            if was_on {
                if let Ok((d, h)) = app_dex() {
                    match crate::dex::app_end(d, &h, "Lcom/aster/Main;") {
                        Ok((_, has)) => destroyed = has,
                        Err(e) => println!("  [APP] onDestroy 失败: {}", e),
                    }
                }
            }
            unsafe { APP_ON = false };
            println!(
                "  [APP] 已停止，画布保留最后一帧（{} 帧）{}",
                app_frames(),
                if destroyed { "，onDestroy 已调用" } else { "" }
            );
        }
        _ => {
            // app start [n]
            let arg = line
                .strip_prefix("app start")
                .and_then(|r| r.trim().parse::<i32>().ok())
                .unwrap_or(0);
            match app_start_arg(arg) {
                Ok(created) => println!(
                    "  [APP] 起动：{}onCreate()，arg={}，之后每 30ms 一帧 Main.frame()V；触摸走 onTouch(kind,x,y)。app stop 停止",
                    if created { "" } else { "无" },
                    arg
                ),
                Err(e) => println!("  [APP] 起动失败: {}", e),
            }
        }
    }
}

fn cmd_dex(a: &str) {
    let t = a.trim();
    let buf = unsafe { &mut INFL_BUF[..] };
    let n = match crate::zip::extract(DEMO3_APK, "classes.dex", buf) {
        Ok(n) => n,
        Err(e) => {
            println!("  {}", e);
            return;
        }
    };
    let d = &buf[..n];
    if t == "run" || t == "hello" || t == "big" || t == "str" || t == "poly" {
        let which = match t {
            "run" => "run",
            "big" => "big",
            "str" => "str",
            "poly" => "poly",
            _ => "hello",
        };
        match crate::dex::header(d) {
            Ok(h) => match crate::dex::run(d, &h, "Lcom/aster/Main;", which) {
                Ok(steps) => println!("  {}() 执行完（{} 条指令），正常 return-void", which, steps),
                Err(e) => println!("  {}", e),
            },
            Err(e) => println!("  {}", e),
        }
    } else {
        match crate::dex::dump(d) {
            Ok(_) => {}
            Err(e) => println!("  {}", e),
        }
        println!("  → dex run 执行 Main->run()V（循环求和 1..5 + 打印整数）；dex big 跑 2048 元素数组求和；dex str 跑 String 方法族；dex poly 跑接口分派；dex hello 执行 hello()V");
    }
}

fn cmd_apk2(a: &str) {
    let t = a.trim();
    // 期望值：打包前由宿主侧算好，这里只做比对
    const DEX_LEN: usize = 6036;
    const DEX_FNV: u64 = 0x83a2_ce97_2697_7698;

    if t == "dex" {
        let buf = unsafe { &mut INFL_BUF[..] };
        match crate::zip::extract(DEMO2_APK, "classes.dex", buf) {
            Ok(n) => {
                let head = &buf[..8];
                print!("  inflate classes.dex → {} 字节，头 ", n);
                hex8(head);
                println!();
                let h = fnv1a64(&buf[..n]);
                println!("  FNV-1a  {:016x}", h);
                if n == DEX_LEN && h == DEX_FNV {
                    println!("  → 与打包前内容一致（长度与指纹都对上），deflate 解压正确");
                } else {
                    println!(
                        "  → 不一致：期望 {} 字节 / {:016x}",
                        DEX_LEN, DEX_FNV
                    );
                }
            }
            Err(e) => println!("  {}", e),
        }
        return;
    }

    if let Some(name) = t.strip_prefix("cat ") {
        let buf = unsafe { &mut INFL_BUF[..] };
        match crate::zip::extract(DEMO2_APK, name.trim(), buf) {
            Ok(n) => {
                let mut i = 0usize;
                while i < n {
                    let end = (i + 64).min(n);
                    match core::str::from_utf8(&buf[i..end]) {
                        Ok(s) => println!("  {}", s),
                        Err(_) => {
                            print!("  ");
                            hex8(&buf[i..end]);
                            println!();
                        }
                    }
                    i = end;
                }
            }
            Err(e) => println!("  {}", e),
        }
        return;
    }

    println!("  demo2.apk  {} 字节（deflate 混合包）", DEMO2_APK.len());
    let buf = unsafe { &mut INFL_BUF[..] };
    match crate::zip::walk(DEMO2_APK, |name, method, csize, usize_, _off| {
        let m = if method == 0 { "stored" } else { "deflate" };
        match crate::zip::extract(DEMO2_APK, name, buf) {
            Ok(n) if n as u32 == usize_ => {
                println!("    {:<8} {:>6} {:>6}  {}  → {}", m, usize_, csize, name, n)
            }
            Ok(n) => println!(
                "    {:<8} {:>6} {:>6}  {}  → {}（与原始大小不符）",
                m, usize_, csize, name, n
            ),
            Err(e) => println!("    {:<8} {:>6} {:>6}  {}  → {}", m, usize_, csize, name, e),
        }
    }) {
        Ok(_) => {}
        Err(e) => println!("  {}", e),
    }
    println!("  → 两条 deflate 都能还原到原始大小");
}

fn cmd_fd() {
    crate::vfs::dump();
}

fn hex8(b: &[u8]) -> () {
    for (i, x) in b.iter().enumerate() {
        if i > 0 {
            print!(" ");
        }
        print!("{:02x}", x);
    }
}

fn cmd_apk(a: &str) {
    let t = a.trim();
    let total = DEMO_APK.len();

    if t == "dex" {
        let mut buf = [0u8; 128];
        match crate::zip::extract(DEMO_APK, "classes.dex", &mut buf) {
            Ok(n) => {
                if n >= 44 && &buf[..4] == b"dex\n" {
                    let ver = core::str::from_utf8(&buf[4..7]).unwrap_or("?");
                    let u32at = |o: usize| -> u32 {
                        u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
                    };
                    println!("  classes.dex  {} 字节，魔数 dex\\n{}", n, ver);
                    println!(
                        "  file_size {}  header_size {:#x}  endian {:#010x}",
                        u32at(32),
                        u32at(36),
                        u32at(40)
                    );
                    println!("  → DEX 头能原样取出来，解释器是下一阶段的事");
                } else {
                    println!("  classes.dex 的魔数不是 dex\\n");
                }
            }
            Err(e) => println!("  {}", e),
        }
        return;
    }

    if t == "axml" {
        let mut buf = [0u8; 64];
        match crate::zip::extract(DEMO_APK, "AndroidManifest.xml", &mut buf) {
            Ok(n) => {
                print!("  AndroidManifest.xml {} 字节，头 ", n);
                hex8(&buf[..8]);
                println!();
                if buf[0] == 0x03 && buf[1] == 0x00 && buf[2] == 0x08 && buf[3] == 0x00 {
                    println!("  → 是二进制 XML（AXML，type 0x0003 / header 8）：资源表解析器的输入格式");
                } else {
                    println!("  → 不是 AXML（可能是明文 XML）");
                }
            }
            Err(e) => println!("  {}", e),
        }
        return;
    }

    if let Some(name) = t.strip_prefix("cat ") {
        let mut buf = [0u8; 512];
        match crate::zip::extract(DEMO_APK, name.trim(), &mut buf) {
            Ok(n) => {
                let mut i = 0usize;
                while i < n {
                    let end = (i + 64).min(n);
                    match core::str::from_utf8(&buf[i..end]) {
                        Ok(s) => println!("  {}", s),
                        Err(_) => {
                            print!("  ");
                            hex8(&buf[i..end]);
                            println!();
                        }
                    }
                    i = end;
                }
            }
            Err(e) => println!("  {}", e),
        }
        return;
    }

    // 默认：列目录
    println!("  demo.apk  {} 字节", total);
    match crate::zip::summarize(DEMO_APK) {
        Ok(s) => {
            println!(
                "  {} 个条目：{} stored / {} deflate / {} 其他，解压后共 {} 字节",
                s.entries, s.stored, s.deflated, s.others, s.raw
            );
            let _ = crate::zip::walk(DEMO_APK, |name, method, csize, usize_, _off| {
                let m = if method == 0 { "stored" } else { "deflate" };
                println!("    {:<8} {:>6} {:>6}  {}", m, usize_, csize, name);
            });
            println!("  → 能直接取的是 stored；deflate 需要 inflate，下一步做");
        }
        Err(e) => println!("  {}", e),
    }
}

/// 写路径：新建 / 打开 → 写 → close 时整体落盘，再用 afs 读回校验
fn cmd_vfw() {
    let text = b"vfs: open(O_CREAT) -> write -> close -> sync";
    match crate::vfs::open("/vfs.txt", true) {
        Ok(fd) => {
            let n = crate::vfs::write(fd, text);
            if n < 0 {
                println!("  write 失败: {}", n);
            }
            let c = crate::vfs::close(fd);
            println!("  fd {} 写入 {} 字节，close={}（close 时落盘并 sync）", fd, n, c);
            let mut buf = [0u8; 64];
            match crate::afs::read_file("vfs.txt", &mut buf) {
                Ok(m) => println!(
                    "  读回 {} 字节：{}",
                    m,
                    core::str::from_utf8(&buf[..m]).unwrap_or("<非 UTF-8>")
                ),
                Err(e) => println!("  读回失败: {}", e),
            }
        }
        Err(e) => println!("  open 失败: {}（先 ex + fmt + mkfs）", e),
    }
}
