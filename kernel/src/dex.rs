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
            let _ = vm;
            return Err("类里没有这个方法");
        }
        i += 1;
    }
    Err("没有这个类")
}

/// 执行 `class_desc` 里的 `method_name`。
/// 返回执行的指令条数；调用的外部方法只有 `Lcom/aster/Log;->i(String)`。
pub fn run(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let coff = code_of(d, h, class_desc, method_name)?;
    let p = coff as usize;
    if p + 16 > d.len() {
        return Err("code_item 越界");
    }
    let n = u32le(d, p + 12) as usize;
    let base = p + 16;
    if base + n * 2 > d.len() {
        return Err("指令越界");
    }
    let unit = |i: usize| -> u16 { u16le(d, base + i * 2) };
    let sx8 = |v: u16| -> i32 { ((v as i8) as i32) };

    let mut regs = [0i32; 16];
    let mut pc: i32 = 0;
    let mut steps = 0u32;
    while pc >= 0 && (pc as usize) < n {
        let u = unit(pc as usize);
        let op = (u & 0xff) as u8;
        match op {
            0x00 => pc += 1,
            0x01 => {
                // move vA, vB  (12x)
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                regs[a] = regs[b];
                pc += 1;
            }
            0x12 => {
                // const/4 vA, #+B
                let a = ((u >> 8) & 0xf) as usize;
                let lit = (u >> 12) as i32;
                regs[a] = if lit & 8 != 0 { lit - 16 } else { lit };
                pc += 1;
            }
            0x13 => {
                // const/16 vAA, #+BBBB
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("const/16 被截断");
                }
                regs[a] = unit(pc as usize + 1) as i32;
                pc += 2;
            }
            0x1a => {
                // const-string vAA, string@BBBB
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("const-string 被截断");
                }
                let idx = unit(pc as usize + 1) as u32;
                string(d, h, idx)?;
                regs[a] = idx as i32;
                pc += 2;
            }
            0x28 => {
                // goto +AA  (10t)
                pc += sx8(((u >> 8) & 0xff) as u16);
            }
            0x29 => {
                // goto/16 +BBBB  (20t)
                if (pc as usize) + 1 >= n {
                    return Err("goto/16 被截断");
                }
                let off = unit(pc as usize + 1) as i16 as i32;
                pc += off;
            }
            0x31..=0x36 => {
                // if-test vA, vB, +CCCC  (22t)
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
                // if-testz vAA, +BBBB  (21t)
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
            0x90..=0x94 => {
                // add/sub/mul/div/rem-int vAA, vBB, vCC  (23x)
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
                // add-int/lit16 vA, vB, #+CCCC  (22s)
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("add-int/lit16 被截断");
                }
                regs[a] = regs[b].wrapping_add(unit(pc as usize + 1) as i16 as i32);
                pc += 2;
            }
            0xd8 => {
                // add-int/lit8 vAA, vBB, #+CC  (22b)
                let a = ((u >> 8) & 0xff) as usize;
                let w = if (pc as usize) + 1 >= n {
                    return Err("add-int/lit8 被截断");
                } else {
                    unit(pc as usize + 1)
                };
                let b = ((w >> 8) & 0xff) as usize;
                regs[a] = regs[b].wrapping_add(sx8(w & 0xff));
                pc += 2;
            }
            0x71 => {
                // invoke-static {vC, ...}, method@BBBB  (35c)
                let argc = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 2 >= n {
                    return Err("invoke-static 被截断");
                }
                let midx = unit(pc as usize + 1) as u32;
                let word = unit(pc as usize + 2);
                let args = [
                    (word & 0xf) as usize,
                    ((word >> 4) & 0xf) as usize,
                    ((word >> 8) & 0xf) as usize,
                    ((word >> 12) & 0xf) as usize,
                    ((u >> 8) & 0xf) as usize,
                ];
                let m = method(d, h, midx)?;
                if m.class == "Lcom/aster/Log;" && m.name == "i" && argc == 1 {
                    let s = string(d, h, regs[args[0]] as u32)?;
                    crate::println!("  [DEX] {}", s);
                } else if m.class == "Lcom/aster/Log;" && m.name == "n" && argc == 1 {
                    crate::println!("  [DEX] {}", regs[args[0]]);
                } else {
                    crate::println!("  [DEX] 未知外部方法 {}->{}{}", m.class, m.name, m.shorty);
                    return Err("调用了本解释器不认识的方法");
                }
                pc += 3;
            }
            0x0e | 0x0f => return Ok(steps),
            _ => {
                crate::println!("  [DEX] 遇到不支持的 opcode {:#04x} @pc {}", op, pc);
                return Err("opcode 未实现");
            }
        }
        steps += 1;
    }
    Ok(steps)
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
