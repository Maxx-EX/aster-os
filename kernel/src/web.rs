// Aster Kernel — 内置浏览器（窗口 3）
//
// 流水线：地址 → net::http_get 拉字节 → html::parse 排版 → 这里画到窗口里。
// 没有 DNS（NET-4 才做），地址只认 IP:port/path；本地有内置起始页，断网也能开。
#![allow(static_mut_refs)]

use crate::fb;
use crate::html;

pub const HOME: &[u8] = include_bytes!("../../assets/web/home.html");
static HOME_URL: &str = "http://aster.local/home";

static mut SCROLL: i32 = 0;
static mut URL: [u8; 96] = [0; 96];
static mut URLN: usize = 0;
static mut TITLE: [u8; 128] = [0; 128];
static mut TITLEN: usize = 0;
static mut STATUS: u8 = 0; // 0 就绪 1 加载中 2 出错
static mut ERRMSG: u8 = 0; // 1 无网络 2 拉取失败 3 地址不认
static mut TOPS: [u32; html::MAX_LINES] = [0; html::MAX_LINES];
static mut TOTAL: u32 = 0;
static mut VIEW_H: u32 = 0;
static mut VIEW_W: u32 = 0;

fn row_h(kind: u8) -> u32 {
    match kind {
        1 => 34,
        2 => 26,
        7 => 18,
        8 => 10,
        9 => {
            let ih = crate::img::height();
            if ih > 0 {
                ih + 14
            } else {
                21
            }
        }
        _ => 21,
    }
}

/// 一行的 href（img 行里存的是 src）转成 UTF-8
fn line_href<'a>(l: &'a html::Line) -> &'a str {
    let mut n = 0usize;
    while n < 72 && l.href[n] != 0 {
        n += 1;
    }
    core::str::from_utf8(&l.href[..n]).unwrap_or("")
}

/// 内置图：按文件名从 assets 里挑（只有一张 logo，够演示）
fn builtin_img(name: &str) -> Option<&'static [u8]> {
    if name.ends_with("logo.png") {
        Some(include_bytes!("../../assets/web/logo.png"))
    } else {
        None
    }
}

/// 走网络取一张图：http://ip:port/path → body 字节
fn http_img(name: &str) -> Option<&'static [u8]> {
    let rest = name.strip_prefix("http://")?;
    let colon = rest.find(':')?;
    let ip_s = &rest[..colon];
    let after = &rest[colon + 1..];
    let slash = after.find('/').unwrap_or(after.len());
    let port_s = &after[..slash];
    let path = if slash < after.len() { &after[slash..] } else { "/" };
    let mut ip = [0u8; 4];
    let mut part = 0usize;
    let mut acc = 0u32;
    let mut digits = 0u32;
    for c in ip_s.bytes() {
        if c == b'.' {
            if part >= 4 {
                return None;
            }
            ip[part] = acc as u8;
            part += 1;
            acc = 0;
            digits = 0;
        } else if c.is_ascii_digit() {
            acc = acc * 10 + (c - b'0') as u32;
            digits += 1;
        } else {
            return None;
        }
    }
    if part != 3 || digits == 0 {
        return None;
    }
    ip[3] = acc as u8;
    let port: u16 = port_s.parse().ok()?;
    if crate::net::http_get(ip, port, path, 3000) < 0 {
        return None;
    }
    let raw = crate::net::cli_data();
    let mut i = 0usize;
    while i + 3 < raw.len() {
        if raw[i] == b'\r' && raw[i + 1] == b'\n' && raw[i + 2] == b'\r' && raw[i + 3] == b'\n' {
            return Some(&raw[i + 4..]);
        }
        i += 1;
    }
    None
}

/// 页面里第一张图去取一下（本地/网络都走这里）
fn load_images() {
    let n = html::count();
    for i in 0..n {
        let l = html::get(i);
        if l.kind != 9 || l.href[0] == 0 {
            continue;
        }
        let name = line_href(&l);
        if crate::img::is_loaded(name) {
            return;
        }
        crate::img::clear();
        let data = if name.starts_with("aster://") {
            builtin_img(name)
        } else if name.starts_with("http://") {
            http_img(name)
        } else {
            None
        };
        if let Some(d) = data {
            let _ = crate::img::load(name, d);
        }
        return;
    }
}

fn relayout() {
    let n = html::count();
    let mut acc = 0u32;
    unsafe {
        for i in 0..n {
            TOPS[i] = acc;
            acc += row_h(html::get(i).kind);
        }
        TOTAL = acc;
    }
}

fn set_url(u: &str) {
    unsafe {
        URL = [0; 96];
        URLN = 0;
        for b in u.as_bytes() {
            if URLN + 1 >= URL.len() {
                break;
            }
            URL[URLN] = *b;
            URLN += 1;
        }
    }
}

fn set_title_from_doc() {
    let mut buf = [0u8; 128];
    let n = html::title_utf8(&mut buf);
    unsafe {
        TITLE = [0; 128];
        TITLEN = n;
        for i in 0..n {
            TITLE[i] = buf[i];
        }
    }
}

pub fn url_str() -> &'static str {
    unsafe { core::str::from_utf8_unchecked(&URL[..URLN]) }
}

pub fn title_str() -> &'static str {
    unsafe { core::str::from_utf8_unchecked(&TITLE[..TITLEN]) }
}

pub fn status() -> u8 {
    unsafe { STATUS }
}

pub fn scroll_by(d: i32) {
    unsafe {
        let maxv = (TOTAL.saturating_sub(VIEW_H)) as i32;
        let mut s = SCROLL + d;
        if s < 0 {
            s = 0;
        }
        if s > maxv {
            s = maxv;
        }
        SCROLL = s;
    }
    fb::win_touch(3);
}

pub fn scroll_reset() {
    unsafe {
        SCROLL = 0;
    }
}

pub fn max_scroll() -> i32 {
    unsafe { (TOTAL.saturating_sub(VIEW_H)) as i32 }
}

pub fn scroll_to(v: i32) {
    let m = max_scroll();
    let c = if v < 0 { 0 } else if v > m { m } else { v };
    unsafe {
        SCROLL = c;
    }
}

pub fn scroll_pos() -> i32 {
    unsafe { SCROLL }
}

pub fn open_home() {
    html::parse(HOME, HOME_URL);
    load_images();
    relayout();
    set_url("aster://home");
    set_title_from_doc();
    unsafe {
        STATUS = 0;
        ERRMSG = 0;
        SCROLL = 0;
    }
    fb::win_touch(3);
}

fn err_page(msg: &str) {
    // 出错也画一页，总比空白强：写明哪一步没成、下一步怎么办
    let mut page: [u8; 512] = [0; 512];
    let head = b"<h2>\xE5\x8A\xA0\xE8\xBD\xBD\xE5\xA4\xB1\xE8\xB4\xA5</h2><p>";
    let mut k = 0usize;
    for b in head {
        if k + 1 < page.len() {
            page[k] = *b;
            k += 1;
        }
    }
    for b in msg.as_bytes() {
        if k + 1 < page.len() {
            page[k] = *b;
            k += 1;
        }
    }
    let tail = b"</p><hr><p>\xE6\x9C\xAC\xE5\x9C\xB0\xE8\xB5\xB7\xE5\xA7\x8B\xE9\xA1\xB5\xEF\xBC\x9A\xE8\xBE\x93\xE5\x85\xA5 web home</p>";
    for b in tail {
        if k + 1 < page.len() {
            page[k] = *b;
            k += 1;
        }
    }
    html::parse(&page[..k], HOME_URL);
    relayout();
    set_title_from_doc();
    unsafe {
        STATUS = 2;
    }
    fb::win_touch(3);
}

/// 打开地址：`10.0.2.2:8088/` 或 `http://10.0.2.2:8088/x`
pub fn open_addr(addr: &str) -> bool {
    let a = if let Some(p) = addr.find("://") {
        &addr[p + 3..]
    } else {
        addr
    };
    // host[:port][/path]
    let mut host = a;
    let mut port: u16 = 80;
    let mut path = "/";
    if let Some(p) = a.find('/') {
        host = &a[..p];
        path = &a[p..];
    }
    if let Some(p) = host.find(':') {
        port = host[p + 1..].parse().unwrap_or(80);
        host = &host[..p];
    }
    // 点分 IP 直接用；带字母的走 DNS（NET-4）
    let ip = match crate::net::parse_ip(host) {
        Some(x) => x,
        None => match crate::net::resolve(host, 900) {
            Some(x) => x,
            None => {
                err_page("DNS 没解析出来。检查 -netdev user 是否挂了，或者先用 IP:port 的写法。");
                return false;
            }
        },
    };

    if !crate::net::available() {
        err_page("没有 virtio-net。启动时加 -device virtio-net-device,netdev=n0 再试。");
        return false;
    }
    unsafe {
        STATUS = 1;
    }
    let n = crate::net::http_get(ip, port, path, 3000);
    if n < 0 {
        err_page(match n {
            -2 => "ARP 拿不到对方 MAC（对端不在同一网段？）",
            -3 => "连不上或没收到数据（端口、路径对得上吗）",
            -4 => "响应超过 16 KiB 缓冲",
            _ => "网络没就绪",
        });
        unsafe {
            set_url(addr);
        }
        return false;
    }
    let raw = crate::net::cli_data();
    // 跳过响应头
    let mut body = raw;
    let mut i = 0usize;
    while i + 3 < raw.len() {
        if raw[i] == b'\r' && raw[i + 1] == b'\n' && raw[i + 2] == b'\r' && raw[i + 3] == b'\n' {
            body = &raw[i + 4..];
            break;
        }
        i += 1;
    }
    html::parse(body, "http://10.0.2.2");
    load_images();
    relayout();
    set_url(addr);
    set_title_from_doc();
    unsafe {
        STATUS = 0;
        ERRMSG = 0;
        SCROLL = 0;
    }
    fb::win_touch(3);
    true
}

/// 把一行的 cells 转成 UTF-8 交给 draw_text
fn line_utf8(l: &html::Line, out: &mut [u8; 256]) -> usize {
    let mut k = 0usize;
    for i in 0..l.len as usize {
        let cp = l.cells[i];
        if cp == 0 || cp == 0xFFFF {
            continue;
        }
        let ch = core::char::from_u32(cp as u32).unwrap_or('?');
        let mut buf = [0u8; 4];
        for b in ch.encode_utf8(&mut buf).as_bytes() {
            if k + 1 >= out.len() {
                return k;
            }
            out[k] = *b;
            k += 1;
        }
    }
    k
}

pub fn draw(x: u32, y: u32, w: u32, h: u32) {
    unsafe {
        VIEW_W = w;
        VIEW_H = h;
    }
    let bar_h = 38u32;
    if h <= bar_h + 20 || w < 200 {
        return;
    }

    // 地址栏：[首页] 地址 网址 …… 标题 ●就绪
    fb::fill_rect(x + 8, y + 3, w - 16, 1, 0x2a2f37);
    let home_x = x + 10;
    fb::draw_text_ui(home_x, y + 9, "首页", fb::ACCENT);
    fb::fill_rect(home_x + 40, y + 7, 1, 16, 0x2a2f37);
    let u = url_str();
    fb::draw_text_ui(home_x + 48, y + 9, "地址 ", fb::DIM);
    let ulen = home_x + 48 + fb::text_len_ui("地址 ");
    fb::draw_text_ui(ulen, y + 9, u, fb::TEXT);
    let st = unsafe { STATUS };
    let (dot, lab) = match st {
        1 => (fb::WARN, "加载中"),
        2 => (fb::ERR, "失败"),
        _ => (fb::OK, "就绪"),
    };
    fb::circle((x + w - 44) as i32, (y + 17) as i32, 4, dot, 255);
    fb::draw_text_ui(x + w - 34, y + 9, lab, fb::TEXT2);

    // 页面标题右对齐放在状态点前；位置跟 URL 撞上就干脆不画（标题栏里已有窗口名）
    let t = title_str();
    if !t.is_empty() {
        let tw = fb::text_len_ui(t);
        let tx = (x + w - 74).saturating_sub(tw);
        if tx > ulen + 24 + 16 {
            fb::draw_text_ui(tx, y + 9, t, fb::ACCENT);
        }
    }
    fb::fill_rect(x + 8, y + bar_h - 4, w - 16, 1, 0x24272c);

    // 内容
    let cy = y + bar_h;
    let ch = h - bar_h - 8;
    let n = html::count();
    let sc = unsafe { SCROLL };
    let mut drawn = 0usize;
    for i in 0..n {
        let top = unsafe { TOPS[i] };
        let yy = top as i32 - sc + cy as i32;
        if yy + 30 < cy as i32 {
            continue;
        }
        if yy > (cy + ch) as i32 {
            break;
        }
        let l = html::get(i);
        if l.kind == 8 {
            continue;
        }
        let lx = x + 16 + (l.indent as u32) * 8;
        let lw = w - 36;
    let _ = lw;
        match l.kind {
            1 => {
                let mut b = [0u8; 256];
                let k = line_utf8(&l, &mut b);
                let s = core::str::from_utf8(&b[..k]).unwrap_or("");
                fb::draw_text(lx, yy as u32 + 2, s, fb::TEXT, 2);
                let uw = (fb::text_len(s, 2)).min(lw);
                fb::fill_rect(lx, yy as u32 + 30, uw, 2, fb::ACCENT);
            }
            2 => {
                let mut b = [0u8; 256];
                let k = line_utf8(&l, &mut b);
                let s = core::str::from_utf8(&b[..k]).unwrap_or("");
                fb::draw_text(lx, yy as u32 + 3, s, fb::ACCENT, 1);
                fb::fill_rect(lx, yy as u32 + 22, 18, 2, fb::ACCENT);
            }
            3 => {
                let mut b = [0u8; 256];
                let k = line_utf8(&l, &mut b);
                let s = core::str::from_utf8(&b[..k]).unwrap_or("");
                fb::draw_text(lx, yy as u32 + 2, s, fb::TEXT, 1);
            }
            7 => {
                fb::fill_rect(x + 16, yy as u32 + 6, w - 40, 1, 0x333941);
            }
            9 => {
                let name = line_href(&l);
                if crate::img::is_loaded(name) && crate::img::height() > 0 {
                    crate::img::draw(lx, yy as u32 + 4, lw, 320);
                } else {
                    let mut b = [0u8; 256];
                    let k = line_utf8(&l, &mut b);
                    let s2 = core::str::from_utf8(&b[..k]).unwrap_or("");
                    fb::draw_text(lx, yy as u32 + 2, s2, fb::DIM, 1);
                }
            }
            _ => {
                let mut b = [0u8; 256];
                let k = line_utf8(&l, &mut b);
                let s = core::str::from_utf8(&b[..k]).unwrap_or("");
                let color = match l.kind {
                    4 => fb::ACCENT,
                    5 => 0x9fd6a0,
                    9 => fb::DIM,
                    _ => fb::TEXT2,
                };
                fb::draw_text(lx, yy as u32 + 2, s, color, 1);
                if l.kind == 4 {
                    // 链接下划线
                    fb::fill_rect(lx, yy as u32 + 19, fb::text_len(s, 1).min(lw), 1, fb::ACCENT);
                }
            }
        }
        drawn += 1;
        if drawn > 60 {
            break;
        }
    }

    // 滚动条：只有内容超高才出现。细轨 + 半透明滑块，别抢正文的视线
    if unsafe { TOTAL } > ch {
        let track_w = 4u32;
        let track_x = x + w - track_w - 4;
        fb::rounded_rect(track_x as i32, (cy + 4) as i32, track_w, ch - 8, 2, 0x1c2026);
        let maxv = max_scroll().max(1) as u32;
        let total = unsafe { TOTAL };
        let th = ((ch as u64 * ch as u64 / total.max(1) as u64) as u32 + 18)
            .min(ch - 12);
        let travel = ch - 12 - th;
        let off = (travel as u64 * sc.max(0) as u64 / maxv as u64) as u32;
        fb::rounded_rect_alpha(
            track_x as i32 - 1,
            (cy + 4 + off) as i32,
            6,
            th,
            3,
            fb::ACCENT,
            120,
        );
    }
}

/// 点击内容区：命中带 href 的行就返回目标地址
static mut HREF_BUF: [u8; 72] = [0; 72];

pub fn link_at(x: u32, y: u32, win_y: u32, win_h: u32) -> Option<&'static str> {
    let bar_h = 38u32;
    let cy = win_y + bar_h;
    if y < cy || y > win_h {
        return None;
    }
    let sc = unsafe { SCROLL };
    let ry = (y - cy) as i32 + sc;
    let n = html::count();
    for i in 0..n {
        let top = unsafe { TOPS[i] } as i32;
        let hh = row_h(html::get(i).kind) as i32;
        if ry >= top && ry < top + hh {
            let l = html::get(i);
            if l.href[0] != 0 {
                let mut end = 0usize;
                while end < 72 && l.href[end] != 0 {
                    end += 1;
                }
                unsafe {
                    HREF_BUF = [0; 72];
                    for k in 0..end {
                        HREF_BUF[k] = l.href[k];
                    }
                    return Some(core::str::from_utf8_unchecked(&HREF_BUF[..end]));
                }
            }
            return None;
        }
    }
    let _ = x;
    None
}
