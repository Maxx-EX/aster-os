//! APK 画布：dex 里的 `Lcom/aster/Canvas;` 在这里落地。
//!
//! 不分配位图，只记"绘制命令"，重绘时按 2 倍缩放画到屏幕中央。
//! 这样 APK 的输出和桌面 UI 共存：桌面照常重绘，画布盖在上面一层。
//! 真要做 `Canvas` 的语义（读回像素、离屏合成、路径填充），得先有
//! 一块按需分配的位图，那是后面的事。

pub const CW: i32 = 320;
pub const CH: i32 = 200;

const MAXC: usize = 128;
const MAXSTR: usize = 32;
const STRMAX: usize = 48;
const SCALE: u32 = 2;

#[derive(Clone, Copy)]
struct Cmd {
    kind: u8, // 0 = rect, 1 = text
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: u32,
    slot: usize,
}
const EMPTY: Cmd = Cmd { kind: 0, x: 0, y: 0, w: 0, h: 0, color: 0, slot: 0 };

static mut CMDS: [Cmd; MAXC] = [EMPTY; MAXC];
static mut NC: usize = 0;
static mut STRS: [[u8; STRMAX]; MAXSTR] = [[0; STRMAX]; MAXSTR];
static mut STRL: [usize; MAXSTR] = [0; MAXSTR];
static mut NS: usize = 0;
static mut COLOR: u32 = 0xd8dde2;
static mut BG: u32 = 0x141619;
static mut ACTIVE: bool = false;
static mut DROPPED: usize = 0;

pub fn reset() {
    unsafe {
        NC = 0;
        NS = 0;
        COLOR = 0xd8dde2;
        BG = 0x141619;
        DROPPED = 0;
    }
}

pub fn set_color(c: u32) {
    unsafe {
        COLOR = c;
    }
}

/// `clear(c)`：换底色并清掉已有命令
pub fn clear(c: u32) {
    unsafe {
        BG = c;
        NC = 0;
    }
}

pub fn set_active(b: bool) {
    unsafe {
        ACTIVE = b;
    }
}

pub fn active() -> bool {
    unsafe { ACTIVE }
}

pub fn rect(x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        if NC >= MAXC {
            DROPPED += 1;
            return;
        }
        CMDS[NC] = Cmd { kind: 0, x, y, w, h, color: COLOR, slot: 0 };
        NC += 1;
    }
}

pub fn text(x: i32, y: i32, s: &str) {
    unsafe {
        if NC >= MAXC || NS >= MAXSTR {
            DROPPED += 1;
            return;
        }
        let slot = NS;
        let n = s.len().min(STRMAX);
        let mut i = 0;
        while i < n {
            STRS[slot][i] = s.as_bytes()[i];
            i += 1;
        }
        STRL[slot] = n;
        NS += 1;
        CMDS[NC] = Cmd { kind: 1, x, y, w: 0, h: 0, color: COLOR, slot };
        NC += 1;
    }
}

/// 重绘时叠加画布（`shell::render_dashboard` 末尾调用）
pub fn draw_if_active() {
    if !unsafe { ACTIVE } {
        return;
    }
    if !crate::fb::available() {
        return;
    }
    let w = CW * SCALE as i32;
    let h = CH * SCALE as i32;
    let (ox, oy) = origin();

    // 外框 + 底色
    crate::fb::fill_rect((ox - 8) as u32, (oy - 8) as u32, (w + 16) as u32, (h + 16) as u32, 0x1b1e23);
    crate::fb::frame_rect((ox - 8) as u32, (oy - 8) as u32, (w + 16) as u32, (h + 16) as u32, 0x3a3f47);
    crate::fb::fill_rect(ox as u32, oy as u32, w as u32, h as u32, unsafe { BG });

    let n = unsafe { NC };
    let mut i = 0;
    while i < n {
        let c = unsafe { CMDS[i] };
        if c.x >= 0 && c.y >= 0 && c.x < CW && c.y < CH {
            if c.kind == 0 {
                let rw = c.w.clamp(0, CW - c.x) as u32;
                let rh = c.h.clamp(0, CH - c.y) as u32;
                if rw > 0 && rh > 0 {
                    crate::fb::fill_rect(
                        (ox + c.x * SCALE as i32) as u32,
                        (oy + c.y * SCALE as i32) as u32,
                        rw * SCALE,
                        rh * SCALE,
                        c.color,
                    );
                }
            } else {
                let len = unsafe { STRL[c.slot] };
                let bytes: &[u8] = unsafe { &STRS[c.slot][..len] };
                if let Ok(s) = core::str::from_utf8(bytes) {
                    crate::fb::draw_text(
                        (ox + c.x * SCALE as i32) as u32,
                        (oy + c.y * SCALE as i32) as u32,
                        s,
                        c.color,
                        SCALE,
                    );
                }
            }
        }
        i += 1;
    }

    // 左下角小标记，说明这块是 APK 画的
    crate::fb::draw_text_ui(
        (ox) as u32,
        (oy + h + 12) as u32,
        "APK Canvas 320x200",
        0x6a7179,
    );
}

// —— 触摸（APK-10/11/12）——
// 一条事件队列，每条记 (x, y, kind)：0=按下 1=拖动 2=抬起。
// APK 两种取法：老的 touchX()/moveX() 按 kind 过滤，新的 next_event() 按 FIFO 全取。
const MAXEV: usize = 16;
static mut EV: [(i32, i32, u8); MAXEV] = [(0, 0, 0); MAXEV];
static mut EVN: usize = 0;
static mut LASTT: (i32, i32) = (0, 0);
static mut LASTMV: (i32, i32) = (0, 0);

const K_DOWN: u8 = 0;
const K_MOVE: u8 = 1;
const K_UP: u8 = 2;

fn origin() -> (i32, i32) {
    let sw = crate::fb::width() as i32;
    let sh = crate::fb::height() as i32;
    ((sw - CW * SCALE as i32) / 2, (sh - CH * SCALE as i32) / 2)
}

/// 屏幕坐标 → 画布坐标；落在画布外返回 None
pub fn screen_to_canvas(sx: i32, sy: i32) -> Option<(i32, i32)> {
    let (ox, oy) = origin();
    let cx = (sx - ox) / SCALE as i32;
    let cy = (sy - oy) / SCALE as i32;
    if cx >= 0 && cy >= 0 && cx < CW && cy < CH {
        Some((cx, cy))
    } else {
        None
    }
}

fn push(kind: u8, x: i32, y: i32) {
    unsafe {
        if EVN >= MAXEV {
            // 满了丢最老的：拖动是连续状态，留新的更合理
            let mut i = 0;
            while i + 1 < EVN {
                EV[i] = EV[i + 1];
                i += 1;
            }
            EVN -= 1;
        }
        EV[EVN] = (x, y, kind);
        EVN += 1;
    }
}

fn remove_at(i: usize) {
    unsafe {
        let mut j = i;
        while j + 1 < EVN {
            EV[j] = EV[j + 1];
            j += 1;
        }
        EVN -= 1;
    }
}

pub fn push_touch(x: i32, y: i32, down: bool) {
    push(if down { K_DOWN } else { K_UP }, x, y);
}

pub fn push_move(x: i32, y: i32) {
    push(K_MOVE, x, y);
}

/// FIFO 取一条（onTouch 分发用）
pub fn next_event() -> Option<(u8, i32, i32)> {
    unsafe {
        if EVN == 0 {
            return None;
        }
        let (x, y, k) = EV[0];
        remove_at(0);
        if k == K_DOWN || k == K_MOVE {
            LASTT = (x, y);
        }
        Some((k, x, y))
    }
}

/// 取第一条按下事件（老 API）
pub fn take_touch() -> Option<(i32, i32)> {
    unsafe {
        let mut i = 0;
        while i < EVN {
            let (x, y, k) = EV[i];
            if k == K_DOWN {
                remove_at(i);
                LASTT = (x, y);
                return Some((x, y));
            }
            i += 1;
        }
        None
    }
}

/// 取第一条拖动事件（老 API）
pub fn take_move() -> Option<(i32, i32)> {
    unsafe {
        let mut i = 0;
        while i < EVN {
            let (x, y, k) = EV[i];
            if k == K_MOVE {
                remove_at(i);
                LASTMV = (x, y);
                return Some((x, y));
            }
            i += 1;
        }
        None
    }
}

pub fn last_touch() -> (i32, i32) {
    unsafe { LASTT }
}

pub fn last_move() -> (i32, i32) {
    unsafe { LASTMV }
}

pub fn stats() -> (usize, usize) {
    unsafe { (NC, DROPPED) }
}
