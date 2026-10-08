//! 流暢播放（依螢幕更新率同步影像）：設定的型別。決策與套用在之後的批次加上。

use serde::{Deserialize, Serialize};

/// 流暢播放的設定。介面上是兩個勾選：
/// 「流暢播放」沒勾 = Off；「使用電池時暫停」勾 = Auto、沒勾 = Always
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmoothMode {
    /// 開，使用電池時暫停
    Auto,
    /// 一直開
    Always,
    /// 關（一般播放）。先預設關，在實際的螢幕上量過之後再改成 Auto
    #[default]
    Off,
}

#[cfg(test)]
mod tests {
    use super::SmoothMode;

    #[test]
    fn default_is_off_and_names_are_kebab_case() {
        assert_eq!(SmoothMode::default(), SmoothMode::Off);
        assert_eq!(serde_json::to_value(SmoothMode::Always).unwrap(), "always");
        assert_eq!(
            serde_json::from_str::<SmoothMode>(r#""auto""#).unwrap(),
            SmoothMode::Auto
        );
    }
}
