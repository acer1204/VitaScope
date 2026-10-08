//! 電源：接著電源還是用電池（流暢播放在「使用電池時暫停」時參考）。
//!
//! 每個平台問作業系統的方式不同，判斷的規則寫成純函式，各自有測試。

use std::path::Path;

/// 電源的來源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerSource {
    /// 接著電源（或桌機，沒有電池）
    Ac,
    /// 用電池
    Battery,
    /// 不知道：當成接著電源（不因為查不到就關掉功能）
    #[default]
    Unknown,
}

impl PowerSource {
    /// 面板上的說明
    pub fn label(self) -> &'static str {
        match self {
            Self::Ac => crate::tr!("接上電源", "Plugged in"),
            Self::Battery => crate::tr!("使用電池", "On battery"),
            Self::Unknown => crate::tr!("不明（當成接上電源）", "Unknown (treated as plugged in)"),
        }
    }
}

/// 目前的電源
pub fn source() -> PowerSource {
    platform::source()
}

/// Windows `GetSystemPowerStatus`：BatteryFlag 的 128 = 沒有系統電池（桌機），但 255（讀不到電池狀態）
/// 也有這個位元，那時照 ACLineStatus；ACLineStatus 0 = 電池、1 = 電源、255 = 不知道
pub fn from_windows(ac_line_status: u8, battery_flag: u8) -> PowerSource {
    if battery_flag != 255 && battery_flag & 128 != 0 {
        return PowerSource::Ac;
    }
    match ac_line_status {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => PowerSource::Unknown,
    }
}

/// macOS `IOPSGetTimeRemainingEstimate()`：-2（kIOPSTimeRemainingUnlimited）= 接著電源；
/// -1（還在估計）或剩餘秒數 = 用電池
pub fn from_time_remaining(seconds: f64) -> PowerSource {
    if seconds == -2.0 {
        PowerSource::Ac
    } else {
        PowerSource::Battery
    }
}

/// Linux：`<root>/class/power_supply/*/{type,online,scope,status}`（root 平常是 /sys）。
/// - 電池以外的電源（Mains、USB、USB-C…）有一個 online=1 → 接著電源
/// - 系統的電池（scope 不是 Device：滑鼠、耳機的電池不算）正在充電 → 接著電源（有些筆電不列出變壓器）
/// - 有系統的電池 → 用電池
/// - 都沒有（桌機、虛擬機）→ 不知道
pub fn from_sysfs(root: &Path) -> PowerSource {
    let Ok(entries) = std::fs::read_dir(root.join("class/power_supply")) else {
        return PowerSource::Unknown;
    };
    let read = |dir: &Path, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut battery = false;
    let mut charging = false;
    for entry in entries.flatten() {
        let dir = entry.path();
        // 周邊裝置自己的電源（滑鼠、手把）跟這台電腦用什麼電無關
        if read(&dir, "scope").eq_ignore_ascii_case("device") {
            continue;
        }
        if read(&dir, "type") == "Battery" {
            battery = true;
            charging |= read(&dir, "status") == "Charging";
        } else if read(&dir, "online") == "1" {
            return PowerSource::Ac;
        }
    }
    match (battery, charging) {
        (true, true) => PowerSource::Ac,
        (true, false) => PowerSource::Battery,
        (false, _) => PowerSource::Unknown,
    }
}

#[cfg(windows)]
mod platform {
    use super::PowerSource;
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    pub fn source() -> PowerSource {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: 單純的系統呼叫
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return PowerSource::Unknown;
        }
        super::from_windows(status.ACLineStatus, status.BatteryFlag)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::PowerSource;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// CFTimeInterval（秒）
        fn IOPSGetTimeRemainingEstimate() -> f64;
    }

    pub fn source() -> PowerSource {
        // SAFETY: 沒有參數、不用釋放
        super::from_time_remaining(unsafe { IOPSGetTimeRemainingEstimate() })
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    pub fn source() -> super::PowerSource {
        super::from_sysfs(std::path::Path::new("/sys"))
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn source() -> super::PowerSource {
        super::PowerSource::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn windows_status() {
        // 桌機：沒有系統電池，ACLineStatus 是什麼都算接著電源
        assert_eq!(from_windows(1, 128), PowerSource::Ac);
        assert_eq!(from_windows(255, 128), PowerSource::Ac);
        assert_eq!(from_windows(0, 128 | 8), PowerSource::Ac);
        // 筆電
        assert_eq!(from_windows(0, 1), PowerSource::Battery);
        assert_eq!(from_windows(1, 8), PowerSource::Ac);
        assert_eq!(from_windows(255, 0), PowerSource::Unknown);
        // 讀不到電池狀態（255 也有 128 這個位元，但不是「沒有電池」）：照 ACLineStatus
        assert_eq!(from_windows(0, 255), PowerSource::Battery);
        assert_eq!(from_windows(1, 255), PowerSource::Ac);
        assert_eq!(from_windows(255, 255), PowerSource::Unknown);
    }

    #[test]
    fn macos_time_remaining() {
        assert_eq!(from_time_remaining(-2.0), PowerSource::Ac);
        assert_eq!(from_time_remaining(-1.0), PowerSource::Battery, "還在估計");
        assert_eq!(from_time_remaining(3600.0), PowerSource::Battery);
    }

    /// 暫存資料夾裡的假 /sys
    struct FakeSys(PathBuf);

    impl FakeSys {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("vitascope-power-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("class/power_supply")).unwrap();
            Self(root)
        }

        fn supply(self, name: &str, files: &[(&str, &str)]) -> Self {
            let dir = self.0.join("class/power_supply").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, value) in files {
                std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
            }
            self
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn linux_sysfs() {
        // 桌機：沒有任何電源資訊
        let desktop = FakeSys::new("desktop");
        assert_eq!(from_sysfs(&desktop.0), PowerSource::Unknown);
        // 連 power_supply 資料夾都沒有（容器）
        assert_eq!(from_sysfs(&desktop.0.join("沒有這個資料夾")), PowerSource::Unknown);

        let on_ac = FakeSys::new("on-ac")
            .supply("AC", &[("type", "Mains"), ("online", "1")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Full"), ("scope", "System")]);
        assert_eq!(from_sysfs(&on_ac.0), PowerSource::Ac);

        let on_battery = FakeSys::new("on-battery")
            .supply("AC", &[("type", "Mains"), ("online", "0")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Discharging")]);
        assert_eq!(from_sysfs(&on_battery.0), PowerSource::Battery);

        // USB-C 充電（UCSI）：type 是 USB（真正的名稱裡有「:」，Windows 的資料夾名稱不能用，改成「.」）。
        // 電池已經充滿（不是 Charging），只有 USB 的 online=1 能判斷接著電源
        let usb_c = FakeSys::new("usb-c")
            .supply("ucsi-source-psy-USBC000.001", &[("type", "USB"), ("online", "1")])
            .supply("ucsi-source-psy-USBC000.002", &[("type", "USB"), ("online", "0")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Not charging")]);
        assert_eq!(from_sysfs(&usb_c.0), PowerSource::Ac);
        // 拔掉充電器
        let usb_c_unplugged = FakeSys::new("usb-c-unplugged")
            .supply("ucsi-source-psy-USBC000.001", &[("type", "USB"), ("online", "0")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Discharging")]);
        assert_eq!(from_sysfs(&usb_c_unplugged.0), PowerSource::Battery);

        // 桌機接了無線滑鼠：滑鼠的電池（scope=Device）不算
        let mouse = FakeSys::new("mouse").supply(
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("status", "Discharging")],
        );
        assert_eq!(from_sysfs(&mouse.0), PowerSource::Unknown);

        // 沒有列出變壓器的筆電：電池正在充電 = 接著電源
        let charging = FakeSys::new("charging").supply("BAT1", &[("type", "Battery"), ("status", "Charging")]);
        assert_eq!(from_sysfs(&charging.0), PowerSource::Ac);
    }

    #[test]
    fn this_machine_has_a_power_source() {
        // 只確認不會出錯（實際的值依電腦而定）
        let _ = source();
    }
}
