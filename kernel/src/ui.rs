// Aster Kernel — 桌面刷新调度
//
// 界面不是每帧都重画（全屏重绘几十毫秒），只在两件事发生时重画：
//   1. 终端内容变了（并且距上次刷新超过 60ms）
//   2. 鼠标把窗口拖动了
// 光标自己是个独立图层，移动不触发重绘，只搬自己那块像素。
#![allow(static_mut_refs)]

use crate::{fb, input, timer};

const MIN_INTERVAL_MS: u64 = 60;

const TITLE_H: i32 = 38;

static mut LAST: u64 = 0;
static mut DRAGGING: bool = false;
static mut OFF_X: i32 = 0;
static mut OFF_Y: i32 = 0;

pub fn tick() {
    if !fb::available() {
        return;
    }
    let want = handle_mouse();
    let now = timer::now_ms();
    let dirty = crate::term::dirty();

    if (want || dirty) && now.saturating_sub(unsafe { LAST }) >= MIN_INTERVAL_MS {
        unsafe { LAST = now };
        redraw();
        // 整屏重画把光标压在下面了，重新贴一次
        crate::cursor::invalidate();
    }
    crate::cursor::update();
}

pub fn redraw() {
    crate::shell::render_dashboard();
}

/// 处理点击与拖拽，返回"需要重绘"
fn handle_mouse() -> bool {
    if !input::mouse_available() {
        return false;
    }
    let mut want = false;

    // 侧栏悬停：鼠标停在哪一项，就把哪一项的背景点亮
    let (mx, my) = input::mouse_pos();
    if fb::set_side_hover(fb::sidebar_item_at(mx, my)) {
        want = true;
    }

    while let Some((x, y, pressed)) = input::take_click() {
        if pressed {
            // 点侧栏 → 切换选中项
            if let Some(i) = fb::sidebar_item_at(x, y) {
                fb::set_side_active(i);
                want = true;
                continue;
            }
            let (wx, wy) = fb::win_pos();
            // 点在标题栏（不用拖边框，整条都能拖）
            if x >= wx && x < wx + 748 && y >= wy && y < wy + TITLE_H {
                unsafe {
                    DRAGGING = true;
                    OFF_X = x - wx;
                    OFF_Y = y - wy;
                }
            }
        } else {
            unsafe { DRAGGING = false };
        }
    }

    if unsafe { DRAGGING } {
        let (mx, my) = input::mouse_pos();
        let (wx, wy) = fb::win_pos();
        let nx = (mx - unsafe { OFF_X }).clamp(-360, 268);
        let ny = (my - unsafe { OFF_Y }).clamp(40, 212);
        if nx != wx || ny != wy {
            // 只重绘窗口扫过的那块，不整屏重画
            fb::move_window_to(nx, ny);
        }
    }
    want
}

pub fn dragging() -> bool {
    unsafe { DRAGGING }
}
