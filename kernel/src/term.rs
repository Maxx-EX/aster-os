// Aster Kernel — 屏幕终端缓冲
//
// 串口的每一行输出同时记在这里，桌面上的终端窗口按格渲染。
// 每个格子存一个 u16：0 = 空，ASCII 存本值，汉字存 Unicode 码位，
// 0xFFFF = "宽字符第二格"（汉字占两格，用来保证中英混排行列对齐）。
#![allow(static_mut_refs)]

pub const COLS: usize = 88;
pub const ROWS: usize = 26;
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
            // 整体上滚一行，底部清空
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
                    // 汉字占两格：若当前只剩最后一列，先换行，避免断字
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

/// 渲染到 framebuffer 的 (x, y) 起点
pub fn render(x: u32, y: u32, color: u32) {
    unsafe {
        for r in 0..ROWS {
            for c in 0..COLS {
                let v = CELLS[r][c];
                if v == 0 || v == WIDE_TAIL {
                    continue;
                }
                let cx = x + (c as u32) * 8;
                let cy = y + (r as u32) * 16;
                if v < 0x80 {
                    crate::fb::draw_char(cx, cy, v as u8 as char, color, 1);
                } else if let Some(ch) = core::char::from_u32(v as u32) {
                    crate::fb::draw_char(cx, cy, ch, color, 1);
                }
            }
        }
    }
}
