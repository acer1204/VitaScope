//! 開發用：自動截圖後關閉（`--shot 輸出.png`）。
//!
//! 截的是 egui 實際畫出來的整個視窗，所以能驗證 mpv → FBO → 視窗這條渲染路徑，
//! 不需要外部工具控制螢幕；之後 CI 的介面測試也用這個。

use eframe::egui::{self, ViewportCommand};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct AutoShot {
    path: PathBuf,
    /// 開始播放（或沒開檔時視窗出現）後等多久再截
    delay: Duration,
    start: Option<Instant>,
    requested: bool,
}

impl AutoShot {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        Self {
            path,
            delay,
            start: None,
            requested: false,
        }
    }

    /// 開始計時（播放開始、或確定沒有要開檔時呼叫）
    pub fn arm(&mut self) {
        self.start.get_or_insert_with(Instant::now);
    }

    /// 每一幀呼叫：時間到就要求截圖，收到截圖就存檔並關閉視窗
    pub fn tick(&mut self, ctx: &egui::Context) {
        let Some(start) = self.start else { return };
        if !self.requested {
            let elapsed = start.elapsed();
            if elapsed >= self.delay {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                self.requested = true;
            } else {
                ctx.request_repaint_after(self.delay - elapsed);
            }
            return;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            match save_png(&self.path, &image) {
                Ok(()) => eprintln!("[vitascope] 截圖已存到 {}", self.path.display()),
                Err(e) => eprintln!("[vitascope] 截圖存檔失敗：{e}"),
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            ctx.request_repaint();
        }
    }
}

fn save_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let [w, h] = image.size;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())
}
