// Aster Kernel — 鼠标光标
//
// 光标不参与桌面重绘：桌面画面是"背景"，光标是叠在上面的一层。
// 移动时只做三件事——把旧位置压住的像素放回去、存下新位置压住的像素、画箭头。
// 这样鼠标移动不需要整屏重画，1024x768 一次全刷要几十毫秒，跟不动手。
#![allow(static_mut_refs)]

use crate::fb;

const FILL_MASK: [u16; 16] = [
    0x8000, 0xc000, 0xf000, 0xf800,
    0xfe00, 0xff00, 0xffc0, 0xffe0,
    0xfc00, 0xce00, 0x8e00, 0x0f00,
    0x0700, 0x0600, 0x0000, 0x0000,
];

const EDGE_MASK: [u16; 16] = [
    0x6000, 0x3800, 0x0c00, 0x0700,
    0x0180, 0x00e0, 0x0030, 0x0010,
    0x03f0, 0x3100, 0x7180, 0xd080,
    0x1880, 0x0980, 0x0f00, 0x0000,
];

static mut X: i32 = 760;
static mut Y: i32 = 600;
static mut SAVED: [[u32; 16]; 16] = [[0; 16]; 16];
static mut VISIBLE: bool = false;

/// 桌面整屏重绘之后必须调一次，否则光标会被画面盖住却仍以为自己在上面
pub fn invalidate() {
    unsafe { VISIBLE = false };
}

pub fn visible() -> bool {
    unsafe { VISIBLE }
}

fn inside(x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && x < fb::width() as i32 && y < fb::height() as i32
}

fn save() {
    unsafe {
        for r in 0..16 {
            for c in 0..16 {
                let x = X + c as i32;
                let y = Y + r as i32;
                SAVED[r][c] = if inside(x, y) { fb::read_px(x as u32, y as u32) } else { 0 };
            }
        }
    }
}

fn restore() {
    unsafe {
        for r in 0..16 {
            for c in 0..16 {
                let x = X + c as i32;
                let y = Y + r as i32;
                if inside(x, y) {
                    fb::write_px(x as u32, y as u32, SAVED[r][c]);
                }
            }
        }
    }
}

fn paint() {
    unsafe {
        for r in 0..16usize {
            let er = EDGE_MASK[r];
            let fr = FILL_MASK[r];
            if er == 0 && fr == 0 {
                continue;
            }
            for c in 0..16usize {
                let bit = 0x8000u16 >> c;
                let x = X + c as i32;
                let y = Y + r as i32;
                if x < 0 || y < 0 {
                    continue;
                }
                if er & bit != 0 {
                    fb::write_px(x as u32, y as u32, 0x0a0e14);
                } else if fr & bit != 0 {
                    fb::write_px(x as u32, y as u32, 0xf2f6ff);
                }
            }
        }
    }
}

/// 跟随鼠标位置重画。返回是否真的动了。
static mut LAST_FRAME: u32 = 0;

pub fn update() -> bool {
    if !crate::input::mouse_available() {
        return false;
    }
    let (mx, my) = crate::input::mouse_pos();
    let frame = fb::frame();
    unsafe {
        // 位置没动、底图也没重画过，才认为屏幕上的箭头仍然有效
        if VISIBLE && mx == X && my == Y && frame == LAST_FRAME {
            return false;
        }
        let ox = X;
        let oy = Y;
        if VISIBLE {
            restore();
        }
        X = mx;
        Y = my;
        save();
        paint();
        VISIBLE = true;
        LAST_FRAME = frame;
        // 旧位置和新位置合成一块矩形推上去，别为了 16x16 的光标搬整个 3 MiB
        let x0 = ox.min(X).max(0) as u32;
        let y0 = oy.min(Y).max(0) as u32;
        let x1 = (ox.max(X) + 18).max(0) as u32;
        let y1 = (oy.max(Y) + 18).max(0) as u32;
        fb::present();
    }
    true
}
