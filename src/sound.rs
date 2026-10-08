//! 音效設定（輸出裝置、獨佔模式、音量上限、轉成立體聲、等化器、音量平衡、音訊直通）：
//! 設定的型別和純函式。套用到 mpv 的部分在之後的批次加上。

use serde::{Deserialize, Serialize};

/// 音效設定（存在 settings.json 的 `audio`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// mpv 的 audio-device 名稱；None = 預設裝置（跟隨系統）
    pub device: Option<String>,
    /// 裝置的顯示名稱（裝置拔掉時選單上還認得出是哪一個）
    pub device_label: Option<String>,
    /// 獨佔模式
    pub exclusive: bool,
    /// 音量上限（%）：100 / 130 / 150 / 200
    pub volume_max: u32,
    /// 多聲道轉成立體聲
    pub downmix: bool,
    /// 轉立體聲時避免破音（mpv 的 audio-normalize-downmix）
    pub normalize_downmix: bool,
    pub eq: Equalizer,
    pub leveling: Leveling,
    pub passthrough: Passthrough,
}

// 不用 derive：音量上限預設 100%、轉立體聲時避免破音預設開
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            device: None,
            device_label: None,
            exclusive: false,
            volume_max: 100,
            downmix: false,
            normalize_downmix: true,
            eq: Equalizer::default(),
            leveling: Leveling::Off,
            passthrough: Passthrough::default(),
        }
    }
}

/// 音量上限的選項（%）
pub const VOLUME_MAX_CHOICES: [u32; 4] = [100, 130, 150, 200];

/// 音量上限對齊到最接近的選項（一樣近時取小的，不會意外變大聲）
pub fn snap_volume_max(v: u32) -> u32 {
    VOLUME_MAX_CHOICES
        .into_iter()
        .min_by_key(|c| c.abs_diff(v))
        .unwrap_or(100)
}

/// 十段等化器的中心頻率（Hz）
pub const EQ_BANDS: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
/// 每一段的增益範圍（dB）
pub const EQ_MAX_GAIN: f32 = 12.0;

/// 等化器
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Equalizer {
    pub enabled: bool,
    pub preset: EqPreset,
    /// 「自訂」的十段增益（dB，−12…12）；選了其他預設時用預設的值
    pub gains: [f32; 10],
    /// 自動防止破音：有段落調高時，整體先降低同樣的量（前級）
    pub auto_preamp: bool,
}

// 不用 derive：自動防止破音預設開
impl Default for Equalizer {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: EqPreset::Flat,
            gains: [0.0; 10],
            auto_preamp: true,
        }
    }
}

impl Equalizer {
    /// 實際使用的十段增益：預設的值，或「自訂」存的值
    pub fn effective_gains(&self) -> [f32; 10] {
        self.preset.gains().unwrap_or(self.gains)
    }
}

/// 等化器預設
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EqPreset {
    /// 平坦
    #[default]
    Flat,
    /// 重低音
    Bass,
    /// 人聲
    Vocal,
    /// 古典
    Classical,
    /// 搖滾
    Rock,
    /// 流行
    Pop,
    /// 爵士
    Jazz,
    /// 電子
    Electronic,
    /// 高音加強
    Treble,
    /// 自訂（用 `Equalizer.gains`）
    Custom,
}

impl EqPreset {
    pub const ALL: [EqPreset; 10] = [
        EqPreset::Flat,
        EqPreset::Bass,
        EqPreset::Vocal,
        EqPreset::Classical,
        EqPreset::Rock,
        EqPreset::Pop,
        EqPreset::Jazz,
        EqPreset::Electronic,
        EqPreset::Treble,
        EqPreset::Custom,
    ];

    /// 預設的十段增益（dB，31 Hz…16 kHz）；「自訂」沒有固定的值。
    /// 這些是起始值，之後實際聽過再調整（記在 ROADMAP）
    pub fn gains(self) -> Option<[f32; 10]> {
        Some(match self {
            EqPreset::Flat => [0.0; 10],
            EqPreset::Bass => [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            EqPreset::Vocal => [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0],
            EqPreset::Classical => [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -2.0, -2.0, -2.0, -4.0],
            EqPreset::Rock => [5.0, 3.0, -2.0, -4.0, -1.0, 2.0, 5.0, 6.0, 6.0, 6.0],
            EqPreset::Pop => [-1.0, 2.0, 4.0, 5.0, 4.0, 0.0, -1.0, -1.0, -1.0, -1.0],
            EqPreset::Jazz => [3.0, 2.0, 1.0, 2.0, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0],
            EqPreset::Electronic => [4.0, 3.0, 1.0, 0.0, -2.0, 2.0, 1.0, 1.0, 3.0, 4.0],
            EqPreset::Treble => [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 7.0],
            EqPreset::Custom => return None,
        })
    }
}

/// 音量平衡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Leveling {
    #[default]
    Off,
    /// 夜間模式（小聲變大、大聲變小）：acompressor
    Night,
    /// 對白加強：speechnorm
    Dialogue,
    /// 音量平均：dynaudnorm
    Normalize,
}

/// 音訊直通（AC-3、DTS… 不解碼，直接交給擴大機）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Passthrough {
    pub enabled: bool,
    pub ac3: bool,
    pub eac3: bool,
    pub dts: bool,
    pub dts_hd: bool,
    pub truehd: bool,
}

// 不用 derive：常見的 AC-3、E-AC-3、DTS 預設勾選（DTS-HD、TrueHD 要 HDMI 支援 HBR，預設不勾）
impl Default for Passthrough {
    fn default() -> Self {
        Self {
            enabled: false,
            ac3: true,
            eac3: true,
            dts: true,
            dts_hd: false,
            truehd: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        // 每個結構都從空的 JSON 讀：沒寫的欄位要是規格的預設值，不是型別的零值
        let a: AudioSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(a, AudioSettings::default());
        assert_eq!(a.volume_max, 100);
        assert!(a.normalize_downmix);
        assert!(!a.downmix && !a.exclusive);
        assert_eq!((a.device.as_deref(), a.device_label.as_deref()), (None, None));
        assert_eq!(a.leveling, Leveling::Off);
        let eq: Equalizer = serde_json::from_str("{}").unwrap();
        assert_eq!(eq, Equalizer::default());
        assert!(!eq.enabled);
        assert!(eq.auto_preamp, "自動防止破音預設開");
        assert_eq!(eq.preset, EqPreset::Flat);
        assert_eq!(eq.gains, [0.0; 10]);
        let p: Passthrough = serde_json::from_str("{}").unwrap();
        assert_eq!(p, Passthrough::default());
        assert!(!p.enabled);
        assert!(p.ac3 && p.eac3 && p.dts);
        assert!(!p.dts_hd && !p.truehd);
    }

    #[test]
    fn presets_have_ten_bands_within_range() {
        for p in EqPreset::ALL {
            let Some(g) = p.gains() else {
                assert_eq!(p, EqPreset::Custom);
                continue;
            };
            assert!(g.iter().all(|v| v.abs() <= EQ_MAX_GAIN), "{p:?}: {g:?}");
        }
        assert_eq!(EqPreset::Rock.gains().unwrap()[7], 6.0);
        let custom = Equalizer {
            preset: EqPreset::Custom,
            gains: [1.5; 10],
            ..Equalizer::default()
        };
        assert_eq!(custom.effective_gains(), [1.5; 10]);
        assert_eq!(
            Equalizer {
                preset: EqPreset::Bass,
                ..custom
            }
            .effective_gains()[0],
            6.0
        );
        assert_eq!(serde_json::to_value(EqPreset::Electronic).unwrap(), "electronic");
    }

    #[test]
    fn volume_max_snaps_to_a_choice() {
        for (v, want) in [
            (0, 100),
            (100, 100),
            (114, 100),
            (115, 100),
            (116, 130),
            (140, 130),
            (141, 150),
            (175, 150),
            (176, 200),
            (9999, 200),
        ] {
            assert_eq!(snap_volume_max(v), want, "{v}");
        }
    }
}
