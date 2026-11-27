// Aster Kernel — ZIP / APK 容器读取（阶段七·第一块）
//
// APK 就是一个 zip：目录在文件尾部的 EOCD 里，每个条目在中央目录里记着
// 名字、压缩方式、压缩后/原始大小、以及本地头的偏移。要"能跑 APK"，第一件事
// 不是解释 DEX，而是能从 APK 里把 classes.dex 原样取出来 —— 这一层就干这个。
//
// 现在支持 method 0（stored，不压缩）。method 8（deflate）留接口、返回明确
// 错误：inflate 是独立的几百行，后面单独做。
#![allow(dead_code)]

pub const SIG_EOCD: u32 = 0x0605_4b50;
pub const SIG_CD: u32 = 0x0201_4b50;
pub const SIG_LOCAL: u32 = 0x0403_4b50;

const EOCD_MIN: usize = 22;
const TAIL_MAX: usize = 22 + 0xffff;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[derive(Clone, Copy)]
pub struct Eocd {
    pub count: u16,
    pub cd_off: u32,
    pub cd_size: u32,
}

/// 从文件尾部倒着找 EOCD。注释字段最长 65535，所以只扫最后 64 KiB + 22 字节。
pub fn eocd(data: &[u8]) -> Option<Eocd> {
    let n = data.len();
    if n < EOCD_MIN {
        return None;
    }
    let floor = n.saturating_sub(TAIL_MAX);
    let mut i = n - EOCD_MIN;
    loop {
        if u32le(data, i) == SIG_EOCD {
            let count = u16le(data, i + 10);
            let cd_size = u32le(data, i + 12);
            let cd_off = u32le(data, i + 16);
            if (cd_off as usize) < n && (cd_off as usize + cd_size as usize) <= n {
                return Some(Eocd { count, cd_off, cd_size });
            }
        }
        if i == floor {
            return None;
        }
        i -= 1;
    }
}

/// 遍历中央目录。回调参数：名字、压缩方式、压缩后大小、原始大小、本地头偏移。
pub fn walk<'a, F>(data: &'a [u8], mut cb: F) -> Result<u16, &'static str>
where
    F: FnMut(&'a str, u16, u32, u32, u32),
{
    let e = eocd(data).ok_or("不是 zip（没找到 EOCD）")?;
    let mut p = e.cd_off as usize;
    let mut listed = 0u16;
    while listed < e.count {
        if p + 46 > data.len() {
            return Err("中央目录越界");
        }
        if u32le(data, p) != SIG_CD {
            return Err("中央目录签名不对");
        }
        let method = u16le(data, p + 10);
        let csize = u32le(data, p + 20);
        let usize_ = u32le(data, p + 24);
        let nlen = u16le(data, p + 28) as usize;
        let elen = u16le(data, p + 30) as usize;
        let clen = u16le(data, p + 32) as usize;
        let loff = u32le(data, p + 42);
        if p + 46 + nlen > data.len() {
            return Err("条目名越界");
        }
        let name = core::str::from_utf8(&data[p + 46..p + 46 + nlen]).unwrap_or("<非 UTF-8>");
        cb(name, method, csize, usize_, loff);
        p += 46 + nlen + elen + clen;
        listed += 1;
    }
    Ok(listed)
}

pub struct Found {
    pub data_off: usize,
    pub size: u32,
    pub method: u16,
}

/// 按名字找条目并算出数据真正开始的偏移（本地头的名字/扩展字段长度可能与
/// 中央目录不同，必须按本地头重新算）。
pub fn find(data: &[u8], want: &str) -> Result<Found, &'static str> {
    let mut hit: Option<Found> = None;
    walk(data, |name, method, _csize, usize_, loff| {
        if hit.is_none() && name.eq_ignore_ascii_case(want) {
            let lo = loff as usize;
            if lo + 30 <= data.len() && u32le(data, lo) == SIG_LOCAL {
                let nlen = u16le(data, lo + 26) as usize;
                let elen = u16le(data, lo + 28) as usize;
                let off = lo + 30 + nlen + elen;
                if off <= data.len() {
                    hit = Some(Found { data_off: off, size: usize_, method });
                }
            }
        }
    })?;
    hit.ok_or("APK 里没有这个条目")
}

/// 取出条目内容。method 0 直接拷，method 8 明确拒绝。
pub fn extract(data: &[u8], want: &str, out: &mut [u8]) -> Result<usize, &'static str> {
    let f = find(data, want)?;
    if f.method != 0 {
        return Err("条目是 deflate 压缩（method 8），需要 inflate，下一阶段做");
    }
    let n = (f.size as usize).min(out.len());
    if f.data_off + n > data.len() {
        return Err("条目数据越界");
    }
    out[..n].copy_from_slice(&data[f.data_off..f.data_off + n]);
    Ok(n)
}

pub struct Summary {
    pub entries: u16,
    pub stored: u16,
    pub deflated: u16,
    pub others: u16,
    pub raw: u32,
}

pub fn summarize(data: &[u8]) -> Result<Summary, &'static str> {
    let mut s = Summary { entries: 0, stored: 0, deflated: 0, others: 0, raw: 0 };
    walk(data, |_n, method, _c, usize_, _o| {
        s.entries += 1;
        match method {
            0 => s.stored += 1,
            8 => s.deflated += 1,
            _ => s.others += 1,
        }
        s.raw = s.raw.saturating_add(usize_);
    })?;
    Ok(s)
}
