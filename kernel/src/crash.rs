// Aster Kernel — 崩溃屏
//
// 内核 panic 和 EL1 致命异常最后都落到这里，做两件事：
//
//   1. 串口先把证据全打出来（没有屏、或者屏初始化之前崩了，也有据可查）；
//   2. 有显示设备时画一张屏：出事的地址、出错原因、访问地址、
//      以及"这台机器已经停了"这句话。
//
// 用户态任务崩了不走这里 —— 内核会把那个任务杀掉继续跑（exceptions.rs
// 里的用户态故障分支），一张全屏红字反而会挡住还活着的窗口。
use crate::fb;

const C_BG: u32 = 0x120609;
const C_BG2: u32 = 0x2A0D10;
const C_BAND: u32 = 0xD93A3F;
const C_TXT: u32 = 0xF4EAE6;
const C_DIM: u32 = 0x9A8683;
const C_ACC: u32 = 0xFF7A7A;
const C_LINE: u32 = 0x3E2224;

/// 64 位值写成 `0x0000_0000_0000_0000` 的定宽形式（ASCII，定长 18）
fn hex64(v: u64, buf: &mut [u8; 18]) {
    buf[0] = b'0';
    buf[1] = b'x';
    for i in 0..16 {
        let d = ((v >> ((15 - i) * 4)) & 0xF) as u8;
        buf[2 + i] = if d < 10 { b'0' + d } else { b'A' + d - 10 };
    }
}

fn hex_str(v: u64, buf: &mut [u8; 18]) -> &str {
    hex64(v, buf);
    core::str::from_utf8(&buf[..]).unwrap_or("0x?")
}

/// 画崩溃屏。必须在 fb 已经初始化、且没有其他线程在画的时候调用。
fn paint(title: &str, subtitle: &str, note: &str, rows: &[(&str, u64)]) {
    let w = fb::width();
    let h = fb::height();
    if w < 320 || h < 200 {
        return;
    }

    fb::fill_rect(0, 0, w, h, C_BG);
    fb::vgrad(0, 0, w, h / 3, C_BG2, C_BG);
    fb::fill_rect(0, 0, w, 6, C_BAND);
    fb::fill_rect(0, 0, 3, h, C_BAND);

    // 大字标题，居中；屏太窄就靠左
    let s = 5u32;
    let glyph_h = crate::font::ASCII_H as u32 * s;
    let tw = fb::text_len(title, s);
    let tx = if w > tw + 48 { (w - tw) / 2 } else { 24 };
    let ty = h / 6;
    fb::draw_text(tx, ty, title, C_TXT, s);

    // 标题底下那条红线：紧贴字形底部，不压字
    let uy = ty + glyph_h + 12;
    fb::fill_rect(tx, uy, tw.min(w.saturating_sub(tx)), 2, C_BAND);

    // 副标题（汉字 16px，靠左对齐标题）
    let sub_y = uy + 14;
    fb::draw_text(tx, sub_y, subtitle, C_ACC, 1);

    // 证据行：标签 16px 灰、值 16px 白，下面一条细线
    let x0 = tx.max(24);
    let label_w = 8 * 2 * 7; // 标签最多 7 个 ASCII 字符（scale 2）
    let mut y = sub_y + 16 + 40;
    for (k, v) in rows {
        if y + 40 > h - 70 {
            break;
        }
        let mut b = [0u8; 18];
        fb::draw_text(x0, y, k, C_DIM, 2);
        fb::draw_text(x0 + label_w + 16, y, hex_str(*v, &mut b), C_TXT, 2);
        fb::fill_rect(x0, y + 32, w.saturating_sub(x0 * 2), 1, C_LINE);
        y += 40;
    }

    // 底部：一句人话 + 出处
    let foot_y = h - 56;
    fb::fill_rect(0, foot_y - 14, w, 1, C_LINE);
    fb::draw_text(x0, foot_y, "串口已留下完整现场 · 复位后可再次开机", C_ACC, 1);
    if !note.is_empty() {
        fb::draw_text(x0, foot_y + 22, note, C_DIM, 1);
    }
    let brand = "ASTER 0.9 · MAXX-EX";
    let bw = fb::text_len(brand, 1);
    if w > bw + 40 {
        fb::draw_text(w - bw - 24, foot_y, brand, C_DIM, 1);
    }

    fb::present();
}

/// 停机并出证据。`rows` 是 (标签, 值) 列表，串口按 `{:#018x}` 打，屏幕按定宽打。
pub fn show(title: &str, subtitle: &str, note: &str, rows: &[(&str, u64)]) -> ! {
    crate::println!();
    crate::println!("[CRASH] {}", title);
    for (k, v) in rows {
        crate::println!("  {:<8}: {:#018x}", k, v);
    }
    if !note.is_empty() {
        crate::println!("  {:<8}: {}", "NOTE", note);
    }
    if fb::available() {
        paint(title, subtitle, note, rows);
        crate::println!("[CRASH] 崩溃屏已绘制 {}x{}", fb::width(), fb::height());
    } else {
        crate::println!("[CRASH] 没有显示设备，证据只在串口");
    }
    crate::println!("  系统已停机，需要复位。");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
