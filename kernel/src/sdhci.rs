// Aster Kernel — SDHCI（EMMC / SD 控制器）最小驱动骨架
//
// WIFI-1 的第一步：树莓派的 BCM43438 无线芯片挂在 SDIO 上，而 SDIO 又挂在
// EMMC(SDHCI) 控制器上。所以要先能把 SDHCI 点亮、发出 CMD0/CMD5/CMD3/CMD7，
// 才谈得上读 CIS、加载固件。这里只做"能探测、能发命令"的骨架：
// 寄存器布局、命令表、超时轮询都是真的，缺的是真机上才有机会验证的时序细节。
#![allow(dead_code)]

pub const REG_SDMA: u64 = 0x00;
pub const REG_BLKSIZE: u64 = 0x04;
pub const REG_BLKCOUNT: u64 = 0x06;
pub const REG_ARG: u64 = 0x08;
pub const REG_XFER: u64 = 0x0C;
pub const REG_CMD: u64 = 0x0E;
pub const REG_RESP0: u64 = 0x10;
pub const REG_RESP1: u64 = 0x14;
pub const REG_RESP2: u64 = 0x18;
pub const REG_RESP3: u64 = 0x1C;
pub const REG_BUF: u64 = 0x20;
pub const REG_HOSTCTL: u64 = 0x28;
pub const REG_CLKCTL: u64 = 0x2C;
pub const REG_TIMEOUT: u64 = 0x2E;
pub const REG_SWRST: u64 = 0x2F;
pub const REG_NORMAL_INT: u64 = 0x30;
pub const REG_ERR_INT: u64 = 0x32;
pub const REG_NORMAL_INT_EN: u64 = 0x34;
pub const REG_ERR_INT_EN: u64 = 0x36;
pub const REG_CAPS: u64 = 0x40;

pub const CMD_RESP_NONE: u16 = 0;
pub const CMD_RESP_136: u16 = 1;
pub const CMD_RESP_48: u16 = 2;
pub const CMD_RESP_48_BUSY: u16 = 3;

#[inline]
unsafe fn rd(base: u64, off: u64) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}

#[inline]
unsafe fn wr(base: u64, off: u64, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v);
}

/// 当前板子有没有 SDHCI；有就返回基址
pub fn base() -> Option<u64> {
    crate::board::ACTIVE.emmc
}

/// 读 CAPABILITIES 的低 16 位（版本号在 bit0..7）；读不到就当作没有控制器
pub fn probe() -> Option<(u64, u32)> {
    let b = base()?;
    let caps = unsafe { rd(b, REG_CAPS) };
    if caps == 0 || caps == 0xffff_ffff {
        return None;
    }
    Some((b, caps))
}

/// 软复位（ALL），等复位位自己落下
pub fn reset(b: u64) -> bool {
    unsafe {
        wr(b, REG_SWRST, 1);
        for _ in 0..100_000u32 {
            if rd(b, REG_SWRST) & 1 == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

/// 设 SD 时钟：先关内部时钟、写分频、再打开
pub fn set_clock(b: u64, divider: u16, sdclk_enable: bool) -> bool {
    unsafe {
        let arg = ((divider as u32) << 8) | if sdclk_enable { 1 } else { 0 };
        wr(b, REG_CLKCTL, arg);
        for _ in 0..10_000u32 {
            if rd(b, REG_CLKCTL) & 0x2 != 0 {
                // internal clock stable
                break;
            }
            core::hint::spin_loop();
        }
        wr(b, REG_CLKCTL, arg | 0x4); // SD clock enable
    }
    true
}

/// 发一条命令，等命令完成；返回响应（仅 48/136 位）
pub fn cmd(b: u64, idx: u8, arg: u32, resp: u16) -> Option<u32> {
    unsafe {
        // 清中断状态
        wr(b, REG_NORMAL_INT, 0xffff_ffff);
        wr(b, REG_ERR_INT, 0xffff_ffff);
        wr(b, REG_ARG, arg);
        wr(b, REG_CMD, ((idx as u32) << 8) | ((resp as u32) << 16) | 0x20);
        for _ in 0..200_000u32 {
            let ni = rd(b, REG_NORMAL_INT);
            if ni & 0x1 != 0 {
                // command complete
                let r = rd(b, REG_RESP0);
                wr(b, REG_NORMAL_INT, ni);
                return Some(r);
            }
            if ni & 0x8000 != 0 {
                // error interrupt
                break;
            }
            core::hint::spin_loop();
        }
    }
    None
}

/// SDIO 探测：CMD5(IO_SEND_OP_COND)。返回 true 说明卡在 IO 模式下应答了
pub fn sdio_probe(b: u64) -> bool {
    // arg: 电压窗口 + 功能号 0
    cmd(b, 5, 0x00FF_FFFF, CMD_RESP_48).is_some()
}

pub fn status_text() -> &'static str {
    match probe() {
        Some((_, caps)) => {
            if caps & 0x0000_00ff >= 2 {
                "SDHCI 控制器在线（版本 ≥ 2）"
            } else {
                "SDHCI 控制器在线（版本 1）"
            }
        }
        None => "没有 SDHCI 控制器（QEMU virt 上没有 EMMC）",
    }
}
