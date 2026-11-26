// Aster Kernel — virtio-input 键盘驱动
//
// QEMU 的 virtio-keyboard-device 挂在 virtio-mmio 上（DeviceID = 18）。
// 它只有一条队列：queue 0 = eventq，方向是"设备→驱动"。
// 每个事件 8 字节，全部小端：u16 type | u16 code | u32 value
//   type 0 = EV_SYN（一次上报结束）、1 = EV_KEY（按键，code 为 Linux 键码）、
//   2 = EV_REL（相对位移，鼠标用）。
// 我们挂 8 个 8 字节缓冲循环使用，每消费一个就立刻归还，否则键盘会卡住。
#![allow(static_mut_refs)]

use crate::virtio::{self, Device, Queue};

const QSIZE: u16 = 8;
const EV_KEY: u16 = 1;

const KEY_ESC: u16 = 1;
const KEY_BKSP: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_ENTER: u16 = 28;
const KEY_LCTRL: u16 = 29;
const KEY_LSHIFT: u16 = 42;
const KEY_RSHIFT: u16 = 54;
const KEY_SPACE: u16 = 57;
const KEY_CAPS: u16 = 58;

#[repr(C, align(4096))]
struct QMem {
    raw: [u8; 8192],
}

static mut QMEM: QMem = QMem { raw: [0; 8192] };
static mut EVBUF: [[u8; 8]; QSIZE as usize] = [[0; 8]; QSIZE as usize];

static mut KB_DEV: Option<Device> = None;
static mut KB_Q: Queue = Queue::empty();
static mut KB_OK: bool = false;

// 键盘字符环形缓冲：shell 和桌面都从这里取
const RING: usize = 128;
static mut RING_BUF: [u8; RING] = [0; RING];
static mut RING_HEAD: usize = 0;
static mut RING_TAIL: usize = 0;

static mut SHIFT: bool = false;
static mut CAPS: bool = false;
static mut EVENTS: u32 = 0; // 累计收到的事件数，便于诊断

static mut MSMEM: QMem = QMem { raw: [0; 8192] };
static mut MEVBUF: [[u8; 8]; QSIZE as usize] = [[0; 8]; QSIZE as usize];
static mut MS_DEV: Option<Device> = None;
static mut MS_Q: Queue = Queue::empty();
static mut MS_OK: bool = false;

const EV_REL: u16 = 2;
const BTN_LEFT: u16 = 0x110;

static mut MX: i32 = 760;
static mut MY: i32 = 600;
static mut MBTN: u8 = 0;

const CQ: usize = 8;
static mut CLICK_Q: [(i32, i32, bool); CQ] = [(0, 0, false); CQ];
static mut CQ_HEAD: usize = 0;
static mut CQ_TAIL: usize = 0;

/// 读 virtio-input 的设备名（前 16 字节），用来区分键盘和鼠标
fn device_name(dev: &Device) -> [u8; 16] {
    dev.write_config_u8(0, 1); // VIRTIO_INPUT_CFG_ID_NAME
    dev.write_config_u8(1, 0);
    let size = dev.read_config_u8(2) as usize;
    let mut buf = [0u8; 16];
    for i in 0..size.min(16) {
        buf[i] = dev.read_config_u8(8 + i as u64);
    }
    buf
}

fn is_mouse(dev: &Device) -> bool {
    let n = device_name(dev);
    n.windows(4).any(|w| w.eq_ignore_ascii_case(b"Mous"))
}

/// 先把设备拉到 DRIVER_OK 之前的状态（两条队列共用这段）
fn bring_up(dev: &Device) -> bool {
    dev.reset();
    let mut st = virtio::STATUS_ACK;
    dev.set_status(st);
    st |= virtio::STATUS_DRIVER;
    dev.set_status(st);
    dev.set_features(0);
    if dev.version == 2 {
        st |= virtio::STATUS_FEATURES_OK;
        dev.set_status(st);
        if dev.status() & virtio::STATUS_FEATURES_OK == 0 {
            return false;
        }
    }
    true
}

/// 初始化键盘。找不到设备就返回 false，串口输入照常可用。
pub fn init() -> bool {
    let mut kb: Option<Device> = None;
    let mut ms: Option<Device> = None;
    for i in 0..4 {
        match virtio::find(virtio::DEV_INPUT, i) {
            Some(d) => {
                let n = device_name(&d);
                let len = n.iter().position(|&b| b == 0).unwrap_or(n.len());
                let shown = core::str::from_utf8(&n[..len]).unwrap_or("?");
                let mouse = is_mouse(&d);
                crate::println!("[IN] virtio-input slot {} name={:?} mouse={}", d.slot, shown, mouse);
                if mouse {
                    if ms.is_none() {
                        ms = Some(d);
                    }
                } else if kb.is_none() {
                    kb = Some(d);
                } else if ms.is_none() {
                    // 两个都读不出名字：按出现顺序，第二个当鼠标
                    ms = Some(d);
                }
            }
            None => break,
        }
    }
    let mut any = false;
    if let Some(d) = kb {
        any |= kb_init(d);
    }
    if let Some(d) = ms {
        any |= ms_init(d);
    }
    any
}

fn kb_init(dev: Device) -> bool {
    if !bring_up(&dev) {
        return false;
    }
    unsafe {
        let base = core::ptr::addr_of_mut!(QMEM) as *mut u8;
        let desc = base as *mut virtio::Desc;
        let avail = base.add((QSIZE as usize) * 16) as *mut virtio::Avail;
        let used = base.add(4096) as *mut virtio::Used;

        KB_Q.bind(desc, avail, used, QSIZE);

        let ok = if dev.version == 2 {
            dev.setup_queue(0, desc, avail, used, QSIZE)
        } else {
            dev.setup_queue_legacy(0, base as u64, QSIZE)
        };
        if !ok {
            return false;
        }

        for i in 0..QSIZE {
            let idx = match KB_Q.alloc() {
                Some(v) => v,
                None => break,
            };
            let a = core::ptr::addr_of!(EVBUF[i as usize]) as u64;
            KB_Q.set(idx, a, 8, virtio::DESC_F_WRITE, 0);
            KB_Q.submit(idx);
        }

        dev.add_status(virtio::STATUS_DRIVER_OK);
        dev.notify(0);

        KB_DEV = Some(dev);
        KB_OK = true;
    }
    true
}

fn ms_init(dev: Device) -> bool {
    if !bring_up(&dev) {
        return false;
    }
    unsafe {
        let base = core::ptr::addr_of_mut!(MSMEM) as *mut u8;
        let desc = base as *mut virtio::Desc;
        let avail = base.add((QSIZE as usize) * 16) as *mut virtio::Avail;
        let used = base.add(4096) as *mut virtio::Used;

        MS_Q.bind(desc, avail, used, QSIZE);

        let ok = if dev.version == 2 {
            dev.setup_queue(0, desc, avail, used, QSIZE)
        } else {
            dev.setup_queue_legacy(0, base as u64, QSIZE)
        };
        if !ok {
            return false;
        }

        for i in 0..QSIZE {
            let idx = match MS_Q.alloc() {
                Some(v) => v,
                None => break,
            };
            let a = core::ptr::addr_of!(MEVBUF[i as usize]) as u64;
            MS_Q.set(idx, a, 8, virtio::DESC_F_WRITE, 0);
            MS_Q.submit(idx);
        }

        dev.add_status(virtio::STATUS_DRIVER_OK);
        dev.notify(0);

        MS_DEV = Some(dev);
        MS_OK = true;
    }
    true
}

pub fn available() -> bool {
    unsafe { KB_OK }
}

/// 收事件（非阻塞）。shell 的空闲循环、桌面刷新循环都会调它。
pub fn poll() {
    poll_kb();
    poll_ms();
}

fn poll_kb() {
    if unsafe { !KB_OK } {
        return;
    }
    loop {
        let head = unsafe { KB_Q.pop_used() };
        let (id, _len) = match head {
            Some(v) => v,
            None => break,
        };
        let id = id as u16;
        let buf = unsafe { core::ptr::addr_of!(EVBUF[id as usize]) as *const u8 };
        let typ = unsafe { u16::from_le_bytes([*buf, *buf.add(1)]) };
        let code = unsafe { u16::from_le_bytes([*buf.add(2), *buf.add(3)]) };
        let val = unsafe { u32::from_le_bytes([*buf.add(4), *buf.add(5), *buf.add(6), *buf.add(7)]) };
        unsafe { EVENTS = EVENTS.wrapping_add(1) };

        if typ == EV_KEY {
            key_event(code, val);
        }

        unsafe {
            let a = core::ptr::addr_of!(EVBUF[id as usize]) as u64;
            KB_Q.set(id, a, 8, virtio::DESC_F_WRITE, 0);
            KB_Q.submit(id);
        }
        if let Some(dev) = unsafe { KB_DEV } {
            dev.notify(0);
        }
    }
}

fn poll_ms() {
    if unsafe { !MS_OK } {
        return;
    }
    loop {
        let head = unsafe { MS_Q.pop_used() };
        let (id, _len) = match head {
            Some(v) => v,
            None => break,
        };
        let id = id as u16;
        let buf = unsafe { core::ptr::addr_of!(MEVBUF[id as usize]) as *const u8 };
        let typ = unsafe { u16::from_le_bytes([*buf, *buf.add(1)]) };
        let code = unsafe { u16::from_le_bytes([*buf.add(2), *buf.add(3)]) };
        let val = unsafe { u32::from_le_bytes([*buf.add(4), *buf.add(5), *buf.add(6), *buf.add(7)]) };

        mouse_event(typ, code, val);

        unsafe {
            let a = core::ptr::addr_of!(MEVBUF[id as usize]) as u64;
            MS_Q.set(id, a, 8, virtio::DESC_F_WRITE, 0);
            MS_Q.submit(id);
        }
        if let Some(dev) = unsafe { MS_DEV } {
            dev.notify(0);
        }
    }
}

fn mouse_event(typ: u16, code: u16, val: u32) {
    match typ {
        EV_REL => {
            // val 是相对位移，可能为负（u32 回绕）
            let d = val as i32;
            match code {
                0 => unsafe { MX = (MX + d).clamp(0, 1023) },
                1 => unsafe { MY = (MY + d).clamp(0, 767) },
                _ => {}
            }
        }
        EV_KEY if code == BTN_LEFT => {
            let pressed = val != 0;
            unsafe {
                if pressed {
                    MBTN |= 1;
                } else {
                    MBTN &= !1;
                }
                let tail = CQ_TAIL;
                CLICK_Q[tail] = (MX, MY, pressed);
                CQ_TAIL = (tail + 1) % CQ;
            }
        }
        _ => {}
    }
}

fn key_event(code: u16, val: u32) {
    // val: 1 = 按下，0 = 抬起，2 = 自动重复
    if code == KEY_LSHIFT || code == KEY_RSHIFT {
        unsafe { SHIFT = val != 0 };
        return;
    }
    if code == KEY_CAPS {
        if val == 1 {
            unsafe { CAPS = !CAPS };
        }
        return;
    }
    if val == 0 {
        return;
    }
    // 控制键先翻译成 shell 认识的单字节
    match code {
        KEY_ENTER => return push(b'\n'),
        KEY_BKSP => return push(0x08),
        KEY_TAB => return push(b'\t'),
        KEY_ESC => return push(0x1B),
        KEY_LCTRL => return,
        KEY_SPACE => return push(b' '),
        _ => {}
    }
    if let Some((plain, shifted)) = key_to_char(code) {
        let upper = unsafe { SHIFT ^ CAPS };
        // 字母受 Shift/CapsLock 影响，符号只受 Shift 影响
        let c = if updated_shift() {
            shifted
        } else if upper && plain.is_ascii_alphabetic() {
            plain.to_ascii_uppercase()
        } else {
            plain
        };
        push(c);
    }
}

fn updated_shift() -> bool {
    unsafe { SHIFT }
}

/// Linux 键码 → (无 Shift, 有 Shift) 两套字符
fn key_to_char(code: u16) -> Option<(u8, u8)> {
    let p = match code {
        2 => (b'1', b'!'),
        3 => (b'2', b'@'),
        4 => (b'3', b'#'),
        5 => (b'4', b'$'),
        6 => (b'5', b'%'),
        7 => (b'6', b'^'),
        8 => (b'7', b'&'),
        9 => (b'8', b'*'),
        10 => (b'9', b'('),
        11 => (b'0', b')'),
        12 => (b'-', b'_'),
        13 => (b'=', b'+'),
        16 => (b'q', b'Q'),
        17 => (b'w', b'W'),
        18 => (b'e', b'E'),
        19 => (b'r', b'R'),
        20 => (b't', b'T'),
        21 => (b'y', b'Y'),
        22 => (b'u', b'U'),
        23 => (b'i', b'I'),
        24 => (b'o', b'O'),
        25 => (b'p', b'P'),
        26 => (b'[', b'{'),
        27 => (b']', b'}'),
        30 => (b'a', b'A'),
        31 => (b's', b'S'),
        32 => (b'd', b'D'),
        33 => (b'f', b'F'),
        34 => (b'g', b'G'),
        35 => (b'h', b'H'),
        36 => (b'j', b'J'),
        37 => (b'k', b'K'),
        38 => (b'l', b'L'),
        39 => (b';', b':'),
        40 => (b'\'', b'"'),
        41 => (b'`', b'~'),
        43 => (b'\\', b'|'),
        44 => (b'z', b'Z'),
        45 => (b'x', b'X'),
        46 => (b'c', b'C'),
        47 => (b'v', b'V'),
        48 => (b'b', b'B'),
        49 => (b'n', b'N'),
        50 => (b'm', b'M'),
        51 => (b',', b'<'),
        52 => (b'.', b'>'),
        53 => (b'/', b'?'),
        _ => return None,
    };
    Some(p)
}

fn push(c: u8) {
    unsafe {
        let tail = RING_TAIL;
        let next = (tail + 1) % RING;
        if next != RING_HEAD {
            RING_BUF[tail] = c;
            RING_TAIL = next;
        }
    }
}

/// 取一个字符（非阻塞）
pub fn read_char() -> Option<u8> {
    unsafe {
        if RING_HEAD == RING_TAIL {
            return None;
        }
        let c = RING_BUF[RING_HEAD];
        RING_HEAD = (RING_HEAD + 1) % RING;
        Some(c)
    }
}

/// 有没有待处理的字符
pub fn pending() -> bool {
    unsafe { RING_HEAD != RING_TAIL }
}

pub fn event_count() -> u32 {
    unsafe { EVENTS }
}

// ---------------- 鼠标对外状态 ----------------

pub fn mouse_available() -> bool {
    unsafe { MS_OK }
}

pub fn mouse_pos() -> (i32, i32) {
    unsafe { (MX, MY) }
}

pub fn mouse_buttons() -> u8 {
    unsafe { MBTN }
}

/// 取一个点击事件（x, y, 是否按下），队列空返回 None
pub fn take_click() -> Option<(i32, i32, bool)> {
    unsafe {
        if CQ_HEAD == CQ_TAIL {
            return None;
        }
        let e = CLICK_Q[CQ_HEAD];
        CQ_HEAD = (CQ_HEAD + 1) % CQ;
        Some(e)
    }
}
