// Aster Kernel — DWC2（BCM2837/BCM2711 USB OTG）主机模式 + HID 键鼠驱动
//
// QEMU virt 上键鼠走 virtio-input；树莓派没有 PCI/virtio，键鼠挂在 SoC 的 DWC2
// 控制器上。这一层自己走完 USB 枚举，不依赖任何现有内核的 USB 子系统：
//
//   init()       核心复位 → 主机模式 → 端口供电 → 检测连接 → 端口复位
//   enumerate()  SET_ADDRESS → GET_DESCRIPTOR(config) → SET_CONFIGURATION
//                → 找 HID 接口 → SET_PROTOCOL(boot) → SET_IDLE(0)
//   poll_hid()   中断 IN 读一次报告，按 boot 格式解析后注入 input
//
// 只支持 HID boot 协议（键盘 8 字节 / 鼠标 4 字节），不解释 report descriptor，
// 这样最少代码覆盖绝大多数键鼠。
#![allow(dead_code)]

// —— DWC2 寄存器（databook 4.0）——
const GOTGCTL: u64 = 0x000;
const GAHBCFG: u64 = 0x008;
const GUSBCFG: u64 = 0x00C;
const GRSTCTL: u64 = 0x010;
const GINTSTS: u64 = 0x014;
const GSNPSID: u64 = 0x040; // 版本号，用来判断地址上到底有没有控制器
const HCFG: u64 = 0x400;
const HPRT0: u64 = 0x440;
const HCCHAR: u64 = 0x500; // + n*0x20：HCINT +8 / HCINTMSK +12 / HCTSIZ +16
const DFIFO0: u64 = 0x1000; // + n*0x1000

// HPRT0
const HPRT_CONN: u32 = 1 << 0;
const HPRT_ENA: u32 = 1 << 2;
const HPRT_RST: u32 = 1 << 8;
const HPRT_PWR: u32 = 1 << 12;
const HPRT_L1: u32 = 1 << 17;

// HCCHAR
const HCCH_CHENA: u32 = 1 << 31;
const HCCH_EPDIR_IN: u32 = 1 << 15;
const HCCH_LSPD: u32 = 1 << 17;
const HCCH_EPTYPE_CTRL: u32 = 0;
const HCCH_EPTYPE_INT: u32 = 3 << 18;

// HCINT
const HCINT_XFER: u32 = 1 << 0;
const HCINT_STALL: u32 = 1 << 3;
const HCINT_NAK: u32 = 1 << 4;
const HCINT_ALL: u32 = 0x7ff;

// HCTSIZ：XFERSIZ bits0..18 / PKTCNT bits19..28 / PID bits29..30
const PID_DATA0: u32 = 0;
const PID_DATA1: u32 = 1;
const PID_SETUP: u32 = 3;

fn rd(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

fn wr(base: u64, off: u64, v: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
}

fn base() -> Option<u64> {
    crate::board::ACTIVE.usb
}

fn spin(ms: u32) {
    // 探测阶段专用忙等，不依赖调度器
    let t0 = crate::timer::now_ms();
    while crate::timer::now_ms().saturating_sub(t0) < ms as u64 {
        core::hint::spin_loop();
    }
}

// ---------------- 状态 ----------------

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    NoController, // 板子上没有 DWC2
    NoResponse,   // 地址存在但寄存器读回全 0
    Idle,         // 控制器配好了，端口没设备
    Ready,        // 端口复位完成、枚举过了
}

#[derive(Clone, Copy)]
pub struct Hid {
    pub is_kbd: bool,
    pub ep: u8,
    pub iface: u8,
    pub mps: u16,
    pub interval: u8,
}

#[derive(Clone, Copy)]
pub struct Probe {
    pub conn: bool,
    pub enabled: bool,
    pub speed_ls: bool,
    pub vid: u16,
    pub pid: u16,
    pub dev_class: u8,
    pub hid: Option<Hid>,
}

static mut LAST: Option<Probe> = None;
static mut STATUS: Status = Status::NoController;
static mut TOGGLE: u32 = PID_DATA0;
static mut KBD_PREV: [u8; 6] = [0; 6];
static mut KBD_MOD: u8 = 0;
static mut EVENTS: u32 = 0;

pub fn status() -> Status {
    unsafe { STATUS }
}

pub fn last() -> Option<Probe> {
    unsafe { LAST }
}

pub fn hid() -> Option<Hid> {
    unsafe { LAST.and_then(|p| p.hid) }
}

pub fn events() -> u32 {
    unsafe { EVENTS }
}

// ---------------- 通道原语 ----------------

fn ch_off(ch: u32) -> u64 {
    HCCHAR + (ch as u64) * 0x20
}

fn fifo_off(ch: u32) -> u64 {
    DFIFO0 + (ch as u64) * 0x1000
}

fn ch_reset(b: u64, ch: u32) {
    let off = ch_off(ch);
    wr(b, off, 0);
    wr(b, off + 12, 0);
    wr(b, off + 8, HCINT_ALL); // 写 1 清
}

fn ch_wait(b: u64, ch: u32, ms: u32) -> Result<u32, &'static str> {
    let off = ch_off(ch);
    let mut i = 0;
    while i <= ms / 3 + 1 {
        let st = rd(b, off + 8);
        if st & (HCINT_XFER | HCINT_STALL | HCINT_NAK | (1 << 1)) != 0 {
            wr(b, off + 8, st);
            if st & HCINT_STALL != 0 {
                return Err("设备 STALL");
            }
            return Ok(st);
        }
        spin(3);
        i += 1;
    }
    Err("通道超时")
}

fn chc(ep: u8, mps: u16, in_dir: bool, int: bool, ls: bool) -> u32 {
    let mut v = HCCH_CHENA | (((ep as u32) & 0xf) << 11) | ((mps as u32) & 0x7ff);
    if in_dir {
        v |= HCCH_EPDIR_IN;
    }
    if int {
        v |= HCCH_EPTYPE_INT;
    } else {
        v |= HCCH_EPTYPE_CTRL;
    }
    if ls {
        v |= HCCH_LSPD;
    }
    v
}

// ---------------- 控制传输（EP0 / 通道 0） ----------------

fn control(b: u64, ls: bool, setup: [u8; 8], in_len: u16, out: &mut [u8]) -> Result<usize, &'static str> {
    let mps: u16 = if ls { 8 } else { 64 };
    let ch = 0u32;
    let off = ch_off(ch);
    let fifo = fifo_off(ch);
    ch_reset(b, ch);

    // SETUP 阶段：8 字节手写进 FIFO0
    wr(b, off + 16, 8 | (1 << 19) | (PID_SETUP << 29));
    wr(b, off, chc(0, mps, false, false, ls));
    let mut i = 0;
    while i < 2 {
        let w = u32::from_le_bytes([
            setup[i * 4],
            setup[i * 4 + 1],
            setup[i * 4 + 2],
            setup[i * 4 + 3],
        ]);
        wr(b, fifo, w);
        i += 1;
    }
    ch_wait(b, ch, 100)?;

    // 无数据：直接状态阶段（OUT 零长）
    if in_len == 0 {
        ch_reset(b, ch);
        wr(b, off + 16, (1 << 19) | (PID_DATA1 << 29));
        wr(b, off, chc(0, mps, false, false, ls));
        ch_wait(b, ch, 100)?;
        return Ok(0);
    }

    // IN 阶段
    let want = (in_len as usize).min(out.len());
    ch_reset(b, ch);
    wr(b, off + 16, (want as u32) | (1 << 19) | (PID_DATA1 << 29));
    wr(b, off, chc(0, mps, true, false, ls));
    let st = ch_wait(b, ch, 100)?;
    if st & HCINT_XFER == 0 {
        return Err("数据阶段没完成");
    }
    let words = (want + 3) / 4;
    let mut k = 0;
    while k < words {
        let w = rd(b, fifo);
        let bytes = w.to_le_bytes();
        let mut j = 0;
        while j < 4 && k * 4 + j < want {
            out[k * 4 + j] = bytes[j];
            j += 1;
        }
        k += 1;
    }

    // 状态阶段
    ch_reset(b, ch);
    wr(b, off + 16, (1 << 19) | (PID_DATA1 << 29));
    wr(b, off, chc(0, mps, false, false, ls));
    let _ = ch_wait(b, ch, 100);
    Ok(want)
}

/// 中断 IN（通道 1）：读一次 HID 报告；NAK 返回 0
fn int_in(b: u64, ls: bool, hid: &Hid, out: &mut [u8]) -> Result<usize, &'static str> {
    let ch = 1u32;
    let off = ch_off(ch);
    let fifo = fifo_off(ch);
    let want = hid.mps as usize;
    if out.len() < want {
        return Err("缓冲太小");
    }
    ch_reset(b, ch);
    wr(b, off + 16, (want as u32) | (1 << 19) | (unsafe { TOGGLE } << 29));
    wr(b, off, chc(hid.ep, hid.mps, true, true, ls));
    let st = ch_wait(b, ch, 60)?;
    if st & HCINT_XFER == 0 {
        return Ok(0); // NAK 也算没数据
    }
    let words = (want + 3) / 4;
    let mut k = 0;
    while k < words {
        let w = rd(b, fifo);
        let bytes = w.to_le_bytes();
        let mut j = 0;
        while j < 4 && k * 4 + j < want {
            out[k * 4 + j] = bytes[j];
            j += 1;
        }
        k += 1;
    }
    unsafe {
        TOGGLE = if TOGGLE == PID_DATA0 { PID_DATA1 } else { PID_DATA0 };
    }
    Ok(want)
}

// ---------------- 标准与类请求 ----------------

fn set_address(b: u64, ls: bool, addr: u8) -> Result<(), &'static str> {
    control(b, ls, [0x00, 0x05, addr, 0, 0, 0, 0, 0], 0, &mut [])?;
    spin(3); // 规范要求 2ms 恢复
    Ok(())
}

fn set_config(b: u64, ls: bool, cfg: u8) -> Result<(), &'static str> {
    control(b, ls, [0x00, 0x09, cfg, 0, 0, 0, 0, 0], 0, &mut [])?;
    Ok(())
}

fn hid_req(b: u64, ls: bool, req: u8, value: u16, iface: u8) -> Result<(), &'static str> {
    let setup = [
        0x21u8,
        req,
        value as u8,
        (value >> 8) as u8,
        iface,
        0,
        0,
        0,
    ];
    control(b, ls, setup, 0, &mut [])?;
    Ok(())
}

/// 解析配置描述符，取第一个 HID 接口的中断 IN 端点
fn parse_config(cfg: &[u8]) -> Option<Hid> {
    let mut i = 0usize;
    let mut iface: u8 = 0;
    let mut is_hid = false;
    while i + 1 < cfg.len() {
        let len = cfg[i] as usize;
        let typ = cfg[i + 1];
        if len < 2 || i + len > cfg.len() {
            break;
        }
        match typ {
            0x04 => {
                // 接口：bInterfaceNumber=2 / bInterfaceClass=5
                iface = cfg[i + 2];
                is_hid = cfg[i + 5] == 0x03;
            }
            0x05 if is_hid => {
                // 端点：bEndpointAddress=2 / bmAttributes=3 / wMaxPacketSize=4 / bInterval=6
                let addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let mps = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
                let interval = cfg[i + 6];
                if addr & 0x80 != 0 && attr & 0x03 == 0x03 {
                    let ep = addr & 0x0f;
                    return Some(Hid {
                        // boot 协议下键盘 MPS 8、鼠标 4；更大的是厂商自定义
                        is_kbd: mps >= 8,
                        ep,
                        iface,
                        mps,
                        interval,
                    });
                }
            }
            _ => {}
        }
        i += len;
    }
    None
}

/// 读设备 + 配置描述符，配好 HID 端点
fn enumerate_dev(b: u64, ls: bool) -> Result<(u16, u16, u8, Option<Hid>), &'static str> {
    let mut dev = [0u8; 18];
    let n = control(b, ls, [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0], 18, &mut dev)?;
    if n < 8 || dev[1] != 0x01 {
        return Err("设备描述符不合法");
    }
    let vid = if n >= 12 { u16::from_le_bytes([dev[8], dev[9]]) } else { 0 };
    let pid = if n >= 12 { u16::from_le_bytes([dev[10], dev[11]]) } else { 0 };
    let cls = if n >= 5 { dev[4] } else { 0 };

    set_address(b, ls, 1)?;

    // 先拿配置头 9 字节，按 wTotalLength 再读全量
    let mut head = [0u8; 9];
    let mut hid = None;
    if let Ok(h) = control(b, ls, [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 9, 0], 9, &mut head) {
        if h >= 9 {
            let total = u16::from_le_bytes([head[2], head[3]]).min(255) as usize;
            let mut cfg = [0u8; 256];
            if let Ok(c) = control(
                b,
                ls,
                [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, total as u8, (total >> 8) as u8],
                total as u16,
                &mut cfg,
            ) {
                if c >= 9 {
                    hid = parse_config(&cfg[..c]);
                }
            }
        }
    }

    set_config(b, ls, 1)?;
    if let Some(h) = hid {
        // boot 协议 + 不自动重复（SET_IDLE 0），失败不致命
        let _ = hid_req(b, ls, 0x0B, 1, h.iface);
        let _ = hid_req(b, ls, 0x0A, 0, h.iface);
    }
    Ok((vid, pid, cls, hid))
}

// ---------------- HID 报告 ----------------

// HID usage → Linux 键码（input.rs 用的是 Linux input event code）
static HID2LINUX: [u16; 67] = [
    0, 0,
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, // a..p
    16, 19, 31, 20, 22, 47, 17, 45, 21, 44, // q..z
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, // 1..0
    28, 1, 14, 15, 57, // enter/esc/backspace/tab/space
    12, 13, 26, 27, 43, 39, 40, 41, 51, 52, 53, // - = [ ] \ ; ' ` , . /
    58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, // F1..F11
    87, 88, // KP
];

fn kbd_report(rep: &[u8]) {
    // rep[0]=modifier, rep[1]=保留, rep[2..8]=最多 6 个按键
    let mut keys = [0u8; 6];
    keys.copy_from_slice(&rep[2..8]);
    let prev = unsafe { KBD_PREV };

    // 抬起：上一帧有、这一帧没有
    let mut i = 0;
    while i < 6 {
        let old = prev[i];
        if old != 0 && !keys.contains(&old) {
            let code = if (old as usize) < HID2LINUX.len() { HID2LINUX[old as usize] } else { 0 };
            if code != 0 {
                crate::input::inject_key(code, false);
                unsafe { EVENTS += 1 };
            }
        }
        i += 1;
    }
    // 按下：这一帧有、上一帧没有
    let mut i = 0;
    while i < 6 {
        let k = keys[i];
        if k != 0 && !prev.contains(&k) {
            let code = if (k as usize) < HID2LINUX.len() { HID2LINUX[k as usize] } else { 0 };
            // modifier 位单独处理：左 shift / 右 shift 已在 input 里区分
            if code != 0 {
                crate::input::inject_key(code, true);
                unsafe { EVENTS += 1 };
            }
        }
        i += 1;
    }

    // 修饰键：bit0=LCtrl bit1=LShift bit2=LAlt bit3=LGui bit4=RCtrl bit5=RShift
    let mods: [(u8, u16); 4] = [(1, 29), (2, 42), (4, 56), (8, 125)];
    let now = rep[0];
    let old = (unsafe { KBD_MOD }) & 0x0f;
    if now != old {
        let mut b = 0;
        while b < 4 {
            let (bit, code) = mods[b];
            let was = old & bit != 0;
            let is = now & bit != 0;
            if was != is {
                crate::input::inject_key(code, is);
                unsafe { EVENTS += 1 };
            }
            b += 1;
        }
        unsafe { KBD_MOD = now & 0x0f };
    }
    unsafe { KBD_PREV = keys };
}

fn mouse_report(rep: &[u8]) {
    // rep[0]=按键位 bit0 左/bit1 右/bit2 中, rep[1]=dx, rep[2]=dy, rep[3]=wheel
    let btn = rep[0] & 0x07;
    let old = unsafe { crate::input::usb_btn_state() };
    if (btn & 1) != (old & 1) {
        crate::input::inject_btn_any(0, btn & 1 != 0);
        unsafe { EVENTS += 1 };
    }
    if (btn & 2) != (old & 2) {
        crate::input::inject_btn_any(1, btn & 2 != 0);
        unsafe { EVENTS += 1 };
    }
    if (btn & 4) != (old & 4) {
        crate::input::inject_btn_any(2, btn & 4 != 0);
        unsafe { EVENTS += 1 };
    }
    crate::input::usb_set_btn(btn);

    let dx = rep[1] as i8 as i32;
    let dy = rep[2] as i8 as i32;
    if dx != 0 || dy != 0 {
        crate::input::inject_move_rel(dx, dy);
        unsafe { EVENTS += 1 };
    }
    // 滚轮（rep[3]）暂不接，UI 还没滚动列表
}

/// 轮询一次 HID 端点，解析报告并注入 input。返回这次处理的事件数（0/1/2…）。
pub fn poll_hid() -> u32 {
    let b = match base() {
        Some(b) => b,
        None => return 0,
    };
    let p = match unsafe { LAST } {
        Some(p) => p,
        None => return 0,
    };
    let h = match p.hid {
        Some(h) => h,
        None => return 0,
    };
    let mut buf = [0u8; 64];
    let n = match int_in(b, p.speed_ls, &h, &mut buf) {
        Ok(n) => n,
        Err(_) => return 0,
    };
    if n == 0 {
        return 0;
    }
    let before = unsafe { EVENTS };
    if h.is_kbd && n >= 8 {
        kbd_report(&buf[..8]);
    } else if !h.is_kbd && n >= 3 {
        mouse_report(&buf[..4.min(n)]);
    }
    unsafe { EVENTS }.saturating_sub(before)
}

// ---------------- 初始化 ----------------

pub fn init() -> Status {
    unsafe { STATUS = Status::NoController };
    let b = match base() {
        Some(b) => b,
        None => return Status::NoController,
    };
    // 地址上真有控制器？GSNPSID 是版本号，真芯片不为 0
    if rd(b, GSNPSID) == 0 && rd(b, GRSTCTL) == 0 {
        unsafe { STATUS = Status::NoResponse };
        return Status::NoResponse;
    }

    // 1. 核心软复位
    wr(b, GRSTCTL, rd(b, GRSTCTL) | 1);
    let mut t = 0;
    while rd(b, GRSTCTL) & 1 != 0 && t < 50 {
        spin(2);
        t += 1;
    }
    if rd(b, GRSTCTL) & 1 != 0 {
        unsafe { STATUS = Status::NoResponse };
        return Status::NoResponse;
    }

    // 2. 主机模式：FHMOD=1 / FDEV=0；TRDT=9
    let v = rd(b, GUSBCFG);
    wr(b, GUSBCFG, (v & !(1 << 30)) | (1 << 29) | (9 << 10));
    spin(10);

    // 3. 48MHz PHY（BCM2837 的 OTG 固定 48M，HCFG=0），关全局中断走轮询
    wr(b, HCFG, 0);
    wr(b, GAHBCFG, rd(b, GAHBCFG) & !1);

    // 4. 端口供电
    wr(b, HPRT0, rd(b, HPRT0) | HPRT_PWR);
    spin(20);

    // 5. 等连接（最多 300ms）
    let mut conn = false;
    let mut i = 0;
    while i < 30 {
        spin(10);
        if rd(b, HPRT0) & HPRT_CONN != 0 {
            conn = true;
            break;
        }
        i += 1;
    }

    let mut p = Probe {
        conn,
        enabled: false,
        speed_ls: false,
        vid: 0,
        pid: 0,
        dev_class: 0,
        hid: None,
    };

    if conn {
        // 6. 端口复位：置 10ms 再清，等控制器拉起 PrtEn
        wr(b, HPRT0, rd(b, HPRT0) | HPRT_RST);
        spin(10);
        wr(b, HPRT0, rd(b, HPRT0) & !HPRT_RST);
        let mut k = 0;
        while k < 30 {
            spin(5);
            let s = rd(b, HPRT0);
            if s & HPRT_ENA != 0 {
                p.enabled = true;
                p.speed_ls = s & HPRT_L1 != 0;
                break;
            }
            if s & HPRT_CONN == 0 {
                break;
            }
            k += 1;
        }

        if p.enabled {
            // 7. 枚举：地址 0 读描述符 → 分配地址 1 → 配置 → HID
            if let Ok((vid, pid, cls, hid)) = enumerate_dev(b, p.speed_ls) {
                p.vid = vid;
                p.pid = pid;
                p.dev_class = cls;
                p.hid = hid;
            }
        }
    }

    unsafe {
        LAST = Some(p);
        STATUS = if p.enabled { Status::Ready } else { Status::Idle };
    }
    unsafe { STATUS }
}

// ---------------- 命令行输出 ----------------

pub fn probe() {
    let b = match base() {
        Some(b) => b,
        None => {
            crate::println!("  USB  这块板子没有 DWC2 控制器");
            return;
        }
    };
    match unsafe { STATUS } {
        Status::NoController => {
            crate::println!("  USB  这块板子没有 DWC2 控制器");
            return;
        }
        Status::NoResponse => {
            crate::println!("  USB  {:#x} 上没有响应：GSNPSID 与 GRSTCTL 读回全 0", b);
            crate::println!("       QEMU 的 raspi 机器没给 USB 建模型，真机上这个地址是真的");
            return;
        }
        _ => {}
    }
    let p = match unsafe { LAST } {
        Some(p) => p,
        None => return,
    };
    crate::println!(
        "  USB  DWC2 @ {:#x}  端口 {}  {}  {}",
        b,
        if p.conn { "有设备" } else { "空闲" },
        if p.enabled { "已复位" } else { "未使能" },
        if !p.enabled { "—" } else if p.speed_ls { "低速" } else { "全速" }
    );
    if p.vid != 0 || p.pid != 0 {
        crate::println!("  VID {:#06x}  PID {:#06x}  class {:#04x}", p.vid, p.pid, p.dev_class);
    }
    match p.hid {
        Some(h) => crate::println!(
            "  HID  {}  EP{}  MPS {}  间隔 {} 帧",
            if h.is_kbd { "键盘（boot 8 字节）" } else { "鼠标（boot 4 字节）" },
            h.ep,
            h.mps,
            h.interval
        ),
        None => {
            if p.enabled {
                crate::println!("  HID  没找到 HID 接口（不是键鼠，或描述符读取失败）");
            }
        }
    }
    crate::println!("  事件累计 {}  轮询一次用 `usb poll`", unsafe { EVENTS });
}
