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
static mut LASTM: (i32, i32) = (-1, -1);
static mut APP_MS: u64 = 0;
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

    let mut want = handle_mouse();

    // APK 帧回调：30ms 一拍，交给 dex 跑 Main.frame()V，跑完就重绘
    if crate::shell::app_active() {
        let now = timer::now_ms();
        if now.saturating_sub(unsafe { APP_MS }) >= 30 {
            unsafe { APP_MS = now };
            if crate::shell::app_frame().is_some() {
                want = true;
            }
        }
    }
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
        // 画布优先：落在 APK 画布上的点不给桌面（否则会当成点窗口）
        if crate::apkcanvas::active() {
            if let Some((cx, cy)) = crate::apkcanvas::screen_to_canvas(x, y) {
                crate::apkcanvas::push_touch(cx, cy, pressed);
                crate::cursor::invalidate();
                want = true;
                continue;
            }
        }
        if pressed {
            // 点窗口：抬到最上层并聚焦；落在标题栏上就开始拖
            if let Some(i) = fb::hit_top(x, y) {
                // 标题栏三个圆点：红关、黄收起、绿最大化/还原
                if let Some(b) = fb::title_button_at(i, x, y) {
                    let w = fb::win(i);
                    fb::raise(i);
                    match b {
                        fb::Btn::Close => fb::close_win(i),
                        fb::Btn::Min => fb::minimize_win(i),
                        fb::Btn::Max => fb::toggle_max(i),
                    }
                    // 旧位置（最大化时还要连新位置一起）擦回壁纸再整桌重画
                    fb::restore_wall(
                        (w.x - 30).max(0) as u32,
                        (w.y - 12).max(0) as u32,
                        (w.w + 60).min(1024) as u32,
                        (w.h + 64).min(768) as u32,
                    );
                    let nw = fb::win(i);
                    fb::restore_wall(
                        (nw.x - 30).max(0) as u32,
                        (nw.y - 12).max(0) as u32,
                        (nw.w + 60).min(1024) as u32,
                        (nw.h + 64).min(768) as u32,
                    );
                    fb::draw_sidebar();
                    fb::draw_all();
                    crate::cursor::invalidate();
                    want = true;
                    continue;
                }
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
            // 松手：拖拽期间省了阴影，这里补一次完整的
            if unsafe { DRAG } != usize::MAX {
                let i = unsafe { DRAG };
                unsafe { DRAG = usize::MAX };
                let w = fb::win(i);
                fb::restore_wall(
                    (w.x - 30).max(0) as u32,
                    (w.y - 12).max(0) as u32,
                    (w.w + 60).min(1024) as u32,
                    (w.h + 64).min(768) as u32,
                );
                fb::draw_sidebar();
                fb::draw_all();
                crate::cursor::invalidate();
                want = true;
            }
        }
    }

    // 画布拖动：按住左键在画布上移动 → move 队列
    if crate::apkcanvas::active() {
        let down = input::mouse_buttons() & 1 != 0;
        if down && (mx, my) != unsafe { LASTM } {
            if let Some((cx, cy)) = crate::apkcanvas::screen_to_canvas(mx, my) {
                crate::apkcanvas::push_move(cx, cy);
                want = true;
            }
            unsafe { LASTM = (mx, my) };
        }
        if !down {
            unsafe { LASTM = (-1, -1) };
        }
    }

    if unsafe { DRAG } != usize::MAX {
        let i = unsafe { DRAG };
        let w = fb::win(i);
        let nx = (mx - unsafe { OFF_X }).clamp(80 - w.w as i32, 1024 - 80);
        let ny = (my - unsafe { OFF_Y }).clamp(40, 768 - 80);
        if nx != w.x || ny != w.y {
            fb::move_window_ex(i, nx, ny, false); // 拖动中不画阴影，手感跟手
            want = true;
        }
    }
    want
}

pub fn dragging() -> bool {
    (unsafe { DRAG }) != usize::MAX
}
