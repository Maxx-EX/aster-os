//! AsterFS v0 —— 4 KiB 块，inode 表，位图，32 B 目录项。
//!
//! 块号相对分区起点（AsterDisk 分区 0 的 LBA）：
//!   0        超级块：AFS1 / 版本 / 几何 / CRC32 / 卷名
//!   1        inode 位图
//!   2        块位图
//!   3..7     inode 表，256 项 × 64 B
//!   7..      数据块
//!
//! inode 64 B：type / mode / size / parent / mtime / 10 个直接块指针
//! 目录项 32 B：名字 24 B / inode / 类型

use crate::disk;

pub const BS: usize = 4096;
pub const NDIRECT: usize = 10;
pub const T_FILE: u16 = 1;
pub const T_DIR: u16 = 2;

const SEC: usize = 512;
const SPB: u64 = 8;
const INODE_SIZE: usize = 64;
const ENT: usize = 32;
const NAME_MAX: usize = 24;
const MAGIC: [u8; 4] = *b"AFS1";
const MAX_INODES: u32 = 256;

const INODE_BITMAP: u32 = 1;
const BLOCK_BITMAP: u32 = 2;
const INODE_TABLE: u32 = 3;
const INODE_TABLE_BLOCKS: u32 = 4;
const DATA_START: u32 = 7;

struct Cell(core::cell::UnsafeCell<[u8; BS]>);
unsafe impl Sync for Cell {}

static CELL: Cell = Cell(core::cell::UnsafeCell::new([0; BS]));
static CELL2: Cell = Cell(core::cell::UnsafeCell::new([0; BS]));
/// inode_read / inode_write 专用：它们会嵌在目录遍历里调用，
/// 跟目录块共用一张 CELL 时，`&mut` 别名会让编译器把读回当成旧值。
static CELL3: Cell = Cell(core::cell::UnsafeCell::new([0; BS]));

fn buf() -> &'static mut [u8; BS] {
    unsafe { &mut *CELL.0.get() }
}

fn buf2() -> &'static mut [u8; BS] {
    unsafe { &mut *CELL2.0.get() }
}

fn buf3() -> &'static mut [u8; BS] {
    unsafe { &mut *CELL3.0.get() }
}

#[derive(Clone, Copy)]
pub struct Inode {
    pub ino: u32,
    pub typ: u16,
    pub mode: u16,
    pub uid: u16,
    pub gid: u16,
    pub size: u32,
    pub parent: u32,
    pub mtime: u32,
    pub blocks: [u32; NDIRECT],
}

pub struct Super {
    pub blocks: u32,
    pub inodes: u32,
    pub root: u32,
    pub data_start: u32,
    pub data_blocks: u32,
    pub free: u32,
    pub name: [u8; 32],
}

pub struct Stat {
    pub blocks: u32,
    pub free_blocks: u32,
    pub inodes: u32,
    pub used_inodes: u32,
    pub data_start: u32,
}

fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn get_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            let m = (c & 1) as u32;
            c = (c >> 1) ^ (0xedb8_8320 & m.wrapping_neg());
        }
    }
    !c
}

fn base() -> Option<u64> {
    let info = disk::probe()?;
    let p = info.parts[0]?;
    Some(p.start)
}

fn rd(b: u32, out: &mut [u8; BS]) -> bool {
    match crate::cache::get(b) {
        Some(s) => {
            out.copy_from_slice(s);
            true
        }
        None => false,
    }
}

fn wr(b: u32, inp: &[u8; BS]) -> bool {
    match crate::cache::get(b) {
        Some(s) => {
            s.copy_from_slice(inp);
            crate::cache::mark_dirty(b);
            true
        }
        None => false,
    }
}

/// 把缓存里的脏块写回磁盘
pub fn sync() -> u32 {
    crate::cache::flush()
}

fn zero(x: &mut [u8; BS]) {
    for v in x.iter_mut() {
        *v = 0;
    }
}

pub fn super_block() -> Option<Super> {
    let x = buf();
    if !rd(0, x) {
        return None;
    }
    if x[0..4] != MAGIC {
        return None;
    }
    if crc32(&x[0..56]) != get_u32(x, 56) {
        return None;
    }
    let mut name = [0u8; 32];
    name.copy_from_slice(&x[64..96]);
    Some(Super {
        blocks: get_u32(x, 12),
        inodes: get_u32(x, 16),
        root: get_u32(x, 20),
        data_start: get_u32(x, 40),
        data_blocks: get_u32(x, 44),
        free: get_u32(x, 48),
        name,
    })
}

pub fn name_str(s: &Super) -> &str {
    let mut n = 0;
    while n < 32 && s.name[n] != 0 {
        n += 1;
    }
    core::str::from_utf8(&s.name[..n]).unwrap_or("?")
}

fn inode_at(ino: u32) -> Option<(u32, usize)> {
    if ino >= MAX_INODES {
        return None;
    }
    let blk_no = INODE_TABLE + ino / 64;
    let off = (ino as usize % 64) * INODE_SIZE;
    Some((blk_no, off))
}

pub fn inode_read(ino: u32) -> Option<Inode> {
    let (blk_no, off) = inode_at(ino)?;
    let x = buf3();
    if !rd(blk_no, x) {
        return None;
    }
    let typ = get_u16(x, off);
    if typ == 0 {
        return None;
    }
    let mut blocks = [0u32; NDIRECT];
    for i in 0..NDIRECT {
        blocks[i] = get_u32(x, off + 16 + i * 4);
    }
    Some(Inode {
        ino,
        typ,
        mode: get_u16(x, off + 2),
        uid: get_u16(x, off + 56),
        gid: get_u16(x, off + 58),
        size: get_u32(x, off + 4),
        parent: get_u32(x, off + 8),
        mtime: get_u32(x, off + 12),
        blocks,
    })
}

fn inode_write(f: &Inode) -> bool {
    let (blk_no, off) = match inode_at(f.ino) {
        Some(x) => x,
        None => return false,
    };
    let x = buf3();
    if !rd(blk_no, x) {
        return false;
    }
    put_u16(x, off, f.typ);
    put_u16(x, off + 2, f.mode);
    put_u16(x, off + 56, f.uid);
    put_u16(x, off + 58, f.gid);
    put_u32(x, off + 4, f.size);
    put_u32(x, off + 8, f.parent);
    put_u32(x, off + 12, f.mtime);
    for i in 0..NDIRECT {
        put_u32(x, off + 16 + i * 4, f.blocks[i]);
    }
    for i in (off + 60)..(off + 64) {
        x[i] = 0;
    }
    wr(blk_no, &*x)
}

fn inode_alloc() -> Option<u32> {
    let x = buf();
    if !rd(INODE_BITMAP, x) {
        return None;
    }
    for ino in 1..MAX_INODES {
        let i = (ino / 8) as usize;
        let m = 1u8 << (ino % 8);
        if x[i] & m == 0 {
            x[i] |= m;
            if !wr(INODE_BITMAP, &*x) {
                return None;
            }
            return Some(ino);
        }
    }
    None
}

fn block_alloc() -> Option<u32> {
    let sup = super_block()?;
    let x = buf();
    if !rd(BLOCK_BITMAP, x) {
        return None;
    }
    for b in sup.data_start..sup.blocks {
        let i = (b / 8) as usize;
        let m = 1u8 << (b % 8);
        if x[i] & m == 0 {
            x[i] |= m;
            if !wr(BLOCK_BITMAP, &*x) {
                return None;
            }
            return Some(b);
        }
    }
    None
}

fn block_free(b: u32) -> bool {
    let x = buf();
    if !rd(BLOCK_BITMAP, x) {
        return false;
    }
    let i = (b / 8) as usize;
    x[i] &= !(1u8 << (b % 8));
    wr(BLOCK_BITMAP, &*x)
}

fn ent_name(e: &[u8]) -> &str {
    let mut n = 0;
    while n < NAME_MAX && e[n] != 0 {
        n += 1;
    }
    core::str::from_utf8(&e[..n]).unwrap_or("?")
}

pub fn dir_find(d: &Inode, name: &str) -> Option<u32> {
    let nents = (d.size as usize) / ENT;
    let per = BS / ENT;
    let x = buf();
    let mut done = 0usize;
    for bi in 0..NDIRECT {
        if done >= nents {
            break;
        }
        if d.blocks[bi] == 0 {
            break;
        }
        if !rd(d.blocks[bi], x) {
            return None;
        }
        for s in 0..per {
            if done >= nents {
                break;
            }
            let o = s * ENT;
            let ino = get_u32(x, o + 24);
            if ino != 0 && ent_name(&x[o..o + NAME_MAX]) == name {
                return Some(ino);
            }
            done += 1;
        }
    }
    None
}

fn dir_add(dino: u32, name: &str, ino: u32, typ: u16) -> bool {
    let mut d = match inode_read(dino) {
        Some(x) => x,
        None => return false,
    };
    let nb = name.as_bytes();
    if nb.len() > NAME_MAX - 1 {
        return false;
    }
    let per = BS / ENT;
    let max_ents = per * NDIRECT;
    let slot = (d.size as usize) / ENT;
    if slot >= max_ents {
        return false;
    }
    let bi = slot / per;
    let si = slot % per;
    let blk_no = if d.blocks[bi] == 0 {
        match block_alloc() {
            Some(b) => {
                d.blocks[bi] = b;
                b
            }
            None => return false,
        }
    } else {
        d.blocks[bi]
    };
    let x = buf();
    if !rd(blk_no, x) {
        return false;
    }
    let o = si * ENT;
    for i in 0..ENT {
        x[o + i] = 0;
    }
    x[o..o + nb.len()].copy_from_slice(nb);
    put_u32(x, o + 24, ino);
    put_u16(x, o + 28, typ);
    if !wr(blk_no, &*x) {
        return false;
    }
    d.size = ((slot + 1) * ENT) as u32;
    if !inode_write(&d) {
        return false;
    }
    true
}

/// 只读 inode 的 size，走第二缓冲区，不打扰调用方的块缓冲
pub fn inode_size(ino: u32) -> u32 {
    match inode_at(ino) {
        Some((blk_no, off)) => {
            let y = buf2();
            if !rd(blk_no, y) {
                return 0;
            }
            if get_u16(y, off) == 0 {
                return 0;
            }
            get_u32(y, off + 4)
        }
        None => 0,
    }
}

/// 读一行的 (mode, uid, gid)
pub fn meta(name: &str) -> Option<(u16, u16, u16)> {
    let sup = super_block()?;
    let root = inode_read(sup.root)?;
    let ino = dir_find(&root, name)?;
    let f = inode_read(ino)?;
    Some((if f.mode == 0 { 0o644 } else { f.mode }, f.uid, f.gid))
}

pub fn set_meta(
    name: &str,
    mode: Option<u16>,
    uid: Option<u16>,
    gid: Option<u16>,
) -> Result<(u16, u16, u16), &'static str> {
    let sup = super_block().ok_or("磁盘未格式化（先 mkfs）")?;
    let root = inode_read(sup.root).ok_or("根 inode 损坏")?;
    let ino = dir_find(&root, name).ok_or("没有这个文件")?;
    let mut f = inode_read(ino).ok_or("文件 inode 损坏")?;
    if f.typ != T_FILE {
        return Err("不是普通文件");
    }
    if let Some(m) = mode {
        // 低 12 位都收：权限 9 位 + setuid/setgid/sticky（SEC-3）
        f.mode = m & 0o7777;
    }
    if let Some(u) = uid {
        f.uid = u;
    }
    if let Some(g) = gid {
        f.gid = g;
    }
    if !inode_write(&f) {
        return Err("写 inode 失败");
    }
    Ok((f.mode, f.uid, f.gid))
}

/// 调试用：把 inode 原始字段吐出来
pub fn raw_inode(name: &str) -> Option<Inode> {
    let sup = super_block()?;
    let root = inode_read(sup.root)?;
    let ino = dir_find(&root, name)?;
    let f = inode_read(ino)?;
    Some(f)
}

pub fn list(cb: &mut dyn FnMut(&str, u32, u16, u32, u16, u16)) -> Result<(), &'static str> {
    let sup = match super_block() {
        Some(s) => s,
        None => return Err("磁盘未格式化（先 mkfs）"),
    };
    let d = match inode_read(sup.root) {
        Some(x) => x,
        None => return Err("根目录 inode 损坏"),
    };
    if d.typ != T_DIR {
        return Err("根 inode 不是目录");
    }
    let dm = if d.mode == 0 { 0o755 } else { d.mode };
    if !crate::auth::can(dm, d.uid, d.gid, crate::auth::W_READ) {
        return Err("没有权限列这个目录（目录 inode 不是你的）");
    }
    let nents = (d.size as usize) / ENT;
    let per = BS / ENT;
    let x = buf();
    let mut done = 0usize;
    for bi in 0..NDIRECT {
        if done >= nents {
            break;
        }
        if d.blocks[bi] == 0 {
            break;
        }
        if !rd(d.blocks[bi], x) {
            return Err("读目录块失败");
        }
        // 拷一份：inode_read / inode_size 会复用同一块 CELL，直接拿 &x 会被后面的读覆盖
        let mut blk = [0u8; BS];
        blk.copy_from_slice(x);
        for s in 0..per {
            if done >= nents {
                break;
            }
            let o = s * ENT;
            let ino = get_u32(x, o + 24);
            if ino != 0 {
                // 名字先拷出来：inode_read 会复用同一块 CELL 缓冲，直接传 &x.. 会被覆盖成空名
                let mut nm = [0u8; NAME_MAX];
                nm.copy_from_slice(&blk[o..o + NAME_MAX]);
                let typ = get_u16(&blk, o + 28);
                // 先拷名字、再读一次 inode，最后才回调：回调里不要再碰 inode_*（会互相覆盖 CELL）
                let (size, m, u) = match inode_read(ino) {
                    Some(f) => (f.size, if f.mode == 0 { 0o644 } else { f.mode }, f.uid),
                    None => (0, 0o644, 1000),
                };
                cb(ent_name(&nm), ino, typ, size, m, u);
            }
            done += 1;
        }
    }
    Ok(())
}

pub fn read_file(name: &str, out: &mut [u8]) -> Result<usize, &'static str> {
    let sup = super_block().ok_or("磁盘未格式化（先 mkfs）")?;
    let root = inode_read(sup.root).ok_or("根目录 inode 损坏")?;
    let ino = dir_find(&root, name).ok_or("没有这个文件")?;
    let f = inode_read(ino).ok_or("文件 inode 损坏")?;
    if f.typ != T_FILE {
        return Err("不是普通文件");
    }
    let m = if f.mode == 0 { 0o644 } else { f.mode };
    if !crate::auth::can(m, f.uid, f.gid, crate::auth::W_READ) {
        return Err("没有读权限（owner 不是你）");
    }
    let mut got = 0usize;
    let x = buf();
    for bi in 0..NDIRECT {
        if got >= out.len() || f.blocks[bi] == 0 {
            break;
        }
        if !rd(f.blocks[bi], x) {
            return Err("读数据块失败");
        }
        let n = (out.len() - got).min(BS);
        out[got..got + n].copy_from_slice(&x[..n]);
        got += n;
    }
    Ok((f.size as usize).min(got))
}

pub fn write_file(name: &str, data: &[u8]) -> Result<usize, &'static str> {
    let sup = super_block().ok_or("磁盘未格式化（先 mkfs）")?;
    let root = inode_read(sup.root).ok_or("根目录 inode 损坏")?;
    // v0 的 inode 有 10 个直接块号，所以单文件上限 10 × 4 KiB
    if data.len() > BS * NDIRECT {
        return Err("v0 单文件上限 40960 字节（10 个直接块）");
    }
    let mut f = match dir_find(&root, name) {
        Some(ino) => {
            let old = inode_read(ino).ok_or("文件 inode 损坏")?;
            if old.typ != T_FILE {
                return Err("同名的不是普通文件");
            }
            let m = if old.mode == 0 { 0o644 } else { old.mode };
            if !crate::auth::can(m, old.uid, old.gid, crate::auth::W_WRITE) {
                return Err("没有写权限（这个文件不是你的）");
            }
            for i in 0..NDIRECT {
                if old.blocks[i] != 0 {
                    block_free(old.blocks[i]);
                }
            }
            Inode {
                ino,
                typ: T_FILE,
                mode: if old.mode == 0 { 0o644 } else { old.mode },
                uid: if old.uid == 0 { crate::auth::current_uid() } else { old.uid },
                gid: if old.gid == 0 { crate::auth::current_gid() } else { old.gid },
                size: 0,
                parent: sup.root,
                mtime: 0,
                blocks: [0; NDIRECT],
            }
        }
        None => Inode {
            ino: inode_alloc().ok_or("没有空闲 inode")?,
            typ: T_FILE,
            mode: 0o644,
            uid: crate::auth::current_uid(),
            gid: crate::auth::current_gid(),
            size: 0,
            parent: sup.root,
            mtime: 0,
            blocks: [0; NDIRECT],
        },
    };
    if !data.is_empty() {
        let nblk = data.len().div_ceil(BS);
        for bi in 0..nblk {
            let b = block_alloc().ok_or("没有空闲块")?;
            let x = buf();
            zero(x);
            let s = bi * BS;
            let e = (s + BS).min(data.len());
            x[..e - s].copy_from_slice(&data[s..e]);
            if !wr(b, &*x) {
                return Err("写数据块失败");
            }
            f.blocks[bi] = b;
        }
    }
    f.size = data.len() as u32;
    if !inode_write(&f) {
        return Err("写 inode 失败");
    }
    if dir_find(&root, name).is_none() && !dir_add(sup.root, name, f.ino, T_FILE) {
        return Err("目录项写失败");
    }
    Ok(data.len())
}

pub fn stat() -> Option<Stat> {
    let sup = super_block()?;
    let x = buf();
    if !rd(BLOCK_BITMAP, x) {
        return None;
    }
    let mut free = 0u32;
    for b in sup.data_start..sup.blocks {
        let i = (b / 8) as usize;
        if x[i] & (1u8 << (b % 8)) == 0 {
            free += 1;
        }
    }
    let y = buf2();
    if !rd(INODE_BITMAP, y) {
        return None;
    }
    let mut used = 0u32;
    for ino in 0..MAX_INODES {
        let i = (ino / 8) as usize;
        if y[i] & (1u8 << (ino % 8)) != 0 {
            used += 1;
        }
    }
    Some(Stat {
        blocks: sup.blocks,
        free_blocks: free,
        inodes: sup.inodes,
        used_inodes: used,
        data_start: sup.data_start,
    })
}

pub fn format(name: &str) -> bool {
    crate::cache::invalidate();
    crate::cache::reset_base();
    let info = match disk::probe() {
        Some(i) => i,
        None => return false,
    };
    let part = match info.parts[0] {
        Some(p) => p,
        None => return false,
    };
    let blocks = (part.sectors as u64 / SPB) as u32;
    if blocks <= DATA_START + 1 {
        return false;
    }
    let data_blocks = blocks - DATA_START;

    // 块 0：超级块
    {
        let x = buf();
        zero(x);
        x[0..4].copy_from_slice(&MAGIC);
        put_u32(x, 4, 1);
        put_u32(x, 8, BS as u32);
        put_u32(x, 12, blocks);
        put_u32(x, 16, MAX_INODES);
        put_u32(x, 20, 0);
        put_u32(x, 24, INODE_TABLE);
        put_u32(x, 28, INODE_TABLE_BLOCKS);
        put_u32(x, 32, INODE_BITMAP);
        put_u32(x, 36, BLOCK_BITMAP);
        put_u32(x, 40, DATA_START);
        put_u32(x, 44, data_blocks);
        put_u32(x, 48, data_blocks - 1);
        let nb = name.as_bytes();
        let n = nb.len().min(31);
        x[64..64 + n].copy_from_slice(&nb[..n]);
        let c = crc32(&x[0..56]);
        put_u32(x, 56, c);
        if !wr(0, &*x) {
            return false;
        }
    }

    // 块 1：inode 位图，0 号已用
    {
        let x = buf();
        zero(x);
        x[0] = 1;
        if !wr(INODE_BITMAP, &*x) {
            return false;
        }
    }

    // 块 2：块位图，元数据 + 根目录块已用
    {
        let x = buf();
        zero(x);
        for b in 0..=DATA_START {
            x[(b / 8) as usize] |= 1u8 << (b % 8);
        }
        if !wr(BLOCK_BITMAP, &*x) {
            return false;
        }
    }

    // 块 3..6：inode 表，只写 0 号（根目录）
    for i in 0..INODE_TABLE_BLOCKS {
        let x = buf();
        zero(x);
        if i == 0 {
            put_u16(x, 0, T_DIR);
            put_u16(x, 2, 0o755);
            put_u32(x, 4, 64);
            put_u32(x, 8, 0);
            put_u32(x, 16, DATA_START);
            put_u16(x, 56, 1000);
            put_u16(x, 58, 1000);
        }
        if !wr(INODE_TABLE + i, &*x) {
            return false;
        }
    }

    // 块 7：根目录两条目
    {
        let x = buf();
        zero(x);
        x[0..1].copy_from_slice(b".");
        put_u32(x, 24, 0);
        put_u16(x, 28, T_DIR);
        x[32..34].copy_from_slice(b"..");
        put_u32(x, 32 + 24, 0);
        put_u16(x, 32 + 28, T_DIR);
        if !wr(DATA_START, &*x) {
            return false;
        }
    }
    crate::cache::flush();
    true
}
