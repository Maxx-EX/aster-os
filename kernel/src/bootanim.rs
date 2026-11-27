// Aster Kernel — 开机动画（星芒点亮 → 散去）
//
// 设计原则：只有一件事在动。深空底色、中心一点光、六个尖依次长出来、
// 全部亮起后缓慢自转、系统名淡入，最后整束光放大散开。
// 不做网格、不做粒子雨、不做圆环叠加 —— 越克制越像"系统"，而不是一个特效。
//
// 时间轴（2400 ms）：
//   0–200      黑场里亮起一个点
//   200–1100   六个尖依次从中心长出（每个错开 90 ms），尖端细、根部略宽
//   1100–1600  完整星芒缓慢自转
//   1600–2100  系统名与副标题淡入
//   2100–2400  星芒放大散开 + 整屏过渡到桌面底色
#![allow(static_mut_refs)]

use crate::{fb, timer};

/// sin(0..90°) × 1000
static SIN: [i32; 91] = [0, 17, 35, 52, 70, 87, 105, 122, 139, 156, 174, 191, 208, 225, 242, 259, 276, 292, 309, 326, 342, 358, 375, 391, 407, 423, 438, 454, 469, 485, 500, 515, 530, 545, 559, 574, 588, 602, 616, 629, 643, 656, 669, 682, 695, 707, 719, 731, 743, 755, 766, 777, 788, 799, 809, 819, 829, 839, 848, 857, 866, 875, 883, 891, 899, 906, 914, 921, 927, 934, 940, 946, 951, 956, 961, 966, 970, 974, 978, 982, 985, 988, 990, 993, 995, 996, 998, 999, 999, 1000, 1000];

fn sin_deg(d: i32) -> i32 {
    let mut d = d % 360;
    if d < 0 {
        d += 360;
    }
    match d {
        0..=90 => SIN[d as usize],
        91..=180 => SIN[(180 - d) as usize],
        181..=270 => -SIN[(d - 180) as usize],
        _ => -SIN[(360 - d) as usize],
    }
}

fn cos_deg(d: i32) -> i32 {
    sin_deg(d + 90)
}

fn mix(bg: u32, fg: u32, a: u32) -> u32 {
    let a = a.min(255);
    let r = (((bg >> 16) & 0xff) * (255 - a) + ((fg >> 16) & 0xff) * a) / 255;
    let g = (((bg >> 8) & 0xff) * (255 - a) + ((fg >> 8) & 0xff) * a) / 255;
    let b = ((bg & 0xff) * (255 - a) + (fg & 0xff) * a) / 255;
    (r << 16) | (g << 8) | b
}

/// 扫描线填三角形，边缘 1 px 混合
fn tri(ax: i32, ay: i32, bx: i32, by: i32, cx: i32, cy: i32, color: u32, a: u32) {
    let ymin = ay.min(by).min(cy);
    let ymax = ay.max(by).max(cy);
    let mut y = ymin;
    while y <= ymax {
        let yc = y + 1;
        let mut xs = [0i32; 3];
        let mut n = 0usize;
        for (x1, y1, x2, y2) in [(ax, ay, bx, by), (bx, by, cx, cy), (cx, cy, ax, ay)] {
            if y1 == y2 {
                continue;
            }
            let (lo, hi, xlo, xhi) = if y1 < y2 { (y1, y2, x1, x2) } else { (y2, y1, x2, x1) };
            if yc >= lo && yc < hi {
                let x = xlo + (xhi - xlo) * (yc - lo) / (hi - lo);
                if n < 3 {
                    xs[n] = x;
                    n += 1;
                }
            }
        }
        if n >= 2 {
            let mut lo = xs[0];
            let mut hi = xs[0];
            let mut k = 1;
            while k < n {
                lo = lo.min(xs[k]);
                hi = hi.max(xs[k]);
                k += 1;
            }
            let mut x = lo;
            while x <= hi {
                if x >= 0 {
                    fb::blend_px(x as u32, y as u32, color, a);
                }
                x += 1;
            }
        }
        y += 1;
    }
}

/// 单根光针：根部在中心，尖端在 r，中段略宽、尖端极细
fn blade(cx: i32, cy: i32, ang: i32, r: i32, color: u32, a: u32) {
    let c = cos_deg(ang);
    let s = sin_deg(ang);
    let nx = -s;
    let ny = c;
    let w_tip = (r / 34).max(1);
    let w_mid = (r / 13).max(2);
    let tipx = cx + r * c / 1000;
    let tipy = cy + r * s / 1000;
    let mx = cx + r * 55 / 100 * c / 1000;
    let my = cy + r * 55 / 100 * s / 1000;
    let t1x = tipx + w_tip * nx / 1000;
    let t1y = tipy + w_tip * ny / 1000;
    let t2x = tipx - w_tip * nx / 1000;
    let t2y = tipy - w_tip * ny / 1000;
    let m1x = mx + w_mid * nx / 1000;
    let m1y = my + w_mid * ny / 1000;
    let m2x = mx - w_mid * nx / 1000;
    let m2y = my - w_mid * ny / 1000;
    tri(cx, cy, m1x, m1y, t1x, t1y, color, a);
    tri(cx, cy, t1x, t1y, t2x, t2y, color, a);
    tri(cx, cy, t2x, t2y, m2x, m2y, color, a);
}

fn grad(top: u32, bot: u32, y: i32, y0: i32, h: i32) -> u32 {
    let i = (y - y0).clamp(0, h);
    mix(top, bot, (i * 255 / h.max(1)) as u32)
}

fn base_layer(w: u32, h: u32) {
    fb::fill_rect(0, 0, w, h, 0x000000);
    fb::vgrad(0, 0, w, h, 0x080b10, 0x121419);
}

/// 画某一时刻的画面（el 毫秒）。play 和 bootframe 共用这一份，保证两者一致
fn frame(el: i32, w: u32, h: u32, cx: i32, cy: i32, bs: i32) {
    let bx = cx - bs / 2;
    let by = cy - bs / 2;

    fb::vgrad(
        bx as u32,
        by as u32,
        bs as u32,
        bs as u32,
        grad(0x080b10, 0x121419, by, by, bs),
        grad(0x080b10, 0x121419, by + bs, by, bs),
    );
    // 只留贴身的一小团光雾：大半径同心圆会有可见台阶，反而掉质感
    fb::circle(cx, cy, 64, 0x456fb0, 7);
    fb::circle(cx, cy, 40, 0x5a86c8, 12);

    // 六个尖依次生长
    let mut k = 0;
    while k < 6 {
        let t_k = ((el - 200 - k * 90).clamp(0, 640) as i32) * 1000 / 640;
        if t_k > 0 {
            let ease = 1000 - (1000 - t_k) * (1000 - t_k) / 1000;
            let rk = 6 + 126 * ease / 1000;
            let ang = k * 60 + el * 22 / 1000;
            blade(cx, cy, ang, rk * 108 / 100, 0x6f9cf0, 55);
            blade(cx, cy, ang, rk, fb::ACCENT, 178);
            blade(cx, cy, ang, rk, 0xdceaff, 42);
            // 尖端一小段提亮，让"光"有方向
            if rk > 40 {
                blade(cx, cy, ang, rk, 0xf4f8ff, 46);
            }
        }
        k += 1;
    }

    // 中心光核
    let core = if el < 200 { (el * 5 / 200).max(1) } else { 5 };
    fb::circle(cx, cy, (core + 26) as u32, 0x9dc0ff, 22);
    fb::circle(cx, cy, (core + 10) as u32, 0xcfdfff, 105);
    fb::circle(cx, cy, core as u32, 0xffffff, 245);

    // 散开
    if el > 2100 {
        let s = (el - 2100).clamp(0, 240) as i32 * 1000 / 240;
        let r2 = 132 + 150 * s / 1000;
        let a2 = (175 - 175 * s / 1000) as u32;
        if a2 > 3 {
            let mut k = 0;
            while k < 6 {
                blade(cx, cy, k * 60 + el * 22 / 1000, r2, fb::ACCENT, a2);
                k += 1;
            }
        }
    }

    // 系统名：下方纯色区，按渐变背景精确混色
    let t_a1 = ((el - 1600).clamp(0, 360) as i32 * 255 / 360) as u32;
    let t_a2 = ((el - 1820).clamp(0, 360) as i32 * 255 / 360) as u32;
    if t_a1 > 3 {
        let ty = (h as i32 - 150) as u32;
        let bg1 = grad(0x080b10, 0x121419, ty as i32, 0, h as i32);
        let name = "Aster";
        let tw = fb::text_len(name, 2);
        let tx = (w - tw) / 2;
        fb::draw_text(tx, ty, name, mix(bg1, 0xf4f7ff, t_a1), 2);

        let sub = "星 芒 · 原 创 内 核 · A R M 6 4";
        let sw = fb::text_len_ui(sub);
        let sx = (w - sw) / 2;
        let bg2 = grad(0x080b10, 0x121419, (ty + 44) as i32, 0, h as i32);
        fb::draw_text_ui(sx, ty + 44, sub, mix(bg2, 0xaab6c8, t_a2 * 7 / 10));
    }

    if el > 2280 {
        let k = ((el - 2280) * 255 / 120).min(255) as u32;
        fb::blend_rect(0, 0, w, h, 0x1a1d22, k);
    }
}

static mut HOLD: bool = false;

/// 冻结桌面刷新：bootframe 画完一帧后停在那儿，直到下一条命令
pub fn set_hold(b: bool) {
    unsafe { HOLD = b };
}

pub fn holding() -> bool {
    unsafe { HOLD }
}

/// 把某一时刻静态画到屏幕上（调试 / 取证用）
pub fn still(ms: i32) {
    if !fb::available() {
        return;
    }
    let w = fb::width();
    let h = fb::height();
    let cx = w as i32 / 2;
    let cy = h as i32 / 2 - 40;
    base_layer(w, h);
    frame(ms.clamp(0, 2399), w, h, cx, cy, 600);
    unsafe { HOLD = true };
}

pub fn play() {
    if !fb::available() {
        return;
    }
    let w = fb::width();
    let h = fb::height();
    let cx = w as i32 / 2;
    let cy = h as i32 / 2 - 40;

    base_layer(w, h);

    let t0 = timer::now_ms();
    loop {
        let el = timer::now_ms().saturating_sub(t0) as i32;
        if el >= 2400 {
            break;
        }
        frame(el, w, h, cx, cy, 600);
    }
    fb::fill_rect(0, 0, w, h, 0x1a1d22);
}
