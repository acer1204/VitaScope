//! 音效設定（輸出裝置、獨佔模式、音量上限、轉成立體聲、等化器、音量平衡、音訊直通）：
//! 設定的型別和純函式（裝置清單、對應到 mpv 的選項）。套用在 `Player::apply_sound` 與 app 的 `sound` 模組；
//! 等化器、音量平衡的濾鏡鏈在之後的批次加上。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

/// 直通的格式：mpv 的 audio-spdif 名稱（依送出的順序）
pub const SPDIF_CODECS: [&str; 5] = ["ac3", "eac3", "dts", "dts-hd", "truehd"];

impl Passthrough {
    /// 某個格式（`SPDIF_CODECS` 的名稱）有沒有勾
    pub fn codec(&self, name: &str) -> bool {
        match name {
            "ac3" => self.ac3,
            "eac3" => self.eac3,
            "dts" => self.dts,
            "dts-hd" => self.dts_hd,
            "truehd" => self.truehd,
            _ => false,
        }
    }

    /// 勾選或取消某個格式
    pub fn set_codec(&mut self, name: &str, on: bool) {
        match name {
            "ac3" => self.ac3 = on,
            "eac3" => self.eac3 = on,
            "dts" => self.dts = on,
            "dts-hd" => self.dts_hd = on,
            "truehd" => self.truehd = on,
            _ => {}
        }
    }
}

/// 音訊直通 → mpv 的 audio-spdif：勾選的格式以「,」連接；沒開（或一個都沒勾）是空字串
pub fn spdif_value(p: &Passthrough) -> String {
    if !p.enabled {
        return String::new();
    }
    SPDIF_CODECS
        .into_iter()
        .filter(|c| p.codec(c))
        .collect::<Vec<_>>()
        .join(",")
}

/// 這條音軌（track-list 的 codec）會不會直通。mpv 的名稱：ac3、eac3、dts、truehd；
/// DTS-HD 的音軌 codec 也是 dts（profile 才分得出來），勾 DTS 或 DTS-HD 都會直通（DTS-HD 送完整的 HD 串流，
/// 只勾 DTS 送核心）。之後的批次在直通開始之前先清掉 af（濾鏡碰到直通的資料會失敗、整個停用）
pub fn predict_spdif(track_codec: &str, p: &Passthrough) -> bool {
    p.enabled
        && match track_codec {
            "ac3" => p.ac3,
            "eac3" => p.eac3,
            "dts" => p.dts || p.dts_hd,
            "truehd" => p.truehd,
            _ => false,
        }
}

/// 直通的格式（`audio-out-params` 的 spdif-xxx，或 `SPDIF_CODECS`）給人看的名稱
pub fn spdif_label(format: &str) -> &str {
    match format {
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        "dts" => "DTS",
        // audio-out-params 寫 spdif-dtshd，audio-spdif 選項寫 dts-hd
        "dtshd" | "dts-hd" => "DTS-HD",
        "truehd" => "TrueHD",
        other => other,
    }
}

// ───────────── 輸出裝置 ─────────────

/// 預設裝置（跟隨系統）的 mpv 名稱
pub const AUTO_DEVICE: &str = "auto";

/// `audio-device-list` 的一個裝置
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AudioDevice {
    /// mpv 的名稱：「輸出方式/裝置」，例如 `wasapi/{…}`、`coreaudio/BuiltInSpeakerDevice`、`pulse/alsa_output…`
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl AudioDevice {
    /// 輸出方式（名稱「/」前面那段：wasapi、coreaudio、pipewire…）
    pub fn family(&self) -> &str {
        self.name.split('/').next().unwrap_or_default()
    }

    /// 給人看的名稱（沒有說明時用 mpv 的名稱）
    pub fn label(&self) -> &str {
        if self.description.is_empty() {
            &self.name
        } else {
            &self.description
        }
    }
}

/// `audio-device-list`（JSON）→ 裝置清單；讀不懂的回傳空的，讀不懂、沒有名稱的項目各自略過（不影響其他裝置）
pub fn parse_devices(json: &str) -> Vec<AudioDevice> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    list.into_iter()
        .filter_map(|v| serde_json::from_value::<AudioDevice>(v).ok())
        .filter(|d| !d.name.is_empty())
        .collect()
}

/// 選單、設定頁列出的裝置：目前輸出方式的裝置（不含「auto」）。
/// mpv 的清單列出每一種輸出方式的裝置（例如 macOS 的 coreaudio 與 coreaudio_exclusive 是同一批裝置），只列一種：
/// macOS 固定 coreaudio（獨佔由 audio-exclusive 處理）；其他系統是目前用的輸出方式（`current_ao`，開檔後才知道），
/// 不知道或清單裡沒有它的裝置時，用清單上第一個裝置的輸出方式（mpv 依優先順序列出，就是它預設會用的）
pub fn family_devices<'a>(list: &'a [AudioDevice], current_ao: Option<&str>, macos: bool) -> Vec<&'a AudioDevice> {
    let Some(family) = device_family(list, current_ao, macos) else {
        return Vec::new();
    };
    list.iter()
        .filter(|d| d.name != AUTO_DEVICE && d.family() == family)
        .collect()
}

/// 目前的輸出方式（見 `family_devices`）；什麼都不知道時 None
pub fn device_family(list: &[AudioDevice], current_ao: Option<&str>, macos: bool) -> Option<String> {
    if macos {
        return Some("coreaudio".to_owned());
    }
    let listed = |f: &str| list.iter().any(|d| d.name != AUTO_DEVICE && d.family() == f);
    current_ao
        .filter(|ao| listed(ao))
        .or_else(|| list.iter().find(|d| d.name != AUTO_DEVICE).map(AudioDevice::family))
        .or(current_ao)
        .map(str::to_owned)
}

/// 獨佔模式要不要顯示：Windows（WASAPI）、macOS（Core Audio 的 hog mode）都支援；
/// Linux 只有 PipeWire 支援（ao_pipewire 看 audio-exclusive，pulse、alsa 不看）
pub fn exclusive_shown(family: Option<&str>) -> bool {
    cfg!(windows) || cfg!(target_os = "macos") || family == Some("pipewire")
}

/// 存下的裝置要怎麼套用
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceChoice {
    /// 交給 mpv 的 audio-device
    pub apply: String,
    /// 存下的裝置不在清單上（拔掉了）：它的顯示名稱（提示用）；設定照樣留著，裝置回來時再切回去
    pub missing: Option<String>,
}

/// 存下的裝置（`saved`、顯示名稱 `label`）對照目前的裝置清單：在清單上就用它，不在就暫時用預設裝置
pub fn resolve_device(saved: Option<&str>, label: Option<&str>, list: &[AudioDevice]) -> DeviceChoice {
    match saved.filter(|s| *s != AUTO_DEVICE) {
        None => DeviceChoice {
            apply: AUTO_DEVICE.to_owned(),
            missing: None,
        },
        Some(name) if list.iter().any(|d| d.name == name) => DeviceChoice {
            apply: name.to_owned(),
            missing: None,
        },
        Some(name) => DeviceChoice {
            apply: AUTO_DEVICE.to_owned(),
            missing: Some(label.filter(|l| !l.is_empty()).unwrap_or(name).to_owned()),
        },
    }
}

// ───────────── 對應到 mpv 的選項 ─────────────

/// `mpv_options` 管理的 mpv 選項（依送出的順序）
pub const MANAGED: [&str; 5] = [
    "audio-device",
    "audio-exclusive",
    "audio-channels",
    "audio-normalize-downmix",
    "audio-spdif",
];

/// mpv 的 audio-channels 預設值（不轉成立體聲時用它，跟原本一樣）
pub const CHANNELS_DEFAULT: &str = "auto-safe";

/// 音效設定 → mpv 選項。每次都回傳 `MANAGED` 的全部選項（套用時只送有變的，見 `Player::apply_sound`），
/// 使用者用 VITASCOPE_MPV_OPTS 指定的（`overrides`）不列。`device`：要用的裝置（存下的裝置不在時是 auto，見 `resolve_device`）。
/// 「混音時避免破音」只在轉成立體聲時開：沒轉的時候維持 mpv 原本的預設（no），跟以前一樣
pub fn mpv_options(a: &AudioSettings, device: &str, overrides: &HashSet<String>) -> Vec<(&'static str, String)> {
    let yes_no = |on: bool| if on { "yes" } else { "no" }.to_owned();
    let opts: [(&'static str, String); 5] = [
        ("audio-device", device.to_owned()),
        ("audio-exclusive", yes_no(a.exclusive)),
        (
            "audio-channels",
            if a.downmix { "stereo" } else { CHANNELS_DEFAULT }.to_owned(),
        ),
        ("audio-normalize-downmix", yes_no(a.downmix && a.normalize_downmix)),
        ("audio-spdif", spdif_value(&a.passthrough)),
    ];
    debug_assert!(opts.iter().map(|(k, _)| *k).eq(MANAGED));
    opts.into_iter().filter(|(k, _)| !overrides.contains(*k)).collect()
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

    /// mpv 的 audio-device-list：第一項一定是 auto，接著是每一種輸出方式的裝置（名稱 = 輸出方式/裝置）
    const WASAPI: &str = r#"[{"name":"auto","description":"Autoselect device"},
        {"name":"wasapi/{5b780f08-2cb8-4fc2-bc80-06d2ac5721d7}","description":"Beyond TV (NVIDIA High Definition Audio)"},
        {"name":"wasapi/{770260a9-dbb6-4a06-9c17-4564575315f2}","description":"喇叭 (High Definition Audio Device)"}]"#;
    const COREAUDIO: &str = r#"[{"name":"auto","description":"Autoselect device"},
        {"name":"coreaudio/BuiltInSpeakerDevice","description":"MacBook Pro Speakers"},
        {"name":"coreaudio/AppleUSBAudioEngine:Generic:USB Audio:1100000:1","description":"USB Audio"},
        {"name":"coreaudio_exclusive/BuiltInSpeakerDevice","description":"MacBook Pro Speakers"},
        {"name":"coreaudio_exclusive/AppleUSBAudioEngine:Generic:USB Audio:1100000:1","description":"USB Audio"},
        {"name":"avfoundation/BuiltInSpeakerDevice","description":"MacBook Pro Speakers"}]"#;
    const PIPEWIRE: &str = r#"[{"name":"auto","description":"Autoselect device"},
        {"name":"pipewire","description":"Default (pipewire)"},
        {"name":"pipewire/alsa_output.pci-0000_00_1f.3.analog-stereo","description":"Built-in Audio Analog Stereo"},
        {"name":"pipewire/alsa_output.pci-0000_01_00.1.hdmi-stereo","description":"HDA NVidia Digital Stereo (HDMI)"},
        {"name":"pulse/alsa_output.pci-0000_00_1f.3.analog-stereo","description":"Built-in Audio Analog Stereo"},
        {"name":"alsa","description":"Default (alsa)"},
        {"name":"alsa/hdmi:CARD=NVidia,DEV=0","description":"HDA NVidia, HDMI 0\nHDMI Audio Output"}]"#;

    fn names<'a>(devices: &[&'a AudioDevice]) -> Vec<&'a str> {
        devices.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn parse_devices_from_each_platform() {
        let w = parse_devices(WASAPI);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].name, AUTO_DEVICE);
        assert_eq!(w[2].label(), "喇叭 (High Definition Audio Device)");
        assert_eq!(w[1].family(), "wasapi");
        let mac = parse_devices(COREAUDIO);
        assert_eq!(mac.len(), 6);
        assert_eq!(mac[2].family(), "coreaudio");
        assert_eq!(mac[3].family(), "coreaudio_exclusive");
        let linux = parse_devices(PIPEWIRE);
        assert_eq!(linux.len(), 7);
        assert_eq!(linux[1].family(), "pipewire");
        assert_eq!(linux[6].family(), "alsa");
        // 沒有說明時用名稱；沒有名稱、讀不懂的不收
        let odd = parse_devices(r#"[{"name":"wasapi/x"},{"description":"no name"},{"name":"","description":"empty"}]"#);
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].label(), "wasapi/x");
        assert!(parse_devices("not json").is_empty());
        assert!(parse_devices("{}").is_empty());
    }

    #[test]
    fn menu_lists_one_output_family() {
        let w = parse_devices(WASAPI);
        // 還沒開檔（不知道 current-ao）：用清單上第一種
        assert_eq!(
            names(&family_devices(&w, None, false)),
            [
                "wasapi/{5b780f08-2cb8-4fc2-bc80-06d2ac5721d7}",
                "wasapi/{770260a9-dbb6-4a06-9c17-4564575315f2}"
            ]
        );
        // macOS 固定 coreaudio（coreaudio_exclusive、avfoundation 是同一批裝置）
        let mac = parse_devices(COREAUDIO);
        assert_eq!(
            names(&family_devices(&mac, Some("avfoundation"), true)),
            [
                "coreaudio/BuiltInSpeakerDevice",
                "coreaudio/AppleUSBAudioEngine:Generic:USB Audio:1100000:1"
            ]
        );
        // Linux：開檔後用目前的輸出方式；ao=null 之類清單上沒有的照第一種
        let linux = parse_devices(PIPEWIRE);
        assert_eq!(family_devices(&linux, None, false).len(), 3);
        assert_eq!(
            names(&family_devices(&linux, Some("pulse"), false)),
            ["pulse/alsa_output.pci-0000_00_1f.3.analog-stereo"]
        );
        assert_eq!(family_devices(&linux, Some("null"), false).len(), 3);
        assert_eq!(device_family(&linux, Some("null"), false).as_deref(), Some("pipewire"));
        // 只有 auto（CI 的 Linux 沒有音訊裝置）
        let none = parse_devices(r#"[{"name":"auto","description":"Autoselect device"}]"#);
        assert!(family_devices(&none, None, false).is_empty());
        assert_eq!(device_family(&none, None, false), None);
        assert_eq!(device_family(&none, Some("pulse"), false).as_deref(), Some("pulse"));
    }

    #[test]
    fn exclusive_is_shown_where_the_output_supports_it() {
        let desktop = cfg!(windows) || cfg!(target_os = "macos");
        assert_eq!(exclusive_shown(None), desktop);
        assert_eq!(exclusive_shown(Some("pulse")), desktop);
        assert_eq!(exclusive_shown(Some("alsa")), desktop);
        assert!(exclusive_shown(Some("pipewire")));
    }

    #[test]
    fn resolve_saved_device() {
        let list = parse_devices(WASAPI);
        let tv = "wasapi/{5b780f08-2cb8-4fc2-bc80-06d2ac5721d7}";
        // 預設裝置
        for saved in [None, Some("auto")] {
            assert_eq!(
                resolve_device(saved, Some("whatever"), &list),
                DeviceChoice {
                    apply: "auto".into(),
                    missing: None
                }
            );
        }
        // 還在
        assert_eq!(
            resolve_device(Some(tv), Some("Beyond TV"), &list),
            DeviceChoice {
                apply: tv.into(),
                missing: None
            }
        );
        // 拔掉了：暫時用預設裝置，提示用存下的顯示名稱（沒有的話用 mpv 的名稱）
        let gone = "wasapi/{00000000-0000-0000-0000-000000000000}";
        assert_eq!(
            resolve_device(Some(gone), Some("USB DAC"), &list),
            DeviceChoice {
                apply: "auto".into(),
                missing: Some("USB DAC".into())
            }
        );
        assert_eq!(resolve_device(Some(gone), None, &list).missing.as_deref(), Some(gone));
        assert_eq!(
            resolve_device(Some(gone), Some(""), &list).missing.as_deref(),
            Some(gone)
        );
        // 清單只有 auto（還沒插上任何裝置）
        assert_eq!(resolve_device(Some(tv), None, &list[..1]).apply, "auto");
        // 又插回來：清單上有了就切回去
        let mut back = list[..1].to_vec();
        back.push(AudioDevice {
            name: gone.into(),
            description: "USB DAC".into(),
        });
        assert_eq!(
            resolve_device(Some(gone), Some("USB DAC"), &back),
            DeviceChoice {
                apply: gone.into(),
                missing: None
            }
        );
    }

    #[test]
    fn spdif_value_joins_the_ticked_codecs() {
        let mut p = Passthrough::default();
        assert_eq!(spdif_value(&p), "", "沒開");
        p.enabled = true;
        assert_eq!(spdif_value(&p), "ac3,eac3,dts");
        p.dts_hd = true;
        p.truehd = true;
        assert_eq!(spdif_value(&p), "ac3,eac3,dts,dts-hd,truehd");
        for c in SPDIF_CODECS {
            p.set_codec(c, false);
        }
        assert_eq!(spdif_value(&p), "", "一個都沒勾");
        p.set_codec("truehd", true);
        assert_eq!(spdif_value(&p), "truehd");
        assert!(p.codec("truehd") && !p.codec("ac3") && !p.codec("flac"));
        assert_eq!(spdif_label("eac3"), "E-AC-3");
        assert_eq!(spdif_label("dts-hd"), "DTS-HD");
        assert_eq!(spdif_label("dtshd"), "DTS-HD", "audio-out-params 的 spdif-dtshd");
        assert_eq!(spdif_label("mystery"), "mystery");
    }

    #[test]
    fn predict_spdif_from_the_track_codec() {
        let mut p = Passthrough::default();
        assert!(!predict_spdif("ac3", &p), "沒開");
        p.enabled = true;
        assert!(predict_spdif("ac3", &p));
        assert!(predict_spdif("eac3", &p));
        assert!(predict_spdif("dts", &p));
        assert!(!predict_spdif("truehd", &p), "TrueHD 預設不勾");
        assert!(!predict_spdif("aac", &p) && !predict_spdif("flac", &p) && !predict_spdif("", &p));
        // DTS-HD 的音軌 codec 也是 dts：只勾 DTS-HD 一樣會直通（mpv 的 select_spdif_codec）
        p.dts = false;
        assert!(!predict_spdif("dts", &p));
        p.dts_hd = true;
        assert!(predict_spdif("dts", &p));
        p.truehd = true;
        assert!(predict_spdif("truehd", &p));
        p.ac3 = false;
        assert!(!predict_spdif("ac3", &p));
    }

    #[test]
    fn default_mpv_options_match_mpv_defaults() {
        let a = AudioSettings::default();
        let none = HashSet::new();
        assert_eq!(
            mpv_options(&a, "auto", &none),
            [
                ("audio-device", "auto".to_owned()),
                ("audio-exclusive", "no".to_owned()),
                ("audio-channels", "auto-safe".to_owned()),
                ("audio-normalize-downmix", "no".to_owned()),
                ("audio-spdif", String::new()),
            ]
        );
        let mut b = a.clone();
        b.exclusive = true;
        b.downmix = true;
        b.passthrough.enabled = true;
        let opts = mpv_options(&b, "wasapi/{x}", &none);
        assert_eq!(
            opts,
            [
                ("audio-device", "wasapi/{x}".to_owned()),
                ("audio-exclusive", "yes".to_owned()),
                ("audio-channels", "stereo".to_owned()),
                ("audio-normalize-downmix", "yes".to_owned()),
                ("audio-spdif", "ac3,eac3,dts".to_owned()),
            ]
        );
        // 混音時不避免破音
        b.normalize_downmix = false;
        assert_eq!(mpv_options(&b, "auto", &none)[3].1, "no");
        // VITASCOPE_MPV_OPTS 指定的不列
        let over: HashSet<String> = ["audio-channels".to_owned(), "audio-spdif".to_owned()].into();
        let names: Vec<&str> = mpv_options(&b, "auto", &over).into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, ["audio-device", "audio-exclusive", "audio-normalize-downmix"]);
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
