//! virtio-gpu（DeviceID 16）—— 2D 资源直接扫描输出
//!
//! 和 ramfb 的区别就是这一节要解决的问题：
//!   ramfb 由 QEMU 每帧从 guest 内存里采样，内核画到一半就被取样 → 画面一闪一闪；
//!   virtio-gpu 只在 guest 主动 TRANSFER_TO_HOST_2D + RESOURCE_FLUSH 时更新，
//!   一帧是原子的，绘制过程中的中间状态永远不会出现在屏幕上。
//!
//! 资源内存仍用 0x5000_0000 那块 3 MiB（1024x768x4），通过 ATTACH_BACKING
//! 把 768 个物理页挂给 host 侧的 resource，之后全靠 flush 呈现。
#![allow(static_mut_refs)]

use crate::virtio::{self, Avail, Desc, Queue, Used};

const QSIZE: u16 = 8;
const RES_ID: u32 = 1;

const CMD_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_FLUSH: u32 = 0x0104;
const CMD_TRANSFER: u32 = 0x0105;
const CMD_ATTACH: u32 = 0x0106;
const RESP_OK_NODATA: u32 = 0x1100;
// 与内核像素 0x00RRGGBB 一致：内存字节序 B,G,R,X
const FORMAT_B8G8R8X8: u32 = 1;

#[repr(C, align(4096))]
struct Ring([u8; 8192]);
static mut RING: Ring = Ring([0; 8192]);
static mut Q: Queue = Queue::empty();
static mut DEV: Option<virtio::Device> = None;
static mut HAVE: bool = false;
static mut W: u32 = 0;
static mut H: u32 = 0;
static mut PRESENTS: u32 = 0;

static mut CMD: [u8; 16384] = [0; 16384];
static mut RESP: [u8; 256] = [0; 256];

#[repr(C)]
#[derive(Clone, Copy)]
struct Hdr {
    ty: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    _pad: u32,
}

fn w32(off: usize, v: u32) {
    unsafe {
        core::ptr::write_unaligned(CMD.as_mut_ptr().add(off) as *mut u32, v);
    }
}

fn w64(off: usize, v: u64) {
    unsafe {
        core::ptr::write_unaligned(CMD.as_mut_ptr().add(off) as *mut u64, v);
    }
}

fn hdr(ty: u32) {
    unsafe {
        core::ptr::write_unaligned(CMD.as_mut_ptr() as *mut Hdr, Hdr {
            ty,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            _pad: 0,
        });
    }
}

unsafe fn ctrl(len: u32) -> Result<u32, &'static str> {
    let dev = match DEV {
        Some(d) => d,
        None => return Err("gpu 未初始化"),
    };
    Q.set(0, CMD.as_ptr() as u64, len, virtio::DESC_F_NEXT, 1);
    Q.set(1, RESP.as_mut_ptr() as u64, 24, virtio::DESC_F_WRITE, 0);
    Q.submit(0);
    dev.notify(0);
    let mut spins = 0u32;
    loop {
        if Q.pop_used().is_some() {
            break;
        }
        spins += 1;
        if spins > 30_000_000 {
            return Err("gpu 命令超时");
        }
        core::hint::spin_loop();
    }
    Ok(u32::from_le_bytes([RESP[0], RESP[1], RESP[2], RESP[3]]))
}

fn create_2d(id: u32, w: u32, h: u32) -> bool {
    hdr(CMD_CREATE_2D);
    w32(24, id);
    w32(28, FORMAT_B8G8R8X8);
    w32(32, w);
    w32(36, h);
    match unsafe { ctrl(40) } {
        Ok(t) if t == RESP_OK_NODATA => true,
        Ok(t) => {
            crate::println!("[GPU] create_2d 失败 响应 {:#x}", t);
            false
        }
        Err(e) => {
            crate::println!("[GPU] create_2d {}", e);
            false
        }
    }
}

fn attach(id: u32, phys: u64, w: u32, h: u32) -> bool {
    let n = ((w as u64 * h as u64 * 4 + 4095) / 4096) as u32;
    if 24usize + 8 + n as usize * 16 > 16384 {
        return false;
    }
    hdr(CMD_ATTACH);
    w32(24, id);
    w32(28, n);
    let mut i = 0usize;
    while i < n as usize {
        w64(32 + i * 16, phys + (i as u64) * 4096);
        w32(40 + i * 16, 4096);
        w32(44 + i * 16, 0);
        i += 1;
    }
    match unsafe { ctrl(32 + n * 16) } {
        Ok(t) if t == RESP_OK_NODATA => true,
        Ok(t) => {
            crate::println!("[GPU] attach_backing 失败 响应 {:#x}", t);
            false
        }
        Err(e) => {
            crate::println!("[GPU] attach_backing {}", e);
            false
        }
    }
}

fn set_scanout(scanout: u32, id: u32, w: u32, h: u32) -> bool {
    hdr(CMD_SET_SCANOUT);
    w32(24, 0);
    w32(28, 0);
    w32(32, w);
    w32(36, h);
    w32(40, scanout);
    w32(44, id);
    match unsafe { ctrl(48) } {
        Ok(t) if t == RESP_OK_NODATA => true,
        Ok(t) => {
            crate::println!("[GPU] set_scanout 失败 响应 {:#x}", t);
            false
        }
        Err(e) => {
            crate::println!("[GPU] set_scanout {}", e);
            false
        }
    }
}

fn transfer(id: u32, x: u32, y: u32, w: u32, h: u32) -> bool {
    hdr(CMD_TRANSFER);
    w32(24, x);
    w32(28, y);
    w32(32, w);
    w32(36, h);
    w64(40, 0);
    w32(48, id);
    w32(52, 0);
    match unsafe { ctrl(56) } {
        Ok(t) if t == RESP_OK_NODATA => true,
        _ => false,
    }
}

fn flush(id: u32, x: u32, y: u32, w: u32, h: u32) -> bool {
    hdr(CMD_FLUSH);
    w32(24, x);
    w32(28, y);
    w32(32, w);
    w32(36, h);
    w32(40, id);
    w32(44, 0);
    match unsafe { ctrl(48) } {
        Ok(t) if t == RESP_OK_NODATA => true,
        _ => false,
    }
}

pub fn init(fb_phys: u64, w: u32, h: u32) -> bool {
    let dev = match virtio::find(virtio::DEV_GPU, 0) {
        Some(d) => d,
        None => return false,
    };

    dev.reset();
    dev.set_status(virtio::STATUS_ACK);
    let mut st = virtio::STATUS_ACK | virtio::STATUS_DRIVER;
    dev.set_status(st);
    // 只要基础 2D：不协商 VIRGL / RESOURCE_BLOB / 3D 那一套
    dev.set_features(0);
    if dev.version == 2 {
        st |= virtio::STATUS_FEATURES_OK;
        dev.set_status(st);
        if dev.status() & virtio::STATUS_FEATURES_OK == 0 {
            crate::println!("[GPU] 设备拒绝特性协商");
            return false;
        }
    }

    let ok = unsafe {
        let mem = core::ptr::addr_of_mut!(RING.0) as *mut u8;
        let desc = mem as *mut Desc;
        let avail = mem.add(QSIZE as usize * core::mem::size_of::<Desc>()) as *mut Avail;
        // legacy 规范里 used 紧跟在 desc+avail 之后按 4 KiB 对齐 → 固定在 +4096；
        // modern 由我们用寄存器指定，同一地址两边都合法（blk.rs 的 +2048 只在 modern 下成立）
        let used = mem.add(4096) as *mut Used;
        virtio::barrier();
        Q.bind(desc, avail, used, QSIZE);
        if dev.version == 2 {
            dev.setup_queue(0, desc, avail, used, QSIZE)
        } else {
            dev.setup_queue_legacy(0, mem as u64, QSIZE)
        }
    };
    if !ok {
        crate::println!("[GPU] 队列建立失败");
        return false;
    }
    dev.add_status(virtio::STATUS_DRIVER_OK);
    crate::println!(
        "[GPU] 就绪 status={:#x} ver={} feat={:#x} qnum_max={}",
        dev.status(), dev.version, dev.device_features(), dev.queue_max(0)
    );
    unsafe { DEV = Some(dev) };

    if !create_2d(RES_ID, w, h) || !attach(RES_ID, fb_phys, w, h) {
        return false;
    }
    if !set_scanout(0, RES_ID, w, h) {
        return false;
    }
    unsafe {
        W = w;
        H = h;
        HAVE = true;
    }
    true
}

pub fn available() -> bool {
    unsafe { HAVE }
}

pub fn presents() -> u32 {
    unsafe { PRESENTS }
}

/// 全屏呈现：把 guest 内存推给 host resource，再让 host 刷新窗口
pub fn present() {
    if !unsafe { HAVE } {
        return;
    }
    let (w, h) = unsafe { (W, H) };
    present_rect(0, 0, w, h);
}

/// 只呈现一块矩形（鼠标光标这种小面积更新用，省一次全屏 3 MiB 拷贝）
pub fn present_rect(x: u32, y: u32, w: u32, h: u32) {
    if !unsafe { HAVE } || w == 0 || h == 0 {
        return;
    }
    let (fw, fh) = unsafe { (W, H) };
    let rw = w.min(fw.saturating_sub(x));
    let rh = h.min(fh.saturating_sub(y));
    if rw == 0 || rh == 0 {
        return;
    }
    if transfer(RES_ID, x, y, rw, rh) && flush(RES_ID, x, y, rw, rh) {
        unsafe { PRESENTS += 1 };
    }
}
