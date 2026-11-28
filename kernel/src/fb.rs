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
/// 双缓冲：绘制全写 BACK_PA（普通 RAM，快），present 时一次性推给真实帧缓冲。
/// GPU 模式下 resource 直接挂在 BACK_PA 上，连 memcpy 都省了；ramfb 模式下必须
/// 拷到 REAL_PA（QEMU 采样的那块），否则画到一半就被采样。
static mut BACK_PA: u64 = 0;
static mut REAL_PA: u64 = 0;
static mut GPU_MODE: bool = false;

// ---- PERF-4 离屏层 ----
// 每个窗口一块自己的像素层，绘制时把"屏幕坐标"换算成"层内坐标"，
// 合成时按 z 序把层 blit 回屏幕。静止的多窗口桌面就只剩几次 memcpy。
pub const LAYER_MAX: usize = NWIN;
static mut TGT_BASE: u64 = 0;
static mut TGT_W: u32 = 0;
static mut TGT_H: u32 = 0;
static mut TGT_X: i32 = 0;
static mut TGT_Y: i32 = 0;

static mut LP_PA: [u64; LAYER_MAX] = [0; LAYER_MAX];
static mut LP_W: [u32; LAYER_MAX] = [0; LAYER_MAX];
static mut LP_H: [u32; LAYER_MAX] = [0; LAYER_MAX];
static mut LP_OK: [bool; LAYER_MAX] = [false; LAYER_MAX];
static mut LP_GEO: [(i32, i32, u32, u32); LAYER_MAX] = [(0, 0, 0, 0); LAYER_MAX];

/// 把后续绘制都写进这块层：base 是层像素起点，ox/oy 是层在屏幕上的左上角
fn set_layer(base: u64, w: u32, h: u32, ox: i32, oy: i32) {
    unsafe {
        TGT_BASE = base;
        TGT_W = w;
        TGT_H = h;
        TGT_X = ox;
        TGT_Y = oy;
    }
}

fn clear_layer() {
    unsafe {
        TGT_BASE = 0;
    }
}

#[inline]
fn in_layer() -> bool {
    unsafe { TGT_BASE != 0 }
}

/// 屏幕坐标 → 层内偏移；不在层里返回 None
#[inline]
fn layer_off(x: u32, y: u32) -> Option<u64> {
    unsafe {
        if TGT_BASE == 0 {
            return None;
        }
        let sx = x as i32 - TGT_X;
        let sy = y as i32 - TGT_Y;
        if sx < 0 || sy < 0 || sx as u32 >= TGT_W || sy as u32 >= TGT_H {
            return None;
        }
        Some((sy as u32 * TGT_W + sx as u32) as u64 * 4)
    }
}

/// 统一的"写一个像素"：层优先，否则写屏幕
#[inline]
fn put(f: &Fb, x: u32, y: u32, color: u32) {
    if let Some(off) = layer_off(x, y) {
        unsafe {
            core::ptr::write_volatile((TGT_BASE + off) as *mut u32, color);
        }
        return;
    }
    if in_layer() || x >= f.w || y >= f.h {
        return;
    }
    unsafe {
        core::ptr::write_volatile((f.base + (y * f.stride + x * 4) as u64) as *mut u32, native(color));
    }
}

/// PERF-5：本轮真正需要贴回屏幕的并集（屏幕坐标，空矩形用 x0>=x1 表示）。
/// 层里的像素只在"某个窗口重画过"的矩形内可能变，其余区域屏幕上的旧像素本来就对，
/// 于是合成只搬这个并集，静止帧可以一块都不搬。
static mut REPAINT: (i32, i32, i32, i32) = (0, 0, 0, 0);

fn repaint_reset() {
    unsafe {
        REPAINT = (0, 0, 0, 0);
    }
}

fn repaint_add(x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let a = &mut REPAINT;
        if a.0 >= a.2 {
            *a = (x, y, x + w, y + h);
            return;
        }
        if x < a.0 {
            a.0 = x;
        }
        if y < a.1 {
            a.1 = y;
        }
        if x + w > a.2 {
            a.2 = x + w;
        }
        if y + h > a.3 {
            a.3 = y + h;
        }
    }
}

fn repaint_box() -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let a = REPAINT;
        if a.0 >= a.2 || a.1 >= a.3 {
            None
        } else {
            Some(a)
        }
    }
}

/// 统一读像素：层优先
#[inline]
fn get(f: &Fb, x: u32, y: u32) -> u32 {
    if let Some(off) = layer_off(x, y) {
        return unsafe { core::ptr::read_volatile((TGT_BASE + off) as *const u32) };
    }
    if in_layer() || x >= f.w || y >= f.h {
        return 0;
    }
    unsafe { core::ptr::read_volatile((f.base + (y * f.stride + x * 4) as u64) as *const u32) }
}

/// 取（或按需分配）第 i 个窗口的层
pub fn layer_prepare(i: usize, ww: u32, hh: u32) -> u64 {
    unsafe {
        if LP_PA[i] != 0 && LP_W[i] == ww && LP_H[i] == hh {
            return LP_PA[i];
        }
        // 尺寸变了：把旧的还回页帧池
        if LP_PA[i] != 0 {
            let old = (LP_W[i] as usize * LP_H[i] as usize * 4).div_ceil(4096);
            for k in 0..old {
                crate::frame::free(LP_PA[i] + (k as u64) * 4096);
            }
            LP_PA[i] = 0;
        }
        let n = (ww as usize * hh as usize * 4).div_ceil(4096);
        match crate::frame::alloc_pages(n) {
            Some(p) => {
                LP_PA[i] = p;
                LP_W[i] = ww;
                LP_H[i] = hh;
                LP_OK[i] = false;
                p
            }
            None => 0,
        }
    }
}

pub fn layer_ok(i: usize) -> bool {
    unsafe { i < LAYER_MAX && LP_OK[i] }
}

pub fn layer_invalidate(i: usize) {
    unsafe {
        if i < LAYER_MAX {
            LP_OK[i] = false;
        }
    }
}

pub fn layer_invalidate_all() {
    unsafe {
        for i in 0..LAYER_MAX {
            LP_OK[i] = false;
        }
    }
}

fn layer_geo_changed(i: usize, w: Win) -> bool {
    unsafe { LP_GEO[i] != (w.x, w.y, w.w, w.h) }
}

fn layer_old_geo(i: usize) -> (i32, i32, u32, u32) {
    unsafe { LP_GEO[i] }
}

fn layer_mark(i: usize, w: Win) {
    unsafe {
        LP_OK[i] = true;
        LP_GEO[i] = (w.x, w.y, w.w, w.h);
    }
}

/// 把第 i 个窗口的层 blit 到屏幕（裁剪到屏内）
fn blit_layer(i: usize) {
    unsafe {
        if LP_PA[i] == 0 || !LP_OK[i] {
            return;
        }
        let w = win(i);
        let f = fb();
        let lw = LP_W[i];
        let x0 = w.x.max(0);
        let y0 = w.y.max(0);
        let x1 = (w.x + w.w as i32).min(f.w as i32);
        let y1 = (w.y + w.h as i32).min(f.h as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let clip = match repaint_box() {
            Some(c) => c,
            None => return,
        };
        let x0 = x0.max(clip.0);
        let y0 = y0.max(clip.1);
        let x1 = x1.min(clip.2);
        let y1 = y1.min(clip.3);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let row = ((x1 - x0) as usize) * 4;
        for y in y0..y1 {
            let sy = (y - w.y) as u32;
            let sx = (x0 - w.x) as u32;
            let src = LP_PA[i] + (sy * lw + sx) as u64 * 4;
            let dst = f.base + (y as u32 * f.stride) as u64 + (x0 as u32 * 4) as u64;
            core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, row);
        }
    }
}

/// 按 z 序把有层的窗口全部合成到屏幕
fn compose_layers() {
    unsafe {
        for k in 0..NWIN {
            let i = ZORD[k];
            if WINS[i].open && LP_OK[i] {
                blit_layer(i);
            }
        }
        repaint_reset();
    }
}

fn alloc_back(real: u64) -> u64 {
    let npages = ((FB_W * FB_H * 4) as usize).div_ceil(4096);
    match crate::frame::alloc_pages(npages) {
        Some(p) => {
            unsafe {
                BACK_PA = p;
                REAL_PA = real;
            }
            p
        }
        None => {
            // 池子不够就退化成单缓冲（还是能跑，只是又回到撕裂的老路）
            unsafe {
                BACK_PA = 0;
                REAL_PA = real;
            }
            real
        }
    }
}

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

#[cfg(not(any(feature = "rpi3", feature = "rpi4")))]
pub fn init() -> bool {
    // 先要一块后台缓冲：绘制写它，present 一次推上去
    let draw_base = alloc_back(FB_PHYS);
    // 再试 virtio-gpu：有它就走 GPU 呈现，帧是原子的，不会画到一半被采样
    if crate::virtio_gpu::init(draw_base, FB_W, FB_H) {
        unsafe {
            GPU_MODE = true;
            FB = Some(Fb { w: FB_W, h: FB_H, stride: FB_W * 4, base: draw_base });
        }
        clear(0x0B1119);
        crate::println!(
            "[FB] virtio-gpu 2D 已接管：{}x{}  双缓冲 @{:#x}（画满一帧才 flush）",
            FB_W, FB_H, draw_base
        );
        return true;
    }
    crate::println!("[FB] 没有 virtio-gpu，退回 ramfb（fw_cfg 采样模式）");
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
    cfg[0..8].copy_from_slice(&draw_base.to_be_bytes());
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
        FB = Some(Fb { w: FB_W, h: FB_H, stride, base: draw_base });
    }
    clear(0x0B1119);
    crate::println!("[FB] ramfb 模式：绘制在 {:#x}，每帧 memcpy 到 {:#x} 再让 QEMU 采", draw_base, FB_PHYS);
    true
}

/// 列出 fw_cfg 目录里的所有文件（调试用）
/// 树莓派：fb 由 VideoCore 分配，像素是 BGRA，颜色在 px() 里换序
#[cfg(any(feature = "rpi3", feature = "rpi4"))]
pub fn init() -> bool {
    let (addr, w, h, stride) = match crate::mailbox::fb_init(FB_W, FB_H) {
        Some(v) => v,
        None => return false,
    };
    let draw_base = alloc_back(addr);
    unsafe {
        FB = Some(Fb { w, h, stride, base: draw_base });
    }
    clear(0x0014_1619);
    crate::println!(
        "[FB] mailbox 拿到 framebuffer：{:#x}  {}x{}  stride {}  后台缓冲 {:#x}",
        addr, w, h, stride, draw_base
    );
    true
}

/// 把当前帧推上屏幕。ramfb 由 QEMU 自己采样，这里是空操作；
/// virtio-gpu 需要显式 transfer + flush，也正因为如此帧才完整。
pub fn present() {
    unsafe {
        // GPU 模式：resource 就挂在 BACK_PA 上，transfer 直接读它，不用拷
        // ramfb / mailbox：必须整帧 memcpy 到真实帧缓冲，否则 QEMU/VideoCore 采到半帧
        if !GPU_MODE && BACK_PA != 0 && REAL_PA != 0 && BACK_PA != REAL_PA {
            let n = (FB_W * FB_H * 4) as usize;
            core::ptr::copy_nonoverlapping(BACK_PA as *const u8, REAL_PA as *mut u8, n);
        }
    }
    crate::virtio_gpu::present();
}

/// 只推一块矩形（光标这种小面积更新）
pub fn present_rect(x: u32, y: u32, w: u32, h: u32) {
    crate::virtio_gpu::present_rect(x, y, w, h);
}

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

/// 真正被 QEMU / VideoCore 采样的那块（双缓冲里的"前台"）
pub fn real_addr() -> u64 {
    unsafe { REAL_PA }
}

pub fn available() -> bool {
    unsafe { FB.is_some() }
}

fn fb() -> &'static Fb {
    unsafe { FB.as_ref().expect("framebuffer 未初始化") }
}

// ---------------- 绘图原语 ----------------

/// QEMU 的 ramfb 是 XRGB8888，树莓派是 BGRA8888。
/// 两者在小端内存里的字节顺序都是 B,G,R,X —— 不需要换位，实测读回一致。
#[inline(always)]
fn native(c: u32) -> u32 {
    c
}

#[inline]
fn px(f: &Fb, x: u32, y: u32, color: u32) {
    put(f, x, y, native(color));
}

pub fn read_px(x: u32, y: u32) -> u32 {
    if !available() {
        return 0;
    }
    let f = fb();
    get(f, x, y)
}

pub fn write_px(x: u32, y: u32, color: u32) {
    if !available() {
        return;
    }
    let f = fb();
    put(f, x, y, native(color));
}

/// 16x16 位图，fill 为实心、edge 为 1px 描边（鼠标箭头用）
pub fn blit16(x: i32, y: i32, fill_mask: &[u16; 16], edge_mask: &[u16; 16], fill: u32, edge: u32) {
    for r in 0..16 {
        let er = edge_mask[r];
        let fr = fill_mask[r];
        if er == 0 && fr == 0 {
            continue;
        }
        for c in 0..16 {
            let bit = 0x8000u16 >> c;
            let xx = x + c as i32;
            let yy = y + r as i32;
            if xx < 0 || yy < 0 {
                continue;
            }
            if er & bit != 0 {
                write_px(xx as u32, yy as u32, edge);
            } else if fr & bit != 0 {
                write_px(xx as u32, yy as u32, fill);
            }
        }
    }
}

pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    if !available() {
        return;
    }
    let f = fb();
    let mut yy = y;
    let ymax = (y + h).min(f.h);
    while yy < ymax {
        let mut xx = x;
        let xmax = (x + w).min(f.w);
        while xx < xmax {
            put(f, xx, yy, color);
            xx += 1;
        }
        yy += 1;
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

// ---------------- 抗锯齿图元 ----------------

/// 按 alpha（0..255）混合一个像素
#[inline]
pub fn blend_px(x: u32, y: u32, color: u32, a: u32) {
    if a == 0 || !available() {
        return;
    }
    let f = fb();
    if let Some(off) = layer_off(x, y) {
        // 层里的混合：读改写同一块
        unsafe {
            let p = (TGT_BASE + off) as *mut u32;
            if a >= 255 {
                core::ptr::write_volatile(p, color);
                return;
            }
            let dst = core::ptr::read_volatile(p);
            let sa = a;
            let ia = 255 - a;
            let r = (((color >> 16) & 0xff) * sa + ((dst >> 16) & 0xff) * ia) / 255;
            let g = (((color >> 8) & 0xff) * sa + ((dst >> 8) & 0xff) * ia) / 255;
            let bl = ((color & 0xff) * sa + (dst & 0xff) * ia) / 255;
            core::ptr::write_volatile(p, (r << 16) | (g << 8) | bl);
        }
        return;
    }
    if in_layer() || x >= f.w || y >= f.h {
        return;
    }
    let off = (y * f.stride + x * 4) as u64;
    unsafe {
        let p = (f.base + off) as *mut u32;
        if a >= 255 {
            core::ptr::write_volatile(p, color);
            return;
        }
        let dst = core::ptr::read_volatile(p);
        let sa = a;
        let ia = 255 - a;
        let r = (((color >> 16) & 0xff) * sa + ((dst >> 16) & 0xff) * ia) / 255;
        let g = (((color >> 8) & 0xff) * sa + ((dst >> 8) & 0xff) * ia) / 255;
        let bl = ((color & 0xff) * sa + (dst & 0xff) * ia) / 255;
        core::ptr::write_volatile(p, (r << 16) | (g << 8) | bl);
    }
}

pub fn blend_rect(x: u32, y: u32, w: u32, h: u32, color: u32, a: u32) {
    if !available() {
        return;
    }
    let f = fb();
    for yy in y..(y + h).min(f.h) {
        for xx in x..(x + w).min(f.w) {
            blend_px(xx, yy, color, a);
        }
    }
}

/// 整数平方根（no_std 里 f32::sqrt 不可用）
fn isqrt(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// 圆角矩形，角落按到圆心的距离做覆盖率混合
pub fn rounded_rect_alpha(x: i32, y: i32, w: u32, h: u32, r: u32, color: u32, a: u32) {
    if !available() || w == 0 || h == 0 {
        return;
    }
    let rr = r.min(w / 2).min(h / 2);
    let ux = |v: i32| -> u32 { v.max(0) as u32 };
    if rr == 0 {
        blend_rect(ux(x), ux(y), w, h, color, a);
        return;
    }
    blend_rect(ux(x + rr as i32), ux(y), w - 2 * rr, h, color, a);
    blend_rect(ux(x), ux(y + rr as i32), rr, h - 2 * rr, color, a);
    blend_rect(ux(x + w as i32 - rr as i32), ux(y + rr as i32), rr, h - 2 * rr, color, a);
    // 以半像素为单位算距离：像素中心到圆心的 dx = rr - dx - 0.5
    for dy in 0..rr {
        for dx in 0..rr {
            let cx = (2 * rr - 2 * dx) as i32 - 1;
            let cy = (2 * rr - 2 * dy) as i32 - 1;
            let dh = isqrt((cx * cx + cy * cy) as u32) as i32;
            let cov = 2 * rr as i32 + 1 - dh; // 半像素单位的覆盖量
            if cov <= 0 {
                continue;
            }
            let av = ((cov.min(2) as u32) * a) / 2;
            if av == 0 {
                continue;
            }
            let px = x + dx as i32;
            let py = y + dy as i32;
            let px2 = x + w as i32 - 1 - dx as i32;
            let py2 = y + h as i32 - 1 - dy as i32;
            blend_px(ux(px), ux(py), color, av);
            blend_px(ux(px2), ux(py), color, av);
            blend_px(ux(px), ux(py2), color, av);
            blend_px(ux(px2), ux(py2), color, av);
        }
    }
}

pub fn rounded_rect(x: i32, y: i32, w: u32, h: u32, r: u32, color: u32) {
    rounded_rect_alpha(x, y, w, h, r, color, 255);
}

/// 柔和投影：三层递减，只露在窗口边缘外
pub fn drop_shadow(x: i32, y: i32, w: u32, h: u32, r: u32) {
    // 只画窗口外的一圈（环带），窗口本体一会儿会被盖住。
    // 原来两层把 800x600 整块扫一遍：478k 像素 × 每像素读+混合+写，
    // 一帧就吃掉三十多毫秒。改成四条边带之后同一条阴影只扫约 40k 像素。
    const SPEC: [(u32, u32, u32); 3] = [(14, 36, 6), (9, 26, 9), (4, 18, 14)];
    for (d, cr, a) in SPEC {
        let ox = x - d as i32;
        let oy = y - (d / 2) as i32 + 2;
        let ow = w + d * 2;
        let oh = h + d * 2;
        // 上带（含左右上圆角）
        let top_h = (y - oy) as u32 + 3;
        rounded_rect_alpha(ox, oy, ow, top_h, cr, 0x000000, a);
        // 下带
        let by = y + h as i32 - 3;
        let bot_h = (oy + oh as i32 - by) as u32;
        rounded_rect_alpha(ox, by, ow, bot_h, cr, 0x000000, a);
        // 左右两条侧带
        if x - ox > 0 {
            blend_rect(ox.max(0) as u32, (y + 2).max(0) as u32, (x - ox) as u32, h.saturating_sub(4), 0x000000, a);
        }
        let rx = x + w as i32 + 2;
        let rw = (ox + ow as i32) - rx;
        if rw > 0 {
            blend_rect(rx.max(0) as u32, (y + 2).max(0) as u32, rw as u32, h.saturating_sub(4), 0x000000, a);
        }
    }
}

/// 实心圆（边缘抗锯齿）
pub fn circle(cx: i32, cy: i32, r: u32, color: u32, a: u32) {
    // 内部变量不要和圆心同名：之前这里把 cx/cy 遮住了，写出去的是
    // "3*dx-3*r+1"，圆会整体错位。现在用 sx/sy 做 2 倍的亚像素距离。
    for dy in 0..(2 * r) {
        for dx in 0..(2 * r) {
            let px = dx as i32 - r as i32;
            let py = dy as i32 - r as i32;
            let sx = 2 * px + 1;
            let sy = 2 * py + 1;
            let dh = isqrt((sx * sx + sy * sy) as u32) as i32;
            let cov = 2 * r as i32 + 1 - dh;
            if cov <= 0 {
                continue;
            }
            let av = ((cov.min(2) as u32) * a) / 2;
            if av > 0 {
                let x = cx + px;
                let y = cy + py;
                if x >= 0 && y >= 0 {
                    blend_px(x as u32, y as u32, color, av);
                }
            }
        }
    }
}

/// 竖直渐变
pub fn vgrad(x: u32, y: u32, w: u32, h: u32, top: u32, bottom: u32) {
    for i in 0..h {
        let c = if h <= 1 { top } else { lerp(top, bottom, i, h - 1) };
        fill_rect(x, y + i, w, 1, c);
    }
}

fn lighten(c: u32, n: u32) -> u32 {
    let r = (((c >> 16) & 0xff) + n).min(255);
    let g = (((c >> 8) & 0xff) + n).min(255);
    let b = ((c & 0xff) + n).min(255);
    (r << 16) | (g << 8) | b
}

/// 一团柔光（壁纸用），三层由外到内
fn glow_rect(x: i32, y: i32, w: u32, h: u32, color: u32, a: u32) {
    for i in 0..3u32 {
        let g = i * 10;
        rounded_rect_alpha(
            x - g as i32,
            y - g as i32,
            w + g * 2,
            h + g * 2,
            60 + g,
            color,
            a / 3,
        );
    }
}

/// 画一个字符点阵（4-bit 灰度，做 alpha 混合）
fn draw_glyph(x: u32, y: u32, rows: &[u8], glyph_w: u32, glyph_h: u32, color: u32, scale: u32) {
    let bpr = (glyph_w / 2) as usize;
    for r in 0..glyph_h {
        let base = r as usize * bpr;
        for c in 0..glyph_w {
            let byte = rows[base + (c / 2) as usize];
            let nib = if c % 2 == 0 { byte >> 4 } else { byte & 0x0f };
            if nib == 0 {
                continue;
            }
            let a = (nib as u32) * 17;
            let dx = x + c * scale;
            let dy = y + r * scale;
            if scale == 1 {
                blend_px(dx, dy, color, a);
            } else {
                for sy in 0..scale {
                    for sx in 0..scale {
                        blend_px(dx + sx, dy + sy, color, a);
                    }
                }
            }
        }
    }
}

fn cjk_lookup(c: char) -> Option<&'static [u8; 128]> {
    for (i, &ch) in font::CJK_CHARS.iter().enumerate() {
        if ch == c {
            return Some(&font::CJK_BITS[i]);
        }
    }
    None
}

/// 混合排版的文字：ASCII 8x16、汉字 16x16，返回结束的 x 坐标
/// 画一个字符（终端逐格渲染用）
pub fn draw_char(x: u32, y: u32, ch: char, color: u32, scale: u32) {
    if !available() {
        return;
    }
    if ch.is_ascii() {
        let code = ch as u8;
        if code >= font::ASCII_FIRST && code <= font::ASCII_LAST {
            let idx = (code - font::ASCII_FIRST) as usize;
            draw_glyph(x, y, &font::ASCII[idx], font::ASCII_W as u32, font::ASCII_H as u32, color, scale);
        }
    } else if let Some(rows) = cjk_lookup(ch) {
        draw_glyph(x, y, rows, font::CJK_W as u32, font::CJK_H as u32, color, 1);
    }
}

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


// ---------------- 桌面 ----------------
//
// 布局（1024x768）：
//   顶栏 28px  —— 系统名 / EX3 短指纹
//   左侧图标列  —— 终端、系统、网络、关于
//   主窗口      —— 终端（内容来自 term 缓冲，和串口输出一致）
//   底栏 48px   —— 开始按钮、运行中程序、键盘/网络状态、运行时长

const W: u32 = 1024;
const H: u32 = 768;

// ---------------- 主题 ----------------
// 中性深灰背景 + 一个蓝色强调色。以前是整片海洋蓝，饱和度过高，看着像屏保。
pub const TEXT: u32 = 0xdadde2;
pub const TEXT2: u32 = 0x9aa1a9;
pub const DIM: u32 = 0x6a7179;
pub const ACCENT: u32 = 0x5b99ff;
pub const OK: u32 = 0x46c46a;
pub const WARN: u32 = 0xd9a441;
pub const ERR: u32 = 0xf2686a;

const BG_TOP: u32 = 0x1d1f24;
const BG_BOT: u32 = 0x121316;
const TOPBAR: u32 = 0x181a1e;
const WIN_BG: u32 = 0x212328;
const WIN_BAR: u32 = 0x2f343c;
const WIN_LINE: u32 = 0x3a3f47;
pub const TERM_BG: u32 = 0x141619;

const WIN_W: u32 = 748;
const WIN_H: u32 = 546;
const TITLE_H: u32 = 44;
const PAD_X: u32 = 18;

const SIDE_X: u32 = 20;
const SIDE_Y0: u32 = 92;
const SIDE_STEP: u32 = 88;
const TILE: u32 = 46;

const SIDE_LABELS: [&str; 4] = ["终端", "系统", "网络", "浏览"];

fn lerp(a: u32, b: u32, t: u32, span: u32) -> u32 {
    let inv = span.saturating_sub(t);
    let ch = |sh: u32| -> u32 {
        let ca = (a >> sh) & 0xff;
        let cb = (b >> sh) & 0xff;
        ((ca * inv + cb * t) / span) & 0xff
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn text_center(cx: u32, y: u32, s: &str, color: u32, scale: u32) {
    let w = text_len(s, scale);
    draw_text(cx.saturating_sub(w / 2), y, s, color, scale);
}

// ---------------- 界面字体（Inter 10x16） ----------------

pub fn draw_text_ui(mut x: u32, y: u32, text: &str, color: u32) -> u32 {
    if !available() {
        return x;
    }
    for c in text.chars() {
        if c.is_ascii() {
            let code = c as u8;
            if code >= font::ASCII_FIRST && code <= font::ASCII_LAST {
                let idx = (code - font::ASCII_FIRST) as usize;
                draw_glyph(
                    x,
                    y,
                    &crate::font_ui::UI_ASCII[idx],
                    crate::font_ui::UI_W as u32,
                    crate::font_ui::UI_H as u32,
                    color,
                    1,
                );
            }
            x += crate::font_ui::UI_W as u32;
        } else if let Some(rows) = cjk_lookup(c) {
            draw_glyph(x, y, rows, font::CJK_W as u32, font::CJK_H as u32, color, 1);
            x += font::CJK_W as u32;
        } else {
            x += font::CJK_W as u32;
        }
    }
    x
}

pub fn text_len_ui(text: &str) -> u32 {
    let mut w = 0;
    for c in text.chars() {
        w += if c.is_ascii() { crate::font_ui::UI_W as u32 } else { font::CJK_W as u32 };
    }
    w
}

fn text_center_ui(cx: u32, y: u32, s: &str, color: u32) {
    let w = text_len_ui(s);
    draw_text_ui(cx.saturating_sub(w / 2), y, s, color);
}

/// 32x32 灰度图标缩到 size 贴上去
pub fn draw_icon(x: u32, y: u32, idx: usize, size: u32, color: u32, alpha_mul: u32) {
    if !available() || idx >= crate::icons::ICON_COUNT {
        return;
    }
    let src = &crate::icons::ICONS[idx];
    for ty in 0..size {
        let sy = ty * 32 / size;
        for tx in 0..size {
            let sx = tx * 32 / size;
            let byte = src[sy as usize * 16 + (sx / 2) as usize];
            let nib = if sx % 2 == 0 { byte >> 4 } else { byte & 0x0f };
            if nib == 0 {
                continue;
            }
            let a = (nib as u32) * 17 * alpha_mul / 255;
            blend_px(x + tx, y + ty, color, a);
        }
    }
}

// ---------------- 侧栏状态 ----------------

static mut SIDE_ACTIVE: usize = 0;
static mut SIDE_HOVER: usize = usize::MAX;

pub fn set_side_active(i: usize) {
    unsafe { SIDE_ACTIVE = i.min(3) };
}

pub fn side_active() -> usize {
    unsafe { SIDE_ACTIVE }
}

/// 返回悬停项是否变化，变了才值得重绘
pub fn set_side_hover(h: Option<usize>) -> bool {
    let v = match h {
        Some(i) => i.min(3),
        None => usize::MAX,
    };
    if unsafe { SIDE_HOVER } == v {
        return false;
    }
    unsafe { SIDE_HOVER = v };
    true
}

pub fn sidebar_item_at(x: i32, y: i32) -> Option<usize> {
    if x < SIDE_X as i32 - 8 || x > (SIDE_X + TILE + 8) as i32 {
        return None;
    }
    for i in 0..4 {
        let y0 = (SIDE_Y0 + i as u32 * SIDE_STEP) as i32;
        if y >= y0 - 6 && y <= y0 + TILE as i32 + 30 {
            return Some(i as usize);
        }
    }
    None
}

pub fn draw_sidebar() {
    for i in 0..4 {
        let y = SIDE_Y0 + i as u32 * SIDE_STEP;
        let active = unsafe { SIDE_ACTIVE } == i;
        let hover = unsafe { SIDE_HOVER } == i;
        if active {
            rounded_rect_alpha(SIDE_X as i32, y as i32, TILE, TILE, 12, ACCENT, 48);
            rounded_rect(SIDE_X as i32 - 7, (y + 13) as i32, 3, 20, 2, ACCENT);
        } else if hover {
            rounded_rect_alpha(SIDE_X as i32, y as i32, TILE, TILE, 12, 0xffffff, 26);
        }
        let fg = if active {
            TEXT
        } else if hover {
            TEXT
        } else {
            TEXT2
        };
        draw_icon(SIDE_X + (TILE - 28) / 2, y + (TILE - 28) / 2, i, 28, fg, 255);
        text_center_ui(
            SIDE_X + TILE / 2,
            y + TILE + 9,
            SIDE_LABELS[i],
            if active { TEXT } else { DIM },
        );
    }
}

// ---------------- 窗口位置 ----------------

static mut FRAME: u32 = 0;

static mut WIN_X: i32 = 138;
static mut WIN_Y: i32 = 86;

pub fn frame() -> u32 {
    unsafe { FRAME }
}

pub fn win_pos() -> (i32, i32) {
    unsafe { (WIN_X, WIN_Y) }
}

pub fn set_win_pos(x: i32, y: i32) {
    unsafe {
        WIN_X = x;
        WIN_Y = y;
    }
}

static mut WALL: [u32; 1024 * 768] = [0; 1024 * 768];

fn wall_addr() -> u64 {
    unsafe { core::ptr::addr_of!(WALL) as u64 }
}

fn set_base(b: u64) -> u64 {
    unsafe {
        match FB.as_mut() {
            Some(f) => {
                let old = f.base;
                f.base = b;
                old
            }
            None => 0,
        }
    }
}

fn clock_str(ms: u64) -> ([u8; 8], usize) {
    let s = ms / 1000;
    let m = (s / 60) % 100;
    let sec = s % 60;
    let mut buf = *b"00:00   ";
    buf[0] = b'0' + (m / 10) as u8;
    buf[1] = b'0' + (m % 10) as u8;
    buf[3] = b'0' + (sec / 10) as u8;
    buf[4] = b'0' + (sec % 10) as u8;
    (buf, 5)
}

const DOCK_Y: u32 = H - 58;
const DOCK_H: u32 = 46;

/// 静态层：壁纸、顶栏、底栏底板。开机画一次存进 .bss 的 WALL
pub fn cache_wall(ex3_short: &str) {
    if !available() {
        return;
    }
    let saved = set_base(wall_addr());

    // 壁纸：中性灰竖向渐变，两团很淡的冷光
    let bands = 128u32;
    let band_h = H / bands + 1;
    let span = (bands - 1) * (bands - 1);
    for i in 0..bands {
        let c = lerp(BG_TOP, BG_BOT, i * i, span);
        fill_rect(0, i * band_h, W, band_h, c);
    }
    glow_rect(620, 30, 420, 300, 0x2a3a55, 14);
    glow_rect(60, 430, 380, 280, 0x1c2433, 12);

    // 顶栏
    fill_rect(0, 0, W, 36, TOPBAR);
    fill_rect(0, 36, W, 1, 0x26292f);
    let mut hx = draw_text_ui(16, 10, "Aster", TEXT) + 10;
    hx = draw_text_ui(hx, 10, "星芒", TEXT2) + 14;
    draw_text_ui(hx, 11, "v0.9  ARM64", DIM);
    let right = text_len_ui("EX3 ") + text_len_ui(ex3_short);
    draw_text_ui(W - right - 16, 11, "EX3 ", DIM);
    draw_text_ui(W - text_len_ui(ex3_short) - 16, 11, ex3_short, OK);

    // 底栏 dock：半透明圆角条，只画底板，内容留给动态层
    rounded_rect_alpha(16, DOCK_Y as i32, 404, DOCK_H, 14, 0x0c0e12, 238);
    rounded_rect_alpha(16, DOCK_Y as i32, 404, DOCK_H, 14, 0xffffff, 16);
    fill_rect(26, DOCK_Y + 1, 384, 1, 0x3d424a);

    // 开始按钮
    circle(40, (DOCK_Y + 23) as i32, 11, ACCENT, 58);
    draw_text_ui(36, DOCK_Y + 11, "A", TEXT);
    draw_text_ui(60, DOCK_Y + 15, "开始", TEXT2);
    fill_rect(104, DOCK_Y + 12, 1, 22, 0x2a2e35);

    // 窗口格子由 draw_dock_items() 每帧画（打开/最小化状态会变）

    set_base(saved);
}

/// 把静态层的一段拷回屏幕。拖拽时只拷两块矩形，不用整屏重算
pub fn restore_wall(x: u32, y: u32, w: u32, h: u32) {
    if !available() {
        return;
    }
    let f = fb();
    if x >= f.w || y >= f.h || w == 0 || h == 0 {
        return;
    }
    let x1 = (x + w).min(f.w);
    let y1 = (y + h).min(f.h);
    // 两像素一次搬：整屏 786k 像素，逐 u32 读写在 QEMU 上要十几毫秒，
    // 成对搬能把访存次数砍一半。x 是奇数或地址没对齐时退回逐像素。
    let pair_ok = x % 2 == 0 && (f.base + (y * f.stride) as u64) % 8 == 0 && (wall_addr() + (y * f.stride) as u64) % 8 == 0;
    for yy in y..y1 {
        let row = (yy * f.stride) as u64;
        if pair_ok {
            let mut xx = x;
            while xx + 1 < x1 {
                let off = row + (xx * 4) as u64;
                let v = unsafe { core::ptr::read_volatile((wall_addr() + off) as *const u64) };
                unsafe { core::ptr::write_volatile((f.base + off) as *mut u64, v) };
                xx += 2;
            }
            if xx < x1 {
                let off = row + (xx * 4) as u64;
                let v = unsafe { core::ptr::read_volatile((wall_addr() + off) as *const u32) };
                unsafe { core::ptr::write_volatile((f.base + off) as *mut u32, v) };
            }
        } else {
            for xx in x..x1 {
                let off = row + (xx * 4) as u64;
                let src = unsafe { core::ptr::read_volatile((wall_addr() + off) as *const u32) };
                unsafe { core::ptr::write_volatile((f.base + off) as *mut u32, src) };
            }
        }
    }
}

static mut LAST_TASKS: u32 = 0;
static mut LAST_KB: u64 = 0;

// ---------------- 多窗口 ----------------
// 三个窗口：0 终端、1 系统、2 网络。ZORD[0] 在最底，ZORD[NSLOT-1] 在最上，
// 点击任意窗口就把它抬到最上层并聚焦；重画一律按 ZORD 顺序来一遍。

pub const NWIN: usize = 4;

#[derive(Clone, Copy)]
pub struct Win {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub open: bool,
}

static mut WINS: [Win; NWIN] = [
    Win { x: 138, y: 86, w: 748, h: 546, open: true },
    Win { x: 636, y: 108, w: 380, h: 330, open: false },
    Win { x: 604, y: 424, w: 372, h: 266, open: false },
    // 浏览器：内置，但先收在 dock，点一下"网页"格子就出来
    Win { x: 286, y: 74, w: 676, h: 468, open: false },
];

// 终端放最上层：手机端最常点的就是它，原来 [0,1,2] 会让系统/网络两块压住终端右上角，
// 点"关窗"实际点到别的窗口上（这就是"关不掉、点不动"的根因）
static mut ZORD: [usize; NWIN] = [1, 2, 0, 3];
static mut FOCUS: usize = 0;

pub fn win(i: usize) -> Win {
    unsafe { WINS[i] }
}

pub fn focus() -> usize {
    unsafe { FOCUS }
}

pub fn win_title(i: usize) -> &'static str {
    match i {
        0 => "终端",
        1 => "系统",
        2 => "网络",
        _ => "浏览器",
    }
}

pub fn set_win_pos_of(i: usize, x: i32, y: i32) {
    unsafe {
        WINS[i].x = x;
        WINS[i].y = y;
    }
}

/// 抬到最上层并聚焦；关着的窗口顺手打开
pub fn raise(i: usize) {
    win_touch_all();
    ask_shadow();
    unsafe {
        if i >= NWIN {
            return;
        }
        WINS[i].open = true;
        let mut pos = 0;
        for k in 0..NWIN {
            if ZORD[k] == i {
                pos = k;
            }
        }
        let v = ZORD[pos];
        for k in pos..(NWIN - 1) {
            ZORD[k] = ZORD[k + 1];
        }
        ZORD[NWIN - 1] = v;
        FOCUS = i;
    }
}

static mut SAVED: [(i32, i32, u32, u32); NWIN] = [(0, 0, 0, 0); NWIN];
static mut MAXED: [bool; NWIN] = [false; NWIN];
/// 最小化：窗口从桌面消失，但还"活着"——dock 上占一个格子，点一下回来。
/// 和关闭的区别就在这里：关闭 = 状态清空，要重新打开得点侧栏图标。
/// 开局就把浏览器收进 dock（其余三窗展开），dock 上点"网页"格子即可打开
static mut MINED: [bool; NWIN] = [false, true, true, true];

/// 0 = 已关闭 / 1 = 在桌面 / 2 = 最小化进 dock
pub fn win_state(i: usize) -> u8 {
    unsafe {
        if WINS[i].open {
            1
        } else if MINED[i] {
            2
        } else {
            0
        }
    }
}

/// dock 上的三个格子：终端 / 系统 / 网络
const DOCK_CHIP_X: u32 = 116;
const DOCK_CHIP_W: u32 = 64;
const DOCK_CHIP_GAP: u32 = 4;

pub fn dock_hit(x: i32, y: i32) -> Option<usize> {
    if y < DOCK_Y as i32 - 4 || y > (DOCK_Y + DOCK_H) as i32 + 4 {
        return None;
    }
    for i in 0..4 {
        // 关掉的窗口不在 dock 上占格子，那里也不该有隐形热区
        if win_state(i) == 0 {
            continue;
        }
        let x0 = DOCK_CHIP_X + i as u32 * (DOCK_CHIP_W + DOCK_CHIP_GAP);
        if x >= x0 as i32 && x <= (x0 + DOCK_CHIP_W) as i32 {
            return Some(i);
        }
    }
    None
}

fn draw_dock_items() {
    for i in 0..4 {
        let st = win_state(i);
        if st == 0 {
            continue;
        }
        let x0 = DOCK_CHIP_X + i as u32 * (DOCK_CHIP_W + DOCK_CHIP_GAP);
        let y0 = DOCK_Y + 8;
        let h = DOCK_H - 16;
        let focused = unsafe { FOCUS } == i && st == 1;
        if st == 2 {
            rounded_rect(x0 as i32, y0 as i32, DOCK_CHIP_W, h, 8, 0x191c21);
            frame_rect(x0, y0, DOCK_CHIP_W, h, 0x2b3038);
        } else if focused {
            rounded_rect(x0 as i32, y0 as i32, DOCK_CHIP_W, h, 8, 0x22304a);
            frame_rect(x0, y0, DOCK_CHIP_W, h, ACCENT);
        } else {
            rounded_rect(x0 as i32, y0 as i32, DOCK_CHIP_W, h, 8, 0x2a2f37);
            frame_rect(x0, y0, DOCK_CHIP_W, h, 0x3a3f47);
        }
        let t = ["终端", "系统", "网络", "网页"][i];
        let tw = text_len_ui(t);
        let fg = if st == 2 { DIM } else if focused { TEXT } else { TEXT2 };
        draw_text_ui(x0 + (DOCK_CHIP_W - tw) / 2, y0 + (h - 16) / 2, t, fg);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Btn {
    Close,
    Min,
    Max,
}

/// 标题栏三个圆点（左起 绿/黄/红）的命中判定，半径 8px 好点
pub fn title_button_at(i: usize, x: i32, y: i32) -> Option<Btn> {
    let w = win(i);
    // 手指比鼠标粗：热区从 16×16 放大到 32×30（视觉上的三色点不变）
    if y < w.y + 8 || y > w.y + 38 {
        return None;
    }
    let cx = w.x + w.w as i32 - 26;
    if (x - cx).abs() <= 14 {
        return Some(Btn::Close);
    }
    if (x - (cx - 20)).abs() <= 14 {
        return Some(Btn::Min);
    }
    if (x - (cx - 40)).abs() <= 14 {
        return Some(Btn::Max);
    }
    None
}

/// 关掉一扇窗：焦点落到还在的最上层窗口
pub fn close_win(i: usize) {
    win_touch_all();
    ask_shadow();
    unsafe {
        WINS[i].open = false;
        MINED[i] = false;
        MAXED[i] = false;
        let mut next = usize::MAX;
        for k in (0..NWIN).rev() {
            let j = ZORD[k];
            if j != i && WINS[j].open {
                next = j;
                break;
            }
        }
        if next != usize::MAX {
            FOCUS = next;
        }
    }
}

/// 最小化等同收起：窗口留在 dock，点侧栏或 dock 再抬起来
pub fn minimize_win(i: usize) {
    win_touch_all();
    ask_shadow();
    unsafe {
        WINS[i].open = false;
        MINED[i] = true;
    }
    // 焦点交给还在桌面上的最上层窗口
    unsafe {
        let mut next = usize::MAX;
        for k in (0..NWIN).rev() {
            let j = ZORD[k];
            if j != i && WINS[j].open {
                next = j;
                break;
            }
        }
        if next != usize::MAX {
            FOCUS = next;
        }
    }
}

/// 最大化/还原：记住原几何，铺到桌面可用区（让出顶栏与 dock）
pub fn toggle_max(i: usize) {
    win_touch_all();
    ask_shadow();
    unsafe {
        let w = WINS[i];
        if !MAXED[i] {
            SAVED[i] = (w.x, w.y, w.w, w.h);
            // 让出左侧栏（不然最大化之后连侧栏都点不到）和底部 dock
            WINS[i].x = 112;
            WINS[i].y = 52;
            WINS[i].w = W - 112 - 16;
            WINS[i].h = H - 52 - 88;
            MAXED[i] = true;
        } else {
            let (x, y, ww, hh) = SAVED[i];
            WINS[i].x = x;
            WINS[i].y = y;
            WINS[i].w = ww;
            WINS[i].h = hh;
            MAXED[i] = false;
        }
    }
}

fn contains(i: usize, x: i32, y: i32) -> bool {
    let w = unsafe { WINS[i] };
    w.open && x >= w.x && x < w.x + w.w as i32 && y >= w.y && y < w.y + w.h as i32
}

/// 从最上层往下找命中的窗口
pub fn hit_top(x: i32, y: i32) -> Option<usize> {
    unsafe {
        for k in (0..NWIN).rev() {
            let i = ZORD[k];
            if contains(i, x, y) {
                return Some(i);
            }
        }
    }
    None
}

pub fn title_hit(i: usize, y: i32) -> bool {
    let w = win(i);
    y >= w.y && y < w.y + TITLE_H as i32
}

/// 拖某一扇窗：擦掉新旧位置的并集，再按 z 序把还开着的窗口全画一遍
pub fn move_window(i: usize, nx: i32, ny: i32) {
    move_window_ex(i, nx, ny, true);
}

/// `shadow=false` 用于拖拽中的每一帧：阴影是 4 层大半径圆角混合，
/// 一帧要盖掉几万个像素，拖动时省掉它手感立刻跟手。松手后再补一次完整的。
pub fn move_window_ex(i: usize, nx: i32, ny: i32, shadow: bool) {
    win_touch_all();
    ask_shadow();
    let w = win(i);
    if nx == w.x && ny == w.y {
        return;
    }
    let ux = w.x.min(nx).max(0) as u32;
    let uy = w.y.min(ny).max(0) as u32;
    let uw = (w.w + (nx - w.x).unsigned_abs()).min(W - ux);
    let uh = (w.h + (ny - w.y).unsigned_abs()).min(H - uy);
    restore_wall(ux, uy, uw, uh);
    set_win_pos_of(i, nx, ny);
    draw_sidebar();
    draw_all_ex(shadow);
    crate::cursor::invalidate();
}

/// 调试：打印三个窗口的状态
pub fn dump_wins() -> [(&'static str, i32, i32, u32, u32, bool); 3] {
    unsafe {
        [
            ("终端", WINS[0].x, WINS[0].y, WINS[0].w, WINS[0].h, WINS[0].open),
            ("系统", WINS[1].x, WINS[1].y, WINS[1].w, WINS[1].h, WINS[1].open),
            ("网络", WINS[2].x, WINS[2].y, WINS[2].w, WINS[2].h, WINS[2].open),
        ]
    }
}

/// 侧栏点击语义：该窗口没开就打开，已开就只抬到最上层（不切换关闭）
pub fn show_win(i: usize) {
    win_touch_all();
    ask_shadow();
    unsafe {
        WINS[i].open = true;
        MINED[i] = false;
    }
    // 浏览器第一次露脸就先装一页内置首页，别让地址栏和内容区空着
    if i == 3 && crate::web::url_str().is_empty() {
        crate::web::open_home();
    }
    raise(i);
}

static mut LAST_MS: u64 = 0;
/// 阴影是整窗最贵的一步（四层大矩形 alpha 混合）。只有窗口位移/开关/抬起之后的
/// 一小段时间才画，纯终端文字刷新走无阴影路径 —— 单帧从 38ms 降到 15ms 上下。
static mut SHADOW_UNTIL: u64 = 0;

pub fn ask_shadow() {
    unsafe {
        SHADOW_UNTIL = crate::timer::now_ms() + 150;
    }
}

pub fn shadow_wanted() -> bool {
    unsafe { crate::timer::now_ms() < SHADOW_UNTIL }
}
static mut MS_RESTORE: u64 = 0;
static mut MS_SIDEBAR: u64 = 0;
static mut MS_WIN: u64 = 0;

pub fn frame_count() -> u64 {
    unsafe { FRAME as u64 }
}

pub fn frame_breakdown() -> (u64, u64, u64) {
    unsafe { (MS_RESTORE, MS_SIDEBAR, MS_WIN) }
}

/// 上一次整桌重绘耗时（ms）
pub fn last_frame_ms() -> u64 {
    unsafe { LAST_MS }
}

pub fn draw_all() {
    draw_all_ex(true);
}

/// 每个窗口是否需要"整窗重画"。只有几何、焦点、标题、z 序变了才置位；
/// 否则第 0 窗口（终端）可以走行级增量，别把 748×546 每帧重涂一遍。
static mut WIN_FULL: [bool; NWIN] = [true; NWIN];

/// 壁纸层是否要重擦。窗口一动、键盘一开合、分钟跳一格才需要；
/// 静止帧上壁纸像素本来就是对的，每帧搬 3 MiB 纯浪费（PERF-3）。
static mut WALL_DIRTY: bool = true;
static mut LAST_MIN: u64 = u64::MAX;

pub fn ask_wall() {
    unsafe {
        WALL_DIRTY = true;
    }
}

fn take_wall() -> bool {
    unsafe {
        let v = WALL_DIRTY;
        WALL_DIRTY = false;
        v
    }
}

pub fn win_touch(i: usize) {
    unsafe {
        if i < NWIN {
            WIN_FULL[i] = true;
        }
    }
}

pub fn win_touch_all() {
    unsafe {
        for i in 0..NWIN {
            WIN_FULL[i] = true;
        }
    }
    ask_shadow();
    ask_wall();
}

fn take_full(i: usize) -> bool {
    unsafe {
        let v = WIN_FULL[i];
        WIN_FULL[i] = false;
        v
    }
}

pub fn draw_all_ex(shadow: bool) {
    let t0 = crate::timer::now_ms();
    unsafe {
        let others_open = (1..NWIN).any(|j| WINS[j].open);
        let only_term = WINS[0].open && !others_open;
        if only_term && !shadow {
            // 单窗（终端）快速路径：只补脏行，标题栏与面板留上一帧的
            layer_invalidate(0);
            if take_full(0) {
                draw_one_ex(0, false);
            } else {
                term_patch();
            }
        } else {
            // 多窗口：每个窗口只在自己层里画一次，静止帧只做合成
            let mut full_any = false;
            for i in 0..NWIN {
                if WIN_FULL[i] {
                    full_any = true;
                }
            }
            let geo_bad = (0..NWIN).any(|i| {
                WINS[i].open && (!layer_ok(i) || layer_geo_changed(i, win(i)))
            });
            // PERF-5：只有终端的几行字变了（打字、回显、命令输出）时，
            // 不必把四块层全贴一遍，只补那几行进层，再贴那几行 + 状态行。
            let can_patch = !shadow
                && !full_any
                && !geo_bad
                && WINS[0].open
                && crate::term::pending_rows().is_some();
            if can_patch {
                term_patch_layer();
            } else {
                for k in 0..NWIN {
                    let i = ZORD[k];
                    if !WINS[i].open {
                        layer_invalidate(i);
                        continue;
                    }
                    let wi = win(i);
                    if layer_geo_changed(i, wi) {
                        // PERF-5：窗口挪走了，旧位置压过的别的窗口要重贴，不然留个洞
                        let og = layer_old_geo(i);
                        repaint_add(og.0, og.1, og.2 as i32, og.3 as i32);
                    }
                    if shadow || take_full(i) || !layer_ok(i) || layer_geo_changed(i, wi) {
                        draw_one_layer(i, shadow);
                    }
                }
            }
            compose_layers();
        }
        FRAME = FRAME.wrapping_add(1);
        let dt = crate::timer::now_ms().saturating_sub(t0);
        LAST_MS = dt;
        MS_WIN = dt;
    }
    // dock 上的窗口格子属于动态层（窗口开关状态每帧都可能变）
    draw_dock_items();
}

fn draw_one(i: usize) {
    draw_one_ex(i, true);
}

/// PERF-4：把一个窗口整画进它自己的层（阴影在层外，直接画屏幕）
fn draw_one_layer(i: usize, shadow: bool) {
    let w = win(i);
    let active = unsafe { FOCUS } == i;
    let title = win_title(i);
    if shadow {
        rounded_rect_alpha(w.x - 26, w.y - 8, w.w + 52, w.h + 52, 34, 0x000000, if active { 18 } else { 12 });
        rounded_rect_alpha(w.x - 8, w.y - 2, w.w + 16, w.h + 16, 20, 0x000000, if active { 22 } else { 16 });
    }
    let lp = layer_prepare(i, w.w, w.h);
    if lp == 0 {
        // 页帧不够就退回直接画
        draw_one_ex(i, shadow);
        return;
    }
    set_layer(lp, w.w, w.h, w.x, w.y);
    rounded_rect(w.x, w.y, w.w, w.h, 14, WIN_BG);
    draw_title(i, w, active, title);
    draw_body(i, w);
    clear_layer();
    layer_mark(i, w);
    repaint_add(w.x, w.y, w.w as i32, w.h as i32);
}

fn draw_one_ex(i: usize, shadow: bool) {
    let w = win(i);
    let active = unsafe { FOCUS } == i;
    let title = win_title(i);

    if !shadow {
        rounded_rect(w.x, w.y, w.w, w.h, 14, WIN_BG);
        draw_title(i, w, active, title);
        draw_body(i, w);
        return;
    }

    rounded_rect_alpha(
        w.x - 26,
        w.y - 8,
        w.w + 52,
        w.h + 52,
        34,
        0x000000,
        if active { 18 } else { 12 },
    );
    rounded_rect_alpha(
        w.x - 8,
        w.y - 2,
        w.w + 16,
        w.h + 16,
        20,
        0x000000,
        if active { 22 } else { 16 },
    );
    rounded_rect(w.x, w.y, w.w, w.h, 14, WIN_BG);

    draw_title(i, w, active, title);
    draw_body(i, w);
}

fn draw_title(i: usize, w: Win, active: bool, title: &str) {
    let bar = if active { WIN_BAR } else { 0x262a31 };
    rounded_rect(w.x + 1, w.y + 1, w.w - 2, TITLE_H as u32, 13, bar);
    fill_rect((w.x + 3) as u32, (w.y + 24) as u32, w.w - 6, 20, bar);
    // 窗口上沿一道极淡高光：玻璃感来自这道边缘，不靠渐变堆出来。
    // 两端留出圆角宽度，避免高光穿出圆角。
    blend_rect((w.x + 16) as u32, w.y as u32, (w.w - 32) as u32, 1, 0xffffff, 12);
    // 当前工作窗口：标题栏底边一条偏蓝的细线 + 左侧 3 px 竖条（非焦点保持中性灰）
    fill_rect(
        (w.x + 1) as u32,
        (w.y + TITLE_H as i32 - 1) as u32,
        w.w - 2,
        1,
        if active { 0x4a72b0 } else { WIN_LINE },
    );
    if active {
        fill_rect(
            (w.x + 5) as u32,
            (w.y + 9) as u32,
            3,
            TITLE_H as u32 - 18,
            ACCENT,
        );
    }

    let mut hx = draw_text_ui((w.x + 18) as u32, (w.y + 14) as u32, title, if active { TEXT } else { TEXT2 }) + 8;
    let sub = match i {
        0 => "TERMINAL",
        1 => "SYSTEM",
        2 => "NETWORK",
        _ => "BROWSER",
    };
    hx = draw_text_ui(hx, (w.y + 15) as u32, sub, DIM);
    let _ = hx;

    let a = if active { 255 } else { 96 };
    let cx = w.x + w.w as i32 - 26;
    circle(cx, w.y + 22, 5, 0xff5f57, a);
    circle(cx - 20, w.y + 22, 5, 0xfebc2e, a);
    circle(cx - 40, w.y + 22, 5, 0x28c840, a);
}

fn draw_body(i: usize, w: Win) {
    match i {
        0 => {
            term_guard((w.x + PAD_X as i32) as u32, (w.y + TITLE_H as i32 + 12) as u32, 88 * 8 + 12);
            draw_term_status(w);
        }
        1 => draw_sys_body(w),
        2 => draw_net_body(w),
        3 => crate::web::draw(
            (w.x + 12) as u32,
            (w.y + TITLE_H as i32) as u32,
            w.w - 24,
            w.h - TITLE_H - 10,
        ),
        _ => {}
    }
}

fn draw_term_status(w: Win) {
    let sy = (w.y + w.h as i32 - 25) as u32;
    let (tasks, kernel_kb) = unsafe { (LAST_TASKS, LAST_KB) };
    let lx = (w.x + 18) as u32;
    draw_text_ui(lx, sy, "任务 ", DIM);
    let lx = draw_num_ui(lx + 3 * 10 + 4, sy, tasks as u64);
    draw_text_ui(lx + 10, sy, "内核 ", DIM);
    let lx = draw_num_ui(lx + 10 + 3 * 10 + 4, sy, kernel_kb);
    draw_text_ui(lx + 6, sy, "KB", DIM);
    let lx = lx + 6 + 2 * 10 + 16;
    draw_text_ui(lx, sy, "页帧 ", DIM);
    let (used_pages, _total, _) = crate::frame::stats();
    let lx = draw_num_ui(lx + 3 * 10 + 4, sy, used_pages as u64);
    draw_text_ui(lx + 6, sy, "/ 32768", DIM);
    draw_text_ui((w.x + w.w as i32 - 250) as u32, sy, "EX3 已校验 · AVP 三级通过", DIM);
}

fn kv(x: u32, y: u32, k: &str, v: &str, c: u32) {
    draw_text_ui(x, y, k, DIM);
    draw_text_ui(x + 68, y, v, c);
}

fn draw_sys_body(w: Win) {
    let x = (w.x + 18) as u32;
    let mut y = (w.y + TITLE_H as i32 + 16) as u32;
    let (tasks, kernel_kb) = unsafe { (LAST_TASKS, LAST_KB) };
    let (used_pages, _t, _) = crate::frame::stats();
    kv(x, y, "版本", "Aster 0.9  ARM64", TEXT);
    y += 24;
    let kb_b = num_str(kernel_kb);
    kv(x, y, "内核", s_of(&kb_b), TEXT);
    draw_text_ui(x + 68 + text_len_ui(s_of(&kb_b)) + 6, y, "KB", DIM);
    y += 24;
    let t_b = num_str(tasks as u64);
    kv(x, y, "任务", s_of(&t_b), TEXT);
    y += 24;
    let p_b = num_str(used_pages as u64);
    kv(x, y, "页帧", s_of(&p_b), TEXT);
    draw_text_ui(x + 68 + text_len_ui(s_of(&p_b)) + 6, y, "页", DIM);
    y += 24;
    kv(x, y, "磁盘", if crate::blk::available() { "已连接" } else { "未连接" }, if crate::blk::available() { OK } else { DIM });
    y += 24;
    kv(x, y, "文件", "AsterFS", TEXT);
    y += 24;
    kv(x, y, "信任", "EX3 已校验", OK);
    let by = (w.y + w.h as i32 - 34) as u32;
    fill_rect(x, by - 12, w.w - 36, 1, 0x33373e);
    draw_text_ui(x, by, "AsterFS · virtio-blk", DIM);
}

fn draw_net_body(w: Win) {
    let x = (w.x + 18) as u32;
    let mut y = (w.y + TITLE_H as i32 + 16) as u32;
    if !crate::net::available() {
        kv(x, y, "设备", "未找到 virtio-net", WARN);
        y += 24;
        draw_text_ui(x, y, "启动时加 -device virtio-net-device", DIM);
        y += 24;
        draw_text_ui(x, y, "加上就有 10.0.2.15 / 网关 10.0.2.2", DIM);
        y += 24;
        draw_text_ui(x, y, "DHCP · DNS · HTTP 客户端已就绪", TEXT2);
        return;
    }
    let m = crate::net::mac();
    let mac = format_mac(m);
    let mac_s = core::str::from_utf8(&mac[..17]).unwrap_or("?");
    let (rx, tx, conns) = crate::net::stats();
    kv(x, y, "MAC", mac_s, TEXT);
    y += 24;
    kv(x, y, "IP", "10.0.2.15/24", TEXT);
    y += 24;
    kv(x, y, "网关", "10.0.2.2", TEXT);
    y += 24;
    let rx_b = num_str(rx as u64);
    kv(x, y, "收包", s_of(&rx_b), TEXT);
    y += 24;
    let tx_b = num_str(tx as u64);
    kv(x, y, "发包", s_of(&tx_b), TEXT);
    y += 24;
    let cn_b = num_str(conns as u64);
    kv(x, y, "连接", s_of(&cn_b), TEXT);
    y += 24;
    kv(x, y, "端口", "TCP :80", ACCENT);
}

fn s_of(b: &[u8]) -> &str {
    let n = b.iter().position(|&c| c == b' ').unwrap_or(b.len());
    core::str::from_utf8(&b[..n]).unwrap_or("?")
}

fn format_mac(m: [u8; 6]) -> [u8; 17] {
    let mut b = [b'0'; 17];
    let mut k = 0;
    for (i, v) in m.iter().enumerate() {
        if i > 0 {
            b[k] = b':';
            k += 1;
        }
        let hi = v >> 4;
        let lo = v & 0x0f;
        b[k] = if hi < 10 { b'0' + hi } else { b'a' + hi - 10 };
        b[k + 1] = if lo < 10 { b'0' + lo } else { b'a' + lo - 10 };
        k += 2;
    }
    b
}



/// 数字转定长字符串（尾部补空格），给简单 layout 用
fn num_str(v: u64) -> [u8; 20] {
    let mut b = [b' '; 20];
    let mut d = [0u8; 20];
    let mut n = 0;
    let mut t = v;
    if t == 0 {
        d[0] = b'0';
        n = 1;
    }
    while t > 0 && n < 20 {
        d[n] = b'0' + (t % 10) as u8;
        t /= 10;
        n += 1;
    }
    for i in 0..n {
        b[i] = d[n - 1 - i];
    }
    b
}

fn draw_num_ui(x: u32, y: u32, v: u64) -> u32 {
    let s = num_str(v);
    let n = s.iter().position(|&c| c == b' ').unwrap_or(20);
    let t = core::str::from_utf8(&s[..n]).unwrap_or("0");
    draw_text_ui(x, y, t, TEXT2)
}

/// PERF-3：终端窗口增量刷新。只擦/只画变化的那几行，标题栏和圆角面板不重画。
/// PERF-5：多窗时把终端脏行补进它自己的层（不整窗重画，也不碰别的窗口）
fn term_patch_layer() {
    let w = win(0);
    let lp = layer_prepare(0, w.w, w.h);
    if lp == 0 {
        term_patch();
        return;
    }
    set_layer(lp, w.w, w.h, w.x, w.y);
    let x = (w.x + PAD_X as i32) as u32;
    let y = (w.y + TITLE_H as i32 + 12) as u32;
    match crate::term::take_rows() {
        Some((r0, r1)) => {
            crate::term::render_range(x, y, r0, r1, true);
            let top = y as i32 + (r0 as i32) * crate::term::LH as i32;
            let h = ((r1 - r0 + 1) as i32) * crate::term::LH as i32;
            repaint_add((w.x + PAD_X as i32) - 6, top - 4, w.w as i32 - 2 * PAD_X as i32 + 12, h + 8);
        }
        None => {}
    }
    // 状态行里的页帧数每帧都在动，一行而已
    draw_term_status(w);
    repaint_add(w.x + 10, w.y + w.h as i32 - 34, w.w as i32 - 24, 22);
    clear_layer();
    layer_mark(0, w);
    crate::cursor::invalidate();
}

fn term_patch() {
    let w = win(0);
    if w.w == 0 || w.h == 0 {
        return;
    }
    let x = (w.x + PAD_X as i32) as u32;
    let y = (w.y + TITLE_H as i32 + 12) as u32;
    match crate::term::take_rows() {
        Some((r0, r1)) => {
            crate::term::render_range(x, y, r0, r1, true);
        }
        None => {}
    }
    // 状态行的页帧/任务数会变，顺手补一下（一行而已，很便宜）
    draw_term_status(w);
    crate::cursor::invalidate();
}

fn term_guard(x: u32, y: u32, w: u32) {
    let ph = crate::term::ROWS as u32 * crate::term::LH + 16;
    rounded_rect(x as i32 - 6, y as i32 - 6, w + 8, ph + 8, 12, 0x191b1f);
    rounded_rect(x as i32 - 4, y as i32 - 4, w, ph, 10, TERM_BG);
    fill_rect(x - 2, y - 4, w - 6, 1, 0x24272c);
    crate::term::render(x, y);
}

/// 壁纸恢复只擦"没有窗口压着"的区段。
///
/// 原来每帧 `restore_wall(0,0,W,H)` 整屏搬 3 MiB，量出来 3–4 ms，其中大半
/// 花在擦马上要被窗口盖住的像素上。顶栏和 dock 底板画在壁纸层里，必须照常
/// 恢复；只有打开窗口的矩形可以跳过 —— 那里下一拍就会被窗口自己重画。
/// 逐行求窗口并集，再搬空隙，段内仍然两像素一搬。
fn restore_wall_gaps() {
    if !available() {
        return;
    }
    let fw = fb().w;
    let fh = fb().h;
    // 先把打开窗口的矩形抄一份，避免每行都进 unsafe 读全局
    let mut rect: [(i32, i32, i32, i32); 4] = [(0, 0, 0, 0); 4];
    let mut rn = 0usize;
    unsafe {
        for i in 0..NWIN {
            if WINS[i].open && WINS[i].w > 0 && WINS[i].h > 0 {
                rect[rn] = (
                    WINS[i].x,
                    WINS[i].y,
                    WINS[i].x + WINS[i].w as i32,
                    WINS[i].y + WINS[i].h as i32,
                );
                rn += 1;
            }
        }
    }
    if rn == 0 {
        restore_wall(0, 0, fw, fh);
        return;
    }

    for y in 0..fh {
        let mut cov: [(u32, u32); 4] = [(0, 0); 4];
        let mut cn = 0usize;
        for k in 0..rn {
            let (x0, y0, x1, y1) = rect[k];
            if (y as i32) < y0 || (y as i32) >= y1 {
                continue;
            }
            let a = x0.max(0).min(fw as i32) as u32;
            let b = x1.max(0).min(fw as i32) as u32;
            if b > a {
                cov[cn] = (a, b);
                cn += 1;
            }
        }
        if cn == 0 {
            restore_wall(0, y, fw, 1);
            continue;
        }
        // 插入排序 + 合并（最多四段）
        for i in 1..cn {
            let mut j = i;
            while j > 0 && cov[j].0 < cov[j - 1].0 {
                cov.swap(j, j - 1);
                j -= 1;
            }
        }
        let mut x = 0u32;
        for k in 0..cn {
            let (a, b) = cov[k];
            if a > x {
                restore_wall(x, y, a - x, 1);
            }
            if b > x {
                x = b;
            }
        }
        if x < fw {
            restore_wall(x, y, fw - x, 1);
        }
    }
}

pub fn draw_desktop(
    ex3_short: &str,
    uptime_ms: u64,
    kb: bool,
    net: bool,
    tasks: u32,
    kernel_kb: u64,
) {
    if !available() {
        return;
    }
    let _ = ex3_short;

    let t_rest = crate::timer::now_ms();
    // 分钟跳一格才重擦壁纸（顶栏时钟在壁纸层里）+ 窗口动过 + 键盘开合
    let minute = match crate::rtc::hm() {
        Some((h, m)) => (h as u64) * 60 + m as u64,
        None => u64::MAX,
    };
    let minute_changed = minute != unsafe { LAST_MIN };
    if minute_changed {
        unsafe {
            LAST_MIN = minute;
        }
    }
    if take_wall() || minute_changed {
        restore_wall_gaps();
    }
    let t_side = crate::timer::now_ms();
    draw_sidebar();
    let t_after_side = crate::timer::now_ms();
    unsafe {
        MS_RESTORE = t_side.saturating_sub(t_rest);
        MS_SIDEBAR = t_after_side.saturating_sub(t_side);
    }

    let lamp_y = (DOCK_Y + 23) as i32;
    circle((W - 232) as i32, lamp_y, 4, if kb { OK } else { 0x454b52 }, 255);
    draw_text_ui(W - 220, DOCK_Y + 15, "键盘", if kb { TEXT2 } else { DIM });
    circle((W - 152) as i32, lamp_y, 4, if net { OK } else { 0x454b52 }, 255);
    draw_text_ui(W - 140, DOCK_Y + 15, "网络", if net { TEXT2 } else { DIM });

    let (cbuf, clen) = match crate::rtc::hm() {
        Some((h, m)) => {
            let mut b = *b"00:00   ";
            b[0] = b'0' + (h / 10) as u8;
            b[1] = b'0' + (h % 10) as u8;
            b[3] = b'0' + (m / 10) as u8;
            b[4] = b'0' + (m % 10) as u8;
            (b, 5)
        }
        None => clock_str(uptime_ms),
    };
    let tstr = core::str::from_utf8(&cbuf[..clen]).unwrap_or("00:00");
    draw_text_ui(W - 64, DOCK_Y + 15, tstr, TEXT);

    unsafe {
        LAST_TASKS = tasks;
        LAST_KB = kernel_kb;
    }
    // 每帧都画阴影太贵；只有刚动过窗口的那一小段才要
    draw_all_ex(shadow_wanted());
}

pub fn desktop_size() -> (u32, u32) {
    (W, H)
}
