// Aster Kernel — 用户与权限
//
// 模型故意做得很窄，够用且能讲清：
//   用户：uid 0 root / 1000 aster（默认） / 1001 guest，可再 useradd 到 8 个
//   权限：Unix 三位 rwx（owner / group / other 位仍存着，但这一版只看 owner 与 other）
//   判定：uid 0 全通；否则 owner 位跟 other 位二选一
//   门：切换成 root 必须 EXTREME 已解锁（Device Lock 解开之后才有），普通用户随便切
//
// 目录项本身不带权限，权限记在 inode 上（`mode` 在 inode+2，`uid` 在 inode+56）。

pub struct User {
    pub name: [u8; 12],
    pub uid: u16,
}

const MAXU: usize = 8;

static mut USERS: [User; MAXU] = [
    User { name: *b"root\0\0\0\0\0\0\0\0", uid: 0 },
    User { name: *b"aster\0\0\0\0\0\0\0", uid: 1000 },
    User { name: *b"guest\0\0\0\0\0\0\0", uid: 1001 },
    User { name: [0; 12], uid: 0 },
    User { name: [0; 12], uid: 0 },
    User { name: [0; 12], uid: 0 },
    User { name: [0; 12], uid: 0 },
    User { name: [0; 12], uid: 0 },
];
static mut NU: usize = 3;
static mut CUR: usize = 1;

pub const W_READ: u8 = 4;
pub const W_WRITE: u8 = 2;
pub const W_EXEC: u8 = 1;

pub fn init() {
    unsafe {
        NU = 3;
        CUR = 1;
    }
}

pub fn count() -> usize {
    unsafe { NU }
}

pub fn user(i: usize) -> (&'static str, u16) {
    unsafe {
        if i >= NU {
            return ("?", 0);
        }
        let u = &USERS[i];
        let mut n = 0usize;
        while n < 12 && u.name[n] != 0 {
            n += 1;
        }
        (core::str::from_utf8_unchecked(&u.name[..n]), u.uid)
    }
}

/// uid → 名字（不在表里就显示 "uid:N"）
pub fn name_of_uid(uid: u16) -> &'static str {
    for i in 0..count() {
        if user(i).1 == uid {
            return user(i).0;
        }
    }
    "uid:?"
}

pub fn current_name() -> &'static str {
    unsafe { user(CUR).0 }
}

pub fn current_uid() -> u16 {
    unsafe { user(CUR).1 }
}

pub fn is_root() -> bool {
    current_uid() == 0
}

pub fn find(name: &str) -> Option<usize> {
    for i in 0..count() {
        if user(i).0 == name {
            return Some(i);
        }
    }
    None
}

/// 切用户：切到 root 需要 EXTREME 已解锁
pub fn su(name: &str) -> Result<u16, &'static str> {
    let i = find(name).ok_or("没有这个用户")?;
    let uid = user(i).1;
    if uid == 0 && !crate::shell::extreme_on() {
        return Err("切 root 要先解开 EXTREME（ex 命令）");
    }
    unsafe {
        CUR = i;
    }
    Ok(uid)
}

/// 加用户：只有 root 或 EXTREME 能加
pub fn add(name: &str) -> Result<u16, &'static str> {
    if name.is_empty() || name.len() > 11 {
        return Err("名字 1..11 字符");
    }
    if !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') {
        return Err("只允许小写字母/数字/下划线");
    }
    if find(name).is_some() {
        return Err("用户已存在");
    }
    if !is_root() && !crate::shell::extreme_on() {
        return Err("加用户需要 root 或 EXTREME");
    }
    unsafe {
        if NU >= MAXU {
            return Err("用户表满了（8 个）");
        }
        let i = NU;
        USERS[i].name = [0; 12];
        let b = name.as_bytes();
        let mut k = 0usize;
        while k < b.len() && k + 1 < 12 {
            USERS[i].name[k] = b[k];
            k += 1;
        }
        USERS[i].uid = 1000 + i as u16;
        NU += 1;
        Ok(USERS[i].uid)
    }
}

/// 权限判定：uid 0 全通；owner 看 owner 位，其它看 other 位（group 位留着以后用）
pub fn can(mode: u16, owner: u16, want: u8) -> bool {
    if current_uid() == 0 {
        return true;
    }
    let bits: u16 = if owner == current_uid() {
        (mode >> 6) & 7
    } else {
        mode & 7
    };
    (bits as u8 & want) == want
}

/// 把 mode 画成 `-rw-r--r--`
pub fn mode_str(mode: u16, typ: u16, out: &mut [u8; 11]) {
    out[0] = if typ == 2 { b'd' } else { b'-' };
    for (i, m) in [(1usize, 6u16), (4, 3), (7, 0)] {
        let b = (mode >> m) & 7;
        out[i] = if b & 4 != 0 { b'r' } else { b'-' };
        out[i + 1] = if b & 2 != 0 { b'w' } else { b'-' };
        out[i + 2] = if b & 1 != 0 { b'x' } else { b'-' };
    }
    out[10] = 0;
}

/// 名字 → uid（供 chown 用），也接受纯数字
pub fn uid_of(s: &str) -> Option<u16> {
    if let Some(i) = find(s) {
        return Some(user(i).1);
    }
    s.parse::<u16>().ok()
}
