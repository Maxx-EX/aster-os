// Aster Kernel — 系统更新逻辑（A/B 槽 + 更新包校验 + 回滚）
//
// 更新这件事拆成四步，每一步都留得下证据：
//
//   check    读更新包（AUP）头：版本、目标构建、CRC32、EX3。
//            只做校验，不碰任何东西。
//   stage    把校验结果记成"待应用"，写进 AsterFS `upd.state`。
//   apply    切到 B 槽：写 `boot.conf` 的 slot=B，并记录上一次的槽。
//   rollback 切回 A 槽，并把失败原因留在 state 里。
//
// 引导器目前还是加载内嵌镜像，读盘启动是 BOARD-6 的事；所以 apply 之后
// 会把这句话如实打印出来，不假装重启就生效了。
use crate::{afs, xce};

pub const HDR: usize = 28;
pub const MAGIC: &[u8; 4] = b"AUP1";
pub const CURRENT_VER: &str = "0.9";

#[derive(Clone, Copy)]
pub struct Pkg {
    pub version: u16,
    pub build: u16,   // 目标构建号（0 = 任意）
    pub body_size: u32,
    pub crc: u32,
    pub crc_ok: bool,
    pub ex3_ok: bool,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

/// 更新包头：
///   0x00 magic "AUP1" / 0x04 version u16 / 0x06 build u16
///   0x08 body_size u32 / 0x0C crc32(body) u32
///   0x10 ex3 12 字节（本机构建 EX3 的前 12 字节，用来确认"给这台设备打的"）
///   0x1C body
pub fn parse(b: &[u8]) -> Result<Pkg, &'static str> {
    if b.len() < HDR {
        return Err("文件太短，不是 AUP 更新包");
    }
    if &b[0..4] != MAGIC {
        return Err("魔数不是 AUP1");
    }
    let version = le16(b, 4);
    let build = le16(b, 6);
    let size = le32(b, 8) as usize;
    let crc = le32(b, 12);
    if size > b.len() - HDR {
        return Err("body 长度超出文件（包被截断）");
    }
    let crc_ok = xce::crc32(&b[HDR..HDR + size]) == crc;
    let ex3 = &b[16..16 + 12];
    let me = crate::ex3::EX3;
    let ex3_ok = ex3.iter().zip(me.iter()).all(|(a, b)| *a == *b);
    Ok(Pkg { version, build, body_size: size as u32, crc, crc_ok, ex3_ok })
}

pub fn version_str(v: u16) -> &'static str {
    match v {
        0x0009 => "0.9",
        0x0008 => "0.8",
        _ => "未知",
    }
}

// ---------------- 槽状态（AsterFS 上的两个小文件） ----------------

pub fn boot_slot() -> u8 {
    let mut buf = [0u8; 64];
    match afs::read_file("boot.conf", &mut buf) {
        Ok(n) => {
            let s = core::str::from_utf8(&buf[..n]).unwrap_or("");
            if s.contains("slot=B") {
                return 1;
            }
            0
        }
        Err(_) => 0,
    }
}

fn write_conf(body: &str) -> Result<(), &'static str> {
    afs::write_file("boot.conf", body.as_bytes()).map(|_| ())?;
    Ok(())
}

pub fn set_slot(b: u8) -> Result<(), &'static str> {
    let s = if b == 1 {
        "slot=B\nnote=staged update, bootloader reads disk after BOARD-6\n"
    } else {
        "slot=A\nnote=embedded image\n"
    };
    write_conf(s)
}

pub fn state() -> &'static str {
    // AsterFS 没有 mmap，读到的字节在局部缓冲里；这里只回固定文案，
    // 细节由调用方自己从文件读（shell 里就是这么做的）。
    let mut buf = [0u8; 128];
    match afs::read_file("upd.state", &mut buf) {
        Ok(n) if n > 0 => "见 upd.state",
        _ => "（还没有更新记录）",
    }
}

pub fn set_state(s: &str) {
    let _ = afs::write_file("upd.state", s.as_bytes());
}

pub fn clear_state() {
    // AsterFS 没有 unlink；把 state 写成"无"即可
    let _ = afs::write_file("upd.state", b"none\n");
}
