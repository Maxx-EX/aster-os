//! AsterDisk：Aster 自己的磁盘布局
//!
//!   扇区 0  磁盘头   magic "ADSK" + 版本 + 扇区大小 + 总扇区数 + 卷名 + CRC32
//!   扇区 1  分区表   4 × 64 字节，名字 / 起始 LBA / 扇区数 / 类型
//!
//! 不学 MBR 也不学 GPT：那两套是为了兼容别人的，这里只需要自己认得。

use crate::blk;

pub const SEC: usize = 512;
pub const MAGIC: [u8; 4] = *b"ADSK";
pub const VERSION: u32 = 1;
pub const MAX_PARTS: usize = 4;

pub struct Part {
    pub name: [u8; 16],
    pub start: u64,
    pub sectors: u64,
    pub kind: u32,
}

pub struct Info {
    pub version: u32,
    pub total: u64,
    pub name: [u8; 32],
    pub parts: [Option<Part>; MAX_PARTS],
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
        }
    }
    !c
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn get_u64(buf: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(b)
}

/// 格式化：扇区 0 写头，扇区 1 写分区表（整盘一个分区，其余三个空）
pub fn format(name: &str) -> bool {
    if !blk::available() {
        return false;
    }
    let total = blk::sectors();
    let mut head = [0u8; SEC];
    head[0..4].copy_from_slice(&MAGIC);
    put_u32(&mut head, 4, VERSION);
    put_u32(&mut head, 8, SEC as u32);
    put_u32(&mut head, 12, if total > u32::MAX as u64 { u32::MAX } else { total as u32 });
    let nb = name.as_bytes();
    let n = nb.len().min(31);
    head[16..16 + n].copy_from_slice(&nb[..n]);
    let crc = crc32(&head[0..48]);
    put_u32(&mut head, 48, crc);
    if !blk::write(0, &head) {
        return false;
    }

    let mut pt = [0u8; SEC];
    // 第 0 项：AsterFS 占住 2..total 的整块
    pt[0..7].copy_from_slice(b"asterfs");
    put_u64(&mut pt, 16, 2);
    put_u64(&mut pt, 24, total.saturating_sub(2));
    put_u32(&mut pt, 32, 1); // kind 1 = AsterFS
    let pcrc = crc32(&pt[0..60]);
    put_u32(&mut pt, 60, pcrc);
    if !blk::write(1, &pt) {
        return false;
    }
    true
}

pub fn probe() -> Option<Info> {
    if !blk::available() {
        return None;
    }
    let mut head = [0u8; SEC];
    if !blk::read(0, &mut head) {
        return None;
    }
    if head[0..4] != MAGIC {
        return None;
    }
    if crc32(&head[0..48]) != get_u32(&head, 48) {
        return None;
    }
    let version = get_u32(&head, 4);
    let total = get_u32(&head, 12) as u64;
    let mut name = [0u8; 32];
    name.copy_from_slice(&head[16..48]);

    let mut parts: [Option<Part>; MAX_PARTS] = [None, None, None, None];
    let mut pt = [0u8; SEC];
    if blk::read(1, &mut pt) {
        for i in 0..MAX_PARTS {
            let base = i * 64;
            if pt[base..base + 8] == [0u8; 8] {
                continue;
            }
            if crc32(&pt[base..base + 60]) != get_u32(&pt, base + 60) {
                continue;
            }
            let mut pname = [0u8; 16];
            pname.copy_from_slice(&pt[base..base + 16]);
            parts[i] = Some(Part {
                name: pname,
                start: get_u64(&pt, base + 16),
                sectors: get_u64(&pt, base + 24),
                kind: get_u32(&pt, base + 32),
            });
        }
    }

    Some(Info { version, total, name, parts })
}

pub fn name_str(n: &[u8; 32]) -> &str {
    let len = n.iter().position(|&c| c == 0).unwrap_or(32);
    core::str::from_utf8(&n[..len]).unwrap_or("?")
}
