//! 自動測試共用的工具：每個測試程式各自 `mod support;`（用不到的部分不算警告）

#![allow(dead_code)]

pub mod fake_inhibit;
pub mod fake_media;
pub mod fake_power;
pub mod fake_ytdl;
pub mod http;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod private_bus;
pub mod release;
