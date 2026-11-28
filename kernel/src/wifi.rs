// Aster Kernel — 无线（WIFI-1）
//
// 现实约束：QEMU virt 没有任何 802.11 设备；树莓派 3/4 的 BCM43438 挂在 SDIO 上，
// 需要先有 SDHCI 驱动、再枚举 SDIO、再上传固件、再做 WPA2 四步握手。
// 所以这一层先立"后端 + 状态机 + 扫描接口"的骨架，硬件路径按板子分派：
//   - 有 SDHCI 的板子：走 SDIO 探测；
//   - QEMU：明确返回"没有无线硬件"，不假装扫出了 AP。
#![allow(dead_code)]

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Backend {
    /// 没有无线硬件：不假装有
    None,
    /// SDIO 上的 BCM43438（树莓派）
    SdioBrcm,
}

#[derive(Clone, Copy)]
pub struct WifiStatus {
    pub backend: Backend,
    /// SDHCI 控制器是否在线
    pub sdio_host: bool,
    /// SDIO 卡是否应答 CMD5
    pub sdio_card: bool,
}

pub fn probe() -> WifiStatus {
    let host = crate::sdhci::probe();
    match host {
        Some((b, _)) => {
            let card = crate::sdhci::sdio_probe(b);
            WifiStatus {
                backend: if card { Backend::SdioBrcm } else { Backend::None },
                sdio_host: true,
                sdio_card: card,
            }
        }
        None => WifiStatus {
            backend: Backend::None,
            sdio_host: false,
            sdio_card: false,
        },
    }
}

pub fn backend_name(b: Backend) -> &'static str {
    match b {
        Backend::None => "无（没有可用的 802.11 适配器）",
        Backend::SdioBrcm => "BCM43438（SDIO）",
    }
}

/// 扫描：目前没有驱动，返回 0 个 AP。
/// 真机接上之后这里会填 (ssid, rssi)，命令层已经按这个签名接好了。
pub fn scan(_buf: &mut [(&'static str, i16)]) -> usize {
    0
}

/// 四步路径的进度：0 未开始，1 SDK 就绪，2 固件加载，3 WPA2 关联，4 已联网
pub fn phase() -> u8 {
    0
}

pub fn phase_text(p: u8) -> &'static str {
    match p {
        0 => "未开始（缺 SDHCI → SDIO → 固件上传 → WPA2 四步）",
        1 => "SDIO 枚举完成",
        2 => "固件已上传",
        3 => "WPA2 握手完成",
        _ => "已联网",
    }
}
