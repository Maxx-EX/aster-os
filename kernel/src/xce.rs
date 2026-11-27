// Aster Kernel — XCE（Aster eXecutable）原生程序格式
//
// 这是 Aster 自己的可执行格式，不是 ELF、不是 PE。装的东西是"一段链接到
// 0x4440_0000 的 EL0 扁平二进制"，外面套 28 字节头做自我描述与完整性校验：
//
//   0x00 magic "XCE1" / 0x04 version / 0x06 kind / 0x08 entry_va
//   0x0C body_size / 0x10 crc32(body) / 0x14 stack_va / 0x18 flags
//   0x1C body
//
// 为什么不做更花哨的段表：内核的用户地址空间是固定一页代码 + 一页栈，
// 多段加载只会把 MMU 和页帧分配器拖进这一层。等哪天用户程序要动态申请内存，
// 再把 "kind=2 内核模块 / 多段" 打开。
use crate::{exceptions, mmu, sched, timer};

pub const HDR: usize = 28;
pub const MAGIC: &[u8; 4] = b"XCE1";

#[derive(Clone, Copy)]
pub struct Info {
    pub version: u16,
    pub kind: u16,
    pub entry: u32,
    pub body_size: u32,
    pub crc: u32,
    pub stack: u32,
    pub flags: u32,
    pub crc_ok: bool,
}

/// CRC32（IEEE 802.3，反射多项式 0xEDB88320）。按位算，body 只有几 KB。
pub fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in data {
        c ^= b as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            k += 1;
        }
    }
    !c
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

pub fn parse(b: &[u8]) -> Result<Info, &'static str> {
    if b.len() < HDR {
        return Err("文件太短，连 XCE 头都没有");
    }
    if &b[0..4] != MAGIC {
        return Err("魔数不是 XCE1");
    }
    let version = be16(b, 4);
    if version != 1 {
        return Err("版本不是 1（当前内核只认 v1）");
    }
    let kind = be16(b, 6);
    let entry = be32(b, 8);
    let size = be32(b, 12) as usize;
    let crc = be32(b, 16);
    let stack = be32(b, 20);
    let flags = be32(b, 24);
    if size > b.len() - HDR {
        return Err("body 长度超出文件");
    }
    if size > 0x1000 {
        return Err("body 超过一页（4 KiB），当前加载器装不下");
    }
    let crc_ok = crc32(&b[HDR..HDR + size]) == crc;
    Ok(Info { version, kind, entry, body_size: size as u32, crc, stack, flags, crc_ok })
}

pub fn kind_name(k: u16) -> &'static str {
    match k {
        0 => "EL0 可执行",
        1 => "数据包",
        2 => "内核模块（未开放）",
        _ => "未知",
    }
}

/// 校验并跑起来。返回任务退出码（所有任务结束后内核重新接管）。
pub fn run(b: &[u8]) -> Result<u64, &'static str> {
    let info = parse(b)?;
    if info.kind != 0 {
        return Err("这个 XCE 不是可执行类型");
    }
    if !info.crc_ok {
        return Err("CRC32 校验失败，拒绝装载");
    }
    if info.entry as u64 != mmu::user_va::CODE {
        return Err("入口地址不在用户代码页（应为 0x44400000）");
    }
    let body = &b[HDR..HDR + info.body_size as usize];

    sched::reset();
    let sp = mmu::new_space().ok_or("页帧分配器耗尽")?;
    mmu::load_image(&sp, body)?;
    if sched::spawn(sp).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[XCE] 装载 {} 字节，入口 {:#x}，切 EL0", body.len(), info.entry);
    timer::arm();
    mmu::activate(&sp);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 只校验不跑，给 `xce info` 用
pub fn describe(b: &[u8]) -> Result<Info, &'static str> {
    parse(b)
}
