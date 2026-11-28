// Aster Kernel — 图片解码（BMP / PNG）
//
// 浏览器要能看图，就得自己把字节解成像素。这里只做最常见的两种：
//   BMP：24/32bpp、BI_RGB 无压缩、bottom-up 与 top-down 都认
//   PNG：8bit、color type 2(RGB)/6(RGBA)、非隔行；IDAT 可多块，zlib 头两字节剥掉后交给 inflate
//
// 不做：调色板、16bit、隔行、Gamma/ICC、alpha 混合（透明像素直接跳过，露出底色）。
// 源图上限定 256×256，解码到 RGBA 缓冲，再整数倍缩小到 MAX_W×MAX_H。
//
// 三块大缓冲（源图 / 解压临时 / 成品）从页帧分配器懒要，不塞进 .bss——BSS 一大，
// C 那边 `&ASTER_USER_CTX` 这种相对取址就会超出 ±1 MiB 的 ADR_PREL_LO21 范围。

pub const MAX_W: usize = 160;
pub const MAX_H: usize = 160;
pub const MAXSRC: usize = 256;

const RAWCAP: usize = MAXSRC * MAXSRC * 4;
const ZCAP: usize = MAXSRC * MAXSRC * 4 + MAXSRC;
const PXCAP: usize = MAX_W * MAX_H * 4;

static mut RAW: *mut u8 = core::ptr::null_mut();
static mut ZBUF: *mut u8 = core::ptr::null_mut();
static mut PX: *mut u32 = core::ptr::null_mut();
static mut IW: u32 = 0;
static mut IH: u32 = 0;
static mut SRC_NAME: [u8; 96] = [0; 96];

fn ensure() -> bool {
    unsafe {
        if RAW.is_null() {
            match crate::frame::alloc_pages(RAWCAP.div_ceil(4096)) {
                Some(p) => RAW = p as *mut u8,
                None => return false,
            }
        }
        if ZBUF.is_null() {
            match crate::frame::alloc_pages(ZCAP.div_ceil(4096)) {
                Some(p) => ZBUF = p as *mut u8,
                None => return false,
            }
        }
        if PX.is_null() {
            match crate::frame::alloc_pages(PXCAP.div_ceil(4096)) {
                Some(p) => PX = p as *mut u32,
                None => return false,
            }
        }
        true
    }
}

pub fn width() -> u32 {
    unsafe { IW }
}
pub fn height() -> u32 {
    unsafe { IH }
}

pub fn clear() {
    unsafe {
        IW = 0;
        IH = 0;
        SRC_NAME = [0; 96];
    }
}

pub fn src_name() -> &'static str {
    unsafe {
        let mut n = 0usize;
        while n < 96 && SRC_NAME[n] != 0 {
            n += 1;
        }
        core::str::from_utf8_unchecked(&SRC_NAME[..n])
    }
}

fn set_src(name: &str) {
    unsafe {
        SRC_NAME = [0; 96];
        let b = name.as_bytes();
        let mut i = 0usize;
        while i < b.len() && i + 1 < 96 {
            SRC_NAME[i] = b[i];
            i += 1;
        }
    }
}

pub fn is_loaded(name: &str) -> bool {
    !name.is_empty() && src_name() == name && width() > 0
}

/// 解码一张图：自动判别 BMP / PNG。成功返回 true。
pub fn load(name: &str, data: &[u8]) -> bool {
    clear();
    if !ensure() {
        crate::println!("[IMG] 页帧不够，开不了图片缓冲");
        return false;
    }
    let ok = if data.len() > 2 && data[0] == b'B' && data[1] == b'M' {
        decode_bmp(data)
    } else if data.len() > 8 && data[0] == 0x89 && data[1] == b'P' && data[2] == b'N' && data[3] == b'G' {
        decode_png(data)
    } else {
        Err("不是 BMP/PNG")
    };
    match ok {
        Ok(()) => {
            set_src(name);
            true
        }
        Err(e) => {
            crate::println!("[IMG] 解码失败 {}：{}", name, e);
            clear();
            false
        }
    }
}

fn set_raw(x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
    unsafe {
        let i = ((y as usize) * MAXSRC + x as usize) * 4;
        *RAW.add(i) = r;
        *RAW.add(i + 1) = g;
        *RAW.add(i + 2) = b;
        *RAW.add(i + 3) = a;
    }
}

fn raw_at(x: u32, y: u32) -> (u8, u8, u8, u8) {
    unsafe {
        let i = ((y as usize) * MAXSRC + x as usize) * 4;
        (*RAW.add(i), *RAW.add(i + 1), *RAW.add(i + 2), *RAW.add(i + 3))
    }
}

// ---------------- BMP ----------------

fn rd_u32(b: &[u8], off: usize) -> u32 {
    (b[off] as u32) | ((b[off + 1] as u32) << 8) | ((b[off + 2] as u32) << 16) | ((b[off + 3] as u32) << 24)
}
/// PNG 里所有整数都是大端，和 BMP 相反
fn rd_be32(b: &[u8], off: usize) -> u32 {
    ((b[off] as u32) << 24) | ((b[off + 1] as u32) << 16) | ((b[off + 2] as u32) << 8) | (b[off + 3] as u32)
}
fn rd_u16(b: &[u8], off: usize) -> u16 {
    (b[off] as u16) | ((b[off + 1] as u16) << 8)
}

fn decode_bmp(d: &[u8]) -> Result<(), &'static str> {
    if d.len() < 54 {
        return Err("BMP 太短");
    }
    let data_off = rd_u32(d, 10) as usize;
    let hdr = rd_u32(d, 14) as usize;
    if hdr < 40 {
        return Err("BMP 头太老");
    }
    let w = rd_u32(d, 18) as i32;
    let h = rd_u32(d, 22) as i32;
    let bpp = rd_u16(d, 28);
    let comp = rd_u32(d, 30);
    if comp != 0 {
        return Err("BMP 有压缩");
    }
    if bpp != 24 && bpp != 32 {
        return Err("BMP 只认 24/32bpp");
    }
    if w <= 0 || h == 0 {
        return Err("BMP 尺寸非法");
    }
    let (w, top_down) = (w as u32, h < 0);
    let ah = if top_down { (-h) as u32 } else { h as u32 };
    if w as usize > MAXSRC || ah as usize > MAXSRC {
        return Err("图片太大");
    }
    let bytes_pp = (bpp / 8) as usize;
    let stride = ((w as usize * bytes_pp + 3) / 4) * 4;
    for y in 0..ah {
        let sy = if top_down { y } else { ah - 1 - y };
        let row = data_off + (sy as usize) * stride;
        for x in 0..w {
            let i = row + (x as usize) * bytes_pp;
            if i + bytes_pp > d.len() {
                return Err("BMP 数据越界");
            }
            let b = d[i];
            let g = d[i + 1];
            let r = d[i + 2];
            let a = if bytes_pp == 4 { d[i + 3] } else { 255 };
            set_raw(x, y, r, g, b, a);
        }
    }
    finish(w, ah)
}

// ---------------- PNG ----------------

fn decode_png(d: &[u8]) -> Result<(), &'static str> {
    let mut p = 8usize;
    let mut w = 0u32;
    let mut h = 0u32;
    let mut depth = 0u8;
    let mut ctype = 0u8;
    let mut interlace = 0u8;
    let mut idat_off = 0usize;
    let mut idat_len = 0usize;
    let mut seen_idat = false;
    while p + 8 <= d.len() {
        let len = rd_be32(d, p) as usize;
        let ty = &d[p + 4..p + 8];
        let body = p + 8;
        if body + len > d.len() {
            break;
        }
        if ty == b"IHDR" {
            if len < 13 {
                return Err("IHDR 太短");
            }
            w = rd_be32(d, body);
            h = rd_be32(d, body + 4);
            depth = d[body + 8];
            ctype = d[body + 9];
            interlace = d[body + 12];
        } else if ty == b"IDAT" {
            // 多个 IDAT 在文件里通常连续；这里只接第一段，够用
            if !seen_idat {
                idat_off = body;
                idat_len = len;
                seen_idat = true;
            }
        } else if ty == b"IEND" {
            break;
        }
        p = body + len + 4;
    }
    if !seen_idat {
        return Err("没有 IDAT");
    }
    if depth != 8 {
        return Err("只认 8bit");
    }
    if ctype != 2 && ctype != 6 {
        return Err("只认 RGB/RGBA");
    }
    if interlace != 0 {
        return Err("不认隔行");
    }
    if w == 0 || h == 0 || w as usize > MAXSRC || h as usize > MAXSRC {
        return Err("图片太大");
    }
    let bpp = if ctype == 6 { 4 } else { 3 };
    let stride = (w as usize) * bpp;
    let need = (stride + 1) * (h as usize);
    if need > ZCAP {
        return Err("解压缓冲不够");
    }
    if idat_len < 3 {
        return Err("IDAT 太短");
    }
    // zlib 头：CMF/FLG 两字节，剥掉；尾部 adler 不管（inflate 不看）
    let zsrc = &d[idat_off + 2..idat_off + idat_len];
    let zbuf = unsafe { core::slice::from_raw_parts_mut(ZBUF, need) };
    let got = match crate::inflate::inflate(zsrc, zbuf) {
        Ok(n) => n,
        Err(_) => return Err("inflate 失败"),
    };
    if got < need {
        return Err("解压不完整");
    }

    // 反滤波：Sub / Up / Average / Paeth，逐行还
    let zb = unsafe { core::slice::from_raw_parts(ZBUF, need) };
    let mut prev = [0u8; MAXSRC * 4];
    let mut cur = [0u8; MAXSRC * 4];
    let mut off = 0usize;
    for y in 0..h as usize {
        let ft = zb[off];
        off += 1;
        let row = &zb[off..off + stride];
        off += stride;
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let x = match ft {
                0 => row[i],
                1 => row[i].wrapping_add(a),
                2 => row[i].wrapping_add(b),
                3 => row[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => row[i].wrapping_add(paeth(a, b, c)),
                _ => return Err("滤波类型不认识"),
            };
            cur[i] = x;
        }
        for x in 0..w as usize {
            let i = x * bpp;
            let r = cur[i];
            let g = cur[i + 1];
            let b2 = cur[i + 2];
            let a2 = if bpp == 4 { cur[i + 3] } else { 255 };
            set_raw(x as u32, y as u32, r, g, b2, a2);
        }
        prev[..stride].copy_from_slice(&cur[..stride]);
    }
    finish(w, h)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------- 落到 PX（整数倍缩小） ----------------

fn finish(w: u32, h: u32) -> Result<(), &'static str> {
    let mut s = 1u32;
    while (w / s) as usize > MAX_W || (h / s) as usize > MAX_H {
        s += 1;
    }
    if s > 16 {
        return Err("缩得太多");
    }
    let dw = (w / s).max(1);
    let dh = (h / s).max(1);
    unsafe {
        IW = dw;
        IH = dh;
        for y in 0..dh {
            for x in 0..dw {
                let (r, g, b, a) = raw_at(x * s, y * s);
                let px = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
                *PX.add((y as usize) * MAX_W + x as usize) = px;
            }
        }
    }
    Ok(())
}

/// 按整数倍缩小画到 (x,y)，返回占用的高度（像素）。
pub fn draw(x: u32, y: u32, max_w: u32, max_h: u32) -> u32 {
    unsafe {
        let (iw, ih) = (IW, IH);
        if iw == 0 || ih == 0 || PX.is_null() {
            return 0;
        }
        let mut s = 1u32;
        while iw / s > max_w || ih / s > max_h {
            s += 1;
        }
        let dw = (iw / s).max(1);
        let dh = (ih / s).max(1);
        for dy in 0..dh {
            for dx in 0..dw {
                let px = *PX.add(((dy * s) as usize) * MAX_W + (dx * s) as usize);
                if (px >> 24) < 128 {
                    continue; // 半透明以上直接跳过，露出底色
                }
                crate::fb::write_px(x + dx, y + dy, px & 0x00FFFFFF);
            }
        }
        dh
    }
}
