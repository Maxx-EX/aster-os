// Aster Kernel — 桌面刷新调度
//
// 界面不是每帧都重画（内核里全屏重绘一次约几十毫秒），
// 只在终端内容真的变化时重绘，并且限制最快 60ms 一次。

use crate::fb;

const MIN_INTERVAL_MS: u64 = 60;

pub fn tick() {
    if !fb::available() {
        return;
    }
    if !crate::term::dirty() {
        return;
    }
    let now = crate::timer::now_ms();
    if now.saturating_sub(crate::term::last_draw()) < MIN_INTERVAL_MS {
        return;
    }
    redraw();
}

pub fn redraw() {
    crate::shell::render_dashboard();
}
