//! 块缓存：8 个 4 KiB 槽，写回式，LRU 淘汰。
//!
//! 上层（AsterFS）只认块号，命中就不发 DMA；写只改内存并标脏，
//! `flush()` 或淘汰时写回。分区起点缓存在 BASE 里，避免每次读盘都去读分区表。

use crate::blk;
use crate::disk;
use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};

pub const BS: usize = 4096;
const NSLOT: usize = 8;
const SPB: u64 = 8;
const SEC: usize = 512;
const EMPTY_TAG: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Slot {
    tag: u32,
    dirty: bool,
    stamp: u32,
    data: [u8; BS],
}

const EMPTY: Slot = Slot {
    tag: EMPTY_TAG,
    dirty: false,
    stamp: 0,
    data: [0; BS],
};

struct Slots(core::cell::UnsafeCell<[Slot; NSLOT]>);
unsafe impl Sync for Slots {}

static SLOTS: Slots = Slots(core::cell::UnsafeCell::new([EMPTY; NSLOT]));
static BASE: AtomicU64 = AtomicU64::new(u64::MAX);
static TICK: AtomicU32 = AtomicU32::new(1);
static HITS: AtomicU32 = AtomicU32::new(0);
static MISSES: AtomicU32 = AtomicU32::new(0);
static WRITES: AtomicU32 = AtomicU32::new(0);

fn slots() -> &'static mut [Slot; NSLOT] {
    unsafe { &mut *SLOTS.0.get() }
}

fn base() -> Option<u64> {
    let v = BASE.load(Ordering::Relaxed);
    if v != u64::MAX {
        return Some(v);
    }
    let info = disk::probe()?;
    let p = info.parts[0]?;
    BASE.store(p.start, Ordering::Relaxed);
    Some(p.start)
}

fn tick() -> u32 {
    let t = TICK.fetch_add(1, Ordering::Relaxed);
    if t == 0 {
        TICK.store(2, Ordering::Relaxed);
        1
    } else {
        t
    }
}

fn write_back(sl: &Slot) {
    if sl.tag == EMPTY_TAG {
        return;
    }
    if let Some(b0) = base() {
        let lba = b0 + sl.tag as u64 * SPB;
        for k in 0..SPB as usize {
            if blk::write(lba + k as u64, &sl.data[k * SEC..(k + 1) * SEC]) {
                WRITES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub fn get(b: u32) -> Option<&'static mut [u8; BS]> {
    let s = slots();
    for i in 0..NSLOT {
        if s[i].tag == b {
            s[i].stamp = tick();
            HITS.fetch_add(1, Ordering::Relaxed);
            return Some(&mut s[i].data);
        }
    }
    MISSES.fetch_add(1, Ordering::Relaxed);

    let mut vic = 0usize;
    let mut best = u32::MAX;
    let mut found = false;
    for i in 0..NSLOT {
        if s[i].tag == EMPTY_TAG {
            vic = i;
            found = true;
            break;
        }
        if s[i].stamp < best {
            best = s[i].stamp;
            vic = i;
        }
    }
    if !found && s[vic].dirty {
        write_back(&s[vic]);
    }
    let b0 = match base() {
        Some(x) => x,
        None => return None,
    };
    let lba = b0 + b as u64 * SPB;
    for k in 0..SPB as usize {
        if !blk::read(lba + k as u64, &mut s[vic].data[k * SEC..(k + 1) * SEC]) {
            s[vic].tag = EMPTY_TAG;
            return None;
        }
    }
    s[vic].tag = b;
    s[vic].dirty = false;
    s[vic].stamp = tick();
    Some(&mut s[vic].data)
}

pub fn mark_dirty(b: u32) -> bool {
    let s = slots();
    for i in 0..NSLOT {
        if s[i].tag == b {
            s[i].dirty = true;
            return true;
        }
    }
    false
}

pub fn flush() -> u32 {
    let s = slots();
    let mut n = 0u32;
    for i in 0..NSLOT {
        if s[i].tag != EMPTY_TAG && s[i].dirty {
            write_back(&s[i]);
            s[i].dirty = false;
            n += 1;
        }
    }
    n
}

pub fn invalidate() {
    let s = slots();
    for i in 0..NSLOT {
        s[i].tag = EMPTY_TAG;
        s[i].dirty = false;
        s[i].stamp = 0;
    }
}

pub fn reset_stats() {
    HITS.store(0, Ordering::Relaxed);
    MISSES.store(0, Ordering::Relaxed);
    WRITES.store(0, Ordering::Relaxed);
}

pub fn stats() -> (u32, u32, u32, u32) {
    let s = slots();
    let mut used = 0u32;
    let mut dirty = 0u32;
    for i in 0..NSLOT {
        if s[i].tag != EMPTY_TAG {
            used += 1;
            if s[i].dirty {
                dirty += 1;
            }
        }
    }
    (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        used,
        dirty,
    )
}

pub fn writes() -> u32 {
    WRITES.load(Ordering::Relaxed)
}

/// 格式化了分区，缓存里的覆盖层全部作废
pub fn reset_base() {
    BASE.store(u64::MAX, Ordering::Relaxed);
}
