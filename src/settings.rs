//! 使用者設定，存成 JSON：
//! - Windows：`%APPDATA%\Vitascope\settings.json`
//! - macOS：`~/Library/Application Support/Vitascope/settings.json`
//! - Linux：`$XDG_CONFIG_HOME/vitascope/settings.json`（預設 `~/.config/vitascope/`）
//!
//! 視窗位置大小也自己存，不用 eframe 的機制：eframe 會連「全螢幕」一起記住，
//! 下次啟動直接全螢幕，跟一般播放器的習慣不同。

pub use crate::keymap::KeySettings;
pub use crate::pacing::SmoothMode;
pub use crate::picture::VideoSettings;
pub use crate::sound::AudioSettings;
pub use crate::theme::ThemeChoice;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f64,
    pub muted: bool,
    /// 上次的視窗位置大小（一般模式下的；全螢幕不記）
    pub window: Option<WindowGeometry>,
    /// 播完自動播放同資料夾的下一個檔案
    pub auto_next: bool,
    /// 再次開啟時從上次看到的地方繼續播放
    pub resume: bool,
    /// 字幕外觀
    pub subtitle: SubStyle,
    /// 視窗置頂（蓋在其他視窗上面）：不置頂 / 永遠置頂 / 播放時置頂。
    /// 以前是開關 `always_on_top`，讀檔時換成這個（見 `migrate`）
    pub on_top: OnTop,
    /// 顯示側邊面板（播放清單、書籤）
    pub show_playlist: bool,
    /// 側邊面板目前的分頁（播放清單 / 書籤）；面板開不開還是看 `show_playlist`
    pub side_tab: SideTab,
    /// 截圖資料夾；None = 「圖片」資料夾裡的 VitaScope
    pub screenshot_dir: Option<PathBuf>,
    /// 截圖包含字幕
    pub screenshot_subtitles: bool,
    /// 介面語言
    pub language: crate::i18n::Lang,
    /// 硬體解碼（失敗時 mpv 自動退回軟解）
    pub hwdec: bool,
    /// ← / → 跳幾秒
    pub seek_short: f64,
    /// Ctrl + ← / → 跳幾秒
    pub seek_long: f64,
    /// 只開一個視窗：開新檔案時交給已經開著的視窗
    pub single_instance: bool,
    /// Windows：加到影音檔的「開啟檔案」選單（登錄在目前使用者底下）
    pub file_associations: bool,
    /// 畫質（影像調整、去交錯、縮放演算法、像素著色器、HDR）
    pub video: VideoSettings,
    /// 音效（輸出裝置、等化器、音量平衡、音訊直通…）
    pub audio: AudioSettings,
    /// 流暢播放（依螢幕更新率同步影像）
    pub smooth: SmoothMode,
    /// 外觀：深色 / 淺色 / 跟隨系統
    pub theme: ThemeChoice,
    /// 快捷鍵（預設組、自己改過的）
    pub keys: KeySettings,
    /// 存檔位置；None = 只放在記憶體（自動測試用：`Settings::default()` 不會動到使用者的設定檔）
    #[serde(skip)]
    path: Option<PathBuf>,
    /// 上次讀檔或存檔時的內容：存檔時只寫這之後改過的設定，其他的以檔案裡的為準
    /// （好幾個視窗同時開著、或解除安裝程式改過設定時，才不會被舊的值蓋回去）
    #[serde(skip)]
    baseline: Option<serde_json::Value>,
}

/// 字幕外觀（文字字幕；勾選「也套用到 ASS」時連 ASS 字幕一起改）。
/// 每一項都明確設定給 mpv：各版本 mpv 的預設值不同（例如字級 55 / 38），三個平台才會一樣
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubStyle {
    /// 字型名稱；空字串 = 依作業系統選常見的中文字型
    pub font: String,
    /// 字級（以 720 像素高的畫面為準，mpv 會依畫面大小縮放）
    pub size: f64,
    /// 顏色 RGBA（不預乘透明度）
    pub color: [u8; 4],
    pub border_color: [u8; 4],
    pub border_size: f64,
    pub shadow: f64,
    pub bold: bool,
    /// 垂直位置：100 = 最下面，0 = 最上面
    pub position: f64,
    /// 也套用到 ASS 字幕（字幕組的特效、排版會被蓋掉）
    pub override_ass: bool,
}

impl Default for SubStyle {
    fn default() -> Self {
        Self {
            font: String::new(),
            size: 42.0,
            color: [255, 255, 255, 255],
            border_color: [0, 0, 0, 255],
            border_size: 2.5,
            shadow: 1.0,
            bold: false,
            position: 100.0,
            override_ass: false,
        }
    }
}

impl SubStyle {
    /// 常見的中文字型（字型名稱, 顯示名稱）
    pub fn font_choices() -> &'static [(&'static str, &'static str)] {
        if cfg!(target_os = "windows") {
            &[
                ("Microsoft JhengHei", "微軟正黑體"),
                ("Microsoft YaHei", "微軟雅黑"),
                ("PMingLiU", "新細明體"),
                ("DFKai-SB", "標楷體"),
            ]
        } else if cfg!(target_os = "macos") {
            &[
                ("PingFang TC", "蘋方-繁"),
                ("Heiti TC", "黑體-繁"),
                ("Songti TC", "宋體-繁"),
                ("Kaiti TC", "楷體-繁"),
            ]
        } else {
            &[
                ("Noto Sans CJK TC", "Noto Sans CJK TC（思源黑體）"),
                ("Noto Serif CJK TC", "Noto Serif CJK TC（思源宋體）"),
                ("WenQuanYi Zen Hei", "文泉驛正黑"),
            ]
        }
    }

    /// 實際使用的字型：沒指定就用這個作業系統的第一個中文字型（沒裝的話 mpv 會自己找替代字型）
    pub fn effective_font(&self) -> &str {
        let font = self.font.trim();
        if font.is_empty() {
            Self::font_choices()[0].0
        } else {
            font
        }
    }

    /// 對應的 mpv 選項。邊框用舊名稱 sub-border-*：新版 mpv 改名為 sub-outline-*，舊名稱仍然有效，
    /// Linux 套件的 mpv 0.37 只認得舊名稱
    pub fn mpv_options(&self) -> Vec<(&'static str, String)> {
        // mpv 的顏色格式是 #AARRGGBB
        let hex = |[r, g, b, a]: [u8; 4]| format!("#{a:02X}{r:02X}{g:02X}{b:02X}");
        let yes_no = |b: bool| if b { "yes" } else { "no" }.to_owned();
        vec![
            ("sub-font", self.effective_font().to_owned()),
            ("sub-font-size", format!("{}", self.size)),
            ("sub-color", hex(self.color)),
            ("sub-border-color", hex(self.border_color)),
            ("sub-border-size", format!("{}", self.border_size)),
            ("sub-shadow-offset", format!("{}", self.shadow)),
            ("sub-shadow-color", "#80000000".to_owned()),
            ("sub-bold", yes_no(self.bold)),
            ("sub-pos", format!("{}", self.position)),
            (
                "sub-ass-override",
                if self.override_ass { "force" } else { "scale" }.to_owned(),
            ),
        ]
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 100.0,
            muted: false,
            window: None,
            auto_next: true,
            resume: true,
            subtitle: SubStyle::default(),
            on_top: OnTop::Never,
            show_playlist: false,
            side_tab: SideTab::Playlist,
            screenshot_dir: None,
            screenshot_subtitles: true,
            language: crate::i18n::Lang::default(),
            hwdec: true,
            seek_short: 5.0,
            seek_long: 30.0,
            single_instance: true,
            file_associations: false,
            video: VideoSettings::default(),
            audio: AudioSettings::default(),
            smooth: SmoothMode::default(),
            theme: ThemeChoice::default(),
            keys: KeySettings::default(),
            path: None,
            baseline: None,
        }
    }
}

/// 側邊面板的分頁
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SideTab {
    /// 播放清單（F6）
    #[default]
    Playlist,
    /// 目前檔案的書籤（H）
    Bookmarks,
}

/// 視窗置頂模式（比照 PotPlayer 的三種）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnTop {
    /// 不置頂
    #[default]
    Never,
    /// 永遠置頂
    Always,
    /// 播放時置頂：暫停、停止、播完（停在最後一格）時回到一般視窗
    WhilePlaying,
}

impl OnTop {
    pub const ALL: [OnTop; 3] = [OnTop::Never, OnTop::Always, OnTop::WhilePlaying];

    pub fn label(self) -> &'static str {
        match self {
            OnTop::Never => crate::tr!("不置頂", "Never"),
            OnTop::Always => crate::tr!("永遠置頂", "Always"),
            OnTop::WhilePlaying => crate::tr!("播放時置頂", "While playing"),
        }
    }

    /// 快捷鍵依序切換：不置頂 → 永遠置頂 → 播放時置頂 → 不置頂
    pub fn next(self) -> Self {
        match self {
            OnTop::Never => OnTop::Always,
            OnTop::Always => OnTop::WhilePlaying,
            OnTop::WhilePlaying => OnTop::Never,
        }
    }

    /// 換成這個模式時的提示
    pub fn osd(self) -> &'static str {
        match self {
            OnTop::Never => crate::tr!("視窗置頂：關閉", "Always on top: off"),
            OnTop::Always => crate::tr!("視窗置頂：永遠", "Always on top: always"),
            OnTop::WhilePlaying => crate::tr!("視窗置頂：播放時", "Always on top: while playing"),
        }
    }

    /// 現在視窗要不要置頂；`playing` = 有檔案、正在播放（沒有暫停、沒有停在最後一格）
    pub fn effective(self, playing: bool) -> bool {
        match self {
            OnTop::Never => false,
            OnTop::Always => true,
            OnTop::WhilePlaying => playing,
        }
    }

    /// 視窗一開始要不要置頂（main.rs 建視窗、App 記下「已經送過的層級」都用這個，兩邊不會不一致）：
    /// 只有永遠置頂；播放時置頂一開始還沒在播，是一般視窗
    pub fn at_launch(self) -> bool {
        self.effective(false)
    }
}

/// 舊版的視窗置頂開關（v0.3.0 以前）：讀檔時換成 `on_top`，存檔時從檔案拿掉
const OLD_ON_TOP: &str = "always_on_top";

/// 讀檔前把舊的設定名稱換成新的。兩個都有時以新的為準
///（新版存檔時會拿掉舊的；兩個都在代表是舊版後來寫的，舊版沒讀到 `on_top`，寫的是它自己的預設值）
fn migrate(value: &mut serde_json::Value) {
    let Some(fields) = value.as_object_mut() else { return };
    if let Some(old) = fields.remove(OLD_ON_TOP)
        && !fields.contains_key("on_top")
        && let Some(on) = old.as_bool()
    {
        let mode = if on { OnTop::Always } else { OnTop::Never };
        if let Ok(v) = serde_json::to_value(mode) {
            fields.insert("on_top".into(), v);
        }
    }
}

/// 單位是 egui 的點（邏輯像素）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// 視窗外框左上角
    pub pos: [f32; 2],
    /// 內容區大小
    pub size: [f32; 2],
    pub maximized: bool,
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("settings.json"))
    }

    /// 讀取設定；檔案不存在或格式錯誤就用預設值。之後 `save` 會存回同一個檔案
    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::load_from(path),
            None => Self::default(),
        }
    }

    /// 從指定的檔案讀取，之後 `save()` 也寫回這個檔案
    pub fn load_from(path: PathBuf) -> Self {
        let mut settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(Self::from_value_lenient)
            .unwrap_or_default()
            .sanitized();
        // 基準是整理過的值：整理掉的部分（例如超出範圍）沒有再改過就不寫回檔案，檔案裡保留原本的
        settings.baseline = serde_json::to_value(&settings).ok();
        settings.path = Some(path);
        settings
    }

    /// 讀不懂的設定（例如新版加的語言、手動改錯）只有那一項用預設值，其他設定照樣讀進來。
    /// 群組（字幕外觀、畫質、音效…）裡也是逐項：一項讀不懂，同一組的其他項目照樣讀進來
    fn from_value_lenient(mut value: serde_json::Value) -> Self {
        migrate(&mut value);
        if let Ok(s) = serde_json::from_value(value.clone()) {
            return s;
        }
        let serde_json::Value::Object(fields) = value else {
            return Self::default();
        };
        let Ok(mut good) = serde_json::to_value(Self::default()) else {
            return Self::default();
        };
        lenient_fill(&mut good, &mut Vec::new(), fields);
        serde_json::from_value(good).unwrap_or_default()
    }

    /// 讀檔後整理：超出範圍的值拉回範圍內、對不上的參照拿掉（手動改過、或新版寫的檔案）
    pub fn sanitized(mut self) -> Self {
        let v = &mut self.video;
        v.adjust = v.adjust.clamped();
        // 開發版存得下 400、1000 nits（超過 203 時 vo_gpu 裁切亮部）：所有讀檔都經過這裡，一起拉回 100…203
        v.tone.target_peak = v.tone.target_peak.map(|p| {
            p.clamp(
                crate::picture::ToneSettings::MIN_PEAK,
                crate::picture::ToneSettings::MAX_PEAK,
            )
        });
        // 編號重複的組合（兩個視窗同時新增、合併後）只留第一個
        let mut seen = std::collections::HashSet::new();
        v.shaders.presets.retain(|p| seen.insert(p.id));
        if v.shaders.active_preset().is_none() {
            v.shaders.active = None;
        }
        let a = &mut self.audio;
        let max_gain = crate::sound::EQ_MAX_GAIN;
        for g in &mut a.eq.gains {
            *g = g.clamp(-max_gain, max_gain);
        }
        a.volume_max = crate::sound::snap_volume_max(a.volume_max);
        self.volume = self.volume.clamp(0.0, f64::from(a.volume_max));
        self.keys = self.keys.sanitized();
        self
    }

    /// 只放在記憶體、不寫回檔案（`--shot` 自動截圖時用，不會改到使用者的視窗大小之類的設定）
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    /// 存檔；不是從檔案讀進來的設定（例如自動測試用的預設值）不寫檔。
    /// 只寫上次讀檔或存檔之後改過的設定，其他的保留檔案裡現在的值（可能是另一個視窗改的）
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        let current = serde_json::to_value(&*self).map_err(std::io::Error::other)?;
        let on_disk = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let mut merged = match (&self.baseline, on_disk) {
            (Some(base), Some(disk)) => merge(&current, base, disk, &mut Vec::new()),
            _ => current.clone(),
        };
        // 舊的設定名稱不再寫（已經換成 `on_top`；其他不認得的設定照樣保留，可能是新版寫的）
        if let Some(fields) = merged.as_object_mut() {
            fields.remove(OLD_ON_TOP);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 先寫暫存檔再改名，中途當掉也不會留下寫一半的設定檔
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&merged).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)?;
        self.baseline = Some(current);
        Ok(())
    }
}

/// 寬鬆讀取：把檔案裡的 `fields`（在 `good` 的 `path` 底下）一項一項放進 `good`，
/// 放進去之後整份設定還讀得懂才留下。整組讀不懂、而且檔案和預設值都是物件時，改成逐項放這一組的內容
fn lenient_fill(
    good: &mut serde_json::Value,
    path: &mut Vec<String>,
    fields: serde_json::Map<String, serde_json::Value>,
) {
    use serde_json::Value;
    for (key, value) in fields {
        path.push(key);
        let mut trial = good.clone();
        let accepted = match slot(&mut trial, path) {
            Some(s) => {
                *s = value.clone();
                serde_json::from_value::<Settings>(trial.clone()).is_ok()
            }
            None => false,
        };
        if accepted {
            *good = trial;
        } else if let Value::Object(children) = value
            && path
                .iter()
                .try_fold(&*good, |node, key| node.get(key))
                .is_some_and(Value::is_object)
        {
            lenient_fill(good, path, children);
        }
        path.pop();
    }
}

/// `path` 指到的位置（最後一層不存在時建立，值是 null）；中間有一層不是物件時回傳 None
fn slot<'a>(root: &'a mut serde_json::Value, path: &[String]) -> Option<&'a mut serde_json::Value> {
    let (last, parents) = path.split_last()?;
    let mut node = root;
    for key in parents {
        node = node.as_object_mut()?.get_mut(key)?;
    }
    Some(
        node.as_object_mut()?
            .entry(last.clone())
            .or_insert(serde_json::Value::Null),
    )
}

/// 依編號（`id`）合併的清單：像素著色器的組合。兩個視窗各自新增、修改、刪除不同的組合，都留下
const MERGE_BY_ID: &[&str] = &["video", "shaders", "presets"];
/// 固定長度、逐格合併的陣列：等化器每一段的增益。兩個視窗各調了不同的段落，都留下
const MERGE_BY_INDEX: &[&str] = &["audio", "eq", "gains"];
/// 項目可以被刪掉的物件：自己改過的快捷鍵（「還原」就是拿掉那一項）。
/// 一般的物件只合併現在有的項目，這個視窗刪掉的會從檔案裡回來；這裡跟上次讀檔、存檔時比，
/// 這個視窗刪掉的也從檔案拿掉（跟 `merge_by_id` 的刪除一樣）。每一項（一個指令的按鍵）還是整個當成一個值
const MERGE_DELETES: &[&str] = &["keys", "custom"];

/// 三方合併：`now` 跟 `base` 不同的地方寫進 `disk`，其他的保留 `disk` 的。
/// 物件（例如字幕外觀）逐項合併：兩個視窗各改了一項，兩項都留下。
/// 陣列整個當成一個值，除了上面兩個（`path` 是目前的位置）
fn merge(
    now: &serde_json::Value,
    base: &serde_json::Value,
    disk: serde_json::Value,
    path: &mut Vec<String>,
) -> serde_json::Value {
    use serde_json::Value;
    match (now, base, disk) {
        (Value::Object(now), Value::Object(base), Value::Object(mut disk)) => {
            for (key, value) in now {
                let merged = match (base.get(key), disk.remove(key)) {
                    (Some(b), Some(d)) => {
                        path.push(key.clone());
                        let m = merge(value, b, d, path);
                        path.pop();
                        m
                    }
                    // 檔案裡沒有（舊版的設定檔）、或上次讀的時候沒有：用現在的
                    _ => value.clone(),
                };
                disk.insert(key.clone(), merged);
            }
            if path.as_slice() == MERGE_DELETES {
                for key in base.keys().filter(|k| !now.contains_key(*k)) {
                    disk.remove(key);
                }
            }
            Value::Object(disk)
        }
        (Value::Array(n), Value::Array(b), Value::Array(d)) if path.as_slice() == MERGE_BY_ID => {
            match merge_by_id(n, b, &d) {
                Some(merged) => Value::Array(merged),
                None => scalar_merge(now, base, Value::Array(d)),
            }
        }
        (Value::Array(n), Value::Array(b), Value::Array(mut d))
            if path.as_slice() == MERGE_BY_INDEX && n.len() == b.len() && b.len() == d.len() =>
        {
            for ((now, base), disk) in n.iter().zip(b).zip(d.iter_mut()) {
                if now != base {
                    *disk = now.clone();
                }
            }
            Value::Array(d)
        }
        (now, base, disk) => scalar_merge(now, base, disk),
    }
}

/// 整個值：這個視窗改了就用現在的，沒改就保留檔案的
fn scalar_merge(now: &serde_json::Value, base: &serde_json::Value, disk: serde_json::Value) -> serde_json::Value {
    if now != base { now.clone() } else { disk }
}

/// 依編號合併清單：檔案裡的照原本的順序留下，這個視窗（跟上次讀檔、存檔時比）刪掉的拿掉、改過的換成現在的，
/// 新增的（檔案裡沒有的）接在後面。這個視窗改過、另一個視窗刪掉的留下（改的比較新）。
/// 有一項沒有編號（不是這個程式寫的）時 None：整個清單照一般的值合併
fn merge_by_id(
    now: &[serde_json::Value],
    base: &[serde_json::Value],
    disk: &[serde_json::Value],
) -> Option<Vec<serde_json::Value>> {
    type Keyed<'a> = Vec<(u64, &'a serde_json::Value)>;
    fn index(list: &[serde_json::Value]) -> Option<Keyed<'_>> {
        list.iter().map(|v| Some((v.get("id")?.as_u64()?, v))).collect()
    }
    fn find<'a>(list: &Keyed<'a>, key: u64) -> Option<&'a serde_json::Value> {
        list.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    let (now, base, disk) = (index(now)?, index(base)?, index(disk)?);
    // 這個視窗新增或改過的
    let touched = |key: u64| find(&now, key).filter(|v| find(&base, key) != Some(*v));
    let deleted = |key: u64| find(&now, key).is_none() && find(&base, key).is_some();
    let mut merged: Vec<serde_json::Value> = disk
        .iter()
        .filter(|(key, _)| !deleted(*key))
        .map(|(key, v)| touched(*key).unwrap_or(v).clone())
        .collect();
    merged.extend(
        now.iter()
            .filter(|(key, _)| find(&disk, *key).is_none() && touched(*key).is_some())
            .map(|(_, v)| (*v).clone()),
    );
    Some(merged)
}

/// 設定與播放紀錄的資料夾
pub(crate) fn config_dir() -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Vitascope"))
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support/Vitascope"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
            .map(|d| d.join("vitascope"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_or_partial_settings_still_load() {
        // 之後新增欄位時，舊的設定檔也要讀得進來
        let s: Settings = serde_json::from_str(r#"{ "volume": 42.0 }"#).unwrap();
        assert_eq!(s.volume, 42.0);
        assert!(!s.muted);
        assert!(s.window.is_none());
        assert!(s.auto_next && s.resume, "新功能預設開啟");
        assert_eq!(s.subtitle, SubStyle::default());
    }

    #[test]
    fn loaded_settings_save_back_to_their_file() {
        let dir = std::env::temp_dir().join(format!("vitascope-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut s = Settings::load_from(path.clone());
        assert_eq!(s.volume, Settings::default().volume, "沒有檔案時用預設值");
        s.on_top = OnTop::Always;
        s.show_playlist = true;
        s.volume = 42.0;
        s.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert!(back.on_top == OnTop::Always && back.show_playlist);
        assert_eq!(back.volume, 42.0);
        // 自動截圖用的設定：讀得到，但不寫回去
        let mut shot = back.detached();
        shot.volume = 1.0;
        shot.save().unwrap();
        assert_eq!(Settings::load_from(path).volume, 42.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_windows_keep_each_others_changes() {
        let dir = std::env::temp_dir().join(format!("vitascope-settings-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        // 兩個視窗都開著
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        // A 換成英文、改跳轉秒數，馬上存檔
        a.language = crate::i18n::Lang::En;
        a.seek_short = 10.0;
        a.save().unwrap();
        // B 只改了音量，關閉時存檔：A 改的不能被 B 舊的值蓋回去
        b.volume = 30.0;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.language, crate::i18n::Lang::En);
        assert_eq!(back.seek_short, 10.0);
        assert_eq!(back.volume, 30.0);
        // B 之後自己改的設定照樣寫得進去（A 也改過的那一項，後存的為準）
        b.seek_short = 3.0;
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.seek_short, 3.0);
        assert_eq!(back.language, crate::i18n::Lang::En);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn subtitle_style_changes_from_two_windows_are_both_kept() {
        let dir = std::env::temp_dir().join(format!("vitascope-settings-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.subtitle.size = 50.0;
        a.save().unwrap();
        b.subtitle.color = [255, 255, 0, 255];
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.subtitle.size, 50.0, "A 改的字級不能被 B 蓋回去");
        assert_eq!(back.subtitle.color, [255, 255, 0, 255]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_unreadable_value_does_not_reset_the_other_settings() {
        let dir = std::env::temp_dir().join(format!("vitascope-settings-lenient-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // 新版加的語言、以後的新設定
        std::fs::write(
            &path,
            r#"{ "volume": 42.0, "language": "ja", "seek_short": 7.0, "future_option": 1 }"#,
        )
        .unwrap();
        let mut s = Settings::load_from(path.clone());
        assert_eq!(s.volume, 42.0);
        assert_eq!(s.seek_short, 7.0);
        assert_eq!(s.language, crate::i18n::Lang::default());
        // 存檔時保留這個版本不認得的設定
        s.volume = 50.0;
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_option"), "{text}");
        assert!(text.contains(r#""ja""#), "沒改過的語言不要蓋掉（新版還讀得懂）：{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 這個測試專用的暫存資料夾（每次重建）
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vitascope-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lenient(json: &str) -> Settings {
        Settings::from_value_lenient(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn defaults_match_spec() {
        // 沒有 L3 設定的檔案：每一項都是規格的預設值（不是型別的零值）
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.video, VideoSettings::default());
        assert_eq!(s.audio, AudioSettings::default());
        assert_eq!(s.smooth, SmoothMode::Off, "流暢播放先預設關");
        assert_eq!(s.theme, ThemeChoice::Dark, "外觀預設深色");
        assert_eq!(s.on_top, OnTop::Never, "預設不置頂");
        assert_eq!(s.side_tab, SideTab::Playlist, "側邊面板預設是播放清單");
        assert_eq!(s.keys.preset, crate::keymap::KeyPreset::Vitascope, "快捷鍵預設是影戲的");
        assert!(s.keys.custom.is_empty());
        assert_eq!(s.keys.mouse.click, "toggle-pause", "單擊畫面預設播放／暫停");
        assert_eq!(s.keys.mouse.double_click, "fullscreen", "雙擊畫面預設全螢幕");
        assert!(s.keys.mouse.middle.is_empty() && s.keys.mouse.back.is_empty() && s.keys.mouse.forward.is_empty());
        assert_eq!(s.keys.mouse.wheel, crate::keymap::WheelMode::Volume, "滾輪預設調音量");
        assert_eq!(s.video.deinterlace, crate::picture::Deinterlace::Auto);
        assert_eq!(s.video.quality, crate::picture::Quality::Standard);
        assert!(s.video.tone.compute_peak);
        assert_eq!(s.audio.volume_max, 100);
        assert!(s.audio.normalize_downmix && s.audio.eq.auto_preamp);
        assert!(s.audio.passthrough.ac3 && s.audio.passthrough.eac3 && s.audio.passthrough.dts);
        assert!(!s.audio.passthrough.enabled && !s.audio.passthrough.dts_hd && !s.audio.passthrough.truehd);
        // 一組只寫了一項：其他項目也是規格的預設值
        let s: Settings = serde_json::from_str(r#"{"audio":{"downmix":true},"video":{"keep_adjust":true}}"#).unwrap();
        assert!(s.audio.downmix && s.audio.normalize_downmix && s.audio.volume_max == 100);
        assert!(s.video.keep_adjust && s.video.tone.compute_peak);
    }

    #[test]
    fn old_file_gets_l3_defaults() {
        let dir = temp_dir("old");
        let path = dir.join("settings.json");
        // v0.2.0 寫的設定檔（沒有 video、audio、smooth）
        std::fs::write(
            &path,
            r#"{
  "volume": 64.0, "muted": true, "window": {"pos": [10.0, 20.0], "size": [800.0, 450.0], "maximized": false},
  "auto_next": false, "resume": true,
  "subtitle": {"font": "", "size": 50.0, "color": [255, 255, 0, 255], "border_color": [0, 0, 0, 255],
               "border_size": 2.5, "shadow": 1.0, "bold": true, "position": 90.0, "override_ass": false},
  "always_on_top": true, "show_playlist": false, "screenshot_dir": null, "screenshot_subtitles": false,
  "language": "en", "hwdec": false, "seek_short": 10.0, "seek_long": 60.0,
  "single_instance": false, "file_associations": false
}"#,
        )
        .unwrap();
        let s = Settings::load_from(path);
        assert_eq!(s.volume, 64.0);
        assert!(s.muted && !s.auto_next && !s.hwdec && !s.single_instance);
        assert_eq!(s.on_top, OnTop::Always, "舊的「視窗置頂」開著：永遠置頂");
        assert_eq!(s.subtitle.size, 50.0);
        assert!(s.subtitle.bold);
        assert_eq!(s.language, crate::i18n::Lang::En);
        assert_eq!(s.seek_long, 60.0);
        assert_eq!(s.window.unwrap().size, [800.0, 450.0]);
        assert_eq!(s.video, VideoSettings::default());
        assert_eq!(s.audio, AudioSettings::default());
        assert_eq!(s.smooth, SmoothMode::default());
        assert_eq!(s.theme, ThemeChoice::Dark, "升級後外觀不變");
        assert_eq!(s.keys, KeySettings::default(), "升級後快捷鍵不變");
        assert_eq!(s.keys.mouse, crate::keymap::MouseSettings::default(), "升級後滑鼠不變");
        assert_eq!(s.side_tab, SideTab::Playlist, "升級後側邊面板還是播放清單");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shortcut_settings_load_leniently_and_save() {
        let dir = temp_dir("keys");
        let path = dir.join("settings.json");
        let mut s = Settings::load_from(path.clone());
        s.keys
            .custom
            .insert("toggle-pause".into(), vec!["Space".into(), "Shift+K".into()]);
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""preset": "vitascope""#), "{text}");
        let back = Settings::load_from(path.clone());
        assert_eq!(back.keys.custom["toggle-pause"], ["Space", "Shift+K"]);
        // 一項讀不懂（不是按鍵清單）：只有那一項不讀，其他自己改過的照樣讀進來
        let s = lenient(
            r#"{"volume": 30.0, "keys": {"custom": {"toggle-pause": 3, "toggle-mute": ["N"], "future-cmd": ["F9"]}}}"#,
        );
        assert_eq!(s.volume, 30.0);
        assert!(!s.keys.custom.contains_key("toggle-pause"));
        assert_eq!(s.keys.custom["toggle-mute"], ["N"]);
        assert_eq!(s.keys.custom["future-cmd"], ["F9"], "認不得的指令照樣保留");
        // 新版的預設組（這版沒有）：用影戲的，自己改過的照樣讀進來
        let s = lenient(r#"{"keys": {"preset": "potplayer-2030", "custom": {"stop": ["S"]}}, "seek_short": 7.0}"#);
        assert_eq!(s.keys.preset, crate::keymap::KeyPreset::Vitascope);
        assert_eq!(s.keys.custom["stop"], ["S"]);
        assert_eq!(s.seek_short, 7.0);
        // 讀檔時整理：去掉重複的、每個指令最多 4 組
        std::fs::write(
            &path,
            r#"{"keys": {"custom": {"stop": ["A", "A", "B", "C", "D", "E"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            Settings::load_from(path.clone()).keys.custom["stop"],
            ["A", "B", "C", "D"]
        );
        // 兩個視窗各改了不同的指令：兩個都留下
        std::fs::remove_file(&path).unwrap();
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.keys.custom.insert("stop".into(), vec!["S".into()]);
        a.save().unwrap();
        b.keys.custom.insert("restart".into(), vec!["Backspace".into()]);
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.keys.custom["stop"], ["S"]);
        assert_eq!(back.keys.custom["restart"], ["Backspace"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mouse_settings_load_leniently_and_merge() {
        use crate::keymap::{MouseSettings, WheelMode};
        // 只寫了快捷鍵（A3 的設定檔）：滑鼠是預設值
        let s = lenient(r#"{"keys": {"preset": "potplayer", "custom": {"stop": ["S"]}}}"#);
        assert_eq!(s.keys.mouse, MouseSettings::default());
        // 一項讀不懂（新版的滾輪動作、不是字串）：只有那一項用預設值，其他照樣讀進來
        let s = lenient(
            r#"{"volume": 30.0, "keys": {"custom": {"stop": ["S"]},
                "mouse": {"wheel": "zoom-2030", "middle": "toggle-mute", "back": 3, "forward": "future-cmd"}}}"#,
        );
        assert_eq!(s.volume, 30.0);
        assert_eq!(s.keys.custom["stop"], ["S"]);
        assert_eq!(s.keys.mouse.wheel, WheelMode::Volume);
        assert_eq!(s.keys.mouse.middle, "toggle-mute");
        assert_eq!(s.keys.mouse.back, "");
        assert_eq!(s.keys.mouse.forward, "future-cmd", "認不得的指令照樣保留");
        assert_eq!(s.keys.mouse.click, "toggle-pause");
        // 存檔、讀回來
        let dir = temp_dir("mouse");
        let path = dir.join("settings.json");
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.keys.mouse.middle = "toggle-mute".into();
        a.keys.mouse.click = String::new();
        a.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""wheel": "volume""#), "{text}");
        // 另一個視窗改了滾輪：兩個視窗改的都留下
        b.keys.mouse.wheel = WheelMode::Seek;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.keys.mouse.middle, "toggle-mute");
        assert_eq!(back.keys.mouse.click, "");
        assert_eq!(back.keys.mouse.wheel, WheelMode::Seek);
        // 「還原成預設組…」：滑鼠回到預設，存檔後不會從檔案回來
        a.keys = a.keys.reset_all();
        a.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.keys.mouse.middle, "");
        assert_eq!(back.keys.mouse.click, "toggle-pause");
        assert_eq!(back.keys.mouse.wheel, WheelMode::Seek, "A 沒改過滾輪：留著 B 改的");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_custom_reset_survives_save() {
        let dir = temp_dir("keys-reset");
        let path = dir.join("settings.json");
        let mut a = Settings::load_from(path.clone());
        a.keys.custom.insert("stop".into(), vec!["S".into()]);
        a.keys.custom.insert("restart".into(), vec!["Backspace".into()]);
        a.save().unwrap();
        // 另一個視窗也開著，改了別的指令
        let mut b = Settings::load_from(path.clone());
        b.keys.custom.insert("toggle-mute".into(), vec!["N".into()]);
        b.save().unwrap();
        // 這個視窗還原了「停止」：存檔後不能從檔案裡回來，別的視窗改的照樣留下
        a.keys.custom.remove("stop");
        a.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert!(!back.keys.custom.contains_key("stop"), "{:?}", back.keys.custom);
        assert_eq!(back.keys.custom["restart"], ["Backspace"]);
        assert_eq!(back.keys.custom["toggle-mute"], ["N"]);
        // 全部還原
        a.keys = a.keys.reset_all();
        a.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.keys.custom.len(), 1, "只剩另一個視窗改的：{:?}", back.keys.custom);
        assert_eq!(back.keys.custom["toggle-mute"], ["N"]);
        // 預設組照一般的值合併
        a.keys.preset = crate::keymap::KeyPreset::Potplayer;
        a.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""preset": "potplayer""#), "{text}");
        assert_eq!(
            Settings::load_from(path).keys.preset,
            crate::keymap::KeyPreset::Potplayer
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_always_on_top_switch_becomes_a_mode() {
        let dir = temp_dir("on-top-old");
        let path = dir.join("settings.json");
        for (old, mode) in [(true, OnTop::Always), (false, OnTop::Never)] {
            std::fs::write(&path, format!(r#"{{"always_on_top": {old}, "volume": 61.0}}"#)).unwrap();
            let mut s = Settings::load_from(path.clone());
            assert_eq!(s.on_top, mode, "always_on_top = {old}");
            assert_eq!(s.volume, 61.0);
            // 存檔：寫新的名稱，舊的不再寫
            s.volume = 62.0;
            s.save().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("always_on_top"), "{text}");
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["on_top"], serde_json::to_value(mode).unwrap(), "{text}");
            assert_eq!(Settings::load_from(path.clone()).on_top, mode);
        }
        // 逐項讀取（有別的設定讀不懂）時也換過來
        let s = lenient(r#"{"always_on_top": true, "language": "ja", "volume": 12.0}"#);
        assert_eq!(s.on_top, OnTop::Always);
        assert_eq!(s.volume, 12.0);
        // 新舊都有：以新的為準；舊的不是開關：當成沒有
        let s = lenient(r#"{"always_on_top": false, "on_top": "while-playing"}"#);
        assert_eq!(s.on_top, OnTop::WhilePlaying);
        let s = lenient(r#"{"always_on_top": "yes", "seek_short": 9.0}"#);
        assert_eq!((s.on_top, s.seek_short), (OnTop::Never, 9.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn on_top_mode_round_trips_and_unknown_values_load_as_never() {
        let dir = temp_dir("on-top");
        let path = dir.join("settings.json");
        for mode in OnTop::ALL {
            let mut s = Settings::load_from(path.clone());
            s.on_top = mode;
            s.save().unwrap();
            assert_eq!(Settings::load_from(path.clone()).on_top, mode);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""on_top": "while-playing""#), "{text}");
        assert_eq!(serde_json::to_value(OnTop::Always).unwrap(), "always");
        assert_eq!(serde_json::to_value(OnTop::Never).unwrap(), "never");
        // 新版加的模式（或手動改錯）：不置頂，其他設定照讀
        let s = lenient(r#"{"on_top": "on-hover", "volume": 33.0, "seek_short": 8.0, "theme": "light"}"#);
        assert_eq!(s.on_top, OnTop::Never);
        assert_eq!((s.volume, s.seek_short, s.theme), (33.0, 8.0, ThemeChoice::Light));
        // 兩個視窗：A 換置頂模式、B 改音量，兩個都留下
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.on_top = OnTop::Always;
        a.save().unwrap();
        b.volume = 20.0;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.on_top, OnTop::Always, "A 改的置頂模式不能被 B 蓋回去");
        assert_eq!(back.volume, 20.0);
        // 舊的設定檔、兩個視窗都開著：A 改了模式，B 只改音量，存檔時不會用舊的值蓋回去
        std::fs::write(&path, r#"{"always_on_top": true}"#).unwrap();
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.on_top = OnTop::WhilePlaying;
        a.save().unwrap();
        b.volume = 40.0;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.on_top, OnTop::WhilePlaying);
        assert_eq!(back.volume, 40.0);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("always_on_top"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn side_tab_round_trips_and_unknown_values_load_as_playlist() {
        let dir = temp_dir("side-tab");
        let path = dir.join("settings.json");
        let mut s = Settings::load_from(path.clone());
        s.side_tab = SideTab::Bookmarks;
        s.show_playlist = true;
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""side_tab": "bookmarks""#), "{text}");
        let back = Settings::load_from(path.clone());
        assert_eq!((back.side_tab, back.show_playlist), (SideTab::Bookmarks, true));
        assert_eq!(serde_json::to_value(SideTab::Playlist).unwrap(), "playlist");
        // 新版加的分頁（或手動改錯）：播放清單，其他設定照讀
        let s = lenient(r#"{"side_tab": "history", "show_playlist": true, "volume": 31.0}"#);
        assert_eq!(s.side_tab, SideTab::Playlist);
        assert!(s.show_playlist);
        assert_eq!(s.volume, 31.0);
        // 兩個視窗：A 換分頁、B 改音量，兩個都留下
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.side_tab = SideTab::Playlist;
        a.save().unwrap();
        b.volume = 20.0;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.side_tab, SideTab::Playlist, "A 換的分頁不能被 B 蓋回去");
        assert_eq!(back.volume, 20.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn on_top_cycle_and_effective_state() {
        assert_eq!(OnTop::Never.next(), OnTop::Always);
        assert_eq!(OnTop::Always.next(), OnTop::WhilePlaying);
        assert_eq!(OnTop::WhilePlaying.next(), OnTop::Never);
        for playing in [false, true] {
            assert!(!OnTop::Never.effective(playing));
            assert!(OnTop::Always.effective(playing));
            assert_eq!(OnTop::WhilePlaying.effective(playing), playing);
        }
        // 開視窗時：只有永遠置頂一開始就置頂，播放時置頂等開始播放
        assert!(!OnTop::Never.at_launch());
        assert!(OnTop::Always.at_launch());
        assert!(!OnTop::WhilePlaying.at_launch());
    }

    #[test]
    fn theme_is_saved_and_unknown_values_load_as_dark() {
        let dir = temp_dir("theme");
        let path = dir.join("settings.json");
        let mut s = Settings::load_from(path.clone());
        s.theme = ThemeChoice::Light;
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""theme": "light""#), "{text}");
        assert_eq!(Settings::load_from(path.clone()).theme, ThemeChoice::Light);
        // 新版加的外觀（或手動改錯）：用深色，其他設定照讀
        let s = lenient(r#"{"theme": "sepia", "volume": 33.0, "seek_short": 8.0}"#);
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!((s.volume, s.seek_short), (33.0, 8.0));
        // 兩個視窗：A 換外觀、B 改音量，兩個都留下
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.theme = ThemeChoice::System;
        a.save().unwrap();
        b.volume = 20.0;
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.theme, ThemeChoice::System, "A 改的外觀不能被 B 蓋回去");
        assert_eq!(back.volume, 20.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_unreadable_nested_value_keeps_its_siblings() {
        let s = lenient(r#"{"volume": 40.0, "video":{"deband":"ultra","adjust":{"brightness":5}}}"#);
        assert_eq!(s.volume, 40.0);
        assert_eq!(s.video.adjust.brightness, 5, "同一組的其他項目要讀進來");
        assert_eq!(s.video.deband, crate::picture::Strength::Off, "讀不懂的那一項用預設值");
        // 更深一層也一樣：讀不懂的曲線不影響目標亮度；讀不懂的增益陣列不影響等化器開關
        let s = lenient(
            r#"{"video":{"tone":{"curve":"st2094-40","target_peak":400}},
                "audio":{"eq":{"enabled":true,"gains":[1,2,3]},"volume_max":150,"leveling":"loud"}}"#,
        );
        assert_eq!(s.video.tone.curve, crate::picture::ToneCurve::Auto);
        assert_eq!(s.video.tone.target_peak, Some(400));
        assert!(s.video.tone.compute_peak);
        assert!(s.audio.eq.enabled);
        assert_eq!(s.audio.eq.gains, [0.0; 10]);
        assert_eq!(s.audio.volume_max, 150);
        assert_eq!(s.audio.leveling, crate::sound::Leveling::Off);
        // 整組不是物件：整組用預設值
        let s = lenient(r#"{"video": 3, "seek_short": 2.0}"#);
        assert_eq!(s.video, VideoSettings::default());
        assert_eq!(s.seek_short, 2.0);
    }

    #[test]
    fn subtitle_nested_lenient() {
        // 以前字幕外觀裡一項讀不懂，整組字幕外觀都會變回預設值
        let s = lenient(r#"{"subtitle":{"size":50.0,"color":"yellow","bold":true},"volume":30.0}"#);
        assert_eq!(s.subtitle.size, 50.0);
        assert!(s.subtitle.bold);
        assert_eq!(s.subtitle.color, SubStyle::default().color);
        assert_eq!(s.volume, 30.0);
    }

    #[test]
    fn future_scaler_name_survives_a_save() {
        let dir = temp_dir("future-scaler");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"volume": 50.0, "video": {"scale": "ewa_future", "quality": "high"}}"#,
        )
        .unwrap();
        let mut s = Settings::load_from(path.clone());
        assert_eq!(s.video.scale, None, "記憶體裡用預設值（跟隨畫質）");
        assert_eq!(s.video.quality, crate::picture::Quality::High);
        s.volume = 60.0;
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("ewa_future"),
            "沒改過的設定不要蓋掉（新版還讀得懂）：{text}"
        );
        let back = Settings::load_from(path);
        assert_eq!(back.volume, 60.0);
        assert_eq!(back.video.quality, crate::picture::Quality::High);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_windows_change_different_audio_fields() {
        let dir = temp_dir("audio-merge");
        let path = dir.join("settings.json");
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        // A 調高音量上限、開等化器；B 選了輸出裝置、改了直通的格式
        a.audio.volume_max = 150;
        a.audio.eq.enabled = true;
        a.audio.eq.preset = crate::sound::EqPreset::Rock;
        a.save().unwrap();
        b.audio.device = Some("wasapi/{abc}".into());
        b.audio.passthrough.truehd = true;
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.audio.volume_max, 150, "A 改的不能被 B 舊的值蓋回去");
        assert!(back.audio.eq.enabled);
        assert_eq!(back.audio.eq.preset, crate::sound::EqPreset::Rock);
        assert_eq!(back.audio.device.as_deref(), Some("wasapi/{abc}"));
        assert!(back.audio.passthrough.truehd);
        assert!(back.audio.passthrough.ac3, "沒改的直通格式維持預設");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_windows_keep_each_others_shader_presets() {
        use crate::picture::ShaderPreset;
        let dir = temp_dir("shader-merge");
        let path = dir.join("settings.json");
        let preset = |id, name: &str, files: &[&str]| ShaderPreset {
            id,
            name: name.into(),
            files: files.iter().map(|f| (*f).to_owned()).collect(),
        };
        let names = |s: &Settings| -> Vec<String> { s.video.shaders.presets.iter().map(|p| p.name.clone()).collect() };
        // 一開始有兩個組合
        let mut first = Settings::load_from(path.clone());
        first.video.shaders.presets = vec![preset(1, "舊的 1", &["a.glsl"]), preset(2, "舊的 2", &["b.glsl"])];
        first.save().unwrap();
        // 兩個視窗都開著
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        // A 新增「組合 1」、加了檔案、開始使用，刪掉舊的 2
        a.video.shaders.presets.push(preset(10, "組合 1", &["Anime4K.glsl"]));
        a.video.shaders.presets.retain(|p| p.id != 2);
        a.video.shaders.active = Some(10);
        a.save().unwrap();
        // B 新增自己的組合、把舊的 1 改名：A 的組合不能不見
        b.video.shaders.presets.push(preset(20, "B 的組合", &["FSRCNNX.glsl"]));
        b.video.shaders.presets[0].name = "改名".into();
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(names(&back), ["改名", "組合 1", "B 的組合"]);
        assert_eq!(back.video.shaders.presets[1].files, ["Anime4K.glsl"]);
        assert_eq!(back.video.shaders.active, Some(10), "A 用的組合還在，照樣使用中");
        // A 沒再改組合、只改了別的設定：存檔時不會把 B 改的蓋回去
        a.volume = 50.0;
        a.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(names(&back), ["改名", "組合 1", "B 的組合"]);
        assert_eq!(back.volume, 50.0);
        // B 刪掉兩邊都有的那一個、A 改了自己組合裡的檔案：兩個都生效
        b.video.shaders.presets.retain(|p| p.id != 1);
        b.save().unwrap();
        a.video.shaders.presets[1].files.push("Anime4K_2.glsl".into());
        a.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(names(&back), ["組合 1", "B 的組合"]);
        assert_eq!(back.video.shaders.presets[0].files, ["Anime4K.glsl", "Anime4K_2.glsl"]);
        // 沒有編號的項目（不是這個程式寫的）：整個清單照一般的值合併
        let odd = serde_json::json!([{"name": "沒有編號"}]);
        assert_eq!(
            merge_by_id(odd.as_array().unwrap(), &[], &[]),
            None,
            "沒有編號就不逐項合併"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_windows_change_different_eq_bands() {
        let dir = temp_dir("eq-merge");
        let path = dir.join("settings.json");
        let mut a = Settings::load_from(path.clone());
        let mut b = Settings::load_from(path.clone());
        a.audio.eq.preset = crate::sound::EqPreset::Custom;
        a.audio.eq.gains[0] = 5.0;
        a.save().unwrap();
        b.audio.eq.preset = crate::sound::EqPreset::Custom;
        b.audio.eq.gains[9] = -3.0;
        b.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert_eq!(back.audio.eq.gains[0], 5.0, "A 調的 31 Hz 不能被 B 蓋回去");
        assert_eq!(back.audio.eq.gains[9], -3.0);
        assert_eq!(back.audio.eq.preset, crate::sound::EqPreset::Custom);
        // 兩個視窗調同一段：後存的為準
        a.audio.eq.gains[0] = 2.0;
        a.save().unwrap();
        b.audio.eq.gains[0] = -1.0;
        b.save().unwrap();
        let back = Settings::load_from(path);
        assert_eq!(back.audio.eq.gains[0], -1.0);
        assert_eq!(back.audio.eq.gains[9], -3.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitized_clamps() {
        use crate::picture::ShaderPreset;
        let mut s = Settings::default();
        s.video.adjust.brightness = 150;
        s.video.adjust.gamma = -500;
        s.video.adjust.hue = 7;
        s.video.tone.target_peak = Some(5000);
        s.audio.eq.gains = [20.0, -20.0, 3.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -12.0];
        s.audio.volume_max = 170;
        s.volume = 180.0;
        let preset = |id, name: &str| ShaderPreset {
            id,
            name: name.into(),
            files: Vec::new(),
        };
        s.video.shaders.presets = vec![preset(7, "A"), preset(9, "B"), preset(7, "A 的重複")];
        s.video.shaders.active = Some(42);
        let s = s.sanitized();
        assert_eq!(
            (s.video.adjust.brightness, s.video.adjust.gamma, s.video.adjust.hue),
            (100, -100, 7)
        );
        assert_eq!(s.video.tone.target_peak, Some(203), "最多 203（vo_gpu 超過就裁切）");
        assert_eq!(&s.audio.eq.gains[..3], &[12.0, -12.0, 3.5]);
        assert_eq!(s.audio.eq.gains[9], -12.0);
        assert_eq!(s.audio.volume_max, 150);
        assert_eq!(s.volume, 150.0, "音量不超過音量上限");
        let names: Vec<&str> = s.video.shaders.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["A", "B"], "重複的編號只留第一個");
        assert_eq!(s.video.shaders.active, None, "不存在的組合不能是使用中");
        // 太小的目標亮度拉到 100；範圍內的值不動；存在的組合照樣使用中
        let mut ok = Settings::default();
        ok.video.tone.target_peak = Some(50);
        ok.video.shaders.presets = vec![preset(3, "C")];
        ok.video.shaders.active = Some(3);
        ok.volume = 80.0;
        let ok = ok.sanitized();
        assert_eq!(ok.video.tone.target_peak, Some(100));
        assert_eq!(ok.video.shaders.active, Some(3));
        assert_eq!(ok.volume, 80.0);
        assert_eq!(ok.audio.volume_max, 100);
        // 讀檔時就會整理
        let dir = temp_dir("sanitize-load");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"volume": 140.0, "video": {"adjust": {"contrast": -300}}}"#).unwrap();
        let loaded = Settings::load_from(path);
        assert_eq!(loaded.volume, 100.0);
        assert_eq!(loaded.video.adjust.contrast, -100);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn target_peak_above_203_loads_as_203() {
        // 開發版（批次 5 起）存得下 400、1000 nits，超過 203 時 vo_gpu 把亮部裁成白色：讀檔時拉回 203
        let dir = temp_dir("peak-clamp");
        let path = dir.join("settings.json");
        for (saved, loaded) in [(400, 203), (1000, 203), (203, 203), (150, 150), (100, 100), (50, 100)] {
            std::fs::write(
                &path,
                format!(r#"{{"volume": 70.0, "video": {{"tone": {{"target_peak": {saved}, "curve": "hable"}}}}}}"#),
            )
            .unwrap();
            let s = Settings::load_from(path.clone());
            assert_eq!(s.video.tone.target_peak, Some(loaded), "存的是 {saved}");
            assert_eq!(
                s.video.tone.curve,
                crate::picture::ToneCurve::Hable,
                "同一組的其他項目照讀"
            );
            assert_eq!(s.volume, 70.0);
        }
        // 一項讀不懂、走逐項讀取時也一樣會拉回（整理在所有讀檔方式之後）
        std::fs::write(
            &path,
            r#"{"video": {"deband": "ultra", "tone": {"target_peak": 1000}}}"#,
        )
        .unwrap();
        assert_eq!(Settings::load_from(path.clone()).video.tone.target_peak, Some(203));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gamut_desaturate_loads_as_auto_and_keeps_the_rest() {
        // 「降低飽和度」拿掉了（vo_gpu 跟自動是同一段程式）：開發版存的值讀成自動，畫質的其他設定不能跟著不見
        use crate::picture::{Gamut, Quality, Strength, ToneCurve};
        let dir = temp_dir("gamut-desaturate");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"volume": 55.0, "video": {"deband": "strong", "quality": "high", "sharpen": "light",
                "tone": {"gamut": "desaturate", "curve": "mobius", "target_peak": 150, "compute_peak": false}}}"#,
        )
        .unwrap();
        let mut s = Settings::load_from(path.clone());
        assert_eq!(s.video.tone.gamut, Gamut::Auto);
        assert_eq!(s.video.tone.curve, ToneCurve::Mobius);
        assert_eq!(s.video.tone.target_peak, Some(150));
        assert!(!s.video.tone.compute_peak);
        assert_eq!(s.video.deband, Strength::Strong);
        assert_eq!(s.video.sharpen, Strength::Light);
        assert_eq!(s.video.quality, Quality::High);
        assert_eq!(s.volume, 55.0);
        // 存檔寫的是新的名稱
        s.video.tone.gamut = Gamut::Clip;
        s.save().unwrap();
        s.video.tone.gamut = Gamut::Auto;
        s.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("desaturate"), "{text}");
        assert_eq!(Settings::load_from(path).video.tone.gamut, Gamut::Auto);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_settings_never_write_to_disk() {
        let mut s = Settings::default();
        assert!(s.path.is_none());
        s.save().unwrap();
    }

    #[test]
    fn subtitle_style_to_mpv_options() {
        let style = SubStyle {
            font: "  標楷體 ".into(),
            color: [255, 200, 0, 255],
            border_color: [0, 0, 0, 128],
            bold: true,
            override_ass: true,
            ..SubStyle::default()
        };
        let opts: std::collections::HashMap<_, _> = style.mpv_options().into_iter().collect();
        assert_eq!(opts["sub-font"], "標楷體");
        assert_eq!(opts["sub-color"], "#FFFFC800");
        assert_eq!(opts["sub-border-color"], "#80000000");
        assert_eq!(opts["sub-bold"], "yes");
        assert_eq!(opts["sub-ass-override"], "force");
        assert_eq!(opts["sub-font-size"], "42");
        // 沒指定字型：用作業系統的中文字型
        assert_eq!(SubStyle::default().effective_font(), SubStyle::font_choices()[0].0);
    }
}
