// Aster Kernel — DEX 解析与最小解释器（阶段七·第二块）
//
// APK 能解压出 classes.dex 之后，下一件事是读懂它。这里做三件：
//   1. 校验头（魔数/版本/大小/校验和/字节序）
//   2. 按 string_ids → type_ids → proto_ids → method_ids → class_defs → class_data
//      把结构走通，能列出类名、方法签名
//   3. 一个只有若干 opcode 的解释器，能执行最简单的 static 方法
//
// 解释器刻意做得极小：const-string / const-4 / const-16 / invoke-static /
// return-void / nop。目的不是"支持 DEX"，而是把"APK → dex → 字节码 → 内核里
// 的打印"这条链路第一次打通 —— 后面的解释器都是在它上面加 opcode。
#![allow(dead_code)]

const ENDIAN_CONSTANT: u32 = 0x1234_5678;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn adler32(b: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut s: u32 = 0;
    for &x in b {
        a = (a + x as u32) % 65521;
        s = (s + a) % 65521;
    }
    (s << 16) | a
}

pub struct Header {
    pub fields: u32,
    pub f_off: u32,
    pub file_size: u32,
    pub strings: u32,
    pub types: u32,
    pub protos: u32,
    pub methods: u32,
    pub classes: u32,
    pub data_size: u32,
    pub checksum_ok: bool,
    pub s_off: u32,
    pub t_off: u32,
    pub p_off: u32,
    pub m_off: u32,
    pub c_off: u32,
}

pub fn header(d: &[u8]) -> Result<Header, &'static str> {
    if d.len() < 0x70 {
        return Err("DEX 太短");
    }
    if &d[..4] != b"dex\n" {
        return Err("魔数不是 dex\\n");
    }
    let file_size = u32le(d, 32);
    if file_size as usize > d.len() {
        return Err("文件大小字段超过实际内容");
    }
    if u32le(d, 40) != ENDIAN_CONSTANT {
        return Err("字节序标记不是 0x12345678");
    }
    let checksum_ok = u32le(d, 8) == adler32(&d[12..(file_size as usize).min(d.len())]);
    Ok(Header {
        file_size,
        strings: u32le(d, 56),
        s_off: u32le(d, 60),
        types: u32le(d, 64),
        t_off: u32le(d, 68),
        protos: u32le(d, 72),
        p_off: u32le(d, 76),
        fields: u32le(d, 80),
        f_off: u32le(d, 84),
        methods: u32le(d, 88),
        m_off: u32le(d, 92),
        classes: u32le(d, 96),
        c_off: u32le(d, 100),
        data_size: u32le(d, 104),
        checksum_ok,
    })
}

fn uleb(d: &[u8], mut p: usize) -> Result<(u32, usize), &'static str> {
    let mut v = 0u32;
    let mut shift = 0;
    loop {
        let b = *d.get(p).ok_or("uleb128 越界")?;
        p += 1;
        v |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 == 0 {
            return Ok((v, p));
        }
        shift += 7;
        if shift > 28 {
            return Err("uleb128 过长");
        }
    }
}

/// string_ids[idx] → 字符串内容（按 0x00 终止，MUTF-8 里我们的样本是纯 ASCII）
pub fn string<'a>(d: &'a [u8], h: &Header, idx: u32) -> Result<&'a str, &'static str> {
    if idx >= h.strings {
        return Err("字符串索引越界");
    }
    let off = u32le(d, h.s_off as usize + idx as usize * 4) as usize;
    let (_n, p) = uleb(d, off)?;
    let mut end = p;
    while end < d.len() && d[end] != 0 {
        end += 1;
    }
    core::str::from_utf8(&d[p..end]).map_err(|_| "字符串不是 UTF-8")
}

pub fn type_desc<'a>(d: &'a [u8], h: &Header, idx: u32) -> Result<&'a str, &'static str> {
    if idx >= h.types {
        return Err("类型索引越界");
    }
    string(d, h, u32le(d, h.t_off as usize + idx as usize * 4))
}

pub struct MethodRef<'a> {
    pub class: &'a str,
    pub name: &'a str,
    pub shorty: &'a str,
}

pub fn method<'a>(d: &'a [u8], h: &Header, idx: u32) -> Result<MethodRef<'a>, &'static str> {
    if idx >= h.methods {
        return Err("方法索引越界");
    }
    let p = h.m_off as usize + idx as usize * 8;
    let class_idx = u16le(d, p) as u32;
    let proto_idx = u16le(d, p + 2) as u32;
    let name_idx = u32le(d, p + 4);
    let proto = h.p_off as usize + proto_idx as usize * 12;
    Ok(MethodRef {
        class: type_desc(d, h, class_idx)?,
        name: string(d, h, name_idx)?,
        shorty: string(d, h, u32le(d, proto))?,
    })
}

/// 找类里某个 static/direct 方法的 code_off
pub fn code_of(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let mut i = 0u32;
    while i < h.classes {
        let p = h.c_off as usize + i as usize * 32;
        let class_idx = u32le(d, p);
        if type_desc(d, h, class_idx)? == class_desc {
            let cd_off = u32le(d, p + 24);
            if cd_off == 0 {
                return Err("类没有 class_data");
            }
            let mut q = cd_off as usize;
            let (sf, q2) = uleb(d, q)?;
            q = q2;
            let (inf, q3) = uleb(d, q)?;
            q = q3;
            let (dm, q4) = uleb(d, q)?;
            q = q4;
            let (vm, q5) = uleb(d, q)?;
            q = q5;
            // 跳过 static + instance fields
            let mut k = 0;
            while k < sf + inf {
                let (_, n1) = uleb(d, q)?;
                let (_, n2) = uleb(d, n1)?;
                q = n2;
                k += 1;
            }
            // direct methods：method_idx_diff 累加
            let mut midx = 0u32;
            let mut k = 0;
            while k < dm {
                let (diff, n1) = uleb(d, q)?;
                let (acc, n2) = uleb(d, n1)?;
                let (coff, n3) = uleb(d, n2)?;
                midx += diff;
                q = n3;
                let m = method(d, h, midx)?;
                if m.name == method_name {
                    return Ok(coff);
                }
                k += 1;
            }
            // virtual methods 的 idx 从 0 重新累加
            let mut midx2 = 0u32;
            let mut k2 = 0;
            while k2 < vm {
                let (diff, n1) = uleb(d, q)?;
                let (acc, n2) = uleb(d, n1)?;
                let (coff, n3) = uleb(d, n2)?;
                midx2 += diff;
                q = n3;
                let m = method(d, h, midx2)?;
                if m.name == method_name {
                    let _ = acc;
                    return Ok(coff);
                }
                k2 += 1;
            }
            return Err("类里没有这个方法");
        }
        i += 1;
    }
    Err("没有这个类")
}

/// 执行 `class_desc` 里的 `method_name`。
/// 返回执行的指令条数；调用的外部方法只有 `Lcom/aster/Log;->i(String)`。
/// 极小的对象堆：每对象 8 个整型字段。够 demo 用，也是后面真堆的雏形。
#[derive(Clone, Copy)]
struct ObjState {
    used: bool,
    class_idx: u32,
    fields: [i32; 8],
}

const NOBJ: usize = 8;
static mut HEAP: [ObjState; NOBJ] = [ObjState { used: false, class_idx: 0, fields: [0; 8] }; NOBJ];

// —— 数组（APK-6）——
// 寄存器里对象用 0..NOBJ，数组用 0x100+i 这一段，两者不会撞车。
#[derive(Clone, Copy)]
struct Arr {
    len: u32,
    d: [i32; 8],
}

const NARR: usize = 4;
const ARR_TAG: i32 = 0x100;
static mut ARRS: [Arr; NARR] = [Arr { len: 0, d: [0; 8] }; NARR];
static mut NARR_USED: usize = 0;

fn arr_new(len: u32) -> Result<i32, &'static str> {
    unsafe {
        if NARR_USED >= NARR {
            return Err("数组堆满（最多 4 个）");
        }
        if len > 8 {
            return Err("数组超过 8 个元素（v0 上限）");
        }
        let i = NARR_USED;
        ARRS[i] = Arr { len, d: [0; 8] };
        NARR_USED += 1;
        Ok(ARR_TAG + i as i32)
    }
}

fn arr_idx(v: i32) -> Result<usize, &'static str> {
    let i = v - ARR_TAG;
    if i < 0 || i as usize >= unsafe { NARR_USED } {
        return Err("不是有效的数组引用");
    }
    Ok(i as usize)
}

fn arr_reset() {
    unsafe {
        NARR_USED = 0;
        let mut i = 0;
        while i < NARR {
            ARRS[i] = Arr { len: 0, d: [0; 8] };
            i += 1;
        }
    }
}

fn heap_reset() {
    arr_reset();
    let mut i = 0;
    while i < NOBJ {
        unsafe {
            HEAP[i] = ObjState { used: false, class_idx: 0, fields: [0; 8] };
        }
        i += 1;
    }
}

fn heap_new(class_idx: u32) -> Result<usize, &'static str> {
    let mut i = 0;
    while i < NOBJ {
        if !unsafe { HEAP[i].used } {
            unsafe {
                HEAP[i] = ObjState { used: true, class_idx, fields: [0; 8] };
            }
            return Ok(i);
        }
        i += 1;
    }
    Err("对象堆满（8 个）")
}

fn heap_idx(v: i32) -> Result<usize, &'static str> {
    if v < 0 || v as usize >= NOBJ || !unsafe { HEAP[v as usize].used } {
        return Err("不是有效对象引用");
    }
    Ok(v as usize)
}

/// 执行一个方法。`args` 是调用方传进来的寄存器值（this 在第一个位置）。
fn exec(
    d: &[u8],
    h: &Header,
    coff: u32,
    args: &[i32],
    depth: u32,
    steps: &mut u32,
) -> Result<i32, &'static str> {
    if depth > 8 {
        return Err("调用栈超过 8 层");
    }
    let p = coff as usize;
    if p + 16 > d.len() {
        return Err("code_item 越界");
    }
    let regs_n = u16le(d, p) as usize;
    let ins_n = u16le(d, p + 2) as usize;
    let n = u32le(d, p + 12) as usize;
    let base = p + 16;
    if base + n * 2 > d.len() {
        return Err("指令越界");
    }
    if regs_n > 16 || ins_n > 16 || args.len() > ins_n {
        return Err("寄存器或参数数量超出解释器上限");
    }
    let unit = |i: usize| -> u16 { u16le(d, base + i * 2) };
    let sx8 = |v: u16| -> i32 { (v as i8) as i32 };

    let mut regs = [0i32; 16];
    if ins_n > 0 {
        let start = regs_n - ins_n;
        let mut k = 0;
        while k < args.len() {
            regs[start + k] = args[k];
            k += 1;
        }
    }
    let mut pc: i32 = 0;
    let mut last: i32 = 0;
    let mut ret = false;
    let mut rv = 0i32;

    while pc >= 0 && (pc as usize) < n && !ret {
        let u = unit(pc as usize);
        let op = (u & 0xff) as u8;
        match op {
            0x00 => pc += 1,
            0x01 => {
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                regs[a] = regs[b];
                pc += 1;
            }
            0x0a => {
                // move-result vAA
                let a = ((u >> 8) & 0xff) as usize;
                regs[a] = last;
                pc += 1;
            }
            0x12 => {
                let a = ((u >> 8) & 0xf) as usize;
                let lit = (u >> 12) as i32;
                regs[a] = if lit & 8 != 0 { lit - 16 } else { lit };
                pc += 1;
            }
            0x13 => {
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("const/16 被截断");
                }
                regs[a] = unit(pc as usize + 1) as i32;
                pc += 2;
            }
            0x1a => {
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("const-string 被截断");
                }
                let idx = unit(pc as usize + 1) as u32;
                string(d, h, idx)?;
                regs[a] = idx as i32;
                pc += 2;
            }
            0x22 => {
                // new-instance vAA, type@BBBB
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("new-instance 被截断");
                }
                let type_idx = unit(pc as usize + 1) as u32;
                type_desc(d, h, type_idx)?;
                regs[a] = heap_new(type_idx)? as i32;
                pc += 2;
            }
            0x21 => {
                // array-length vA, vB  (12x)
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                let i = arr_idx(regs[b])?;
                regs[a] = unsafe { ARRS[i].len } as i32;
                pc += 1;
            }
            0x23 => {
                // new-array vA, vB, type@CCCC  (22c)
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("new-array 被截断");
                }
                let ty = unit(pc as usize + 1) as u32;
                let td = type_desc(d, h, ty)?;
                if !td.starts_with('[') {
                    return Err("new-array 的 type 不是数组描述符");
                }
                if regs[b] < 0 {
                    return Err("数组长度是负数");
                }
                regs[a] = arr_new(regs[b] as u32)?;
                pc += 2;
            }
            0x44 => {
                // aget vA, vB, vC  (23x)
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("aget 被截断");
                }
                let w = unit(pc as usize + 1);
                let b = (w & 0xff) as usize;
                let c = ((w >> 8) & 0xff) as usize;
                let i = arr_idx(regs[b])?;
                let k = regs[c];
                let len = unsafe { ARRS[i].len } as i32;
                if k < 0 || k >= len {
                    return Err("数组下标越界（aget）");
                }
                regs[a] = unsafe { ARRS[i].d[k as usize] };
                pc += 2;
            }
            0x4b => {
                // aput vA, vB, vC  (23x)
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("aput 被截断");
                }
                let w = unit(pc as usize + 1);
                let b = (w & 0xff) as usize;
                let c = ((w >> 8) & 0xff) as usize;
                let i = arr_idx(regs[b])?;
                let k = regs[c];
                let len = unsafe { ARRS[i].len } as i32;
                if k < 0 || k >= len {
                    return Err("数组下标越界（aput）");
                }
                unsafe { ARRS[i].d[k as usize] = regs[a] };
                pc += 2;
            }
            0x28 => pc += sx8((u >> 8) & 0xff),
            0x29 => {
                if (pc as usize) + 1 >= n {
                    return Err("goto/16 被截断");
                }
                pc += unit(pc as usize + 1) as i16 as i32;
            }
            0x31..=0x36 => {
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("if-test 被截断");
                }
                let off = sx8(unit(pc as usize + 1) >> 8);
                let x = regs[a];
                let y = regs[b];
                let take = match op {
                    0x31 => x == y,
                    0x32 => x != y,
                    0x33 => x < y,
                    0x34 => x >= y,
                    0x35 => x > y,
                    _ => x <= y,
                };
                pc += if take { off } else { 2 };
            }
            0x38..=0x3d => {
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("if-testz 被截断");
                }
                let off = sx8(unit(pc as usize + 1) >> 8);
                let x = regs[a];
                let take = match op {
                    0x38 => x == 0,
                    0x39 => x != 0,
                    0x3a => x < 0,
                    0x3b => x >= 0,
                    0x3c => x > 0,
                    _ => x <= 0,
                };
                pc += if take { off } else { 2 };
            }
            0x52 => {
                // iget vA, vB, field@CCCC  (22c)
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("iget 被截断");
                }
                let w = unit(pc as usize + 1);
                let f = (w & 0xffff) as usize;
                let obj = heap_idx(regs[b])?;
                if f >= 8 {
                    return Err("字段槽越界");
                }
                regs[a] = unsafe { HEAP[obj].fields[f] };
                pc += 2;
            }
            0x59 => {
                // iput vA, vB, field@CCCC
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("iput 被截断");
                }
                let w = unit(pc as usize + 1);
                let f = (w & 0xffff) as usize;
                let obj = heap_idx(regs[b])?;
                if f >= 8 {
                    return Err("字段槽越界");
                }
                unsafe { HEAP[obj].fields[f] = regs[a] };
                pc += 2;
            }
            0x90..=0x94 => {
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("算术指令被截断");
                }
                let w = unit(pc as usize + 1);
                let b = ((w >> 8) & 0xff) as usize;
                let c = (w & 0xff) as usize;
                let x = regs[b];
                let y = regs[c];
                regs[a] = match op {
                    0x90 => x.wrapping_add(y),
                    0x91 => x.wrapping_sub(y),
                    0x92 => x.wrapping_mul(y),
                    0x93 => {
                        if y == 0 {
                            return Err("整数除零");
                        }
                        x.wrapping_div(y)
                    }
                    _ => {
                        if y == 0 {
                            return Err("整数除零");
                        }
                        x.wrapping_rem(y)
                    }
                };
                pc += 2;
            }
            0xd0 => {
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("add-int/lit16 被截断");
                }
                regs[a] = regs[b].wrapping_add(unit(pc as usize + 1) as i16 as i32);
                pc += 2;
            }
            0xd8 => {
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("add-int/lit8 被截断");
                }
                let w = unit(pc as usize + 1);
                let b = ((w >> 8) & 0xff) as usize;
                regs[a] = regs[b].wrapping_add(sx8(w & 0xff));
                pc += 2;
            }
            0x6e | 0x70 | 0x71 => {
                // invoke-virtual / invoke-direct / invoke-static （都是 35c）
                let argc = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 2 >= n {
                    return Err("invoke 被截断");
                }
                let midx = unit(pc as usize + 1) as u32;
                let word = unit(pc as usize + 2);
                let mut args = [0i32; 5];
                let mut k = 0;
                let reg_of = [
                    (word & 0xf) as usize,
                    ((word >> 4) & 0xf) as usize,
                    ((word >> 8) & 0xf) as usize,
                    ((word >> 12) & 0xf) as usize,
                    ((u >> 8) & 0xf) as usize,
                ];
                while k < argc {
                    args[k] = regs[reg_of[k]];
                    k += 1;
                }
                let m = method(d, h, midx)?;
                if m.class == "Lcom/aster/Log;" {
                    if m.name == "i" {
                        let s = string(d, h, args[0] as u32)?;
                        crate::println!("  [DEX] {}", s);
                    } else if m.name == "n" {
                        crate::println!("  [DEX] {}", args[0]);
                    } else {
                        return Err("Log 上没有这个方法");
                    }
                    last = 0;
                } else {
                    let c2 = code_of(d, h, m.class, m.name)?;
                    last = exec(d, h, c2, &args[..argc], depth + 1, steps)?;
                }
                pc += 3;
            }
            0x0e => {
                ret = true;
                rv = 0;
                pc += 1;
            }
            0x0f => {
                // return vAA
                let a = ((u >> 8) & 0xff) as usize;
                ret = true;
                rv = regs[a];
                pc += 1;
            }
            _ => {
                crate::println!("  [DEX] 不支持的 opcode {:#04x} @pc {}", op, pc);
                return Err("opcode 未实现");
            }
        }
        *steps += 1;
    }
    Ok(rv)
}

pub fn run(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let coff = code_of(d, h, class_desc, method_name)?;
    heap_reset();
    let mut steps = 0u32;
    exec(d, h, coff, &[], 0, &mut steps)?;
    Ok(steps)
}

pub fn field_name<'a>(d: &'a [u8], h: &Header, idx: u32) -> Result<&'a str, &'static str> {
    if idx >= h.fields {
        return Err("字段索引越界");
    }
    let p = h.f_off as usize + idx as usize * 8;
    string(d, h, u32le(d, p + 4))
}

pub fn dump(d: &[u8]) -> Result<(), &'static str> {
    let h = header(d)?;
    crate::println!(
        "  DEX {} 字节，字符串 {} / 类型 {} / 原型 {} / 方法 {} / 类 {}，data {} 字节",
        h.file_size, h.strings, h.types, h.protos, h.methods, h.classes, h.data_size
    );
    crate::println!("  checksum adler32 {}", if h.checksum_ok { "PASS" } else { "FAIL" });
    let mut i = 0u32;
    while i < h.strings {
        crate::println!("  str[{}] {}", i, string(d, &h, i)?);
        i += 1;
    }
    let mut i = 0u32;
    while i < h.methods {
        let m = method(d, &h, i)?;
        crate::println!("  method[{}] {}->{}  shorty {}", i, m.class, m.name, m.shorty);
        i += 1;
    }
    Ok(())
}
