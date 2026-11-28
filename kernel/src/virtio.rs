// Aster Kernel — virtio-mmio 传输层（modern 接口，version 2）
//
// QEMU 的 virt 机器在 0x0a000000 起摆了 32 个 virtio-mmio 槽，每个 0x200 字节。
// 比走 PCI 简单得多：没有 ECAM 枚举、没有 BAR 映射、没有 MSI，直接按偏移读写寄存器。
//
// 寄存器（偏移）：
//   0x000 MagicValue  0x74726976 ("virt")
//   0x004 Version     2 = modern (virtio 1.0)
//   0x008 DeviceID    1=net 2=block 18=input
//   0x00c VendorID    0x554d4551 ("QEMU")
//   0x010/0x014 DeviceFeatures / Sel
//   0x020/0x024 DriverFeatures / Sel
//   0x030 QueueSel
//   0x034 QueueNumMax
//   0x038 QueueNum
//   0x044 QueueReady
//   0x050 QueueNotify
//   0x060 InterruptStatus
//   0x064 InterruptACK
//   0x070 Status
//   0x080 QueueDescLow / 0x084 High
//   0x090 QueueDriverLow / 0x094 High
//   0x0a0 QueueDeviceLow / 0x0a4 High
#![allow(dead_code, static_mut_refs)]

use core::arch::asm;

pub const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;
pub const VIRTIO_MMIO_SLOTS: u64 = 32;

pub const DEV_NET: u32 = 1;
pub const DEV_BLOCK: u32 = 2;
pub const DEV_INPUT: u32 = 18;
pub const DEV_GPU: u32 = 16;

pub const STATUS_ACK: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;

pub const DESC_F_NEXT: u16 = 1;
pub const DESC_F_WRITE: u16 = 2;

#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C, align(2))]
#[derive(Clone, Copy)]
pub struct Avail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 64],
    pub used_event: u16,
}

#[repr(C, align(4))]
#[derive(Clone, Copy, Default)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct Used {
    pub flags: u16,
    pub idx: u16,
    pub ring: [UsedElem; 64],
    pub avail_event: u16,
}

#[inline]
fn r32(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

#[inline]
fn w32(base: u64, off: u64, v: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
}

#[inline]
pub fn barrier() {
    unsafe {
        asm!("dsb sy", options(nostack, preserves_flags));
    }
}

/// 一个 virtio-mmio 设备
#[derive(Clone, Copy)]
pub struct Device {
    pub base: u64,
    pub dev_id: u32,
    pub slot: u32,
    pub version: u32, // 1 = legacy，2 = modern
}

pub fn probe(slot: u64) -> Option<Device> {
    let base = VIRTIO_MMIO_BASE + slot * VIRTIO_MMIO_STRIDE;
    let magic = r32(base, 0x000);
    if magic != 0x7472_6976 {
        return None;
    }
    let version = r32(base, 0x004);
    // QEMU 6.2 的 virtio-mmio 默认是 legacy（version 1），有些版本是 2，两种都得接
    if version != 1 && version != 2 {
        return None;
    }
    let dev_id = r32(base, 0x008);
    if dev_id == 0 {
        return None;
    }
    Some(Device { base, dev_id, slot: slot as u32, version })
}

/// 调试：把所有非空槽位的 magic/version/deviceid 打出来
pub fn dump_slots() {
    for i in 0..VIRTIO_MMIO_SLOTS {
        let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        let magic = r32(base, 0x000);
        if magic != 0 {
            crate::println!(
                "[VIRTIO] slot {:2} magic={:#010x} version={} dev={} vendor={:#x}",
                i, magic, r32(base, 0x004), r32(base, 0x008), r32(base, 0x00c)
            );
        }
    }
}

/// 扫描全部槽位，找指定类型的设备（可跳过前 n 个）
pub fn find(dev_id: u32, skip: usize) -> Option<Device> {
    let mut seen = 0;
    for i in 0..VIRTIO_MMIO_SLOTS {
        if let Some(d) = probe(i) {
            if d.dev_id == dev_id {
                if seen == skip {
                    return Some(d);
                }
                seen += 1;
            }
        }
    }
    None
}

impl Device {
    pub fn reset(&self) {
        w32(self.base, 0x070, 0);
    }

    pub fn status(&self) -> u32 {
        r32(self.base, 0x070)
    }

    pub fn set_status(&self, v: u32) {
        w32(self.base, 0x070, v);
    }

    pub fn add_status(&self, bits: u32) {
        let v = self.status() | bits;
        self.set_status(v);
    }

    pub fn queue_max(&self, idx: u32) -> u32 {
        w32(self.base, 0x030, idx);
        r32(self.base, 0x034)
    }

    pub fn device_features(&self) -> u64 {
        w32(self.base, 0x014, 0);
        let lo = r32(self.base, 0x010) as u64;
        w32(self.base, 0x014, 1);
        let hi = r32(self.base, 0x010) as u64;
        (hi << 32) | lo
    }

    /// 协商特性：只接受 want 里的位（legacy 没有 Sel，直接写 0x020）
    pub fn set_features(&self, want: u64) {
        if self.version == 1 {
            w32(self.base, 0x020, (want & 0xffff_ffff) as u32);
        } else {
            w32(self.base, 0x024, 0);
            w32(self.base, 0x020, (want & 0xffff_ffff) as u32);
            w32(self.base, 0x024, 1);
            w32(self.base, 0x020, (want >> 32) as u32);
        }
    }

    /// legacy 队列：desc|avail|used 必须在同一块页对齐的连续内存里，写 QueuePFN 即可
    pub fn setup_queue_legacy(&self, idx: u32, mem: u64, size: u16) -> bool {
        w32(self.base, 0x030, idx);
        let max = r32(self.base, 0x034);
        if size > max as u16 || size == 0 {
            return false;
        }
        w32(self.base, 0x038, size as u32);
        w32(self.base, 0x080, 4096); // QueueAlign
        w32(self.base, 0x084, (mem >> 12) as u32); // QueuePFN
        true
    }

    pub fn setup_queue(&self, idx: u32, desc: *const Desc, avail: *const Avail, used: *const Used, num: u16) -> bool {
        w32(self.base, 0x030, idx);
        let max = r32(self.base, 0x034);
        if num > max as u16 || num == 0 {
            return false;
        }
        w32(self.base, 0x038, num as u32);
        let d = desc as u64;
        let a = avail as u64;
        let u = used as u64;
        w32(self.base, 0x080, d as u32);
        w32(self.base, 0x084, (d >> 32) as u32);
        w32(self.base, 0x090, a as u32);
        w32(self.base, 0x094, (a >> 32) as u32);
        w32(self.base, 0x0a0, u as u32);
        w32(self.base, 0x0a4, (u >> 32) as u32);
        w32(self.base, 0x044, 1);
        true
    }

    pub fn notify(&self, queue: u32) {
        barrier();
        w32(self.base, 0x050, queue);
    }

    /// 写 config 空间（virtio-input 用它选子区域）
    pub fn write_config_u8(&self, off: u64, v: u8) {
        unsafe { core::ptr::write_volatile((self.base + 0x100 + off) as *mut u8, v) }
    }

    pub fn read_config_u8(&self, off: u64) -> u8 {
        unsafe { core::ptr::read_volatile((self.base + 0x100 + off) as *const u8) }
    }

    pub fn read_config_u32(&self, off: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + 0x100 + off) as *const u32) }
    }

    pub fn read_config_u64(&self, off: u64) -> u64 {
        unsafe { core::ptr::read_volatile((self.base + 0x100 + off) as *const u64) }
    }

    pub fn read_config_u16(&self, off: u64) -> u16 {
        unsafe { core::ptr::read_volatile((self.base + 0x100 + off) as *const u16) }
    }
}

/// split 队列的宿主侧视图
pub struct Queue {
    pub desc: *mut Desc,
    pub avail: *mut Avail,
    pub used: *mut Used,
    pub size: u16,
    pub last_used: u16,
    pub free_head: u16,
    pub num_free: u16,
}

impl Queue {
    pub const fn empty() -> Queue {
        Queue {
            desc: core::ptr::null_mut(),
            avail: core::ptr::null_mut(),
            used: core::ptr::null_mut(),
            size: 0,
            last_used: 0,
            free_head: 0,
            num_free: 0,
        }
    }

    /// 只做宿主侧绑定（清环、串空闲链），设备侧由调用方按 legacy/modern 各自处理
    pub unsafe fn bind(&mut self, desc: *mut Desc, avail: *mut Avail, used: *mut Used, size: u16) {
        self.desc = desc;
        self.avail = avail;
        self.used = used;
        self.size = size;
        self.last_used = 0;
        self.free_head = 0;
        self.num_free = size;
        for i in 0..size {
            (*desc.add(i as usize)).next = if i + 1 < size { i + 1 } else { 0 };
        }
        core::ptr::write_volatile(&mut (*avail).idx, 0);
        core::ptr::write_volatile(&mut (*avail).flags, 0);
        core::ptr::write_volatile(&mut (*used).idx, 0);
        core::ptr::write_volatile(&mut (*used).flags, 0);
    }

    /// 分配一个描述符索引
    pub fn alloc(&mut self) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }
        let i = self.free_head;
        unsafe {
            self.free_head = (*self.desc.add(i as usize)).next;
        }
        self.num_free -= 1;
        Some(i)
    }

    pub fn free_chain(&mut self, head: u16) {
        unsafe {
            (*self.desc.add(head as usize)).next = self.free_head;
        }
        self.free_head = head;
        self.num_free += 1;
    }

    pub unsafe fn set(&mut self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let d = self.desc.add(i as usize);
        (*d).addr = addr;
        (*d).len = len;
        (*d).flags = flags;
        (*d).next = next;
    }

    pub unsafe fn submit(&mut self, head: u16) {
        let a = self.avail;
        let idx = (*a).idx;
        (*a).ring[(idx as usize) % (self.size as usize)] = head;
        barrier();
        core::ptr::write_volatile(&mut (*a).idx, idx.wrapping_add(1));
    }

    /// 有没有新完成的缓冲（不阻塞）
    pub unsafe fn pop_used(&mut self) -> Option<(u32, u32)> {
        let u = self.used;
        let used_idx = core::ptr::read_volatile(&(*u).idx);
        if used_idx == self.last_used {
            return None;
        }
        let e = (*u).ring[(self.last_used as usize) % (self.size as usize)];
        self.last_used = self.last_used.wrapping_add(1);
        Some((e.id, e.len))
    }
}
