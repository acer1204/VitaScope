//! 影戲 VitaScope — 跨平台影片播放器。
//!
//! - [`mpv`]：libmpv 的安全包裝
//! - [`player`]：播放器核心（狀態、事件、操作），介面與自動測試共用
//! - [`app`]：播放器視窗（egui）
//! - [`video`]：把 mpv 的畫面畫進 egui

pub mod app;
pub mod assoc;
pub mod autoshot;
pub mod fonts;
pub mod formats;
pub mod geometry;
pub mod history;
pub mod i18n;
pub mod icon;
pub mod instance;
pub mod m3u;
#[cfg(target_os = "macos")]
pub mod macos_open;
pub mod mediainfo;
pub mod mpv;
pub mod pacing;
pub mod picture;
pub mod player;
pub mod playlist;
pub mod power;
pub mod screens;
pub mod screenshot;
pub mod settings;
pub mod sound;
pub mod subs;
pub mod syscmd;
pub mod theme;
pub mod thumbs;
pub mod update;
pub mod video;
