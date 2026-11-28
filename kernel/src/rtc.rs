//! PL031 实时时钟（QEMU virt 机器挂在 0x0901_0000）
//!
//! RTCDR 是自 1970 起的秒数，RTCCR 的 bit0 是使能位。QEMU 默认 base=utc，
//! 上电值就是宿主机的当前时间；如果读到 2017 年以前的数，说明这版机器
//! 没给 RTC 供电（比如 -rtc base=2000-01-01），直接当没有时钟用。

const RTC_BASE: usize = 0x0901_0000;
const RTC_DR: usize = 0x00;
const RTC_CR: usize = 0x0c;
/// 固定上海时区（不跟随宿主机、不看 QEMU 的 -rtc 参数）
pub const TZ_NAME: &str = "Asia/Shanghai (CST)";
pub const TZ_OFFSET_HOURS: u32 = 8;
const CST_OFFSET: u64 = (TZ_OFFSET_HOURS as u64) * 3600;

pub fn offset_hours() -> u32 {
    TZ_OFFSET_HOURS
}

/// 顶栏日期：MM-DD
pub fn md() -> Option<(u32, u32)> {
    let t = local()?;
    Some((t.1, t.2))
}

pub fn read_utc() -> Option<u64> {
    unsafe {
        let cr = core::ptr::read_volatile((RTC_BASE + RTC_CR) as *const u32);
        if cr & 1 == 0 {
            return None;
        }
        let v = core::ptr::read_volatile((RTC_BASE + RTC_DR) as *const u32) as u64;
        if v < 1_500_000_000 {
            None
        } else {
            Some(v)
        }
    }
}

fn civil_from_days(z: i64) -> (u32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as u32, m, d)
}

/// 上海时间：年 月 日 时 分 秒
pub fn local() -> Option<(u32, u32, u32, u32, u32, u32)> {
    let secs = read_utc()? + CST_OFFSET;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    Some((y, m, d, (rem / 3_600) as u32, ((rem % 3_600) / 60) as u32, (rem % 60) as u32))
}

/// 底栏时钟用：HH:MM
pub fn hm() -> Option<(u32, u32)> {
    let t = local()?;
    Some((t.3, t.4))
}
