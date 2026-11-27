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
pub const FIRST_USER_FD: usize = 3;

pub const ENOENT: i64 = -2;
pub const EBADF: i64 = -9;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const ENOSPC: i64 = -28;
pub const EROFS: i64 = -30;
pub const ENAMETOOLONG: i64 = -36;

#[derive(Clone, Copy)]
struct Fd {
    used: bool,
    ino: u32,
    off: u32,
    len: u32,
    wr: bool,
    nlen: u8,
    name: [u8; 24],
    buf: [u8; BS],
}

const EMPTY: Fd = Fd {
    used: false,
    ino: 0,
    off: 0,
    len: 0,
    wr: false,
    nlen: 0,
    name: [0; 24],
    buf: [0; BS],
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

fn slot_for(name: &str, ino: u32, len: u32, wr: bool, buf: &[u8; BS]) -> Result<i32, i64> {
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
    f.ino = ino;
    f.off = 0;
    f.len = len;
    f.wr = wr;
    f.nlen = bytes.len() as u8;
    f.name = [0; 24];
    f.name[..bytes.len()].copy_from_slice(bytes);
    f.buf = *buf;
    unsafe { OPENS += 1 };
    Ok(idx as i32)
}

/// 打开文件。`wr` 为真时若文件不存在则创建空文件，close 时整体写回。
pub fn open(path: &str, wr: bool) -> Result<i32, i64> {
    let name = path.trim_start_matches('/');
    if name.is_empty() || name.len() > 24 {
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
        return Err(EINVAL);
    }
    let mut buf = [0u8; BS];
    let n = afs::read_file(name, &mut buf).map_err(|_| EINVAL)?;
    let len = (f.size as usize).min(n) as u32;
    slot_for(name, f.ino, len, wr, &buf)
}

pub fn read(fd: i32, out: &mut [u8]) -> i64 {
    if fd < 0 || fd as usize >= MAX_FD {
        return EBADF;
    }
    let f = unsafe { &mut FDS[fd as usize] };
    if !f.used {
        return EBADF;
    }
    let avail = f.len.saturating_sub(f.off) as usize;
    let n = out.len().min(avail);
    if n == 0 {
        return 0;
    }
    out[..n].copy_from_slice(&f.buf[f.off as usize..f.off as usize + n]);
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
    if !f.wr {
        return EROFS;
    }
    let off = f.off as usize;
    if off + data.len() > BS {
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
            if f.wr { "rw" } else { "ro" },
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
