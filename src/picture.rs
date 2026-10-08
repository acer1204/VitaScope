//! 畫質設定（亮度等影像調整、去交錯、去色帶、銳化、縮放演算法、像素著色器、HDR 色調映射）：
//! 設定的型別和純函式。影像調整已經套用到 mpv（`Adjust::mpv_options`），其他項目在之後的批次加上。

use serde::{Deserialize, Serialize};

/// 畫質設定（存在 settings.json 的 `video`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    /// 亮度、對比、飽和度、色相、Gamma。只在這次執行跨檔案沿用；勾了 `keep_adjust` 才存檔
    pub adjust: Adjust,
    /// 下次開啟時沿用影像調整
    pub keep_adjust: bool,
    pub deinterlace: Deinterlace,
    pub deband: Strength,
    pub sharpen: Strength,
    /// 縮放演算法的整體設定；下面三項是個別指定（None = 跟隨畫質）
    pub quality: Quality,
    pub scale: Option<Upscaler>,
    pub dscale: Option<Downscaler>,
    pub cscale: Option<ChromaScaler>,
    pub shaders: ShaderSettings,
    pub tone: ToneSettings,
}

// 不用 derive：去交錯預設「自動」、畫質預設「標準」、動態峰值偵測預設開（derive 會全部變成第一項 / false）
impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            adjust: Adjust::default(),
            keep_adjust: false,
            deinterlace: Deinterlace::Auto,
            deband: Strength::Off,
            sharpen: Strength::Off,
            quality: Quality::Standard,
            scale: None,
            dscale: None,
            cscale: None,
            shaders: ShaderSettings::default(),
            tone: ToneSettings::default(),
        }
    }
}

/// 影像調整，每一項 −100…100，0 = 不調整（mpv 的 brightness、contrast、saturation、hue、gamma）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjust {
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub hue: i32,
    pub gamma: i32,
}

impl Adjust {
    pub const MIN: i32 = -100;
    pub const MAX: i32 = 100;

    /// 每一項都限制在 −100…100
    pub fn clamped(self) -> Self {
        let c = |v: i32| v.clamp(Self::MIN, Self::MAX);
        Self {
            brightness: c(self.brightness),
            contrast: c(self.contrast),
            saturation: c(self.saturation),
            hue: c(self.hue),
            gamma: c(self.gamma),
        }
    }

    /// 全部都是 0（沒有調整）
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    pub fn get(&self, kind: AdjustKind) -> i32 {
        match kind {
            AdjustKind::Brightness => self.brightness,
            AdjustKind::Contrast => self.contrast,
            AdjustKind::Saturation => self.saturation,
            AdjustKind::Hue => self.hue,
            AdjustKind::Gamma => self.gamma,
        }
    }

    /// 設定一項（限制在 −100…100）
    pub fn set(&mut self, kind: AdjustKind, value: i32) {
        let v = value.clamp(Self::MIN, Self::MAX);
        match kind {
            AdjustKind::Brightness => self.brightness = v,
            AdjustKind::Contrast => self.contrast = v,
            AdjustKind::Saturation => self.saturation = v,
            AdjustKind::Hue => self.hue = v,
            AdjustKind::Gamma => self.gamma = v,
        }
    }

    /// 對應的 mpv 選項，五項都列（啟動時同步套用）。mpv 的 brightness 等是畫面輸出的選項，
    /// 不是 FFmpeg 的 eq 濾鏡（GPL）；軟體繪圖的簡化流程（gpu-dumb-mode）也有效
    pub fn mpv_options(&self) -> Vec<(&'static str, String)> {
        AdjustKind::ALL
            .iter()
            .map(|k| (k.mpv(), self.get(*k).to_string()))
            .collect()
    }

    /// 不是 0 的項目：「亮度 +10、對比 -5」（`skip` 的項目不列）
    pub fn summary(&self, skip: impl Fn(AdjustKind) -> bool) -> String {
        AdjustKind::ALL
            .into_iter()
            .filter(|k| self.get(*k) != 0 && !skip(*k))
            .map(|k| k.describe(self.get(k)))
            .collect::<Vec<_>>()
            .join(crate::tr!("、", ", "))
    }
}

/// 影像調整的一項
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjustKind {
    Brightness,
    Contrast,
    Saturation,
    Hue,
    Gamma,
}

impl AdjustKind {
    pub const ALL: [AdjustKind; 5] = [
        AdjustKind::Brightness,
        AdjustKind::Contrast,
        AdjustKind::Saturation,
        AdjustKind::Hue,
        AdjustKind::Gamma,
    ];

    /// mpv 的選項名稱
    pub fn mpv(self) -> &'static str {
        match self {
            AdjustKind::Brightness => "brightness",
            AdjustKind::Contrast => "contrast",
            AdjustKind::Saturation => "saturation",
            AdjustKind::Hue => "hue",
            AdjustKind::Gamma => "gamma",
        }
    }

    /// 介面上的名稱
    pub fn label(self) -> &'static str {
        match self {
            AdjustKind::Brightness => crate::tr!("亮度", "Brightness"),
            AdjustKind::Contrast => crate::tr!("對比", "Contrast"),
            AdjustKind::Saturation => crate::tr!("飽和度", "Saturation"),
            AdjustKind::Hue => crate::tr!("色相", "Hue"),
            AdjustKind::Gamma => "Gamma",
        }
    }

    /// 「亮度 +3」「對比 -2」「飽和度 0」
    pub fn describe(self, value: i32) -> String {
        format!("{} {}", self.label(), fmt_signed(value))
    }
}

/// 調整值：0 →「0」、3 →「+3」、-2 →「-2」（跟延遲之類的提示一樣用一般的減號）
pub fn fmt_signed(value: i32) -> String {
    if value == 0 {
        "0".to_owned()
    } else {
        format!("{value:+}")
    }
}

/// 去交錯
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Deinterlace {
    /// 交錯的影片才去交錯（引擎不支援 auto 時當成關閉）
    #[default]
    Auto,
    On,
    Off,
}

impl Deinterlace {
    pub fn mpv(self) -> &'static str {
        match self {
            Deinterlace::Auto => "auto",
            Deinterlace::On => "yes",
            Deinterlace::Off => "no",
        }
    }
}

/// 去色帶、銳化的強度
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strength {
    #[default]
    Off,
    Light,
    Medium,
    Strong,
}

/// 去色帶的參數（mpv 的 deband-iterations / threshold / range / grain）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deband {
    pub iterations: u32,
    pub threshold: u32,
    pub range: u32,
    pub grain: u32,
}

impl Strength {
    /// 銳化（mpv 的 sharpen，反銳利化遮罩的強度）
    pub fn sharpen(self) -> &'static str {
        match self {
            Strength::Off => "0",
            Strength::Light => "0.25",
            Strength::Medium => "0.5",
            Strength::Strong => "1",
        }
    }

    /// 去色帶的參數；None = 關閉（deband=no）。中等就是 mpv 的預設值
    pub fn deband(self) -> Option<Deband> {
        let d = |iterations, threshold, range, grain| {
            Some(Deband {
                iterations,
                threshold,
                range,
                grain,
            })
        };
        match self {
            Strength::Off => None,
            Strength::Light => d(1, 32, 16, 16),
            Strength::Medium => d(1, 48, 16, 32),
            Strength::Strong => d(2, 64, 16, 48),
        }
    }
}

/// 縮放演算法的整體設定
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    /// 全部用 bilinear
    Fast,
    /// mpv 的預設值
    #[default]
    Standard,
    /// 放大用 ewa_lanczossharp（mpv 的 high-quality 設定檔）
    High,
}

/// 放大用的演算法。存檔的名稱就是 mpv 的名稱
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Upscaler {
    #[serde(rename = "bilinear")]
    Bilinear,
    #[serde(rename = "bicubic")]
    Bicubic,
    #[serde(rename = "catmull_rom")]
    CatmullRom,
    #[serde(rename = "mitchell")]
    Mitchell,
    #[serde(rename = "spline36")]
    Spline36,
    #[serde(rename = "lanczos")]
    Lanczos,
    #[serde(rename = "ewa_lanczos")]
    EwaLanczos,
    #[serde(rename = "ewa_lanczossharp")]
    EwaLanczosSharp,
    #[serde(rename = "ewa_lanczos4sharpest")]
    EwaLanczos4Sharpest,
}

impl Upscaler {
    pub const ALL: [Upscaler; 9] = [
        Upscaler::Bilinear,
        Upscaler::Bicubic,
        Upscaler::CatmullRom,
        Upscaler::Mitchell,
        Upscaler::Spline36,
        Upscaler::Lanczos,
        Upscaler::EwaLanczos,
        Upscaler::EwaLanczosSharp,
        Upscaler::EwaLanczos4Sharpest,
    ];

    pub fn mpv(self) -> &'static str {
        match self {
            Upscaler::Bilinear => "bilinear",
            Upscaler::Bicubic => "bicubic",
            Upscaler::CatmullRom => "catmull_rom",
            Upscaler::Mitchell => "mitchell",
            Upscaler::Spline36 => "spline36",
            Upscaler::Lanczos => "lanczos",
            Upscaler::EwaLanczos => "ewa_lanczos",
            Upscaler::EwaLanczosSharp => "ewa_lanczossharp",
            Upscaler::EwaLanczos4Sharpest => "ewa_lanczos4sharpest",
        }
    }
}

/// 縮小用的演算法
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Downscaler {
    #[serde(rename = "bilinear")]
    Bilinear,
    #[serde(rename = "hermite")]
    Hermite,
    #[serde(rename = "mitchell")]
    Mitchell,
    #[serde(rename = "catmull_rom")]
    CatmullRom,
    #[serde(rename = "spline36")]
    Spline36,
    #[serde(rename = "lanczos")]
    Lanczos,
}

impl Downscaler {
    pub const ALL: [Downscaler; 6] = [
        Downscaler::Bilinear,
        Downscaler::Hermite,
        Downscaler::Mitchell,
        Downscaler::CatmullRom,
        Downscaler::Spline36,
        Downscaler::Lanczos,
    ];

    pub fn mpv(self) -> &'static str {
        match self {
            Downscaler::Bilinear => "bilinear",
            Downscaler::Hermite => "hermite",
            Downscaler::Mitchell => "mitchell",
            Downscaler::CatmullRom => "catmull_rom",
            Downscaler::Spline36 => "spline36",
            Downscaler::Lanczos => "lanczos",
        }
    }
}

/// 色度（顏色資訊）放大用的演算法
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChromaScaler {
    #[serde(rename = "bilinear")]
    Bilinear,
    #[serde(rename = "spline36")]
    Spline36,
    #[serde(rename = "lanczos")]
    Lanczos,
    #[serde(rename = "ewa_lanczos")]
    EwaLanczos,
}

impl ChromaScaler {
    pub const ALL: [ChromaScaler; 4] = [
        ChromaScaler::Bilinear,
        ChromaScaler::Spline36,
        ChromaScaler::Lanczos,
        ChromaScaler::EwaLanczos,
    ];

    pub fn mpv(self) -> &'static str {
        match self {
            ChromaScaler::Bilinear => "bilinear",
            ChromaScaler::Spline36 => "spline36",
            ChromaScaler::Lanczos => "lanczos",
            ChromaScaler::EwaLanczos => "ewa_lanczos",
        }
    }
}

/// 這個引擎的縮放預設值（啟動時從 option-info/…/default-value 讀；「標準」畫質就是這些值）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureDefaults {
    pub scale: String,
    pub dscale: String,
    /// 空字串 = 跟放大用同一個
    pub cscale: String,
    pub scale_antiring: String,
}

impl Default for PictureDefaults {
    /// 讀不到時用 mpv 文件寫的預設值
    fn default() -> Self {
        Self {
            scale: "lanczos".into(),
            dscale: "hermite".into(),
            cscale: String::new(),
            scale_antiring: "0.000000".into(),
        }
    }
}

/// 像素著色器的組合
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShaderSettings {
    pub presets: Vec<ShaderPreset>,
    /// 使用中的組合；None = 不使用
    pub active: Option<u32>,
}

/// 一組像素著色器（依序套用的 .glsl 檔案）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShaderPreset {
    /// 隨機產生（見 `new_preset_id`）：兩個視窗同時新增組合也不會撞號
    pub id: u32,
    pub name: String,
    /// 檔案路徑。用字串存：非 UTF-8 的路徑 serde 寫不出來，整個設定檔都會存不了（mpv 也只收 UTF-8）
    pub files: Vec<String>,
}

impl ShaderSettings {
    /// 使用中的組合
    pub fn active_preset(&self) -> Option<&ShaderPreset> {
        let id = self.active?;
        self.presets.iter().find(|p| p.id == id)
    }
}

/// 新組合的編號：時間 ^ 計數器 ^ 行程編號打散後取 32 位元，不是 0 也不跟現有的重複。
/// 不用遞增的編號：兩個視窗各自新增時會拿到同一個號碼，存檔合併後只剩一組
pub fn new_preset_id(taken: &[ShaderPreset]) -> u32 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = mix_preset_id(nanos, count, std::process::id());
        if id != 0 && !taken.iter().any(|p| p.id == id) {
            return id;
        }
    }
}

/// 時間、這個行程的計數器、行程編號 → 編號。兩個視窗是兩個行程，計數器都從 0 開始，
/// 靠行程編號與時間區分
fn mix_preset_id(nanos: u64, count: u64, pid: u32) -> u32 {
    (splitmix64(nanos ^ count.rotate_left(32) ^ u64::from(pid)) >> 32) as u32
}

/// 把相近的數字打散成看起來隨機的數字（SplitMix64 的最後一步）
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// HDR 轉 SDR
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneSettings {
    pub curve: ToneCurve,
    /// 目標亮度（nits，100…1000）；None = 自動
    pub target_peak: Option<u32>,
    pub gamut: Gamut,
    /// 依畫面動態調整亮度（hdr-compute-peak；macOS 不顯示）
    pub compute_peak: bool,
}

// 不用 derive：動態峰值偵測預設開
impl Default for ToneSettings {
    fn default() -> Self {
        Self {
            curve: ToneCurve::Auto,
            target_peak: None,
            gamut: Gamut::Auto,
            compute_peak: true,
        }
    }
}

impl ToneSettings {
    pub const MIN_PEAK: u32 = 100;
    pub const MAX_PEAK: u32 = 1000;
}

/// 色調映射曲線（只列 vo_gpu 支援的）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToneCurve {
    #[default]
    Auto,
    Bt2390,
    Hable,
    Mobius,
    Reinhard,
    Clip,
    Gamma,
    Linear,
}

impl ToneCurve {
    pub const ALL: [ToneCurve; 8] = [
        ToneCurve::Auto,
        ToneCurve::Bt2390,
        ToneCurve::Hable,
        ToneCurve::Mobius,
        ToneCurve::Reinhard,
        ToneCurve::Clip,
        ToneCurve::Gamma,
        ToneCurve::Linear,
    ];

    pub fn mpv(self) -> &'static str {
        match self {
            ToneCurve::Auto => "auto",
            ToneCurve::Bt2390 => "bt.2390",
            ToneCurve::Hable => "hable",
            ToneCurve::Mobius => "mobius",
            ToneCurve::Reinhard => "reinhard",
            ToneCurve::Clip => "clip",
            ToneCurve::Gamma => "gamma",
            ToneCurve::Linear => "linear",
        }
    }
}

/// 色域對應
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gamut {
    #[default]
    Auto,
    Clip,
    Desaturate,
}

impl Gamut {
    pub fn mpv(self) -> &'static str {
        match self {
            Gamut::Auto => "auto",
            Gamut::Clip => "clip",
            Gamut::Desaturate => "desaturate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        // 每個結構都從空的 JSON 讀：沒寫的欄位要是規格的預設值，不是型別的零值
        let v: VideoSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(v, VideoSettings::default());
        assert_eq!(v.deinterlace, Deinterlace::Auto);
        assert_eq!(v.quality, Quality::Standard);
        // 型別自己的預設值也要一致（之後的批次可能直接用 `Deinterlace::default()` 之類的）
        assert_eq!(Deinterlace::default(), Deinterlace::Auto);
        assert_eq!(Quality::default(), Quality::Standard);
        assert_eq!(Strength::default(), Strength::Off);
        assert_eq!(v.deband, Strength::Off);
        assert_eq!(v.sharpen, Strength::Off);
        assert!(!v.keep_adjust);
        assert!(v.adjust.is_neutral());
        assert_eq!((v.scale, v.dscale, v.cscale), (None, None, None));
        assert!(v.tone.compute_peak);
        let t: ToneSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(t, ToneSettings::default());
        assert!(t.compute_peak, "動態峰值偵測預設開");
        assert_eq!((t.curve, t.gamut, t.target_peak), (ToneCurve::Auto, Gamut::Auto, None));
        let s: ShaderSettings = serde_json::from_str("{}").unwrap();
        assert!(s.presets.is_empty() && s.active.is_none());
        let a: Adjust = serde_json::from_str(r#"{"hue":5}"#).unwrap();
        assert_eq!(
            a,
            Adjust {
                hue: 5,
                ..Adjust::default()
            }
        );
    }

    #[test]
    fn scaler_names_are_mpv_names() {
        // 存檔的名稱就是 mpv 的名稱
        for s in Upscaler::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.mpv(), "{s:?}");
        }
        for s in Downscaler::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.mpv(), "{s:?}");
        }
        for s in ChromaScaler::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.mpv(), "{s:?}");
        }
        assert_eq!(
            serde_json::from_str::<Upscaler>(r#""ewa_lanczossharp""#).unwrap(),
            Upscaler::EwaLanczosSharp
        );
    }

    #[test]
    fn enum_values_for_mpv() {
        assert_eq!(
            [Deinterlace::Auto, Deinterlace::On, Deinterlace::Off].map(Deinterlace::mpv),
            ["auto", "yes", "no"]
        );
        assert_eq!(ToneCurve::ALL.map(ToneCurve::mpv)[1], "bt.2390");
        assert_eq!(serde_json::to_value(ToneCurve::Bt2390).unwrap(), "bt2390");
        assert_eq!(
            [Gamut::Auto, Gamut::Clip, Gamut::Desaturate].map(Gamut::mpv),
            ["auto", "clip", "desaturate"]
        );
        assert_eq!(Strength::Off.deband(), None);
        // 中等 = mpv 的預設值
        assert_eq!(
            Strength::Medium.deband(),
            Some(Deband {
                iterations: 1,
                threshold: 48,
                range: 16,
                grain: 32
            })
        );
        assert_eq!(
            [Strength::Off, Strength::Light, Strength::Medium, Strength::Strong].map(Strength::sharpen),
            ["0", "0.25", "0.5", "1"]
        );
    }

    #[test]
    fn adjust_is_clamped() {
        let a = Adjust {
            brightness: 300,
            contrast: -101,
            saturation: 5,
            hue: -100,
            gamma: 100,
        }
        .clamped();
        assert_eq!(
            [a.brightness, a.contrast, a.saturation, a.hue, a.gamma],
            [100, -100, 5, -100, 100]
        );
    }

    #[test]
    fn adjust_get_set_and_options() {
        let mut a = Adjust::default();
        for (i, k) in AdjustKind::ALL.into_iter().enumerate() {
            a.set(k, i as i32 * 10 - 20);
        }
        assert_eq!(
            [a.brightness, a.contrast, a.saturation, a.hue, a.gamma],
            [-20, -10, 0, 10, 20]
        );
        for k in AdjustKind::ALL {
            assert_eq!(
                a.get(k),
                [a.brightness, a.contrast, a.saturation, a.hue, a.gamma][k as usize]
            );
        }
        // 超出範圍時拉回
        a.set(AdjustKind::Hue, 250);
        a.set(AdjustKind::Gamma, -101);
        assert_eq!((a.hue, a.gamma), (100, -100));
        assert_eq!(
            a.mpv_options(),
            [
                ("brightness", "-20".to_owned()),
                ("contrast", "-10".to_owned()),
                ("saturation", "0".to_owned()),
                ("hue", "100".to_owned()),
                ("gamma", "-100".to_owned()),
            ]
        );
    }

    #[test]
    fn adjust_text() {
        assert_eq!([3, -2, 0].map(fmt_signed), ["+3", "-2", "0"]);
        assert_eq!(AdjustKind::Contrast.describe(-2), "對比 -2");
        assert_eq!(AdjustKind::Gamma.describe(5), "Gamma +5");
        let a = Adjust {
            brightness: 10,
            contrast: -5,
            hue: 1,
            ..Adjust::default()
        };
        assert_eq!(a.summary(|_| false), "亮度 +10、對比 -5、色相 +1");
        assert_eq!(a.summary(|k| k == AdjustKind::Contrast), "亮度 +10、色相 +1");
        assert_eq!(Adjust::default().summary(|_| false), "");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(AdjustKind::Saturation.describe(0), "Saturation 0");
        assert_eq!(a.summary(|_| false), "Brightness +10, Contrast -5, Hue +1");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    #[test]
    fn preset_ids_are_unique_and_nonzero() {
        let mut presets: Vec<ShaderPreset> = Vec::new();
        for _ in 0..500 {
            let id = new_preset_id(&presets);
            assert_ne!(id, 0);
            assert!(!presets.iter().any(|p| p.id == id), "重複的編號 {id}");
            presets.push(ShaderPreset {
                id,
                ..Default::default()
            });
        }
        let shaders = ShaderSettings {
            active: Some(presets[3].id),
            presets,
        };
        assert_eq!(shaders.active_preset().map(|p| p.id), shaders.active);
        // 不知道彼此的編號時（兩個視窗各自新增）也不會撞號：不是遞增的流水號
        let ids: Vec<u32> = (0..200).map(|_| new_preset_id(&[])).collect();
        let distinct: std::collections::HashSet<u32> = ids.iter().copied().collect();
        assert_eq!(distinct.len(), ids.len());
        assert!(ids.windows(2).all(|w| w[1].abs_diff(w[0]) > 1), "{ids:?}");
        // 兩個行程同一時間各自的第一個編號
        let t = 1_790_000_000_000_000_000;
        assert_ne!(mix_preset_id(t, 0, 4242), mix_preset_id(t, 0, 4243));
        assert_ne!(mix_preset_id(t, 0, 4242), mix_preset_id(t + 1, 0, 4242));
    }
}
