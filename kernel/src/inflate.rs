// Aster Kernel — DEFLATE 解压（RFC 1951）
//
// APK 里绝大多数条目都是 method 8（deflate），不做这一步就取不出 classes.dex。
// 这里实现三条路径：stored 块、固定 Huffman、动态 Huffman，外加 LZ77 回拷。
//
// 解码用"逐位走 canonical 码"的做法（count/symbol 两张表 + 累加 first/index），
// 不需要一次建满 15 层查找表；速度比不上查表法，但对几百 KB 的 dex 足够，
// 代码量只有查表法的三分之一，也更好读。
#![allow(dead_code)]

const MAXBITS: usize = 15;
const MAXLCODES: usize = 288;
const MAXDCODES: usize = 32;
const MAXCODES: usize = MAXLCODES + MAXDCODES;

/// 长度码 257..285 对应的基长与附加位
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// 距离码 0..29 对应的基距与附加位
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];
/// 动态块里"码长码"的传输顺序
const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

struct Huff {
    count: [u16; MAXBITS + 1],
    symbol: [u16; MAXCODES],
}

impl Huff {
    const EMPTY: Huff = Huff { count: [0; MAXBITS + 1], symbol: [0; MAXCODES] };
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn need(&mut self, n: u32) -> Result<u32, &'static str> {
        while self.cnt < n {
            let b = *self.src.get(self.pos).ok_or("数据提前结束")?;
            self.buf |= (b as u32) << self.cnt;
            self.cnt += 8;
            self.pos += 1;
        }
        Ok(n)
    }

    fn bits(&mut self, n: u32) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.buf & ((1u32 << n) - 1);
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }

    /// 跳到字节边界（stored 块要求）
    fn align(&mut self) {
        let drop = self.cnt % 8;
        self.buf >>= drop;
        self.cnt -= drop;
    }
}

/// 由码长表构建 canonical Huffman；返回 Ok，全零返回 Ok 但标记为"空表"
fn build(lengths: &[u8], h: &mut Huff) -> Result<(), &'static str> {
    h.count = [0; MAXBITS + 1];
    for &l in lengths {
        if l as usize > MAXBITS {
            return Err("码长超过 15");
        }
        h.count[l as usize] += 1;
    }
    if h.count[0] as usize == lengths.len() {
        return Ok(()); // 全零：合法但解码时用不到
    }
    // 过订阅检查：每一层的码空间不能超
    let mut left: i32 = 1;
    for len in 1..=MAXBITS {
        left <<= 1;
        left -= h.count[len] as i32;
        if left < 0 {
            return Err("Huffman 表过订阅");
        }
    }
    // 首符号偏移
    let mut offs = [0u16; MAXBITS + 1];
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + h.count[len];
    }
    for (sym, &l) in lengths.iter().enumerate() {
        if l != 0 {
            h.symbol[offs[l as usize] as usize] = sym as u16;
            offs[l as usize] += 1;
        }
    }
    Ok(())
}

fn decode(b: &mut Bits, h: &Huff) -> Result<u16, &'static str> {
    let mut code: i32 = 0;
    let mut first: i32 = 0;
    let mut index: i32 = 0;
    for len in 1..=MAXBITS {
        code |= b.bits(1)? as i32;
        let count = h.count[len] as i32;
        if code - first < count {
            let i = (index + (code - first)) as usize;
            if i >= MAXCODES {
                return Err("Huffman 索引越界");
            }
            return Ok(h.symbol[i]);
        }
        index += count;
        first = (first + count) << 1;
        code <<= 1;
    }
    Err("Huffman 码非法")
}

fn stored(b: &mut Bits, dst: &mut [u8], out: &mut usize) -> Result<(), &'static str> {
    b.align();
    let len = b.bits(16)? as usize;
    let nlen = b.bits(16)? as usize;
    if len != ((!nlen) & 0xffff) {
        return Err("stored 块 LEN/NLEN 不匹配");
    }
    if b.cnt >= 8 {
        return Err("位缓冲残留");
    }
    // 此时 buf 里剩下的就是待拷字节
    let mut left = len;
    while left > 0 {
        let byte = if b.cnt >= 8 {
            let v = (b.buf & 0xff) as u8;
            b.buf >>= 8;
            b.cnt -= 8;
            v
        } else {
            let v = *b.src.get(b.pos).ok_or("stored 数据提前结束")?;
            b.pos += 1;
            v
        };
        if *out >= dst.len() {
            return Err("输出超出缓冲");
        }
        dst[*out] = byte;
        *out += 1;
        left -= 1;
    }
    Ok(())
}

fn codes(
    b: &mut Bits,
    dst: &mut [u8],
    out: &mut usize,
    lencode: &Huff,
    distcode: &Huff,
) -> Result<(), &'static str> {
    loop {
        let sym = decode(b, lencode)? as usize;
        if sym < 256 {
            if *out >= dst.len() {
                return Err("输出超出缓冲");
            }
            dst[*out] = sym as u8;
            *out += 1;
        } else if sym == 256 {
            return Ok(());
        } else {
            let i = sym - 257;
            if i >= 29 {
                return Err("长度码超出 285");
            }
            let len = LEN_BASE[i] as usize + b.bits(LEN_EXTRA[i] as u32)? as usize;
            let dsym = decode(b, distcode)? as usize;
            if dsym >= 30 {
                return Err("距离码超出 29");
            }
            let dist = DIST_BASE[dsym] as usize + b.bits(DIST_EXTRA[dsym] as u32)? as usize;
            if dist > *out {
                return Err("距离超出已输出数据");
            }
            if *out + len > dst.len() {
                return Err("输出超出缓冲");
            }
            let mut k = 0;
            while k < len {
                dst[*out] = dst[*out - dist];
                *out += 1;
                k += 1;
            }
        }
    }
}

fn fixed_tables(lencode: &mut Huff, distcode: &mut Huff) {
    let mut lengths = [0u8; MAXLCODES];
    for i in 0..144 {
        lengths[i] = 8;
    }
    for i in 144..256 {
        lengths[i] = 9;
    }
    for i in 256..280 {
        lengths[i] = 7;
    }
    for i in 280..288 {
        lengths[i] = 8;
    }
    let _ = build(&lengths, lencode);
    // 固定距离码是 32 个 5 位码，其中 30/31 永不出现（解码时按非法处理）
    let dl = [5u8; MAXDCODES];
    let _ = build(&dl, distcode);
}

fn dynamic(b: &mut Bits, dst: &mut [u8], out: &mut usize) -> Result<(), &'static str> {
    let nlen = b.bits(5)? as usize + 257;
    let ndist = b.bits(5)? as usize + 1;
    let ncode = b.bits(4)? as usize + 4;
    if nlen > MAXLCODES || ndist > MAXDCODES {
        return Err("动态块尺寸非法");
    }
    let mut lengths = [0u8; MAXCODES];
    let mut k = 0;
    while k < ncode {
        lengths[ORDER[k]] = b.bits(3)? as u8;
        k += 1;
    }
    let mut lencode = Huff::EMPTY;
    build(&lengths[..19], &mut lencode)?;

    // 解码 nlen + ndist 个码长
    lengths = [0u8; MAXCODES];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = decode(b, &lencode)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
        } else {
            let (rep, val): (usize, u8) = match sym {
                16 => {
                    if i == 0 {
                        return Err("重复码 16 出现在开头");
                    }
                    (3 + b.bits(2)? as usize, lengths[i - 1])
                }
                17 => (3 + b.bits(3)? as usize, 0),
                _ => (11 + b.bits(7)? as usize, 0),
            };
            if i + rep > nlen + ndist {
                return Err("码长重复越界");
            }
            let mut k = 0;
            while k < rep {
                lengths[i] = val;
                i += 1;
                k += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err("缺少块结束符");
    }
    let mut lc = Huff::EMPTY;
    let mut dc = Huff::EMPTY;
    build(&lengths[..nlen], &mut lc)?;
    build(&lengths[nlen..nlen + ndist], &mut dc)?;
    codes(b, dst, out, &lc, &dc)
}

/// 解压 src 到 dst，返回写出的字节数。
pub fn inflate(src: &[u8], dst: &mut [u8]) -> Result<usize, &'static str> {
    let mut b = Bits { src, pos: 0, buf: 0, cnt: 0 };
    let mut out = 0usize;
    let mut fixed_l = Huff::EMPTY;
    let mut fixed_d = Huff::EMPTY;
    let mut fixed_ready = false;

    loop {
        let last = b.bits(1)?;
        let kind = b.bits(2)?;
        match kind {
            0 => stored(&mut b, dst, &mut out)?,
            1 => {
                if !fixed_ready {
                    fixed_tables(&mut fixed_l, &mut fixed_d);
                    fixed_ready = true;
                }
                codes(&mut b, dst, &mut out, &fixed_l, &fixed_d)?
            }
            2 => dynamic(&mut b, dst, &mut out)?,
            _ => return Err("块类型 3 非法"),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}
