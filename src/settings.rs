//! 使用者設定，存成 JSON：
//! - Windows：`%APPDATA%\Vitascope\settings.json`
//! - macOS：`~/Library/Application Support/Vitascope/settings.json`
//! - Linux：`$XDG_CONFIG_HOME/vitascope/settings.json`（預設 `~/.config/vitascope/`）
//!
//! 視窗位置大小也自己存，不用 eframe 的機制：eframe 會連「全螢幕」一起記住，
//! 下次啟動直接全螢幕，跟一般播放器的習慣不同。

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
    /// 視窗置頂（蓋在其他視窗上面）
    pub always_on_top: bool,
    /// 顯示播放清單面板
    pub show_playlist: bool,
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
            always_on_top: false,
            show_playlist: false,
            screenshot_dir: None,
            screenshot_subtitles: true,
            language: crate::i18n::Lang::default(),
            hwdec: true,
            seek_short: 5.0,
            seek_long: 30.0,
            single_instance: true,
            file_associations: false,
            path: None,
            baseline: None,
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
            .unwrap_or_default();
        settings.baseline = serde_json::to_value(&settings).ok();
        settings.path = Some(path);
        settings
    }

    /// 讀不懂的設定（例如新版加的語言、手動改錯）只有那一項用預設值，其他設定照樣讀進來
    fn from_value_lenient(value: serde_json::Value) -> Self {
        if let Ok(s) = serde_json::from_value(value.clone()) {
            return s;
        }
        let serde_json::Value::Object(fields) = value else {
            return Self::default();
        };
        let Ok(serde_json::Value::Object(mut good)) = serde_json::to_value(Self::default()) else {
            return Self::default();
        };
        for (key, v) in fields {
            let mut trial = good.clone();
            trial.insert(key, v);
            let trial = serde_json::Value::Object(trial);
            if serde_json::from_value::<Self>(trial.clone()).is_ok()
                && let serde_json::Value::Object(t) = trial
            {
                good = t;
            }
        }
        serde_json::from_value(serde_json::Value::Object(good)).unwrap_or_default()
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
        let merged = match (&self.baseline, on_disk) {
            (Some(base), Some(disk)) => merge(&current, base, disk),
            _ => current.clone(),
        };
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

/// 三方合併：`now` 跟 `base` 不同的地方寫進 `disk`，其他的保留 `disk` 的。
/// 物件（例如字幕外觀）逐項合併：兩個視窗各改了一項，兩項都留下
fn merge(now: &serde_json::Value, base: &serde_json::Value, disk: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match (now, base, disk) {
        (Value::Object(now), Value::Object(base), Value::Object(mut disk)) => {
            for (key, value) in now {
                let merged = match (base.get(key), disk.remove(key)) {
                    (Some(b), Some(d)) => merge(value, b, d),
                    // 檔案裡沒有（舊版的設定檔）、或上次讀的時候沒有：用現在的
                    _ => value.clone(),
                };
                disk.insert(key.clone(), merged);
            }
            Value::Object(disk)
        }
        (now, base, disk) => {
            if now != base {
                now.clone()
            } else {
                disk
            }
        }
    }
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
        s.always_on_top = true;
        s.show_playlist = true;
        s.volume = 42.0;
        s.save().unwrap();
        let back = Settings::load_from(path.clone());
        assert!(back.always_on_top && back.show_playlist);
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
