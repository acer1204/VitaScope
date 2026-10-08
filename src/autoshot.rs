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
    /// 開始播放後這些時間依序縮到最小、還原（流暢播放的實機測試用，見 `parse_minimize`）
    minimize: Vec<Duration>,
    /// 已經做了幾個
    minimize_done: usize,
}

/// `VITASCOPE_TEST_MINIMIZE=3,6`：開始播放後第 3 秒縮到最小、第 6 秒還原（可以再接下去，輪流）。
/// 看不懂的部分略過
pub fn parse_minimize(value: &str) -> Vec<Duration> {
    let mut times: Vec<Duration> = value
        .split(',')
        .filter_map(|t| t.trim().parse::<f64>().ok())
        .filter(|t| t.is_finite() && *t >= 0.0)
        .map(Duration::from_secs_f64)
        .collect();
    times.sort();
    times
}

impl AutoShot {
    pub fn new(path: PathBuf, delay: Duration) -> Self {
        Self {
            path,
            delay,
            start: None,
            requested: false,
            minimize: Vec::new(),
            minimize_done: 0,
        }
    }

    /// 截圖前在這些時間（開始播放後）輪流縮到最小、還原
    pub fn minimize_at(mut self, times: Vec<Duration>) -> Self {
        self.minimize = times;
        self
    }

    /// 開始計時（播放開始、或確定沒有要開檔時呼叫）
    pub fn arm(&mut self) {
        self.start.get_or_insert_with(Instant::now);
    }

    /// 每一幀呼叫：時間到就要求截圖，收到截圖就存檔並關閉視窗。要求截圖的那一幀回傳 true
    pub fn tick(&mut self, ctx: &egui::Context) -> bool {
        let Some(start) = self.start else { return false };
        if !self.requested {
            let elapsed = start.elapsed();
            // 依序縮到最小、還原（第 1、3… 個時間縮小，第 2、4… 個還原）
            let before = self.minimize_done;
            self.minimize_done += self.minimize[before..].iter().take_while(|t| **t <= elapsed).count();
            if self.minimize_done > before {
                let minimized = self.minimize_done % 2 == 1;
                eprintln!(
                    "[vitascope] 測試：{}（開始播放後 {:.1} 秒）",
                    if minimized { "縮到最小" } else { "還原視窗" },
                    elapsed.as_secs_f64()
                );
                ctx.send_viewport_cmd(ViewportCommand::Minimized(minimized));
            }
            if elapsed >= self.delay {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                self.requested = true;
                return true;
            }
            let next = self
                .minimize
                .get(self.minimize_done)
                .map_or(self.delay, |t| (*t).min(self.delay));
            ctx.request_repaint_after(next.saturating_sub(elapsed));
            return false;
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
            // CI 用這行判斷畫面是否真的畫出來（全黑代表 OpenGL / 影片渲染失敗）
            eprintln!("[vitascope] 截圖統計：非黑色像素 {:.1}%", non_black_percent(&image));
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            ctx.request_repaint();
        }
        false
    }
}

/// 非黑色像素（任一色版 > 16）的比例
fn non_black_percent(image: &egui::ColorImage) -> f64 {
    if image.pixels.is_empty() {
        return 0.0;
    }
    let lit = image
        .pixels
        .iter()
        .filter(|c| c.r() > 16 || c.g() > 16 || c.b() > 16)
        .count();
    lit as f64 * 100.0 / image.pixels.len() as f64
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimize_times() {
        let s = Duration::from_secs_f64;
        assert_eq!(parse_minimize("3,6"), vec![s(3.0), s(6.0)]);
        assert_eq!(parse_minimize(" 6 , 3,x,-1,1.5"), vec![s(1.5), s(3.0), s(6.0)]);
        assert!(parse_minimize("").is_empty());
    }
}
