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
    /// 每一輪都要求重畫（模擬滑鼠移過按鈕的動畫，見 `busy_ui`）
    busy: bool,
    /// 開始播放後第幾秒讓介面卡住多久（模擬開著對話框、拖動視窗；見 `stall_at`）
    stall: Option<(Duration, Duration)>,
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

/// `VITASCOPE_TEST_STALL=3,10`：開始播放後第 3 秒讓介面執行緒停 10 秒。兩個數字都要有、不能是負的，停的時間要大於 0；
/// 看不懂就是不停（None）
pub fn parse_stall(value: &str) -> Option<(Duration, Duration)> {
    let mut it = value.split(',').map(|t| t.trim().parse::<f64>().ok());
    let (Some(Some(at)), Some(Some(len)), None) = (it.next(), it.next(), it.next()) else {
        return None;
    };
    (at.is_finite() && len.is_finite() && at >= 0.0 && len > 0.0)
        .then(|| (Duration::from_secs_f64(at), Duration::from_secs_f64(len)))
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
            busy: false,
            stall: None,
        }
    }

    /// 開始播放後 `at` 讓介面執行緒停 `len`（像以前開著檔案對話框、拖曳視窗；見 `parse_stall`）
    pub fn stall_at(mut self, stall: Option<(Duration, Duration)>) -> Self {
        self.stall = stall;
        self
    }

    /// `VITASCOPE_TEST_BUSY_UI=1`（流暢播放的實機測試）：介面一直在重畫（每次螢幕更新一輪），
    /// 像滑鼠在視窗上移動、按鈕的動畫；比真的移動滑鼠穩定
    pub fn busy_ui(mut self, on: bool) -> Self {
        self.busy = on;
        self
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
        if self.busy {
            ctx.request_repaint();
        }
        let Some(start) = self.start else { return false };
        if !self.requested {
            let elapsed = start.elapsed();
            if let Some((at, len)) = self.stall
                && elapsed >= at
            {
                self.stall = None;
                eprintln!(
                    "[vitascope] 測試：介面停 {:.1} 秒（開始播放後 {:.1} 秒）",
                    len.as_secs_f64(),
                    elapsed.as_secs_f64()
                );
                std::thread::sleep(len);
                // 實機測試從這一行之後算追上聲音花了多久
                eprintln!("[vitascope] 測試：介面恢復");
            }
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
            // 下一件事（縮小／還原、停住、截圖）的時間到了要醒來（暫停中介面不會自己重畫）
            let next = self
                .minimize
                .get(self.minimize_done)
                .copied()
                .into_iter()
                .chain(self.stall.map(|(at, _)| at))
                .fold(self.delay, Duration::min);
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
            // CI 用這行判斷畫面是否真的畫出來（全黑代表 OpenGL / 影片渲染失敗）；
            // 中央的平均亮度給影像調整的截圖檢查用（亮度 +50 要比 0 亮）
            eprintln!(
                "[vitascope] 截圖統計：非黑色像素 {:.1}%，中央平均亮度 {:.1}",
                non_black_percent(&image),
                center_luma(&image)
            );
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

/// 畫面中央（寬、高各取中間一半）的平均亮度（Rec.709 的 Y，0–255）。
/// 只看中央：上方的提示文字、下方的控制列不算進去
fn center_luma(image: &egui::ColorImage) -> f64 {
    let [w, h] = image.size;
    let (xs, ys) = (w / 4..w - w / 4, h / 4..h - h / 4);
    let (mut sum, mut n) = (0.0, 0usize);
    for y in ys {
        for x in xs.clone() {
            let c = image.pixels[y * w + x];
            sum += 0.2126 * f64::from(c.r()) + 0.7152 * f64::from(c.g()) + 0.0722 * f64::from(c.b());
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
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

    #[test]
    fn stall_hook_parsing() {
        let s = Duration::from_secs_f64;
        assert_eq!(parse_stall("3,10"), Some((s(3.0), s(10.0))));
        assert_eq!(parse_stall(" 0 , 0.5 "), Some((s(0.0), s(0.5))));
        for bad in [
            "", "3", "3,", "x,10", "3,x", "-1,10", "3,0", "3,-2", "inf,1", "3,NaN", "3,10,5",
        ] {
            assert_eq!(parse_stall(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn center_luma_ignores_the_edges() {
        // 4×4：中央 2×2 是白色，外圈是黑色
        let mut image = egui::ColorImage::filled([4, 4], egui::Color32::BLACK);
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            image.pixels[y * 4 + x] = egui::Color32::WHITE;
        }
        assert!((center_luma(&image) - 255.0).abs() < 1e-6);
        // 純綠色的亮度比純藍色高很多（人眼的感受）
        let green = egui::ColorImage::filled([4, 4], egui::Color32::from_rgb(0, 255, 0));
        let blue = egui::ColorImage::filled([4, 4], egui::Color32::from_rgb(0, 0, 255));
        assert!(center_luma(&green) > 5.0 * center_luma(&blue));
        assert_eq!(
            center_luma(&egui::ColorImage::filled([0, 0], egui::Color32::BLACK)),
            0.0
        );
    }

    #[test]
    fn busy_ui_repaints_every_pass() {
        // 跑幾輪（egui 一開始會自己多畫幾輪），最後一輪之後還有沒有要求重畫
        let repaints = |shot: AutoShot| {
            let ctx = egui::Context::default();
            let mut shot = shot;
            for _ in 0..5 {
                let mut out = ctx.run_ui(Default::default(), |ui| {
                    shot.tick(ui.ctx());
                });
                // 沒有真的畫：字型貼圖的更新直接丟掉
                out.textures_delta.clear();
            }
            ctx.has_requested_repaint_for(&egui::ViewportId::ROOT)
        };
        let shot = || AutoShot::new(PathBuf::from("x.png"), Duration::from_secs(5));
        // 一般的自動截圖：還沒開始播放時不要求重畫
        assert!(!repaints(shot()));
        // VITASCOPE_TEST_BUSY_UI=1：每一輪都要求重畫
        assert!(repaints(shot().busy_ui(true)));
    }
}
