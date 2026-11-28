// Aster Kernel — 极简 HTML 解析与块级排版
//
// 目标不是"兼容浏览器"，是**把一段真实 HTML 排成能读的文字**：
// 认标题、段落、列表、分隔线、链接、代码，折叠空白，按窗宽自动折行。
// 不做 CSS 层叠、不做表格布局、不解码图片（img 只显示占位与 alt）。
//
// 输出是一串"行"（Line），窗口滚动就是在这串行上平移 ——
// 排版只算一次，滚动只改偏移。
#![allow(static_mut_refs)]

pub const MAX_LINES: usize = 320;
pub const CELLS: usize = 96;
/// 一行最宽（半角单位）：ASCII 1，汉字 2
pub const MAXW: usize = 76;

/// kind：0 正文　1 h1　2 h2　3 h3　4 链接　5 代码　6 引用　7 分隔线　8 空行　9 图片占位
#[derive(Clone, Copy)]
pub struct Line {
    pub cells: [u16; CELLS],
    pub len: u16,
    pub kind: u8,
    pub indent: u8,
    pub href: [u8; 72],
}

impl Line {
    pub const EMPTY: Line = Line {
        cells: [0; CELLS],
        len: 0,
        kind: 0,
        indent: 0,
        href: [0; 72],
    };
}

static mut L: [Line; MAX_LINES] = [Line::EMPTY; MAX_LINES];
static mut N: usize = 0;
static mut TITLE: [u16; 64] = [0; 64];
static mut TN: usize = 0;

static mut CUR_W: usize = 0;
static mut CUR_KIND: u8 = 0;
static mut CUR_INDENT: u8 = 0;
static mut CUR_HREF: [u8; 72] = [0; 72];
static mut IN_TITLE: bool = false;
/// 当前行是否已"封口"：flush/blank/折行之后必须开新行，不能再往里写
static mut CUR_CLOSED: bool = true;
static mut SKIP: u8 = 0;

pub fn count() -> usize {
    unsafe { N }
}

pub fn get(i: usize) -> Line {
    unsafe { L[i] }
}

pub fn title_utf8(out: &mut [u8; 128]) -> usize {
    unsafe {
        let mut k = 0usize;
        for i in 0..TN {
            let cp = TITLE[i];
            if cp == 0 {
                break;
            }
            let ch = core::char::from_u32(cp as u32).unwrap_or('?');
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            for b in s.as_bytes() {
                if k + 1 >= out.len() {
                    return k;
                }
                out[k] = *b;
                k += 1;
            }
        }
        k
    }
}

fn push_line(kind: u8, indent: u8) {
    unsafe {
        if N >= MAX_LINES {
            return;
        }
        let i = N;
        L[i].len = 0;
        L[i].kind = kind;
        L[i].indent = indent;
        L[i].href = CUR_HREF;
        N += 1;
        CUR_W = 0;
        CUR_CLOSED = false;
    }
}

/// 收掉当前行（有内容才有效），并把"行内状态"复位
fn flush() {
    unsafe {
        if N > 0 && CUR_W > 0 {
            CUR_W = 0;
            CUR_CLOSED = true;
            CUR_KIND = 0;
            CUR_INDENT = 0;
            CUR_HREF = [0; 72];
        }
    }
}

fn blank() {
    unsafe {
        if N >= MAX_LINES {
            return;
        }
        if N > 0 && L[N - 1].kind == 8 {
            return;
        }
        let i = N;
        L[i].len = 0;
        L[i].kind = 8;
        L[i].indent = 0;
        N += 1;
        CUR_W = 0;
        CUR_CLOSED = true;
    }
}

fn rule() {
    unsafe {
        if N >= MAX_LINES {
            return;
        }
        let i = N;
        L[i].len = 0;
        L[i].kind = 7;
        L[i].indent = 0;
        N += 1;
        CUR_W = 0;
        CUR_CLOSED = true;
    }
}

fn add_title(cp: u16) {
    unsafe {
        if TN < TITLE.len() - 1 {
            TITLE[TN] = cp;
            TN += 1;
        }
    }
}

fn add_char(cp: u16, w: usize) {
    unsafe {
        if SKIP != 0 {
            return;
        }
        if IN_TITLE {
            add_title(cp);
            return;
        }
        if CUR_CLOSED {
            push_line(CUR_KIND, CUR_INDENT);
        }
        if CUR_W + w > MAXW {
            CUR_CLOSED = true;
            push_line(CUR_KIND, CUR_INDENT);
        }
        let i = N - 1;
        let k = L[i].len as usize;
        if k + 1 < CELLS {
            L[i].cells[k] = cp;
            L[i].len += 1;
            L[i].kind = CUR_KIND;
        }
        CUR_W += w;
    }
}

fn add_space() {
    unsafe {
        if N == 0 || CUR_W == 0 {
            return;
        }
        let i = N - 1;
        let k = L[i].len as usize;
        if k > 0 && L[i].cells[k - 1] != b' ' as u16 {
            add_char(b' ' as u16, 1);
        }
    }
}

fn ascii_lower(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' {
        b + 32
    } else {
        b
    }
}

fn tag_is(tag: &[u8], name: &[u8]) -> bool {
    if tag.len() < name.len() {
        return false;
    }
    for i in 0..name.len() {
        if ascii_lower(tag[i]) != name[i] {
            return false;
        }
    }
    tag.len() == name.len() || tag[name.len()] == b' ' || tag[name.len()] == b'\t' || tag[name.len()] == b'/'
}

fn attr<'a>(tag: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0usize;
    while i + key.len() <= tag.len() {
        let mut hit = true;
        for k in 0..key.len() {
            if ascii_lower(tag[i + k]) != key[k] {
                hit = false;
                break;
            }
        }
        if hit {
            let mut j = i + key.len();
            while j < tag.len() && (tag[j] == b' ' || tag[j] == b'\t') {
                j += 1;
            }
            if j < tag.len() && tag[j] == b'=' {
                j += 1;
                while j < tag.len() && (tag[j] == b' ' || tag[j] == b'\t') {
                    j += 1;
                }
                if j < tag.len() && (tag[j] == b'"' || tag[j] == b'\'') {
                    let q = tag[j];
                    j += 1;
                    let s = j;
                    while j < tag.len() && tag[j] != q {
                        j += 1;
                    }
                    return Some(&tag[s..j.min(tag.len())]);
                }
            }
        }
        i += 1;
    }
    None
}

fn set_href(h: &[u8], base: &str) {
    unsafe {
        CUR_HREF = [0; 72];
        let mut k = 0usize;
        if h.starts_with(b"http") || h.starts_with(b"aster://") {
            for c in h {
                if k + 1 < 72 {
                    CUR_HREF[k] = *c;
                    k += 1;
                }
            }
            return;
        }
        // 相对地址 → 拼 base 的 scheme://host
        let bb = base.as_bytes();
        let mut host_end = 0usize;
        let mut slashes = 0usize;
        for (n, c) in bb.iter().enumerate() {
            if *c == b'/' {
                slashes += 1;
                if slashes == 3 {
                    host_end = n;
                    break;
                }
            }
        }
        let host = if host_end > 0 { &base[..host_end] } else { "" };
        for c in host.as_bytes() {
            if k + 1 < 72 {
                CUR_HREF[k] = *c;
                k += 1;
            }
        }
        if !h.starts_with(b"/") {
            if k + 1 < 72 {
                CUR_HREF[k] = b'/';
                k += 1;
            }
        }
        for c in h {
            if k + 1 < 72 {
                CUR_HREF[k] = *c;
                k += 1;
            }
        }
    }
}

pub fn parse(src: &[u8], base: &str) {
    unsafe {
        N = 0;
        TN = 0;
        CUR_CLOSED = true;
        CUR_W = 0;
        CUR_KIND = 0;
        CUR_INDENT = 0;
        CUR_HREF = [0; 72];
        IN_TITLE = false;
        SKIP = 0;
    }
    let mut i = 0usize;
    while i < src.len() {
        let b = src[i];
        if b == b'<' {
            if i + 3 < src.len() && src[i + 1] == b'!' && src[i + 2] == b'-' && src[i + 3] == b'-' {
                i += 4;
                while i + 2 < src.len() && !(src[i] == b'-' && src[i + 1] == b'-' && src[i + 2] == b'>') {
                    i += 1;
                }
                i = (i + 3).min(src.len());
                continue;
            }
            let mut j = i + 1;
            let close = j < src.len() && src[j] == b'/';
            if close {
                j += 1;
            }
            let s = j;
            while j < src.len() && src[j] != b'>' {
                j += 1;
            }
            let tag = &src[s..j];
            i = (j + 1).min(src.len());

            // script / style：整段丢掉
            if tag_is(tag, b"script") || tag_is(tag, b"style") {
                if !close {
                    let name: &[u8] = if tag_is(tag, b"script") { b"</script" } else { b"</style" };
                    while i + name.len() <= src.len() {
                        if &src[i..i + name.len()] == name {
                            while i < src.len() && src[i] != b'>' {
                                i += 1;
                            }
                            i = (i + 1).min(src.len());
                            break;
                        }
                        i += 1;
                    }
                }
                continue;
            }

            if close {
                if tag_is(tag, b"title") {
                    unsafe {
                        IN_TITLE = false;
                    }
                    continue;
                }
                if tag_is(tag, b"p") || tag_is(tag, b"h1") || tag_is(tag, b"h2") || tag_is(tag, b"h3") || tag_is(tag, b"hr") {
                    flush();
                    blank();
                } else if tag_is(tag, b"li") {
                    flush();
                    unsafe {
                        CUR_INDENT = 0;
                    }
                } else if tag_is(tag, b"a") || tag_is(tag, b"code") || tag_is(tag, b"pre") {
                    flush();
                    unsafe {
                        CUR_KIND = 0;
                        CUR_INDENT = 0;
                        CUR_HREF = [0; 72];
                    }
                }
                continue;
            }

            // 块级开始前先收行
            if tag_is(tag, b"p") || tag_is(tag, b"div") || tag_is(tag, b"h1") || tag_is(tag, b"h2")
                || tag_is(tag, b"h3") || tag_is(tag, b"li") || tag_is(tag, b"ul") || tag_is(tag, b"ol")
                || tag_is(tag, b"pre") || tag_is(tag, b"br") || tag_is(tag, b"hr") || tag_is(tag, b"body")
            {
                flush();
            }

            if tag_is(tag, b"title") {
                unsafe {
                    IN_TITLE = true;
                }
            } else if tag_is(tag, b"h1") {
                unsafe {
                    CUR_KIND = 1;
                    CUR_INDENT = 0;
                }
            } else if tag_is(tag, b"h2") {
                unsafe {
                    CUR_KIND = 2;
                    CUR_INDENT = 0;
                }
            } else if tag_is(tag, b"h3") {
                unsafe {
                    CUR_KIND = 3;
                    CUR_INDENT = 0;
                }
            } else if tag_is(tag, b"li") {
                unsafe {
                    CUR_KIND = 0;
                    CUR_INDENT = 4;
                }
                add_char(0x00B7, 2); // ·
                add_char(b' ' as u16, 1);
            } else if tag_is(tag, b"code") || tag_is(tag, b"pre") {
                unsafe {
                    CUR_KIND = 5;
                    CUR_INDENT = 4;
                }
            } else if tag_is(tag, b"a") {
                // 链接另起一行，行级样式（kind=4）才落得上；href 由下次 push_line 带进行里
                flush();
                unsafe {
                    CUR_KIND = 4;
                    CUR_INDENT = 0;
                    CUR_HREF = [0; 72];
                }
                if let Some(h) = attr(tag, b"href") {
                    set_href(h, base);
                }
            } else if tag_is(tag, b"hr") {
                rule();
            } else if tag_is(tag, b"img") {
                flush();
                unsafe {
                    CUR_KIND = 9;
                    CUR_INDENT = 0;
                    CUR_HREF = [0; 72];
                }
                if let Some(src) = attr(tag, b"src") {
                    set_href(src, base);
                }
                add_char(b'[' as u16, 1);
                add_char(b'i' as u16, 1);
                add_char(b'm' as u16, 1);
                add_char(b'g' as u16, 1);
                add_char(b']' as u16, 1);
                if let Some(alt) = attr(tag, b"alt") {
                    for &c in alt {
                        add_char(c as u16, 1);
                    }
                }
                unsafe {
                    CUR_KIND = 0;
                }
            }
            continue;
        }

        if b == b'\n' || b == b'\r' || b == b'\t' || b == b' ' {
            add_space();
            i += 1;
            continue;
        }
        if b < 0x80 {
            if b == b'&' {
                let mut end = i + 1;
                while end < src.len() && end - i < 8 && src[end] != b';' {
                    end += 1;
                }
                if end < src.len() && src[end] == b';' {
                    let e = &src[i + 1..end];
                    let ch = match e {
                        b"amp" => Some('&'),
                        b"lt" => Some('<'),
                        b"gt" => Some('>'),
                        b"quot" => Some('"'),
                        b"nbsp" => Some(' '),
                        _ => None,
                    };
                    if let Some(c) = ch {
                        add_char(c as u16, 1);
                        i = end + 1;
                        continue;
                    }
                }
            }
            add_char(b as u16, 1);
            i += 1;
        } else {
            let start = i;
            let mut k = i + 1;
            while k < src.len() && src[k] & 0xC0 == 0x80 {
                k += 1;
            }
            if let Ok(s) = core::str::from_utf8(&src[start..k]) {
                if let Some(ch) = s.chars().next() {
                    add_char(ch as u32 as u16, 2);
                }
            }
            i = k
        }
    }
    flush();
}
