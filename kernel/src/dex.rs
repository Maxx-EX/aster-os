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
fn code_in_class(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
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

fn class_def_pos(d: &[u8], h: &Header, class_desc: &str) -> Result<usize, &'static str> {
    let mut i = 0u32;
    while i < h.classes {
        let p = h.c_off as usize + i as usize * 32;
        let class_idx = u32le(d, p);
        if type_desc(d, h, class_idx)? == class_desc {
            return Ok(p);
        }
        i += 1;
    }
    Err("没有这个类")
}

/// 沿 superclass 链解析方法：先在本类找，找不到上溯父类（最多 4 层）。
/// 继承是 APK 的常态：`Child` 只有构造器，`get()` 声明在 `Base` 上。
pub fn code_of(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let mut desc = class_desc;
    let mut depth = 0;
    loop {
        match code_in_class(d, h, desc, method_name) {
            Ok(c) => return Ok(c),
            Err(e) => {
                if e == "没有这个类" {
                    return Err(e);
                }
                let p = class_def_pos(d, h, desc)?;
                let sup = u32le(d, p + 8);
                if sup == u32::MAX {
                    return Err("类里没有这个方法（已到根类）");
                }
                desc = type_desc(d, h, sup)?;
                depth += 1;
                if depth > 4 {
                    return Err("方法解析超过 4 层父类");
                }
            }
        }
    }
}

// —— 对象堆（APK-13）：页帧承载，不再有固定槽数上限 ——
//
// 一页 4 KiB 由 frame::alloc() 供给：页头 64 字节放链指针，后面 63 个 64 字节槽。
// 页头：
//   +0  next  u64     下一页（0 = 链尾）
//   +8  nslot u16     本页槽数（固定 63，留给将来变长）
//   +10 used  u16     本页已用槽数
// 槽（64 字节）：
//   +0  used      u32   0 = 空，1 = 活
//   +4  class_idx u32
//   +8  pad 8
//   +16 fields    [i32; 8]
//   +48 16 字节余量（将来 monitor / refcount / GC 标记）
//
// 引用不再是 0..NOBJ 的小整数，而是 OBJ_TAG + 全局槽号。全局槽号跨页单调递增，
// 只在 heap_reset 时归零；没有单个释放，所以不是 GC，只是"只增不减的分配器"。
const OBJ_TAG: i32 = 0x1_0000;
const SLOT_SIZE: usize = 64;
const PG_HDR: usize = 64;
const PG_SIZE: usize = 4096;
const SLOTS_PER_PAGE: usize = (PG_SIZE - PG_HDR) / SLOT_SIZE; // 63
const SF_SIZE: usize = 8;

static mut HEAP_HEAD: u64 = 0; // 页链头（恒等映射，物理地址即虚拟地址）
static mut HEAP_TAIL: u64 = 0; // 页链尾，新页直接接上，避免每次 O(页数) 走链
static mut HEAP_PAGES: usize = 0;
static mut HEAP_GIDX: usize = 0; // 已分配槽数（单调）

#[inline]
fn rd_u64(p: u64) -> u64 {
    unsafe { core::ptr::read_volatile(p as *const u64) }
}
#[inline]
fn wv_u64(p: u64, v: u64) {
    unsafe { core::ptr::write_volatile(p as *mut u64, v) }
}
#[inline]
fn rd_u32(p: u64) -> u32 {
    unsafe { core::ptr::read_volatile(p as *const u32) }
}
#[inline]
fn wv_u32(p: u64, v: u32) {
    unsafe { core::ptr::write_volatile(p as *mut u32, v) }
}
#[inline]
fn rd_i32(p: u64) -> i32 {
    unsafe { core::ptr::read_volatile(p as *const i32) }
}
#[inline]
fn wv_i32(p: u64, v: i32) {
    unsafe { core::ptr::write_volatile(p as *mut i32, v) }
}

/// 遍历页链，取第 `pg` 页的物理地址
fn page_at(pg: usize) -> Option<u64> {
    let mut p = unsafe { HEAP_HEAD };
    let mut k = 0;
    while k < pg {
        if p == 0 {
            return None;
        }
        p = rd_u64(p);
        k += 1;
    }
    if p == 0 {
        None
    } else {
        Some(p)
    }
}

/// 保证至少有 `need_pages` 页挂在链上
fn grow_pages(need_pages: usize) -> Result<(), &'static str> {
    while unsafe { HEAP_PAGES } <= need_pages {
        let pa = match crate::frame::alloc() {
            Some(p) => p,
            None => return Err("物理页耗尽，对象堆无法继续增长"),
        };
        wv_u64(pa, 0);
        wv_u32(pa + 8, SLOTS_PER_PAGE as u32);
        wv_u32(pa + 12, 0);
        let tail = unsafe { HEAP_TAIL };
        if tail == 0 {
            unsafe {
                HEAP_HEAD = pa;
                HEAP_TAIL = pa;
            }
        } else {
            wv_u64(tail, pa);
            unsafe { HEAP_TAIL = pa };
        }
        unsafe { HEAP_PAGES += 1 };
    }
    Ok(())
}

fn slot_ptr(g: usize) -> Option<u64> {
    let pg = g / SLOTS_PER_PAGE;
    let base = page_at(pg)?;
    Some(base + (PG_HDR + (g % SLOTS_PER_PAGE) * SLOT_SIZE) as u64)
}

// —— 数组（APK-6）——
// 寄存器里对象用 0..NOBJ，数组用 0x100+i 这一段，两者不会撞车。
#[derive(Clone, Copy)]
struct Arr {
    len: u32,
    d: [i32; 32],
}

const NARR: usize = 16;
const ARR_TAG: i32 = 0x100;
static mut ARRS: [Arr; NARR] = [Arr { len: 0, d: [0; 32] }; NARR];
static mut NARR_USED: usize = 0;

fn arr_new(len: u32) -> Result<i32, &'static str> {
    unsafe {
        if NARR_USED >= NARR {
            return Err("数组堆满（最多 16 个）");
        }
        if len > 32 {
            return Err("数组超过 32 个元素（v0 上限）");
        }
        let i = NARR_USED;
        ARRS[i] = Arr { len, d: [0; 32] };
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
            ARRS[i] = Arr { len: 0, d: [0; 32] };
            i += 1;
        }
    }
}

fn heap_reset() {
    // 整堆还页：页链逐页 free，页帧池的位图跟着回落
    unsafe {
        let mut p = HEAP_HEAD;
        while p != 0 {
            let nx = rd_u64(p);
            crate::frame::free(p);
            p = nx;
        }
        HEAP_HEAD = 0;
        HEAP_TAIL = 0;
        HEAP_PAGES = 0;
        HEAP_GIDX = 0;
    }
    arr_reset();
}

// —— 静态字段 + StringBuilder（APK-7）——
// 静态字段按 field_idx 直接索引：fixture 只有一个类有静态字段，够 demo 用。
const NSFIELD: usize = 8;
static mut SFIELDS: [i32; NSFIELD] = [0; NSFIELD];

// 运行时生成的字符串没有 string_idx 可用，单独开一张槽表。
// 寄存器值 >= 0x200 表示"动态字符串 0x200+i"，与数组的 0x100+i 区间分开。
const DYN_TAG: i32 = 0x200;
const NDYN: usize = 4;
static mut DYN: [[u8; 96]; NDYN] = [[0; 96]; NDYN];
static mut DYN_LEN: [usize; NDYN] = [0; NDYN];
static mut DYN_USED: usize = 0;

// StringBuilder 只支持一个实例（真实实现要按对象分配缓冲）。
static mut SB: [u8; 128] = [0; 128];
static mut SB_LEN: usize = 0;

fn sb_reset() {
    unsafe {
        SB_LEN = 0;
    }
}

fn sb_push_str(s: &str) -> Result<(), &'static str> {
    let n = s.len();
    unsafe {
        if SB_LEN + n > SB.len() {
            return Err("StringBuilder 超过 128 字节");
        }
        let mut i = 0;
        while i < n {
            SB[SB_LEN + i] = s.as_bytes()[i];
            i += 1;
        }
        SB_LEN += n;
    }
    Ok(())
}

fn sb_push_int(v: i32) -> Result<(), &'static str> {
    let neg = v < 0;
    let mut x: i64 = if neg { -(v as i64) } else { v as i64 };
    let mut out = [0u8; 12];
    let mut n = 0;
    if x == 0 {
        out[0] = b'0';
        n = 1;
    }
    while x > 0 {
        out[n] = b'0' + (x % 10) as u8;
        x /= 10;
        n += 1;
    }
    // 反转
    let mut rev = [0u8; 12];
    let mut k = 0;
    if neg {
        rev[0] = b'-';
        k = 1;
    }
    let mut i = 0;
    while i < n {
        rev[k + i] = out[n - 1 - i];
        i += 1;
    }
    let s = core::str::from_utf8(&rev[..k + n]).map_err(|_| "整数格式化失败")?;
    sb_push_str(s)
}

fn sb_freeze() -> Result<i32, &'static str> {
    unsafe {
        if DYN_USED >= NDYN {
            return Err("动态字符串槽用尽（4 个）");
        }
        let i = DYN_USED;
        DYN_USED += 1;
        if SB_LEN > 96 {
            return Err("字符串超过 96 字节");
        }
        let mut k = 0;
        while k < SB_LEN {
            DYN[i][k] = SB[k];
            k += 1;
        }
        DYN_LEN[i] = SB_LEN;
        Ok(DYN_TAG + i as i32)
    }
}

fn dyn_str(v: i32) -> Result<&'static str, &'static str> {
    let i = (v - DYN_TAG) as usize;
    if (v - DYN_TAG) < 0 || i >= unsafe { DYN_USED } {
        return Err("动态字符串索引无效");
    }
    let n = unsafe { DYN_LEN[i] };
    let b: &'static [u8] = unsafe { &DYN[i][..n] };
    core::str::from_utf8(b).map_err(|_| "动态字符串不是合法 UTF-8")
}

fn str_of<'a>(d: &'a [u8], h: &Header, v: i32) -> Result<&'a str, &'static str> {
    if v >= DYN_TAG && v < OBJ_TAG {
        dyn_str(v)
    } else {
        string(d, h, v as u32)
    }
}

fn call_reset() {
    unsafe {
        SFIELDS = [0; NSFIELD];
        DYN_USED = 0;
        SB_LEN = 0;
        let mut i = 0;
        while i < NDYN {
            DYN_LEN[i] = 0;
            i += 1;
        }
    }
}

fn heap_new(class_idx: u32) -> Result<i32, &'static str> {
    let g = unsafe { HEAP_GIDX };
    grow_pages(g / SLOTS_PER_PAGE)?;
    let sp = slot_ptr(g).ok_or("对象槽定位失败")?;
    wv_u32(sp, 1);
    wv_u32(sp + 4, class_idx);
    wv_u64(sp + 8, 0);
    let mut f = 0;
    while f < SF_SIZE {
        wv_i32(sp + 16 + (f * 4) as u64, 0);
        f += 1;
    }
    unsafe { HEAP_GIDX += 1 };
    Ok(OBJ_TAG + g as i32)
}

/// 引用 → 槽地址。同时校验 tag、槽号范围和槽是否还活着。
fn heap_idx(v: i32) -> Result<u64, &'static str> {
    let g = v - OBJ_TAG;
    if g < 0 {
        return Err("不是有效对象引用");
    }
    let g = g as usize;
    if g >= unsafe { HEAP_GIDX } {
        return Err("不是有效对象引用");
    }
    let sp = slot_ptr(g).ok_or("对象槽定位失败")?;
    if rd_u32(sp) == 0 {
        return Err("对象已释放");
    }
    Ok(sp)
}

/// (页数, 已用槽, 总槽容量)
pub fn heap_stats() -> (usize, usize, usize) {
    (unsafe { HEAP_PAGES }, unsafe { HEAP_GIDX }, unsafe { HEAP_PAGES } * SLOTS_PER_PAGE)
}

// —— 异常（APK-8）——
// 跨方法传播不返回专用类型，而是用一条哨兵错误 + 一个全局异常对象引用：
// exec 收到哨兵时先在本方法的 try 表里找 handler，找不到才继续往上抛。
const THROW_SENTINEL: &str = "__aster_throw";
static mut THROW_VAL: i32 = 0;

// —— 启动参数（APK-13）——
// `app start <n>` 把 n 存进来，APK 侧用 `Lcom/aster/App;->arg()I` 取回。
// 语义故意做得极窄：一个 i32，没有 Bundle、没有 key。
static mut START_ARG: i32 = 0;

pub fn set_start_arg(v: i32) {
    unsafe { START_ARG = v };
}

pub fn start_arg() -> i32 {
    unsafe { START_ARG }
}

/// DEX 里的 sleb128（handler 的 size 字段是有符号的）
fn sleb(d: &[u8], mut p: usize) -> Result<(i32, usize), &'static str> {
    let mut v = 0i32;
    let mut shift = 0;
    loop {
        if p >= d.len() {
            return Err("sleb 越界");
        }
        let byte = d[p];
        p += 1;
        v |= ((byte & 0x7f) as i32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 32 && (byte & 0x40) != 0 {
                v |= -1i32 << shift;
            }
            return Ok((v, p));
        }
        if shift > 28 {
            return Err("sleb 过长");
        }
    }
}

/// 在方法 `coff` 里找覆盖 `pc` 的 handler，返回 handler 的地址（方法内 pc）。
/// 只取第一个 handler 的地址——fixture 只有一个 catch-all，不做异常类型匹配。
fn find_handler(d: &[u8], coff: u32, pc: usize) -> Result<Option<u32>, &'static str> {
    let p = coff as usize;
    let tries = u16le(d, p + 6) as usize;
    if tries == 0 {
        return Ok(None);
    }
    let n = u32le(d, p + 12) as usize;
    let mut base = p + 16 + n * 2;
    if n % 2 == 1 {
        base += 2; // insns 奇数条时要 2 字节对齐填充
    }
    let handlers = base + tries * 8;
    let mut i = 0;
    while i < tries {
        let t = base + i * 8;
        let start = u32le(d, t) as usize;
        let count = u16le(d, t + 4) as usize;
        let hoff = u16le(d, t + 6) as usize;
        if pc >= start && pc < start + count {
            let hp = handlers + hoff;
            let (size, q) = sleb(d, hp)?;
            if size <= 0 {
                // catch-all：地址紧跟 size
                let (addr, _) = uleb(d, q)?;
                return Ok(Some(addr));
            }
            let (_, q2) = uleb(d, q)?; // typed handler 的 type_idx
            let (addr, _) = uleb(d, q2)?;
            return Ok(Some(addr));
        }
        i += 1;
    }
    Ok(None)
}

/// 执行一个方法。`args` 是调用方传进来的寄存器值（this 在第一个位置）。
/// 单次调用允许的指令上限：正常 fixture 几十条，真 APK 也远小于这个数。
/// 到顶就报错而不是把内核转死 —— 这条是死循环的保险丝。
const STEP_LIMIT: u32 = 200_000;

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
        *steps += 1;
        if *steps > STEP_LIMIT {
            crate::println!("  [DEX] 执行超过指令上限，pc={}（疑似死循环）", pc);
            return Err("执行超过指令上限（疑似死循环）");
        }
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
            0x0d => {
                // move-exception vAA
                let a = ((u >> 8) & 0xff) as usize;
                regs[a] = unsafe { THROW_VAL };
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
            0x14 => {
                // const vAA, #+BBBBBBBB  (31i)
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 2 >= n {
                    return Err("const 被截断");
                }
                let lo = unit(pc as usize + 1) as u32;
                let hi = unit(pc as usize + 2) as u32;
                regs[a] = ((hi << 16) | lo) as i32;
                pc += 3;
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
                regs[a] = heap_new(type_idx)?;
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
            0x27 => {
                // throw vAA
                let a = ((u >> 8) & 0xff) as usize;
                unsafe { THROW_VAL = regs[a] };
                match find_handler(d, coff, pc as usize)? {
                    Some(hp) => pc = hp as i32,
                    None => return Err(THROW_SENTINEL),
                }
            }
            0x28 => pc += sx8((u >> 8) & 0xff),
            0x29 => {
                if (pc as usize) + 1 >= n {
                    return Err("goto/16 被截断");
                }
                pc += unit(pc as usize + 1) as i16 as i32;
            }
            0x32..=0x37 => {
                // 22t if-test：0x32 if-eq / 0x33 if-ne / 0x34 if-lt
                //               0x35 if-ge / 0x36 if-gt / 0x37 if-le
                let a = ((u >> 8) & 0xf) as usize;
                let b = ((u >> 12) & 0xf) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("if-test 被截断");
                }
                let off = sx8(unit(pc as usize + 1) >> 8);
                let x = regs[a];
                let y = regs[b];
                let take = match op {
                    0x32 => x == y,
                    0x33 => x != y,
                    0x34 => x < y,
                    0x35 => x >= y,
                    0x36 => x > y,
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
            0x60 | 0x67 => {
                // sget / sput vAA, field@BBBB  (21c)
                let a = ((u >> 8) & 0xff) as usize;
                if (pc as usize) + 1 >= n {
                    return Err("sget/sput 被截断");
                }
                let f = unit(pc as usize + 1) as usize;
                if f >= NSFIELD {
                    return Err("静态字段槽越界");
                }
                if op == 0x60 {
                    regs[a] = unsafe { SFIELDS[f] };
                } else {
                    unsafe { SFIELDS[f] = regs[a] };
                }
                pc += 2;
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
                regs[a] = rd_i32(obj + 16 + (f * 4) as u64);
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
                wv_i32(obj + 16 + (f * 4) as u64, regs[a]);
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
            0x6e | 0x6f | 0x70 | 0x71 => {
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
                if m.class == "Lcom/aster/App;" {
                    if m.name == "arg" {
                        last = unsafe { START_ARG };
                    } else {
                        return Err("App 上没有这个方法");
                    }
                } else if m.class == "Lcom/aster/Log;" {
                    if m.name == "i" {
                        let s = str_of(d, h, args[0])?;
                        crate::println!("  [DEX] {}", s);
                    } else if m.name == "n" {
                        crate::println!("  [DEX] {}", args[0]);
                    } else {
                        return Err("Log 上没有这个方法");
                    }
                    last = 0;
                } else if m.class == "Ljava/lang/StringBuilder;" {
                    // 内建类：不查 code_of，直接在内核里拼
                    if m.name == "<init>" {
                        sb_reset();
                        last = 0;
                    } else if m.name == "append" {
                        // append(I) 与 append(String) 靠 shorty 第二个字符区分
                        let t = m.shorty.as_bytes().get(1).copied().unwrap_or(b'?');
                        if t == b'I' {
                            sb_push_int(args[1])?;
                        } else {
                            let sv = str_of(d, h, args[1])?;
                            sb_push_str(sv)?;
                        }
                        last = args[0];
                    } else if m.name == "toString" {
                        last = sb_freeze()?;
                    } else {
                        return Err("StringBuilder 上没有这个方法");
                    }
                } else if m.class == "Lcom/aster/Canvas;" {
                    // 内建画布：不查 code_of，命令交给 apkcanvas 记录
                    match m.name {
                        "<init>" => {
                            crate::apkcanvas::reset();
                            crate::apkcanvas::set_active(true);
                        }
                        "clear" => crate::apkcanvas::clear(args[1] as u32),
                        "setColor" => crate::apkcanvas::set_color(args[1] as u32),
                        "drawRect" => crate::apkcanvas::rect(args[1], args[2], args[3], args[4]),
                        "drawText" => {
                            let sv = str_of(d, h, args[1])?;
                            crate::apkcanvas::text(args[2], args[3], sv);
                        }
                        "touchX" => {
                            last = match crate::apkcanvas::take_touch() {
                                Some((tx, _)) => tx,
                                None => -1,
                            };

                        }
                        "touchY" => last = crate::apkcanvas::last_touch().1,
                        "moveX" => {
                            last = match crate::apkcanvas::take_move() {
                                Some((tx, _)) => tx,
                                None => -1,
                            };
                        }
                        "moveY" => last = crate::apkcanvas::last_move().1,
                        "width" => last = crate::apkcanvas::CW,
                        "height" => last = crate::apkcanvas::CH,
                        _ => return Err("Canvas 上没有这个方法"),
                    }
                } else {
                    let c2 = code_of(d, h, m.class, m.name)?;
                    match exec(d, h, c2, &args[..argc], depth + 1, steps) {
                        Ok(v) => last = v,
                        Err(e) if e == THROW_SENTINEL => {
                            // 异常跨方法传播：本方法有 handler 就接住
                            match find_handler(d, coff, pc as usize)? {
                                Some(hp) => {
                                    pc = hp as i32;
                                    *steps += 1;
                                    continue;
                                }
                                None => return Err(THROW_SENTINEL),
                            }
                        }
                        Err(e) => return Err(e),
                    }
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

/// 解析某个类的 static_values，把初值写进静态字段槽（APK-11 新增）。
/// 只认 VALUE_INT（demo 里的静态字段全是 int）。
pub fn load_statics(d: &[u8], h: &Header, class_desc: &str) -> Result<(), &'static str> {
    let cd = class_def_pos(d, h, class_desc)?;
    let class_data_off = u32le(d, cd + 24);
    let sv_off = u32le(d, cd + 28);
    if class_data_off == 0 || class_data_off == 0xFFFF_FFFF {
        return Ok(());
    }
    let mut p = class_data_off as usize;
    let (n_static, np) = uleb(d, p)?;
    p = np;
    let (_, np) = uleb(d, p)?;
    p = np;
    let (_, np) = uleb(d, p)?;
    p = np;
    let (_, np) = uleb(d, p)?;
    p = np;

    let mut fids = [0u32; NSFIELD];
    let mut nf = 0usize;
    let mut idx = 0u32;
    let mut i = 0u32;
    while i < n_static {
        let (diff, np) = uleb(d, p)?;
        idx += diff;
        p = np;
        let (_, np) = uleb(d, p)?;
        p = np;
        if nf < NSFIELD {
            fids[nf] = idx;
            nf += 1;
        }
        i += 1;
    }

    if sv_off == 0 || sv_off == 0xFFFF_FFFF {
        return Ok(());
    }
    let mut q = sv_off as usize;
    let (size, nq) = uleb(d, q)?;
    q = nq;
    let mut k = 0u32;
    while k < size {
        if q >= d.len() {
            return Err("static_values 越界");
        }
        let b = d[q];
        q += 1;
        let nb = (b >> 5) as usize + 1;
        let mut v: i64 = 0;
        let mut j = 0usize;
        while j < nb {
            if q >= d.len() {
                return Err("static_values 越界");
            }
            v |= (d[q] as i64) << (j * 8);
            q += 1;
            j += 1;
        }
        let bits = nb * 8;
        if bits < 64 && (v & (1i64 << (bits - 1))) != 0 {
            v -= 1i64 << bits;
        }
        if (k as usize) < nf {
            let fi = fids[k as usize] as usize;
            if fi < NSFIELD {
                unsafe { SFIELDS[fi] = v as i32 };
            }
        }
        k += 1;
    }
    Ok(())
}

pub fn has_method(d: &[u8], h: &Header, class_desc: &str, name: &str) -> bool {
    code_of(d, h, class_desc, name).is_ok()
}

/// 启动 app：清堆、清静态、装初值、调 onCreate（若有）、确认 frame 存在（APK-11/12）
pub fn app_begin(d: &[u8], h: &Header, class_desc: &str) -> Result<(u32, bool), &'static str> {
    heap_reset();
    call_reset();
    unsafe { THROW_VAL = 0 };
    load_statics(d, h, class_desc)?;
    let mut steps = 0u32;
    let has_create = has_method(d, h, class_desc, "onCreate");
    if has_create {
        let coff = code_of(d, h, class_desc, "onCreate")?;
        exec(d, h, coff, &[], 0, &mut steps)?;
    }
    code_of(d, h, class_desc, "frame")?;
    Ok((steps, has_create))
}

/// 投递一次触摸给 onTouch(int kind, int x, int y)（kind: 0=按下 1=拖动 2=抬起）
pub fn app_touch(
    d: &[u8],
    h: &Header,
    class_desc: &str,
    kind: i32,
    x: i32,
    y: i32,
) -> Result<u32, &'static str> {
    if !has_method(d, h, class_desc, "onTouch") {
        return Ok(0);
    }
    let coff = code_of(d, h, class_desc, "onTouch")?;
    let mut steps = 0u32;
    exec(d, h, coff, &[kind, x, y], 0, &mut steps)?;
    Ok(steps)
}

/// 停止 app：调 onDestroy（若有）
pub fn app_end(d: &[u8], h: &Header, class_desc: &str) -> Result<(u32, bool), &'static str> {
    if !has_method(d, h, class_desc, "onDestroy") {
        return Ok((0, false));
    }
    let coff = code_of(d, h, class_desc, "onDestroy")?;
    let mut steps = 0u32;
    exec(d, h, coff, &[], 0, &mut steps)?;
    Ok((steps, true))
}

/// 跑一帧（不重置任何状态，静态字段/对象堆跨帧保留）
pub fn app_step(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let coff = code_of(d, h, class_desc, method_name)?;
    let mut steps = 0u32;
    exec(d, h, coff, &[], 0, &mut steps)?;
    Ok(steps)
}

pub fn run(d: &[u8], h: &Header, class_desc: &str, method_name: &str) -> Result<u32, &'static str> {
    let coff = code_of(d, h, class_desc, method_name)?;
    heap_reset();
    call_reset();
    unsafe { THROW_VAL = 0; }
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
