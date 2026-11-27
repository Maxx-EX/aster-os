// Aster Kernel — VideoCore mailbox（树莓派）
//
// 树莓派没有 fw_cfg，屏幕要从 VC 那边"申请"：往 mailbox 的 property 通道发一串
// tag，VC 回填宽高、深度，最后给一块 framebuffer 的总线地址。
//
// 通道 8 = property tags。消息格式：
//   u32 总长(字节) | u32 0 | tag* | u32 0(结束)
//   tag = u32 id | u32 值字节数 | u32 请求码(0) | 值...
// 请求发出去后 code 变成 0x8000_0000 | 原值，表示"已处理"。
//
// 关键坑：写进 mailbox 的是**总线地址**，不是 ARM 物理地址。BCM2837 的偏移是
// 0x4000_0000，BCM2711 是 0xC000_0000。拿回来的 framebuffer 地址同样要减掉偏移。
#![allow(dead_code, static_mut_refs)]

use crate::board;

const CH_PROP: u32 = 8;

// mailbox 寄存器偏移（基址见 board::ACTIVE.mailbox）
const MBOX0_READ: u64 = 0x00;
const MBOX0_STATUS: u64 = 0x18;
const MBOX1_WRITE: u64 = 0x20;
const MBOX1_STATUS: u64 = 0x38;

const STATUS_EMPTY: u32 = 1 << 30;
const STATUS_FULL: u32 = 1 << 31;

// property tags
const TAG_SET_PHYS_WH: u32 = 0x0004_8003;
const TAG_SET_VIRT_WH: u32 = 0x0004_8004;
const TAG_SET_DEPTH: u32 = 0x0004_8005;
const TAG_SET_VIRT_OFFSET: u32 = 0x0004_8009;
const TAG_ALLOCATE: u32 = 0x0004_0001;
const TAG_GET_PITCH: u32 = 0x0004_0008;

#[repr(C, align(16))]
struct Buf {
    d: [u32; 36],
}

static mut BUF: Buf = Buf { d: [0; 36] };

fn base() -> u64 {
    board::ACTIVE.mailbox.unwrap_or(0)
}

#[inline]
unsafe fn rd(off: u64) -> u32 {
    core::ptr::read_volatile((base() + off) as *const u32)
}

#[inline]
unsafe fn wr(off: u64, v: u32) {
    core::ptr::write_volatile((base() + off) as *mut u32, v)
}

/// 发一次 property 调用，成功返回 0
fn call(addr_phys: u64) -> Option<u32> {
    if base() == 0 {
        return None;
    }
    let val = (addr_phys + board::ACTIVE.bus_offset) as u32 | CH_PROP;
    unsafe {
        // 等 MBOX1 不忙
        let mut t = 0u32;
        while rd(MBOX1_STATUS) & STATUS_FULL != 0 {
            t += 1;
            if t > 1_000_000 {
                return None;
            }
        }
        wr(MBOX1_WRITE, val);
        // 等 VC 回包
        t = 0;
        loop {
            while rd(MBOX0_STATUS) & STATUS_EMPTY != 0 {
                t += 1;
                if t > 1_000_000 {
                    return None;
                }
            }
            let r = rd(MBOX0_READ);
            if r & 0xF == CH_PROP {
                return Some(r & !0xF);
            }
        }
    }
}

/// 申请一块 framebuffer。返回 (物理地址, 宽, 高, pitch)
pub fn fb_init(w: u32, h: u32) -> Option<(u64, u32, u32, u32)> {
    let buf = unsafe { core::ptr::addr_of_mut!(BUF) as *mut u32 };
    unsafe {
        // ---- 组包 ----
        let mut i = 0usize;
        let mut put = |v: u32| {
            *buf.add(i) = v;
            i += 1;
        };
        put(0); // 总长，最后回填
        put(0); // 请求码
        put(TAG_SET_PHYS_WH);
        put(8);
        put(0);
        put(w);
        put(h);
        put(TAG_SET_VIRT_WH);
        put(8);
        put(0);
        put(w);
        put(h);
        put(TAG_SET_DEPTH);
        put(4);
        put(0);
        put(32);
        put(TAG_SET_VIRT_OFFSET);
        put(8);
        put(0);
        put(0);
        put(0);
        put(TAG_ALLOCATE);
        put(8);
        put(0);
        put(4096); // 对齐要求
        put(0); // VC 回填总线地址
        put(TAG_GET_PITCH);
        put(4);
        put(0);
        put(0);
        put(0); // 结束 tag

        *buf = (i as u32) * 4;

        let phys = core::ptr::addr_of!(BUF) as u64;
        crate::println!("[MBOX] 请求 {}x{}  缓冲 phys={:#x} status0={:#x} status1={:#x}",
            w, h, phys, rd(MBOX0_STATUS), rd(MBOX1_STATUS));
        let r = call(phys);
        crate::println!("[MBOX] call -> {:?} code={:#x}", r, *buf.add(1));
        r?;

        // ---- 取值 ----
        let code = *buf.add(1);
        if code & 0x8000_0000 == 0 {
            return None;
        }
        // 逐 tag 扫，找 ALLOCATE 与 GET_PITCH 的回填位置
        let mut p = 2usize;
        let mut fb_bus = 0u32;
        let mut pitch = 0u32;
        let mut real_w = w;
        let mut real_h = h;
        while *buf.add(p) != 0 {
            let id = *buf.add(p);
            let sz = *buf.add(p + 1) as usize;
            let ok = *buf.add(p + 2) & 0x8000_0000 != 0;
            match id {
                TAG_SET_PHYS_WH | TAG_SET_VIRT_WH if ok => {
                    real_w = *buf.add(p + 3);
                    real_h = *buf.add(p + 4);
                }
                TAG_ALLOCATE if ok => fb_bus = *buf.add(p + 4),
                TAG_GET_PITCH if ok => pitch = *buf.add(p + 3),
                _ => {}
            }
            p += 2 + sz / 4 + 1;
        }
        crate::println!("[MBOX] fb bus={:#x} 宽高 {}x{} pitch={}", fb_bus, real_w, real_h, pitch);
        if fb_bus == 0 {
            return None;
        }
        // QEMU 的 raspi 模型直接给物理地址；真机的 VC 给总线地址，要减偏移。
        // 判据：低于 RAM 大小的当物理地址用。
        let raw = fb_bus as u64;
        let fb_phys = if raw < board::ACTIVE.ram_size {
            raw
        } else {
            raw - board::ACTIVE.bus_offset
        };
        if pitch == 0 {
            pitch = real_w * 4;
        }
        Some((fb_phys, real_w, real_h, pitch))
    }
}

/// 读一次板子的温度/时钟之类（用于调试，失败返回 None）
pub fn arm_memory() -> Option<(u32, u64)> {
    const TAG_GET_ARM_MEMORY: u32 = 0x0001_0008;
    let buf = unsafe { core::ptr::addr_of_mut!(BUF) as *mut u32 };
    unsafe {
        *buf.add(0) = 24;
        *buf.add(1) = 0;
        *buf.add(2) = TAG_GET_ARM_MEMORY;
        *buf.add(3) = 8;
        *buf.add(4) = 0;
        *buf.add(5) = 0;
        *buf.add(6) = 0;
        *buf.add(7) = 0;
        let phys = core::ptr::addr_of!(BUF) as u64;
        call(phys)?;
        if *buf.add(1) & 0x8000_0000 == 0 {
            return None;
        }
        let base_bytes = *buf.add(5) as u64;
        let size_bytes = *buf.add(6) as u64;
        Some((base_bytes as u32, size_bytes))
    }
}
