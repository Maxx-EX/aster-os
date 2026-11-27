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
    }
}

fn newline() {
    unsafe {
        ROW += 1;
        COL = 0;
        if ROW >= ROWS {
            for r in 1..ROWS {
                CELLS[r - 1] = CELLS[r];
            }
            for c in 0..COLS {
                CELLS[ROWS - 1][c] = 0;
            }
            ROW = ROWS - 1;
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
        DIRTY = true;
    }
}

/// 写一段文本（UTF-8）。串口那边每写一次就调一次这里。
pub fn write(s: &str) {
    for ch in s.chars() {
        match ch {
            '\n' => newline(),
            '\r' => unsafe { COL = 0 },
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
    unsafe {
        for r in 0..ROWS {
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
}
