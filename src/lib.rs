//! 影戲 VitaScope — 跨平台影片播放器。
//!
//! - [`mpv`]：libmpv 的安全包裝
//! - [`player`]：播放器核心（狀態、事件、操作），介面與自動測試共用
//! - [`app`]：播放器視窗（egui）
//! - [`video`]：把 mpv 的畫面畫進 egui

pub mod app;
pub mod autoshot;
pub mod fonts;
pub mod formats;
pub mod history;
pub mod mpv;
pub mod player;
pub mod playlist;
pub mod settings;
pub mod subs;
pub mod update;
pub mod video;
