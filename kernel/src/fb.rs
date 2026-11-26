// Aster Kernel — 图形输出（ramfb + fw_cfg）
//
// QEMU virt 上没有 PCI 显卡，但有 ramfb：一块由 fw_cfg 配置的线性 framebuffer。
// 流程是标准的 fw_cfg 用法：
//   1. MMIO 基址 0x0902_0000，selector 寄存器在 +8，data 在 +0（8 字节、大端）
//   2. selector = 0x0022（文件目录），遍历找 "etc/ramfb"
//   3. 读 28 字节配置：addr(u64) fourcc(u32) flags(u32) width(u32) height(u32) stride(u32)
//   4. 填上自己的 framebuffer 地址与分辨率，写回去，QEMU 就会开始把这块内存当屏幕
//
// framebuffer 放在 0x5000_0000（512 MiB RAM 的高段），1024x768x4 = 3 MiB。
#![allow(dead_code, static_mut_refs)]

use crate::font;
use core::arch::asm;

const FW_CFG_BASE: u64 = 0x0902_0000;
// fw_cfg 的文件目录 key 是 0x0019（trace 里写着 "fw_cfg_add_bytes key 0x0019 'file_dir'"）。
// 0x0022 是目录中某个文件项的 selector，选它会读到别的东西 —— 踩过一次。
const FW_CFG_FILE_DIR: u16 = 0x0019;
const FB_PHYS: u64 = 0x5000_0000;
const FB_W: u32 = 1024;
const FB_H: u32 = 768;
const FOURCC_XRGB8888: u32 = 0x3432_5258; // 'XR24'

#[derive(Clone, Copy)]
pub struct Fb {
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub base: u64,
}

static mut FB: Option<Fb> = None;

// ---------------- fw_cfg 字节流 ----------------

struct Stream {
    buf: [u8; 8],
    pos: usize,
}

impl Stream {
    fn new() -> Self {
        Stream { buf: [0; 8], pos: 8 }
    }

    fn refill(&mut self) {
        let v = unsafe { core::ptr::read_volatile(FW_CFG_BASE as *const u64) };
        // 实测：guest 读到的 u64 低字节就是流首字节（QEMU 内部按 device-big-endian 再反转一次）
        self.buf = v.to_le_bytes();
        self.pos = 0;
    }

    fn u8(&mut self) -> u8 {
        if self.pos >= 8 {
            self.refill();
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        b
    }

    // 流里数值字段是大端排列（count 是 0x0000000A 而非 0x0A000000），
    // 所以按大端拼回数值；字符串字段本来就按顺序读，不受影响。
    fn u16(&mut self) -> u16 {
        let mut v = 0u16;
        for _ in 0..2 {
            v = (v << 8) | self.u8() as u16;
        }
        v
    }

    fn u32(&mut self) -> u32 {
        let mut v = 0u32;
        for _ in 0..4 {
            v = (v << 8) | self.u8() as u32;
        }
        v
    }

    fn u64(&mut self) -> u64 {
        let mut v = 0u64;
        for _ in 0..8 {
            v = (v << 8) | self.u8() as u64;
        }
        v
    }

    fn read_name(&mut self, n: usize) -> [u8; 64] {
        let mut out = [0u8; 64];
        for i in 0..n.min(63) {
            out[i] = self.u8();
        }
        out
    }
}

fn select(key: u16) {
    // 踩坑记录：fw_cfg 的 ctl 寄存器按"设备大端"处理，而 aarch64 侧是小端访问，
    // QEMU 会把写入值再按大端反转一次。所以这里必须先 to_be() 预反转，
    // 否则 key=0x0022 到设备那侧会变成 0x2200，选不中任何条目。
    unsafe { core::ptr::write_volatile((FW_CFG_BASE + 8) as *mut u16, key.to_be()) }
}

/// 选中条目并丢掉首块残留。
/// 实测 QEMU 在 selector 切换后，第一次 8 字节读可能仍返回上一条目的内容，
/// 因此这里先空读一次。
fn open(key: u16) -> Stream {
    select(key);
    Stream::new()
}

// ---------------- fw_cfg DMA ----------------
// MMIO 一次只搬 8 字节，读目录这种多字段结构很容易和内部偏移错位。
// DMA 接口让 QEMU 自己把整块写进内存：
//   描述符 16 字节 { control(u32), length(u32), address(u64) }，字段都是大端；
//   写完描述符后把它的物理地址写进 0x9020010，QEMU 异步填充，control 的 bit0 清零表示完成。

#[repr(C, align(16))]
struct DmaDesc {
    control: u32,
    length: u32,
    address: u64,
}

static mut DMA_DESC: DmaDesc = DmaDesc { control: 0, length: 0, address: 0 };
static mut DMA_BUF: [u8; 4096] = [0; 4096];

/// 用 DMA 读一个 fw_cfg 条目到 DMA_BUF，返回字节数
fn dma_read(key: u16, len: usize) -> Option<usize> {
    if len > 4096 {
        return None;
    }
    select(key);
    unsafe {
        let desc = core::ptr::addr_of_mut!(DMA_DESC);
        let buf = core::ptr::addr_of!(DMA_BUF) as u64;
        // control: bit0=error, bit1=read, bit2=skip, bit3=select, bit4=write
        (*desc).control = 2u32.to_be(); // 0x02 = 读请求
        (*desc).length = (len as u32).to_be();
        (*desc).address = buf.to_be();
        let d = desc as u64;
        core::ptr::write_volatile((FW_CFG_BASE + 0x10) as *mut u64, d.to_be());
        for _ in 0..200_000u32 {
            let c = u32::from_be(core::ptr::read_volatile(&(*desc).control));
            if c & 1 == 0 {
                return Some(len);
            }
            core::hint::spin_loop();
        }
    }
    None
}

/// 用 DMA 往一个 fw_cfg 条目写数据（control bit4 = write）
fn dma_write(key: u16, data: &[u8]) -> bool {
    if data.len() > 4096 {
        return false;
    }
    select(key);
    unsafe {
        let buf = core::ptr::addr_of!(DMA_BUF) as *mut u8;
        for (i, b) in data.iter().enumerate() {
            core::ptr::write_volatile(buf.add(i), *b);
        }
        let desc = core::ptr::addr_of_mut!(DMA_DESC);
        (*desc).control = 0x10u32.to_be(); // 0x10 = 写
        (*desc).length = (data.len() as u32).to_be();
        (*desc).address = (buf as u64).to_be();
        let d = desc as u64;
        core::ptr::write_volatile((FW_CFG_BASE + 0x10) as *mut u64, d.to_be());
        for _ in 0..200_000u32 {
            let c = u32::from_be(core::ptr::read_volatile(&(*desc).control));
            if c & 1 == 0 {
                return (c & 2) == 0; // bit1 置位表示出错
            }
            core::hint::spin_loop();
        }
    }
    false
}

fn dma_buf() -> &'static [u8] {
    unsafe { &DMA_BUF }
}

fn write_word(bytes: [u8; 8]) {
    let v = u64::from_le_bytes(bytes);
    unsafe { core::ptr::write_volatile(FW_CFG_BASE as *mut u64, v) }
}

/// 在 fw_cfg 文件目录里找指定名字，返回它的 selector。
/// 目录项内存布局（DMA 写进来的是 host 序，即小端）：
///   u32 count
///   每项 64 字节：u32 size(=名字长度) | u16 select | u16 reserved | char name[56]
fn find_file(name: &[u8]) -> Option<u16> {
    let n = dma_read(FW_CFG_FILE_DIR, 2052)?;
    let b = dma_buf();
    let count = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    if count == 0 || count > 64 || n < 4 + count * 64 {
        return None;
    }
    for i in 0..count {
        let off = 4 + i * 64;
        let size = u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as usize;
        let sel = u16::from_be_bytes([b[off + 4], b[off + 5]]);
        if size >= name.len() && size <= 55 {
            let nm = &b[off + 8..off + 8 + size];
            if &nm[..name.len()] == name {
                return Some(sel);
            }
        }
    }
    None
}

// ---------------- 初始化 ----------------

pub fn init() -> bool {
    let sel = match find_file(b"etc/ramfb") {
        Some(s) => s,
        None => {
            crate::println!("[FWCFG] 目录里没有 etc/ramfb");
            return false;
        }
    };

    let cfg_len = 28;
    if dma_read(sel, cfg_len).is_none() {
        crate::println!("[FWCFG] 读 ramfb 配置超时");
        return false;
    }

    // ramfb 配置结构（QEMU 直接 memcpy 成 host 序结构体，所以按小端排布）：
    //   u64 addr | u32 fourcc | u32 flags | u32 width | u32 height | u32 stride
    let stride = FB_W * 4;
    // fw_cfg 的写回调给到设备的 val 按大端解释，所以结构体字段要按大端排布
    let mut cfg = [0u8; 32];
    cfg[0..8].copy_from_slice(&FB_PHYS.to_be_bytes());
    cfg[8..12].copy_from_slice(&FOURCC_XRGB8888.to_be_bytes());
    cfg[12..16].copy_from_slice(&0u32.to_be_bytes());
    cfg[16..20].copy_from_slice(&FB_W.to_be_bytes());
    cfg[20..24].copy_from_slice(&FB_H.to_be_bytes());
    cfg[24..28].copy_from_slice(&stride.to_be_bytes());

    if !dma_write(sel, &cfg[..cfg_len]) {
        crate::println!("[FWCFG] 写 ramfb 配置失败");
        return false;
    }

    unsafe {
        FB = Some(Fb { w: FB_W, h: FB_H, stride, base: FB_PHYS });
    }
    clear(0x0B1119);
    true
}

/// 列出 fw_cfg 目录里的所有文件（调试用）
pub fn dump_fwcfg() {
    let n = match dma_read(FW_CFG_FILE_DIR, 2052) {
        Some(n) => n,
        None => {
            crate::println!("[FWCFG] 目录读取失败");
            return;
        }
    };
    let b = dma_buf();
    let count = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    crate::println!("[FWCFG] 条目 {} 个（读回 {} 字节）", count, n);
    for i in 0..count.min(16) {
        let off = 4 + i * 64;
        let size = u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as usize;
        let sel = u16::from_be_bytes([b[off + 4], b[off + 5]]);
        let mut end = 0;
        while end < 55 && b[off + 8 + end] != 0 {
            end += 1;
        }
        crate::println!("[FWCFG] sel={:#06x}  {}",
            sel, unsafe { core::str::from_utf8_unchecked(&b[off + 8..off + 8 + end]) });
    }
}

pub fn width() -> u32 {
    unsafe { FB.map(|f| f.w).unwrap_or(0) }
}

pub fn height() -> u32 {
    unsafe { FB.map(|f| f.h).unwrap_or(0) }
}

pub fn base_addr() -> u64 {
    unsafe { FB.map(|f| f.base).unwrap_or(0) }
}

pub fn available() -> bool {
    unsafe { FB.is_some() }
}

fn fb() -> &'static Fb {
    unsafe { FB.as_ref().expect("framebuffer 未初始化") }
}

// ---------------- 绘图原语 ----------------

#[inline]
fn px(f: &Fb, x: u32, y: u32, color: u32) {
    if x >= f.w || y >= f.h {
        return;
    }
    let off = (y * f.stride + x * 4) as u64;
    unsafe {
        core::ptr::write_volatile((f.base + off) as *mut u32, color);
    }
}

pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    if !available() {
        return;
    }
    let f = fb();
    for yy in y..(y + h).min(f.h) {
        for xx in x..(x + w).min(f.w) {
            let off = (yy * f.stride + xx * 4) as u64;
            unsafe {
                core::ptr::write_volatile((f.base + off) as *mut u32, color);
            }
        }
    }
}

pub fn clear(color: u32) {
    if !available() {
        return;
    }
    let f = fb();
    fill_rect(0, 0, f.w, f.h, color);
}

pub fn frame_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    fill_rect(x, y, w, 2, color);
    fill_rect(x, y + h - 2, w, 2, color);
    fill_rect(x, y, 2, h, color);
    fill_rect(x + w - 2, y, 2, h, color);
}

/// 画一个字符点阵（scale 只用于 ASCII，1 或 2）
fn draw_glyph(x: u32, y: u32, rows: &[u8], glyph_w: u32, glyph_h: u32, color: u32, scale: u32) {
    let bpr = ((glyph_w + 7) / 8) as usize;
    for r in 0..glyph_h {
        for c in 0..glyph_w {
            let byte = rows[r as usize * bpr + (c / 8) as usize];
            if byte & (0x80 >> (c % 8)) != 0 {
                fill_rect(x + c * scale, y + r * scale, scale, scale, color);
            }
        }
    }
}

fn cjk_lookup(c: char) -> Option<&'static [u8; 32]> {
    for (i, &ch) in font::CJK_CHARS.iter().enumerate() {
        if ch == c {
            return Some(&font::CJK_BITS[i]);
        }
    }
    None
}

/// 混合排版的文字：ASCII 8x16、汉字 16x16，返回结束的 x 坐标
pub fn draw_text(mut x: u32, y: u32, text: &str, color: u32, scale: u32) -> u32 {
    if !available() {
        return x;
    }
    for c in text.chars() {
        if c.is_ascii() {
            let code = c as u8;
            if code >= font::ASCII_FIRST && code <= font::ASCII_LAST {
                let idx = (code - font::ASCII_FIRST) as usize;
                draw_glyph(x, y, &font::ASCII[idx], font::ASCII_W as u32, font::ASCII_H as u32, color, scale);
            }
            x += font::ASCII_W as u32 * scale;
        } else if let Some(rows) = cjk_lookup(c) {
            draw_glyph(x, y, rows, font::CJK_W as u32, font::CJK_H as u32, color, 1);
            x += font::CJK_W as u32;
        } else {
            x += font::CJK_W as u32; // 字库没有的字留空
        }
    }
    x
}

pub fn text_len(text: &str, scale: u32) -> u32 {
    let mut w = 0;
    for c in text.chars() {
        w += if c.is_ascii() { font::ASCII_W as u32 * scale } else { font::CJK_W as u32 };
    }
    w
}

// ---------------- 界面 ----------------

const C_BG: u32 = 0x0B1119;
const C_PANEL: u32 = 0x152130;
const C_PANEL2: u32 = 0x1B2A3C;
const C_LINE: u32 = 0x2E4053;
const C_TITLE: u32 = 0xEAF2F8;
const C_TEXT: u32 = 0xAEB6BF;
const C_DIM: u32 = 0x6C7A89;
const C_ACCENT: u32 = 0x5DADE2;
const C_OK: u32 = 0x58D68D;
const C_WARN: u32 = 0xF5B041;

pub struct Dashboard<'a> {
    pub kernel_kb: u64,
    pub ex3_tag: &'a str,
    pub ex3_ok: bool,
    pub tasks: usize,
    pub switches: u32,
    pub uptime_ms: u64,
    pub slice_ms: u64,
    pub trusted: bool,
}

pub fn draw_dashboard(d: &Dashboard) {
    if !available() {
        return;
    }
    let f = fb();
    clear(C_BG);

    // 顶部标题条
    fill_rect(0, 0, f.w, 88, C_PANEL2);
    fill_rect(0, 86, f.w, 2, C_ACCENT);

    // logo：几个方块拼的星芒
    let lx = 32u32;
    let ly = 22u32;
    fill_rect(lx, ly + 16, 14, 14, C_ACCENT);
    fill_rect(lx + 22, ly, 14, 14, C_ACCENT);
    fill_rect(lx + 14, ly + 34, 14, 14, C_ACCENT);

    draw_text(96, 18, "ASTER", C_TITLE, 3);
    draw_text(96 + 5 * 8 * 3 + 16, 26, "星芒", C_TITLE, 1);
    draw_text(96, 62, "原创内核 · EL0 用户态 · 抢占调度 · 图形输出", C_TEXT, 1);

    // 右上角状态
    let rx = 700u32;
    draw_text(rx, 22, "SYSTEM", C_DIM, 1);
    draw_text(rx, 44, if d.trusted { "TRUSTED" } else { "UNTRUSTED" }, if d.trusted { C_OK } else { C_WARN }, 2);

    // 左侧信息卡
    let px0 = 32u32;
    let py0 = 120u32;
    let pw = 460u32;
    let ph = 520u32;
    fill_rect(px0, py0, pw, ph, C_PANEL);
    frame_rect(px0, py0, pw, ph, C_LINE);
    draw_text(px0 + 20, py0 + 16, "内核 KERNEL", C_ACCENT, 1);
    fill_rect(px0 + 20, py0 + 40, pw - 40, 1, C_LINE);

    let mut y = py0 + 56;
    let row = |y: u32| y;
    draw_text(px0 + 20, row(y), "版本", C_DIM, 1);
    draw_text(px0 + 150, row(y), "v0.4  ARM64", C_TEXT, 1);
    y += 30;
    draw_text(px0 + 20, row(y), "大小", C_DIM, 1);
    draw_kb(px0 + 150, row(y), d.kernel_kb);
    y += 30;
    draw_text(px0 + 20, row(y), "校验", C_DIM, 1);
    draw_text(px0 + 150, row(y), d.ex3_tag, if d.ex3_ok { C_OK } else { C_WARN }, 1);
    y += 30;
    draw_text(px0 + 20, row(y), "内存", C_DIM, 1);
    draw_text(px0 + 150, row(y), "L1/L2/L3  4KiB 页", C_TEXT, 1);
    y += 30;
    draw_text(px0 + 20, row(y), "任务", C_DIM, 1);
    draw_num(px0 + 150, row(y), d.tasks as u64);
    y += 30;
    draw_text(px0 + 20, row(y), "切换", C_DIM, 1);
    draw_num(px0 + 150, row(y), d.switches as u64);
    y += 30;
    draw_text(px0 + 20, row(y), "运行", C_DIM, 1);
    draw_num(px0 + 150, row(y), d.uptime_ms);
    draw_text(px0 + 150 + 8 * (digits(d.uptime_ms) as u32) + 8, row(y), "ms", C_DIM, 1);
    y += 30;
    draw_text(px0 + 20, row(y), "时间片", C_DIM, 1);
    draw_num(px0 + 150, row(y), d.slice_ms);
    draw_text(px0 + 150 + 8 + 8, row(y), "ms", C_DIM, 1);

    // 右侧任务卡
    let qx = 516u32;
    let qy = 120u32;
    let qw = 476u32;
    let qh = 520u32;
    fill_rect(qx, qy, qw, qh, C_PANEL);
    frame_rect(qx, qy, qw, qh, C_LINE);
    draw_text(qx + 20, qy + 16, "调度 SCHEDULER", C_ACCENT, 1);
    fill_rect(qx + 20, qy + 40, qw - 40, 1, C_LINE);

    draw_text(qx + 20, qy + 60, "轮转 · GICv2 PPI 30 · CNTP", C_TEXT, 1);
    draw_text(qx + 20, qy + 96, "任务表 TASK TABLE", C_DIM, 1);
    fill_rect(qx + 20, qy + 116, qw - 40, 1, C_LINE);

    let mut ty = qy + 132;
    if d.tasks == 0 {
        fill_rect(qx + 20, ty, qw - 40, 48, C_PANEL2);
        fill_rect(qx + 32, ty + 18, 12, 12, C_DIM);
        draw_text(qx + 56, ty + 14, "空闲 NO TASK", C_DIM, 1);
        draw_text(qx + 220, ty + 14, "用 run 启动", C_DIM, 1);
    }
    for i in 0..d.tasks.min(4) {
        fill_rect(qx + 20, ty, qw - 40, 48, C_PANEL2);
        let dot = if i == 0 { C_OK } else { C_ACCENT };
        fill_rect(qx + 32, ty + 18, 12, 12, dot);
        let label = ["TASK 1", "TASK 2", "TASK 3", "TASK 4"][i];
        draw_text(qx + 56, ty + 14, label, C_TITLE, 1);
        draw_text(qx + 160, ty + 14, if i == 0 { "running" } else { "ready  " }, C_DIM, 1);
        ty += 58;
    }

    // 底部状态栏
    fill_rect(0, f.h - 44, f.w, 44, C_PANEL2);
    fill_rect(0, f.h - 44, f.w, 2, C_LINE);
    draw_text(24, f.h - 32, "remote: VNC :1 (5901)   serial TCP :4444", C_TEXT, 1);
    let w = text_len("Aster 图形界面已就绪", 1);
    draw_text(f.w - w - 24, f.h - 32, "Aster 图形界面已就绪", C_ACCENT, 1);
}

fn digits(mut v: u64) -> usize {
    if v == 0 {
        return 1;
    }
    let mut n = 0;
    while v > 0 {
        v /= 10;
        n += 1;
    }
    n
}

fn draw_num(x: u32, y: u32, v: u64) {
    let mut buf = [0u8; 21];
    let mut n = buf.len();
    let mut v = v;
    if v == 0 {
        n -= 1;
        buf[n] = b'0';
    }
    while v > 0 && n > 0 {
        n -= 1;
        buf[n] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let s = unsafe { core::str::from_utf8_unchecked(&buf[n..]) };
    draw_text(x, y, s, C_TEXT, 1);
}

fn draw_kb(x: u32, y: u32, kb: u64) {
    draw_num(x, y, kb);
    draw_text(x + 8 * (digits(kb) as u32) + 8, y, "KB", C_DIM, 1);
}
