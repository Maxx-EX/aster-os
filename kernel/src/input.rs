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

/// 初始化键盘。找不到设备就返回 false，串口输入照常可用。
pub fn init() -> bool {
    let dev = match virtio::find(virtio::DEV_INPUT, 0) {
        Some(d) => d,
        None => return false,
    };

    dev.reset();
    let mut st = virtio::STATUS_ACK;
    dev.set_status(st);
    st |= virtio::STATUS_DRIVER;
    dev.set_status(st);

    // 输入设备不需要任何特性
    dev.set_features(0);
    if dev.version == 2 {
        st |= virtio::STATUS_FEATURES_OK;
        dev.set_status(st);
        if dev.status() & virtio::STATUS_FEATURES_OK == 0 {
            return false;
        }
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

        // 把 8 个事件缓冲全部挂进队列
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

pub fn available() -> bool {
    unsafe { KB_OK }
}

/// 收事件（非阻塞）。shell 的空闲循环、桌面刷新循环都会调它。
pub fn poll() {
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

        // 立刻归还缓冲，否则键盘队列很快就满
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
