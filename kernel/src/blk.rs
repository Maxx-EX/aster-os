//! virtio-blk（DeviceID 2）
//!
//! 只用 modern 队列（QEMU 默认 force-legacy=1，必须显式关掉）。
//! 每次请求是一条三段描述符链：16 字节请求头 → 512 字节数据 → 1 字节状态。

use crate::virtio::{self, Avail, Desc, Queue, Used};

const QSIZE: u16 = 8;
pub const SEC: usize = 512;

const REQ_IN: u32 = 0;
const REQ_OUT: u32 = 1;

#[repr(C, align(16))]
struct ReqHdr {
    ty: u32,
    reserved: u32,
    sector: u64,
}

#[repr(C, align(4096))]
struct Ring([u8; 4096]);

static mut RING: Ring = Ring([0; 4096]);
static mut HDR: ReqHdr = ReqHdr { ty: 0, reserved: 0, sector: 0 };
static mut DATA: [u8; SEC] = [0; SEC];
static mut STATUS: u8 = 0xff;

static mut DEV: Option<virtio::Device> = None;
static mut Q: Queue = Queue::empty();
static mut HAVE: bool = false;
static mut SECTORS: u64 = 0;
static mut REQS: u32 = 0;

pub fn init() -> bool {
    let dev = match virtio::find(virtio::DEV_BLOCK, 0) {
        Some(d) => d,
        None => return false,
    };

    dev.reset();
    let mut st = virtio::STATUS_ACK;
    dev.set_status(st);
    st |= virtio::STATUS_DRIVER;
    dev.set_status(st);

    // 不要任何可选特性：不协商 FLUSH / RO / 多队列，请求路径最简单
    dev.set_features(0);

    if dev.version == 2 {
        st |= virtio::STATUS_FEATURES_OK;
        dev.set_status(st);
        if dev.status() & virtio::STATUS_FEATURES_OK == 0 {
            crate::println!("[BLK] 设备拒绝特性协商");
            return false;
        }
    }

    let sectors = dev.read_config_u64(0);
    let ok = unsafe {
        let mem = core::ptr::addr_of_mut!(RING.0) as *mut u8;
        let desc = mem as *mut Desc;
        let avail = mem.add((QSIZE as usize) * core::mem::size_of::<Desc>()) as *mut Avail;
        let used = mem.add(2048) as *mut Used;
        crate::virtio::barrier();
        Q.bind(desc, avail, used, QSIZE);
        SECTORS = sectors;
        if dev.version == 2 {
            dev.setup_queue(0, desc, avail, used, QSIZE)
        } else {
            dev.setup_queue_legacy(0, mem as u64, QSIZE)
        }
    };
    if !ok {
        crate::println!("[BLK] 队列建立失败");
        return false;
    }

    dev.add_status(virtio::STATUS_DRIVER_OK);
    unsafe {
        DEV = Some(dev);
        HAVE = true;
    }
    true
}

pub fn available() -> bool {
    unsafe { HAVE }
}

/// 扇区数（512 字节为单位）
pub fn sectors() -> u64 {
    unsafe { SECTORS }
}

pub fn requests() -> u32 {
    unsafe { REQS }
}

unsafe fn request(write: bool, lba: u64) -> bool {
    let dev = match DEV {
        Some(d) => d,
        None => return false,
    };
    HDR.ty = if write { REQ_OUT } else { REQ_IN };
    HDR.reserved = 0;
    HDR.sector = lba;
    STATUS = 0xff;

    let hdr = core::ptr::addr_of!(HDR) as u64;
    let data = core::ptr::addr_of_mut!(DATA) as u64;
    let status = core::ptr::addr_of_mut!(STATUS) as u64;

    Q.set(0, hdr, 16, virtio::DESC_F_NEXT, 1);
    let dflags = virtio::DESC_F_NEXT | if write { 0 } else { virtio::DESC_F_WRITE };
    Q.set(1, data, SEC as u32, dflags, 2);
    Q.set(2, status, 1, virtio::DESC_F_WRITE, 0);

    Q.submit(0);
    dev.notify(0);

    let mut spins = 0u32;
    loop {
        if Q.pop_used().is_some() {
            break;
        }
        spins += 1;
        if spins > 5_000_000 {
            crate::println!("[BLK] 请求超时 lba={}", lba);
            return false;
        }
        core::hint::spin_loop();
    }
    REQS += 1;
    STATUS == 0
}

pub fn read(lba: u64, buf: &mut [u8]) -> bool {
    if buf.len() > SEC {
        return false;
    }
    if !unsafe { request(false, lba) } {
        return false;
    }
    unsafe {
        for i in 0..buf.len() {
            buf[i] = DATA[i];
        }
    }
    true
}

pub fn write(lba: u64, buf: &[u8]) -> bool {
    if buf.len() > SEC {
        return false;
    }
    unsafe {
        for i in 0..SEC {
            DATA[i] = if i < buf.len() { buf[i] } else { 0 };
        }
        request(true, lba)
    }
}
