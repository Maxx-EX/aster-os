// Aster Kernel — 桌面刷新调度
//
// 只在两件事上重画：终端内容变了（并且距上次刷新超过 70ms），或者鼠标动了窗口。
// 光标是独立图层，移动不触发整屏重绘，只搬自己那块像素。
#![allow(static_mut_refs)]

use crate::{fb, input, timer};

const MIN_INTERVAL_MS: u64 = 70;

static mut LAST: u64 = 0;
static mut BLINK: bool = true;
static mut DRAG: usize = usize::MAX;
static mut OFF_X: i32 = 0;
static mut OFF_Y: i32 = 0;

pub fn tick() {
    if !fb::available() {
        return;
    }
    // bootframe 冻结期间不要重绘，否则一帧动画立刻被桌面盖掉
    if crate::bootanim::holding() {
        crate::cursor::update();
        return;
    }

    // 光标闪烁是唯一"没有输入也要重画"的理由：500 ms 一个相位，相位一变就整屏重绘
    let now0 = timer::now_ms();
    let blink = (now0 / 500) % 2 == 0;
    if blink != unsafe { BLINK } {
        unsafe {
            BLINK = blink;
            LAST = now0;
        }
        redraw();
        crate::cursor::invalidate();
    }

    let want = handle_mouse();
    let now = timer::now_ms();
    let dirty = crate::term::dirty();

    if (want || dirty) && now.saturating_sub(unsafe { LAST }) >= MIN_INTERVAL_MS {
        unsafe { LAST = now };
        redraw();
        crate::cursor::invalidate();
    }
    crate::cursor::update();
}

pub fn redraw() {
    crate::shell::render_dashboard();
}

/// 处理悬停、点击、拖拽，返回"需要重绘"
fn handle_mouse() -> bool {
    if !input::mouse_available() {
        return false;
    }
    let mut want = false;
    let (mx, my) = input::mouse_pos();

    // 侧栏悬停
    if fb::set_side_hover(fb::sidebar_item_at(mx, my)) {
        want = true;
    }

    while let Some((x, y, pressed)) = input::take_click() {
        if pressed {
            // 点窗口：抬到最上层并聚焦；落在标题栏上就开始拖
            if let Some(i) = fb::hit_top(x, y) {
                fb::raise(i);
                want = true;
                if fb::title_hit(i, y) {
                    let w = fb::win(i);
                    unsafe {
                        DRAG = i;
                        OFF_X = x - w.x;
                        OFF_Y = y - w.y;
                    }
                }
                continue;
            }
            // 点侧栏：切换选中并打开对应窗口
            if let Some(s) = fb::sidebar_item_at(x, y) {
                fb::set_side_active(s);
                let which = match s {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    _ => 1,
                };
                fb::show_win(which);
                want = true;
                continue;
            }
        } else {
            unsafe { DRAG = usize::MAX };
        }
    }

    if unsafe { DRAG } != usize::MAX {
        let i = unsafe { DRAG };
        let w = fb::win(i);
        let nx = (mx - unsafe { OFF_X }).clamp(80 - w.w as i32, 1024 - 80);
        let ny = (my - unsafe { OFF_Y }).clamp(40, 768 - 80);
        if nx != w.x || ny != w.y {
            fb::move_window(i, nx, ny);
            want = true;
        }
    }
    want
}

pub fn dragging() -> bool {
    (unsafe { DRAG }) != usize::MAX
}
