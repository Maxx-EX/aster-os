// Aster Kernel — 屏幕软键盘（UI-9）
//
// 手机上没有硬件键盘，QEMU 前端也送不进 scancode。这里在屏幕上画一块键盘，
// 点击命中就把字符直接塞进 input 的字符环，shell 的 read_line 照样能读到。
// 只做 ASCII：命令、文件名、数字都用得上；中文输入留到后面（要 IME，另做）。

use crate::fb;

const PANEL_W: u32 = 500;
const PANEL_H: u32 = 270;
const KEY_W: u32 = 42;
const KEY_H: u32 = 34;
const GAP: u32 = 4;
const CHIP_Y: u32 = 26;
const CHIP_H: u32 = 24;
const ROWS_Y0: u32 = 58;

/// 一键命令：手机上敲 "dex big" 要点八次，这些 chip 点一下就跑
pub const CHIPS: &[&str] = &["help", "info", "dex run", "dex big", "apk", "wins", "clear"];

/// 命中结果：普通字符 / 一键命令
pub enum Key {
    Ch(u8),
    Chip(usize),
}

/// 特殊键的哨兵值（不会和 ASCII 冲突）
pub const K_BACKSPACE: u8 = 0xF7;
pub const K_ENTER: u8 = 0xF6;
pub const K_SPACE: u8 = 0xF5;
pub const K_CLOSE: u8 = 0xF4;
/// 历史翻页（read_line 认这两个伪键）
pub const K_UP: u8 = 0x90;
pub const K_DOWN: u8 = 0x91;

static mut OPEN: bool = false;
/// 每行：字符 + 显示文本。长度不齐的行各自缩进居中。
const ROW0: &[(u8, &str)] = &[
    (b'1', "1"), (b'2', "2"), (b'3', "3"), (b'4', "4"), (b'5', "5"),
    (b'6', "6"), (b'7', "7"), (b'8', "8"), (b'9', "9"), (b'0', "0"),
];
const ROW1: &[(u8, &str)] = &[
    (b'q', "q"), (b'w', "w"), (b'e', "e"), (b'r', "r"), (b't', "t"),
    (b'y', "y"), (b'u', "u"), (b'i', "i"), (b'o', "o"), (b'p', "p"),
];
const ROW2: &[(u8, &str)] = &[
    (b'a', "a"), (b's', "s"), (b'd', "d"), (b'f', "f"), (b'g', "g"),
    (b'h', "h"), (b'j', "j"), (b'k', "k"), (b'l', "l"),
];
const ROW3: &[(u8, &str)] = &[
    (b'z', "z"), (b'x', "x"), (b'c', "c"), (b'v', "v"), (b'b', "b"),
    (b'n', "n"), (b'm', "m"), (K_BACKSPACE, "DEL"),
];
const ROW4: &[(u8, &str)] = &[
    (K_UP, "UP"), (K_DOWN, "DN"), (K_SPACE, "SPACE"), (K_ENTER, "ENTER"), (K_CLOSE, "HIDE"),
];

pub fn open() -> bool {
    unsafe { OPEN }
}

pub fn set_open(v: bool) {
    unsafe { OPEN = v };
}

pub fn toggle() {
    unsafe { OPEN = !OPEN };
}

pub fn panel_pos() -> (u32, u32) {
    // 贴在屏幕右下：窗口是 748×546 居中，键盘落在它下面的空白里
    let sw = fb::width();
    let sh = fb::height();
    let x = if sw > PANEL_W + 24 { sw - PANEL_W - 12 } else { 0 };
    let y = if sh > PANEL_H + 24 { sh - PANEL_H - 12 } else { 0 };
    (x, y)
}

fn key_w(code: u8) -> u32 {
    if code == K_SPACE || code == K_ENTER {
        KEY_W * 2 + GAP
    } else {
        KEY_W
    }
}

fn row_geom(row: usize, items: &[(u8, &str)]) -> (u32, u32) {
    let (px, py) = panel_pos();
    let total: u32 = items.iter().map(|(c, _)| key_w(*c)).sum::<u32>()
        + GAP * items.len().saturating_sub(1) as u32;
    let x0 = px + (PANEL_W - total) / 2;
    let y = py + ROWS_Y0 + row as u32 * (KEY_H + GAP);
    (x0, y)
}

/// 第 row 行第 idx 个键的 (x, y, w)
fn key_rect(row: usize, idx: usize, items: &[(u8, &str)]) -> (u32, u32, u32) {
    let (x0, y) = row_geom(row, items);
    let mut x = x0;
    for (c, _) in items.iter().take(idx) {
        x += key_w(*c) + GAP;
    }
    (x, y, key_w(items[idx].0))
}

fn rows() -> [&'static [(u8, &'static str)]; 5] {
    [ROW0, ROW1, ROW2, ROW3, ROW4]
}

/// 命中测试：返回被点的键值
fn chip_rect(idx: usize) -> (u32, u32, u32) {
    let (px, py) = panel_pos();
    let total: u32 = CHIPS.iter().map(|c| chip_w(c)).sum::<u32>() + 4 * CHIPS.len() as u32 - 4;
    let x0 = px + (PANEL_W.saturating_sub(total)) / 2;
    let mut x = x0;
    for c in CHIPS.iter().take(idx) {
        x += chip_w(c) + 4;
    }
    (x, py + CHIP_Y, chip_w(CHIPS[idx]))
}

fn chip_w(label: &str) -> u32 {
    fb::text_len_ui(label) + 16
}

pub fn hit(mx: i32, my: i32) -> Option<Key> {
    if !open() {
        return None;
    }
    let (px, py) = panel_pos();
    if mx < px as i32 || my < py as i32
        || mx > (px + PANEL_W) as i32 || my > (py + PANEL_H) as i32
    {
        return None;
    }
    // 顶部 chip 行
    for i in 0..CHIPS.len() {
        let (x, y, w) = chip_rect(i);
        if mx >= x as i32 && mx <= (x + w) as i32
            && my >= y as i32 && my <= (y + CHIP_H) as i32
        {
            return Some(Key::Chip(i));
        }
    }
    let r = rows();
    for (i, row) in r.iter().enumerate() {
        let (_, y) = row_geom(i, row);
        if my < y as i32 || my > (y + KEY_H) as i32 {
            continue;
        }
        for j in 0..row.len() {
            let (x, _, w) = key_rect(i, j, row);
            if mx >= x as i32 && mx <= (x + w) as i32 {
                return Some(Key::Ch(row[j].0));
            }
        }
    }
    None
}

pub fn draw_if_open() {
    if !open() {
        return;
    }
    let (px, py) = panel_pos();
    // 面板背景 + 细边：和窗口同一套色板
    fb::rounded_rect_alpha(px as i32 - 6, py as i32 - 6, PANEL_W + 12, PANEL_H + 12, 12, 0x1a1d22, 232);
    fb::rounded_rect(px as i32, py as i32, PANEL_W, PANEL_H, 10, 0x232830);
    fb::frame_rect(px, py, PANEL_W, PANEL_H, 0x3a3f47);
    for i in 0..CHIPS.len() {
        let (x, y, w) = chip_rect(i);
        fb::rounded_rect(x as i32, y as i32, w, CHIP_H, 5, 0x2b3542);
        fb::frame_rect(x, y, w, CHIP_H, 0x3d4a5c);
        let tw = fb::text_len_ui(CHIPS[i]);
        fb::draw_text_ui(x + (w - tw) / 2, y + 4, CHIPS[i], 0x8fb6f0);
    }
    fb::draw_text_ui(px + 12, py + 9, "TOUCH KEYBOARD", 0x9aa1a9);
    fb::draw_text_ui(px + PANEL_W - 58, py + 9, "ASCII", 0x6a7179);

    let r = rows();
    for (i, row) in r.iter().enumerate() {
        for (j, (code, label)) in row.iter().enumerate() {
            let (x, y, w) = key_rect(i, j, row);
            let special = *code >= 0xF4;
            fb::rounded_rect(x as i32, y as i32, w, KEY_H, 6, if special { 0x333a45 } else { 0x2b3038 });
            fb::frame_rect(x, y, w, KEY_H, 0x3a3f47);
            let tw = fb::text_len_ui(label);
            let tx = x + (w.saturating_sub(tw)) / 2;
            let ty = y + 9;
            fb::draw_text_ui(tx, ty, label, if special { 0xd9a441 } else { 0xdadde2 });
        }
    }
}
