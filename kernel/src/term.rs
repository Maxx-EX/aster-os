// Aster Kernel — 屏幕终端缓冲
//
// 串口的每一行输出同时记在这里，桌面上的终端窗口按格渲染。
// 每个格子存一个 u16：0 = 空，ASCII 存本值，汉字存 Unicode 码位，
// 0xFFFF = "宽字符第二格"（汉字占两格，用来保证中英混排行列对齐）。
//
// 行高从 16 提到 20，字与字之间留出呼吸；颜色按行首判定：
// 提示符吃强调色，`!!!` 开头的行红，`  [EX]` 开头的行黄，FAIL 红 / PASS 绿。
#![allow(static_mut_refs)]

pub const COLS: usize = 88;
pub const ROWS: usize = 22;
pub const LH: u32 = 20;
const WIDE_TAIL: u16 = 0xFFFF;

static mut CELLS: [[u16; COLS]; ROWS] = [[0; COLS]; ROWS];
static mut ROW: usize = 0;
static mut COL: usize = 0;
static mut DIRTY: bool = true;
static mut LAST_DRAW: u64 = 0;

/// 脏行范围（含端点），配合 PERF-3 的行级增量重绘。
/// 空区间用 D0 = ROWS（大于任何行）表示；ALL 为真时整屏都算脏（滚动、清屏、几何变化）。
static mut D0: usize = 0;
static mut D1: usize = ROWS - 1;
static mut ALL: bool = true;

fn mark_row(r: usize) {
    unsafe {
        if r < D0 {
            D0 = r;
        }
        if r > D1 {
            D1 = r;
        }
    }
}

/// 光标那两格也要跟着脏（光标是反相块，位置一变旧位置必须擦掉）
fn mark_cursor() {
    unsafe {
        mark_row(ROW);
    }
}

pub fn touch_cursor() {
    mark_cursor();
    unsafe {
        DIRTY = true;
    }
}

/// 取脏行区间并把区间清空
pub fn take_rows() -> Option<(usize, usize)> {
    unsafe {
        if ALL {
            ALL = false;
            D0 = ROWS;
            D1 = 0;
            return Some((0, ROWS - 1));
        }
        if D0 <= D1 {
            let r = (D0, D1);
            D0 = ROWS;
            D1 = 0;
            Some(r)
        } else {
            None
        }
    }
}

/// 看一眼脏行区间但不清空（PERF-5：多窗时决定能不能只补这几行）
pub fn pending_rows() -> Option<(usize, usize)> {
    unsafe {
        if ALL {
            return Some((0, ROWS - 1));
        }
        if D0 <= D1 {
            Some((D0, D1))
        } else {
            None
        }
    }
}

/// 请求一次整屏重绘（滚动、窗口位移、字号变化）
pub fn mark_full() {
    unsafe {
        ALL = true;
        DIRTY = true;
    }
}

pub fn clear() {
    unsafe {
        for r in 0..ROWS {
            for c in 0..COLS {
                CELLS[r][c] = 0;
            }
        }
        ROW = 0;
        COL = 0;
        DIRTY = true;
        ALL = true;
    }
}

fn newline() {
    unsafe {
        let from = ROW;
        ROW += 1;
        COL = 0;
        if ROW >= ROWS {
            // 整屏上滚一格：所有行都变了，只能整屏重画
            for r in 1..ROWS {
                CELLS[r - 1] = CELLS[r];
            }
            for c in 0..COLS {
                CELLS[ROWS - 1][c] = 0;
            }
            ROW = ROWS - 1;
            ALL = true;
        } else {
            mark_row(from);
            mark_row(ROW);
        }
        DIRTY = true;
    }
}

fn put(cell: u16) {
    unsafe {
        if COL >= COLS {
            newline();
        }
        CELLS[ROW][COL] = cell;
        COL += 1;
        let r = ROW;
        mark_row(r);
        DIRTY = true;
    }
}

/// 写一段文本（UTF-8）。串口那边每写一次就调一次这里。
pub fn write(s: &str) {
    for ch in s.chars() {
        match ch {
            '\n' => newline(),
            '\r' => unsafe { COL = 0 },
            '\u{8}' => {
                unsafe {
                    if COL > 0 {
                        COL -= 1;
                    }
                    CELLS[ROW][COL] = b' ' as u16;
                    DIRTY = true;
                }
            }
            '\t' => {
                for _ in 0..4 {
                    put(b' ' as u16);
                }
            }
            _ => {
                let cp = ch as u32;
                if cp < 0x80 {
                    put(cp as u16);
                } else if cp <= 0xFFFF {
                    if unsafe { COL } + 1 >= COLS {
                        newline();
                    }
                    unsafe {
                        CELLS[ROW][COL] = cp as u16;
                        CELLS[ROW][COL + 1] = WIDE_TAIL;
                        COL += 2;
                        DIRTY = true;
                    }
                } else {
                    put(b'?' as u16);
                }
            }
        }
    }
}

pub fn dirty() -> bool {
    unsafe { DIRTY }
}

pub fn mark_clean(now_ms: u64) {
    unsafe {
        DIRTY = false;
        LAST_DRAW = now_ms;
    }
}

pub fn last_draw() -> u64 {
    unsafe { LAST_DRAW }
}

/// 取一行开头若干格拼成 ASCII 前缀，用来做颜色判定；碰到汉字或空就停
fn row_prefix(r: usize, out: &mut [u8; 12]) -> usize {
    let mut n = 0;
    unsafe {
        for c in 0..COLS {
            let v = CELLS[r][c];
            if v == 0 || v == WIDE_TAIL {
                break;
            }
            if v >= 0x80 {
                break;
            }
            if n >= out.len() {
                break;
            }
            out[n] = v as u8;
            n += 1;
        }
    }
    n
}

fn starts(p: &[u8], pat: &[u8]) -> bool {
    p.len() >= pat.len() && &p[..pat.len()] == pat
}

fn contains(p: &[u8], pat: &[u8]) -> bool {
    if pat.len() > p.len() {
        return false;
    }
    for i in 0..=(p.len() - pat.len()) {
        if &p[i..i + pat.len()] == pat {
            return true;
        }
    }
    false
}

fn row_color(r: usize) -> u32 {
    let mut p = [0u8; 12];
    let n = row_prefix(r, &mut p);
    let p = &p[..n];
    if starts(p, b"!!!") {
        return crate::fb::ERR;
    }
    if n > 0 && starts(p, b" ") && contains(p, b"FAIL") {
        return crate::fb::ERR;
    }
    if contains(p, b"PASS") || contains(p, b"[OK]") || contains(p, b"FLASH OK") {
        return crate::fb::OK;
    }
    if contains(p, b"[EX]") {
        return crate::fb::WARN;
    }
    if contains(p, b"[NET]") || contains(p, b"[IN]") || contains(p, b"[BLK]") {
        return crate::fb::DIM;
    }
    crate::fb::TEXT
}

/// 渲染到 framebuffer 的 (x, y) 起点，行高 LH
pub fn render(x: u32, y: u32) {
    render_range(x, y, 0, ROWS - 1, true);
}

/// 只画 [r0, r1] 这几行（PERF-3 的增量路径）。`paint_bg` 时先擦该行底色。
pub fn render_range(x: u32, y: u32, r0: usize, r1: usize, paint_bg: bool) {
    let r0 = r0.min(ROWS - 1);
    let r1 = r1.min(ROWS - 1);
    if paint_bg {
        crate::fb::fill_rect(x, y + (r0 as u32) * LH, COLS as u32 * 8, ((r1 - r0 + 1) as u32) * LH, crate::fb::TERM_BG);
    }
    unsafe {
        for r in r0..=r1 {
            let base = row_color(r);
            for c in 0..COLS {
                let v = CELLS[r][c];
                if v == 0 || v == WIDE_TAIL {
                    continue;
                }
                let cx = x + (c as u32) * 8;
                let cy = y + (r as u32) * LH;
                let color = if c == 0 && (v == b'$' as u16 || v == b'#' as u16) {
                    crate::fb::ACCENT
                } else {
                    base
                };
                if v < 0x80 {
                    crate::fb::draw_char(cx, cy, v as u8 as char, color, 1);
                } else if let Some(ch) = core::char::from_u32(v as u32) {
                    crate::fb::draw_char(cx, cy, ch, color, 1);
                }
            }
        }
    }

    // 光标：画在下一个输入格上，500 ms 一个相位反相 —— 让人一眼看出"这里在等输入"
    let (r, c) = unsafe { (ROW, COL) };
    if r >= r0 && r <= r1 && c < COLS && (crate::timer::now_ms() / 500) % 2 == 0 {
        let cx = x + (c as u32) * 8;
        let cy = y + (r as u32) * LH + 2;
        crate::fb::fill_rect(cx, cy, 2, 14, crate::fb::TEXT);
    }
}
