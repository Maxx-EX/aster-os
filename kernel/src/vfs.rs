// Aster Kernel — VFS 层（阶段三·5）
//
// 内核里原来只有"按名字整文件读"（`afs::read_file`），用户程序没有句柄概念。
// 这一层补上 fd 表：open 拿一个整数，read 从当前偏移往后拷，close 时把改过的
// 内容落回磁盘。偏移是显式的，所以同一份数据可以被两个 fd 各自读各自的进度
// —— 这是后面 ELF 加载器一次 mmap 一段、APK 读 zip 目录的前提。
//
// 容量：16 个 fd，0/1/2 留给 stdin/stdout/stderr；每个 fd 自带一块 4 KiB
// 页面缓着文件内容，所以 v0 的单文件上限仍是 4096 字节（`afs` 的约束）。
#![allow(static_mut_refs)]

use crate::afs::{self, BS};

pub const MAX_FD: usize = 16;
/// 一次打开能缓存的最大文件（原来是 4 KiB = 一块，网络下载的东西稍大就 ENOSPC）
pub const VFS_CAP: usize = 16 * 1024;
pub const FIRST_USER_FD: usize = 3;

pub const ENOENT: i64 = -2;
pub const EACCES: i64 = -13;
pub const EBADF: i64 = -9;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const ENOSPC: i64 = -28;
pub const EROFS: i64 = -30;
pub const ENAMETOOLONG: i64 = -36;

#[derive(Clone, Copy)]
struct Fd {
    used: bool,
    /// 目录句柄：readdir 用，dpos 是游标
    dir: bool,
    dino: u32,
    dpos: usize,
    ino: u32,
    off: u32,
    len: u32,
    wr: bool,
    nlen: u8,
    name: [u8; 24],
    buf: [u8; VFS_CAP],
}

const EMPTY: Fd = Fd {
    used: false,
    dir: false,
    dino: 0,
    dpos: 0,
    ino: 0,
    off: 0,
    len: 0,
    wr: false,
    nlen: 0,
    name: [0; 24],
    buf: [0; VFS_CAP],
};

static mut FDS: [Fd; MAX_FD] = [EMPTY; MAX_FD];
static mut OPENS: u32 = 0;
static mut READS: u32 = 0;
static mut WRITES: u32 = 0;

pub fn reset() {
    unsafe {
        FDS = [EMPTY; MAX_FD];
        OPENS = 0;
        READS = 0;
        WRITES = 0;
    }
}

/// `/motd` 与 `motd` 等价；多级路径逐段解析（`apps/demo/classes.dex` 也能走）
fn resolve(name: &str) -> Option<afs::Inode> {
    let sup = afs::super_block()?;
    let root = afs::inode_read(sup.root)?;
    let mut cur = root;
    let mut rest = name;
    loop {
        let (seg, tail) = match rest.find('/') {
            Some(p) => (&rest[..p], Some(&rest[p + 1..])),
            None => (rest, None),
        };
        if seg.is_empty() {
            return None;
        }
        let ino = afs::dir_find(&cur, seg)?;
        let f = afs::inode_read(ino)?;
        match tail {
            None => return Some(f),
            Some(t) => {
                if f.typ != afs::T_DIR {
                    return None;
                }
                cur = f;
                rest = t;
            }
        }
    }
}

fn slot_for(name: &str, ino: u32, len: u32, wr: bool, buf: Option<&[u8; VFS_CAP]>) -> Result<i32, i64> {
    let bytes = name.as_bytes();
    if bytes.len() > 24 {
        return Err(ENAMETOOLONG);
    }
    let idx = unsafe {
        (FIRST_USER_FD..MAX_FD)
            .find(|i| !FDS[*i].used)
            .ok_or(EBADF as i64)
    }?;
    let f = unsafe { &mut FDS[idx as usize] };
    f.used = true;
    f.dir = false;
    f.dino = 0;
    f.dpos = 0;
    f.ino = ino;
    f.off = 0;
    f.len = len;
    f.wr = wr;
    f.nlen = bytes.len() as u8;
    f.name = [0; 24];
    f.name[..bytes.len()].copy_from_slice(bytes);
    match buf {
        Some(b) => f.buf = *b,
        None => f.buf = [0; VFS_CAP],
    }
    unsafe { OPENS += 1 };
    Ok(idx as i32)
}

/// 打开文件。`wr` 为真时若文件不存在则创建空文件，close 时整体写回。
pub fn open(path: &str, wr: bool) -> Result<i32, i64> {
    let name = path.trim_start_matches('/');
    if name.is_empty() {
        // "/"：根目录句柄
        return if wr { Err(EACCES) } else { opendir("") };
    }
    if name.len() > 24 {
        return Err(EINVAL);
    }
    let f = match resolve(name) {
        Some(f) => f,
        None => {
            if !wr {
                return Err(ENOENT);
            }
            afs::write_file(name, b"").map_err(|_| ENOSPC)?;
            afs::sync();
            resolve(name).ok_or(ENOENT)?
        }
    };
    if f.typ != afs::T_FILE {
        // 打开一个目录：只读时给目录句柄，读用 readdir
        if !wr {
            return opendir(&name);
        }
        return Err(EINVAL);
    }
    if wr {
        // 写打开过一道权限门：owner 的 w 位 / 别人看 other 位 / root 全通
        if let Some((mode, owner, grp)) = afs::meta(name) {
            if !crate::auth::can(mode, owner, grp, crate::auth::W_WRITE) {
                return Err(EACCES);
            }
        }
    }
    if !wr {
        // 只读打开不搬内容：read 时按偏移从块缓存取，文件多大都不占内存
        return slot_for(name, f.ino, f.size, false, None);
    }
    let mut buf = [0u8; VFS_CAP];
    let n = afs::read_file(name, &mut buf).map_err(|_| EINVAL)?;
    let len = n as u32;
    slot_for(name, f.ino, len, wr, Some(&buf))
}

pub fn read(fd: i32, out: &mut [u8]) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used {
        return EBADF;
    }
    if f.dir {
        return EINVAL;
    }
    let avail = f.len.saturating_sub(f.off) as usize;
    if avail == 0 {
        return 0;
    }
    if f.wr {
        let n = out.len().min(avail);
        out[..n].copy_from_slice(&f.buf[f.off as usize..f.off as usize + n]);
        f.off += n as u32;
        unsafe { READS += 1 };
        return n as i64;
    }
    let want = out.len().min(avail);
    let n = afs::read_at(f.ino, f.off, &mut out[..want]);
    f.off += n as u32;
    unsafe { READS += 1 };
    n as i64
}

pub fn write(fd: i32, data: &[u8]) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used {
        return EBADF;
    }
    if f.dir {
        return EINVAL;
    }
    if !f.wr {
        return EROFS;
    }
    let off = f.off as usize;
    if off + data.len() > VFS_CAP {
        return ENOSPC;
    }
    f.buf[off..off + data.len()].copy_from_slice(data);
    f.off += data.len() as u32;
    if f.off > f.len {
        f.len = f.off;
    }
    unsafe { WRITES += 1 };
    data.len() as i64
}

pub fn close(fd: i32) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used {
        return EBADF;
    }
    let n = f.nlen as usize;
    let mut name_buf = [0u8; 24];
    name_buf[..n].copy_from_slice(&f.name[..n]);
    let name = core::str::from_utf8(&name_buf[..n]).unwrap_or("");
    let mut code = 0i64;
    if f.wr {
        match afs::write_file(name, &f.buf[..f.len as usize]) {
            Ok(_) => {
                afs::sync();
            }
            Err(_) => code = ENOSPC,
        }
    }
    *f = EMPTY;
    code
}

/// 打开目录：拿到一个只用于 readdir 的句柄
pub fn opendir(path: &str) -> Result<i32, i64> {
    let name = path.trim_start_matches('/');
    if name.len() > 24 {
        return Err(ENAMETOOLONG);
    }
    let d = if name.is_empty() {
        match resolve(".") {
            Some(x) => x,
            None => afs::inode_read(0).ok_or(ENOENT)?,
        }
    } else {
        match resolve(name) {
            Some(x) => x,
            None => return Err(ENOENT),
        }
    };
    if d.typ != afs::T_DIR {
        return Err(EINVAL);
    }
    let idx = unsafe {
        (FIRST_USER_FD..MAX_FD)
            .find(|i| !FDS[*i].used)
            .ok_or(EBADF as i64)
    }?;
    let f = unsafe { &mut FDS[idx as usize] };
    *f = EMPTY;
    f.used = true;
    f.dir = true;
    f.dino = d.ino;
    f.dpos = 0;
    f.len = d.size;
    f.ino = d.ino;
    f.nlen = name.len() as u8;
    f.name[..name.len()].copy_from_slice(name.as_bytes());
    unsafe { OPENS += 1 };
    Ok(idx as i32)
}

/// 读目录：每次填 32 字节一项（ino u32 / typ u16 / namelen u16 / name[24]），
/// 返回字节数，0 = 读完了。
pub fn readdir(fd: i32, out: &mut [u8]) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used || !f.dir {
        return EBADF;
    }
    let total = afs::dir_count(f.dino);
    let mut n = 0usize;
    while f.dpos < total && n + 32 <= out.len() {
        if let Some((ino, typ, name, nl)) = afs::dir_ent(f.dino, f.dpos) {
            out[n..n + 4].copy_from_slice(&ino.to_le_bytes());
            out[n + 4..n + 6].copy_from_slice(&typ.to_le_bytes());
            out[n + 6..n + 8].copy_from_slice(&(nl as u16).to_le_bytes());
            out[n + 8..n + 32].copy_from_slice(&name);
        }
        f.dpos += 1;
        n += 32;
    }
    n as i64
}

/// lseek：SEEK_SET=0 / SEEK_CUR=1 / SEEK_END=2，返回新偏移（允许指到文件尾之后）
pub fn lseek(fd: i32, off: i64, whence: u32) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used {
        return EBADF;
    }
    let base = match whence {
        0 => 0i64,
        1 => f.off as i64,
        2 => f.len as i64,
        _ => return EINVAL,
    };
    let np = base.saturating_add(off);
    if np < 0 || np > 0x7fff_ffff {
        return EINVAL;
    }
    f.off = np as u32;
    np
}

/// fstat：把 fd 的元信息摊平给 syscall 层（size/ino/mode/typ/uid/gid/mtime）
pub fn meta_of(fd: i32) -> Result<(u32, u32, u16, u16, u16, u16, u32, u32), i64> {
    if fd < 0 || fd as usize >= MAX_FD {
        return Err(EBADF);
    }
    let f = unsafe { &FDS[fd as usize] };
    if !f.used {
        return Err(EBADF);
    }
    if f.dir {
        let d = afs::inode_read(f.dino).ok_or(ENOENT)?;
        return Ok((d.size, d.ino, d.mode, d.typ, d.uid, d.gid, d.mtime, d.blocks[0]));
    }
    let d = afs::inode_read(f.ino).ok_or(ENOENT)?;
    Ok((f.len, d.ino, d.mode, d.typ, d.uid, d.gid, d.mtime, d.blocks[0]))
}

pub fn stat(path: &str) -> Result<(u32, u32), i64> {
    let name = path.trim_start_matches('/');
    match resolve(name) {
        Some(f) if f.typ == afs::T_FILE => Ok((f.size, f.ino)),
        Some(_) => Err(EINVAL),
        None => Err(ENOENT),
    }
}

/// fd 表快照，给 shell 的 `fd` 命令看
pub fn dump() {
    let (o, r, w) = unsafe { (OPENS, READS, WRITES) };
    crate::println!("  fd 表 {}/{} 槽在用，open {} / read {} / write {}", count(), MAX_FD, o, r, w);
    for i in 0..MAX_FD {
        let f = unsafe { &FDS[i] };
        if !f.used {
            continue;
        }
        let n = f.nlen as usize;
        let name = core::str::from_utf8(&f.name[..n]).unwrap_or("?");
        crate::println!(
            "  {}  {}  {}  {}/{}  ino {}",
            i,
            if f.dir { "dir" } else if f.wr { "rw" } else { "ro" },
            name,
            f.off,
            f.len,
            f.ino
        );
    }
}

pub fn count() -> usize {
    let mut n = 0;
    for i in 0..MAX_FD {
        if unsafe { FDS[i].used } {
            n += 1;
        }
    }
    n
}
