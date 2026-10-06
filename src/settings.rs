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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 100.0,
            muted: false,
            window: None,
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

    /// 讀取設定；檔案不存在或格式錯誤就用預設值
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("找不到設定資料夾"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 先寫暫存檔再改名，中途當掉也不會留下寫一半的設定檔
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}

fn config_dir() -> Option<PathBuf> {
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
    }
}
