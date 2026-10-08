//! 畫質設定（亮度等影像調整、去交錯、去色帶、銳化、縮放演算法、像素著色器、HDR 色調映射）：
//! 設定的型別和純函式。影像調整由 `Adjust::mpv_options` 對應到 mpv；去交錯、去色帶、銳化、
//! 縮放演算法、HDR 由 `mpv_options` 對應；像素著色器（glsl-shaders）見 `shader`。

pub mod shader;

use crate::player::EngineCaps;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
    pub const ALL: [Deinterlace; 3] = [Deinterlace::Auto, Deinterlace::On, Deinterlace::Off];

    pub fn mpv(self) -> &'static str {
        match self {
            Deinterlace::Auto => "auto",
            Deinterlace::On => "yes",
            Deinterlace::Off => "no",
        }
    }

    /// 引擎實際用的值：不支援 auto 的引擎（系統的 libmpv 0.37）把「自動」當成關閉
    pub fn effective(self, caps: &EngineCaps) -> Self {
        match self {
            Deinterlace::Auto if !caps.deint_auto => Deinterlace::Off,
            d => d,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Deinterlace::Auto => crate::tr!("自動", "Auto"),
            Deinterlace::On => crate::tr!("開啟", "On"),
            Deinterlace::Off => crate::tr!("關閉", "Off"),
        }
    }

    /// 選單上的名稱（「自動」是建議的設定）
    pub fn menu_label(self) -> &'static str {
        match self {
            Deinterlace::Auto => crate::tr!("自動（建議）", "Auto (recommended)"),
            d => d.label(),
        }
    }
}

/// 去交錯目前的狀態（「已去交錯」「逐行影片」）。`active` 是 mpv 的 deinterlace-active；
/// 設定是自動卻沒有去交錯 = 影片本身是逐行的
pub fn deinterlace_status(setting: Deinterlace, active: bool) -> &'static str {
    match (active, setting) {
        (true, _) => crate::tr!("已去交錯", "deinterlacing"),
        (false, Deinterlace::Auto) => crate::tr!("逐行影片", "progressive video"),
        (false, _) => crate::tr!("未去交錯", "not deinterlacing"),
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

impl Deband {
    /// mpv 的預設值（0.37 起都是這組）
    pub const MPV_DEFAULT: Deband = Deband {
        iterations: 1,
        threshold: 48,
        range: 16,
        grain: 32,
    };
}

impl Strength {
    pub const ALL: [Strength; 4] = [Strength::Off, Strength::Light, Strength::Medium, Strength::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Strength::Off => crate::tr!("關閉", "Off"),
            Strength::Light => crate::tr!("輕微", "Light"),
            Strength::Medium => crate::tr!("中等", "Medium"),
            Strength::Strong => crate::tr!("強", "Strong"),
        }
    }

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
            Strength::Medium => Some(Deband::MPV_DEFAULT),
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

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Fast, Quality::Standard, Quality::High];

    /// 短的名稱（提示、設定頁的按鈕）
    pub fn label(self) -> &'static str {
        match self {
            Quality::Fast => crate::tr!("快速", "Fast"),
            Quality::Standard => crate::tr!("標準", "Standard"),
            Quality::High => crate::tr!("高品質", "High quality"),
        }
    }

    /// 選單上的名稱
    pub fn menu_label(self) -> &'static str {
        match self {
            Quality::Standard => crate::tr!("標準（mpv 預設）", "Standard (mpv default)"),
            q => q.label(),
        }
    }
}

/// 縮放演算法的介面名稱（演算法本身的名字，中英文一樣）
fn scaler_label(mpv: &str) -> &'static str {
    match mpv {
        "bilinear" => "Bilinear",
        "bicubic" => "Bicubic",
        "catmull_rom" => "Catmull-Rom",
        "mitchell" => "Mitchell",
        "hermite" => "Hermite",
        "spline36" => "Spline36",
        "lanczos" => "Lanczos",
        "ewa_lanczos" => "EWA Lanczos",
        "ewa_lanczossharp" => "EWA Lanczos Sharp",
        "ewa_lanczos4sharpest" => "EWA Lanczos 4 Sharpest",
        _ => "?",
    }
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

    pub fn label(self) -> &'static str {
        scaler_label(self.mpv())
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

    pub fn label(self) -> &'static str {
        scaler_label(self.mpv())
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

    pub fn label(self) -> &'static str {
        scaler_label(self.mpv())
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

    pub fn preset(&self, id: u32) -> Option<&ShaderPreset> {
        self.presets.iter().find(|p| p.id == id)
    }

    pub fn preset_mut(&mut self, id: u32) -> Option<&mut ShaderPreset> {
        self.presets.iter_mut().find(|p| p.id == id)
    }
}

impl ShaderPreset {
    /// 介面上的名稱（沒有名稱時「（未命名）」）
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            crate::tr!("（未命名）", "(unnamed)")
        } else {
            &self.name
        }
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
    /// 選單上的目標亮度（nits）：一般 SDR 螢幕、BT.2408 的參考白、常見的 HDR 電視
    pub const PEAK_PRESETS: [u32; 4] = [100, 203, 400, 1000];
}

/// 目標亮度的名稱：「自動」「400 nits」
pub fn peak_label(peak: Option<u32>) -> String {
    match peak {
        None => crate::tr!("自動", "Auto").to_owned(),
        Some(nits) => format!("{nits} nits"),
    }
}

/// 色調映射曲線（只列 vo_gpu 支援的）。不列 gamma：vo_gpu 的 gamma 曲線著色器對純量用了 .x，
/// OpenGL 3.3／4.1（GLSL 4.20 以前）編譯不過，畫面變成一片藍（RTX 3090 實測）
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
    Linear,
}

impl ToneCurve {
    pub const ALL: [ToneCurve; 7] = [
        ToneCurve::Auto,
        ToneCurve::Bt2390,
        ToneCurve::Hable,
        ToneCurve::Mobius,
        ToneCurve::Reinhard,
        ToneCurve::Clip,
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
            ToneCurve::Linear => "linear",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ToneCurve::Auto => crate::tr!("自動", "Auto"),
            ToneCurve::Bt2390 => "BT.2390",
            ToneCurve::Hable => "Hable",
            ToneCurve::Mobius => "Mobius",
            ToneCurve::Reinhard => "Reinhard",
            ToneCurve::Clip => crate::tr!("裁切", "Clip"),
            ToneCurve::Linear => crate::tr!("線性", "Linear"),
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
    pub const ALL: [Gamut; 3] = [Gamut::Auto, Gamut::Clip, Gamut::Desaturate];

    pub fn mpv(self) -> &'static str {
        match self {
            Gamut::Auto => "auto",
            Gamut::Clip => "clip",
            Gamut::Desaturate => "desaturate",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Gamut::Auto => crate::tr!("自動", "Auto"),
            Gamut::Clip => crate::tr!("裁切", "Clip"),
            Gamut::Desaturate => crate::tr!("降低飽和度", "Desaturate"),
        }
    }
}

/// `mpv_options` 管理的 mpv 選項（依送出的順序）。影像調整另外由 `Adjust::mpv_options` 管
pub const MANAGED: [&str; 15] = [
    "deinterlace",
    "deband",
    "deband-iterations",
    "deband-threshold",
    "deband-range",
    "deband-grain",
    "sharpen",
    "scale",
    "dscale",
    "cscale",
    "scale-antiring",
    "tone-mapping",
    "target-peak",
    "gamut-mapping-mode",
    "hdr-compute-peak",
];

/// 「高品質」的放大演算法與抗振鈴（mpv 的 high-quality 設定檔）
const HIGH_SCALE: &str = "ewa_lanczossharp";
const HIGH_ANTIRING: &str = "0.6";

/// 畫質設定 → mpv 選項。每次都回傳 `MANAGED` 的全部選項（套用時只送有變的，見 `Player::apply_picture`），
/// 但使用者用 VITASCOPE_MPV_OPTS 指定的（`overrides`）不列。
/// 軟體繪圖的簡化流程（`caps.dumb`）照樣對應：畫面輸出會忽略去色帶、縮放、HDR 這些選項，沒有害處，
/// 而且設定跟 mpv 的值永遠一致（介面上這些項目停用，改不了）
pub fn mpv_options(
    v: &VideoSettings,
    caps: &EngineCaps,
    defaults: &PictureDefaults,
    overrides: &HashSet<String>,
) -> Vec<(&'static str, String)> {
    // 關閉去色帶時參數用 mpv 的預設值（= 中等）：預設設定跟 mpv 原本的值完全一樣
    let deband = v.deband.deband();
    let params = deband.unwrap_or(Deband::MPV_DEFAULT);
    let d = defaults;
    let (scale, dscale, cscale, antiring) = match v.quality {
        Quality::Fast => ("bilinear", "bilinear", "bilinear", d.scale_antiring.as_str()),
        Quality::Standard => (
            d.scale.as_str(),
            d.dscale.as_str(),
            d.cscale.as_str(),
            d.scale_antiring.as_str(),
        ),
        Quality::High => (HIGH_SCALE, d.dscale.as_str(), d.cscale.as_str(), HIGH_ANTIRING),
    };
    // 個別指定的演算法優先
    let scale = v.scale.map_or(scale, |s| s.mpv());
    let dscale = v.dscale.map_or(dscale, |s| s.mpv());
    let cscale = v.cscale.map_or(cscale, |s| s.mpv());
    let t = &v.tone;
    let opts: [(&'static str, String); 15] = [
        ("deinterlace", v.deinterlace.effective(caps).mpv().to_owned()),
        ("deband", if deband.is_some() { "yes" } else { "no" }.to_owned()),
        ("deband-iterations", params.iterations.to_string()),
        ("deband-threshold", params.threshold.to_string()),
        ("deband-range", params.range.to_string()),
        ("deband-grain", params.grain.to_string()),
        ("sharpen", v.sharpen.sharpen().to_owned()),
        ("scale", scale.to_owned()),
        ("dscale", dscale.to_owned()),
        ("cscale", cscale.to_owned()),
        ("scale-antiring", antiring.to_owned()),
        ("tone-mapping", t.curve.mpv().to_owned()),
        (
            "target-peak",
            t.target_peak.map_or_else(|| "auto".to_owned(), |p| p.to_string()),
        ),
        ("gamut-mapping-mode", t.gamut.mpv().to_owned()),
        (
            "hdr-compute-peak",
            if t.compute_peak { "auto" } else { "no" }.to_owned(),
        ),
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

    /// 偵測到 deinterlace=auto 的引擎（本專案建置的）
    fn caps() -> EngineCaps {
        EngineCaps {
            deint_auto: true,
            deint_status: true,
            ..EngineCaps::default()
        }
    }

    fn opts(v: &VideoSettings) -> Vec<(&'static str, String)> {
        mpv_options(v, &caps(), &PictureDefaults::default(), &HashSet::new())
    }

    /// 選項清單 → 名稱 → 值
    fn get<'a>(opts: &'a [(&'static str, String)], name: &str) -> &'a str {
        opts.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("沒有 {name}：{opts:?}"))
    }

    #[test]
    fn default_settings_keep_mpv_defaults_except_deinterlace() {
        // 預設設定 = mpv 原本的值（mpv 文件、0.37 與本專案的引擎都一樣），只有去交錯改成自動
        let expected = [
            ("deinterlace", "auto"),
            ("deband", "no"),
            ("deband-iterations", "1"),
            ("deband-threshold", "48"),
            ("deband-range", "16"),
            ("deband-grain", "32"),
            ("sharpen", "0"),
            ("scale", "lanczos"),
            ("dscale", "hermite"),
            ("cscale", ""),
            ("scale-antiring", "0.000000"),
            ("tone-mapping", "auto"),
            ("target-peak", "auto"),
            ("gamut-mapping-mode", "auto"),
            ("hdr-compute-peak", "auto"),
        ];
        let got = opts(&VideoSettings::default());
        let got: Vec<(&str, &str)> = got.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(got, expected);
        assert!(got.iter().map(|(k, _)| *k).eq(MANAGED), "每次都是完整的一組，順序固定");
        // 不支援 auto 的引擎（系統的 libmpv 0.37）：自動 = 關閉，跟 mpv 的預設值一樣
        let old = EngineCaps::default();
        let o = mpv_options(
            &VideoSettings::default(),
            &old,
            &PictureDefaults::default(),
            &HashSet::new(),
        );
        assert_eq!(get(&o, "deinterlace"), "no");
        let on = VideoSettings {
            deinterlace: Deinterlace::On,
            ..VideoSettings::default()
        };
        let o = mpv_options(&on, &old, &PictureDefaults::default(), &HashSet::new());
        assert_eq!(get(&o, "deinterlace"), "yes", "開啟照樣是開啟");
        assert_eq!(get(&opts(&on), "deinterlace"), "yes");
        let off = VideoSettings {
            deinterlace: Deinterlace::Off,
            ..VideoSettings::default()
        };
        assert_eq!(get(&opts(&off), "deinterlace"), "no");
    }

    #[test]
    fn quality_presets_give_the_full_scaler_set_and_overrides_win() {
        // 這個引擎讀到的預設值（「標準」用的）跟 mpv 文件的不一樣也照用
        let defaults = PictureDefaults {
            scale: "spline36".into(),
            dscale: "mitchell".into(),
            cscale: "".into(),
            scale_antiring: "0.100000".into(),
        };
        let scalers = |v: &VideoSettings| -> [String; 4] {
            let o = mpv_options(v, &caps(), &defaults, &HashSet::new());
            assert!(o.iter().map(|(k, _)| *k).eq(MANAGED), "{o:?}");
            ["scale", "dscale", "cscale", "scale-antiring"].map(|k| get(&o, k).to_owned())
        };
        let with = |quality| VideoSettings {
            quality,
            ..VideoSettings::default()
        };
        assert_eq!(
            scalers(&with(Quality::Fast)),
            ["bilinear", "bilinear", "bilinear", "0.100000"]
        );
        assert_eq!(
            scalers(&with(Quality::Standard)),
            ["spline36", "mitchell", "", "0.100000"]
        );
        assert_eq!(
            scalers(&with(Quality::High)),
            ["ewa_lanczossharp", "mitchell", "", "0.6"]
        );
        // 個別指定的優先，其他照畫質
        let mut v = with(Quality::High);
        v.scale = Some(Upscaler::Spline36);
        assert_eq!(scalers(&v), ["spline36", "mitchell", "", "0.6"]);
        v.dscale = Some(Downscaler::CatmullRom);
        v.cscale = Some(ChromaScaler::EwaLanczos);
        assert_eq!(scalers(&v), ["spline36", "catmull_rom", "ewa_lanczos", "0.6"]);
        v.quality = Quality::Fast;
        assert_eq!(scalers(&v), ["spline36", "catmull_rom", "ewa_lanczos", "0.100000"]);
        // 每個演算法都對應到它自己的 mpv 名稱
        for s in Upscaler::ALL {
            let v = VideoSettings {
                scale: Some(s),
                ..VideoSettings::default()
            };
            assert_eq!(scalers(&v)[0], s.mpv());
        }
    }

    #[test]
    fn deband_and_sharpen_tables() {
        let deband = |s| {
            let o = opts(&VideoSettings {
                deband: s,
                ..VideoSettings::default()
            });
            [
                "deband",
                "deband-iterations",
                "deband-threshold",
                "deband-range",
                "deband-grain",
            ]
            .map(|k| get(&o, k).to_owned())
        };
        assert_eq!(deband(Strength::Off), ["no", "1", "48", "16", "32"]);
        assert_eq!(deband(Strength::Light), ["yes", "1", "32", "16", "16"]);
        assert_eq!(deband(Strength::Medium), ["yes", "1", "48", "16", "32"]);
        assert_eq!(deband(Strength::Strong), ["yes", "2", "64", "16", "48"]);
        let sharpen = |s| {
            get(
                &opts(&VideoSettings {
                    sharpen: s,
                    ..VideoSettings::default()
                }),
                "sharpen",
            )
            .to_owned()
        };
        assert_eq!(Strength::ALL.map(sharpen), ["0", "0.25", "0.5", "1"]);
    }

    #[test]
    fn tone_mapping_options() {
        let tone = |t: ToneSettings| {
            let o = opts(&VideoSettings {
                tone: t,
                ..VideoSettings::default()
            });
            ["tone-mapping", "target-peak", "gamut-mapping-mode", "hdr-compute-peak"].map(|k| get(&o, k).to_owned())
        };
        assert_eq!(tone(ToneSettings::default()), ["auto", "auto", "auto", "auto"]);
        assert_eq!(
            tone(ToneSettings {
                curve: ToneCurve::Hable,
                target_peak: Some(400),
                gamut: Gamut::Desaturate,
                compute_peak: false,
            }),
            ["hable", "400", "desaturate", "no"]
        );
        for c in ToneCurve::ALL {
            let t = ToneSettings {
                curve: c,
                ..ToneSettings::default()
            };
            assert_eq!(tone(t)[0], c.mpv());
        }
        for g in Gamut::ALL {
            let t = ToneSettings {
                gamut: g,
                ..ToneSettings::default()
            };
            assert_eq!(tone(t)[2], g.mpv());
        }
        assert_eq!(peak_label(None), "自動");
        assert_eq!(peak_label(Some(203)), "203 nits");
    }

    #[test]
    fn user_overridden_options_are_skipped() {
        let overrides: HashSet<String> = ["scale", "deinterlace", "hdr-compute-peak", "not-ours"]
            .map(str::to_owned)
            .into();
        let v = VideoSettings {
            quality: Quality::High,
            ..VideoSettings::default()
        };
        let o = mpv_options(&v, &caps(), &PictureDefaults::default(), &overrides);
        let names: Vec<&str> = o.iter().map(|(k, _)| *k).collect();
        let expected: Vec<&str> = MANAGED
            .into_iter()
            .filter(|k| !["scale", "deinterlace", "hdr-compute-peak"].contains(k))
            .collect();
        assert_eq!(names, expected);
        assert_eq!(get(&o, "scale-antiring"), "0.6", "同一組的其他選項照送");
    }

    #[test]
    fn dumb_mode_still_maps_every_option() {
        // 軟體繪圖的簡化流程：選項照樣對應（畫面輸出會忽略），設定跟 mpv 的值才一致
        let dumb = EngineCaps { dumb: true, ..caps() };
        let v = VideoSettings {
            quality: Quality::High,
            deband: Strength::Strong,
            ..VideoSettings::default()
        };
        let plain = mpv_options(&v, &caps(), &PictureDefaults::default(), &HashSet::new());
        assert_eq!(
            mpv_options(&v, &dumb, &PictureDefaults::default(), &HashSet::new()),
            plain
        );
    }

    #[test]
    fn deinterlace_status_text() {
        assert_eq!(deinterlace_status(Deinterlace::Auto, true), "已去交錯");
        assert_eq!(deinterlace_status(Deinterlace::Auto, false), "逐行影片");
        assert_eq!(deinterlace_status(Deinterlace::On, true), "已去交錯");
        assert_eq!(deinterlace_status(Deinterlace::Off, false), "未去交錯");
        let old = EngineCaps::default();
        assert_eq!(Deinterlace::Auto.effective(&old), Deinterlace::Off);
        assert_eq!(Deinterlace::Auto.effective(&caps()), Deinterlace::Auto);
        assert_eq!(Deinterlace::On.effective(&old), Deinterlace::On);
    }
}
