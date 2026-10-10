//! 縮圖總覽圖上的文字與畫布：用介面同一套字型（`fonts::definitions`）在 epaint 排版，
//! 再把每個字形從字型圖集貼到圖上（不用另外的繪圖套件）。
//!
//! - 排版：`with_pixels_per_point(1.0)`，單位就是像素；位置 = 起點 + 那一列的位置 + 字形的位置 + 字形圖的偏移，
//!   四捨五入到整數像素（跟 egui 畫字的規則一樣）。
//! - 圖集的每個像素是「白色 × 覆蓋率」（預乘），透明度就是覆蓋率：`底色 × (1 − a) + 字的顏色 × a`。
//! - 畫布是 RGB（每個像素 3 位元組）：最大的總覽圖約 6 千萬像素，不用透明度省下四分之一的記憶體。

use crate::screenshot::Image;
use eframe::egui::epaint::{Color32, FontId, Fonts, TextOptions};

/// RGB 畫布（由上而下，每列 `w * 3` 位元組）
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

impl Canvas {
    /// 整張塗成 `color`
    pub fn new(w: usize, h: usize, color: [u8; 3]) -> Canvas {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            rgb.extend_from_slice(&color);
        }
        Canvas { w, h, rgb }
    }

    /// 一個像素（超出範圍時 None）
    pub fn pixel(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        (x < self.w && y < self.h).then(|| {
            let i = (y * self.w + x) * 3;
            [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
        })
    }

    /// 把一個像素混上 `color`，透明度 `a`（0–1；超出畫布的不畫）
    fn blend(&mut self, x: i64, y: i64, color: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 3;
        let a = a.min(1.0);
        for (c, dst) in color.iter().zip(&mut self.rgb[i..i + 3]) {
            *dst = (f32::from(*dst) * (1.0 - a) + f32::from(*c) * a).round() as u8;
        }
    }

    /// 塗滿一個方塊（超出畫布的部分不畫）
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; 3]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                let i = (yy * self.w + xx) * 3;
                self.rgb[i..i + 3].copy_from_slice(&color);
            }
        }
    }

    /// 半透明的圓角方塊（`rgba` 的第 4 個是透明度；`radius` = 圓角的半徑，像素）
    pub fn blend_rounded(&mut self, x: i64, y: i64, w: i64, h: i64, rgba: [u8; 4], radius: f32) {
        let a = f32::from(rgba[3]) / 255.0;
        let color = [rgba[0], rgba[1], rgba[2]];
        let r = radius.max(0.0).min(w.min(h) as f32 / 2.0);
        for yy in 0..h {
            for xx in 0..w {
                // 圓角：離最近的角的圓心超過半徑的部分不畫（邊緣畫一半，看起來比較平滑）
                let cx = (xx as f32 + 0.5).clamp(r, w as f32 - r);
                let cy = (yy as f32 + 0.5).clamp(r, h as f32 - r);
                let d = ((xx as f32 + 0.5 - cx).powi(2) + (yy as f32 + 0.5 - cy).powi(2)).sqrt();
                let cover = (r - d + 0.5).clamp(0.0, 1.0);
                self.blend(x + xx, y + yy, color, a * cover);
            }
        }
    }

    /// 把 RGBA 圖貼在 (`x`, `y`)（不看透明度；超出畫布的部分不貼）
    pub fn blit(&mut self, x: usize, y: usize, img: &Image) {
        for yy in 0..img.h.min(self.h.saturating_sub(y)) {
            let w = img.w.min(self.w.saturating_sub(x));
            let src = &img.rgba[yy * img.w * 4..][..w * 4];
            let dst = &mut self.rgb[((y + yy) * self.w + x) * 3..][..w * 3];
            for (d, s) in dst.as_chunks_mut::<3>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                *d = [s[0], s[1], s[2]];
            }
        }
    }
}

/// 在畫布上寫字
pub struct TextPainter {
    fonts: Fonts,
}

impl Default for TextPainter {
    fn default() -> Self {
        Self::new()
    }
}

/// 字型圖集的邊長上限（標頭的中日韓字約 40 像素，一張圖用到的字不會超過）
const ATLAS_SIDE: usize = 4096;

impl TextPainter {
    pub fn new() -> TextPainter {
        TextPainter {
            fonts: Fonts::new(Self::options(), crate::fonts::definitions()),
        }
    }

    fn options() -> TextOptions {
        TextOptions {
            max_texture_side: ATLAS_SIDE,
            ..Default::default()
        }
    }

    /// 一行字的大小（寬、高，像素）：字的大小 `px`，不換行
    pub fn measure(&mut self, text: &str, px: f32) -> (f32, f32) {
        let galley = self.fonts.with_pixels_per_point(1.0).layout_no_wrap(
            text.to_owned(),
            FontId::proportional(px),
            Color32::WHITE,
        );
        let size = galley.size();
        (size.x, size.y)
    }

    /// 放不下 `max_w` 像素時從後面截掉，加上「…」（放得下時照原樣）
    pub fn fit(&mut self, text: &str, px: f32, max_w: f32) -> String {
        if self.measure(text, px).0 <= max_w {
            return text.to_owned();
        }
        let chars: Vec<char> = text.chars().collect();
        // 二分搜尋留幾個字
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let candidate: String = chars[..mid].iter().collect::<String>() + "…";
            if self.measure(&candidate, px).0 <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        chars[..lo].iter().collect::<String>() + "…"
    }

    /// 寫一行字：(`x`, `y`) 是字的方塊的左上角（跟 `measure` 的大小一起用），顏色 `color`
    pub fn draw(&mut self, canvas: &mut Canvas, x: f32, y: f32, text: &str, px: f32, color: [u8; 3]) {
        // 圖集快滿時重建（一張圖用不到那麼多字，保險而已）；排版的快取也清掉
        self.fonts.begin_pass(Self::options());
        let galley = self.fonts.with_pixels_per_point(1.0).layout_no_wrap(
            text.to_owned(),
            FontId::proportional(px),
            Color32::WHITE,
        );
        let atlas = self.fonts.texture_atlas().image();
        let atlas_w = atlas.size[0];
        for row in &galley.rows {
            for glyph in &row.row.glyphs {
                let uv = glyph.uv_rect;
                if uv.is_nothing() {
                    continue;
                }
                let left = (x + row.pos.x + glyph.pos.x + uv.offset.x).round() as i64;
                let top = (y + row.pos.y + glyph.pos.y + uv.offset.y).round() as i64;
                let (u0, v0) = (usize::from(uv.min[0]), usize::from(uv.min[1]));
                let (u1, v1) = (usize::from(uv.max[0]), usize::from(uv.max[1]));
                for v in v0..v1 {
                    for u in u0..u1 {
                        let a = atlas.pixels[v * atlas_w + u].a();
                        if a > 0 {
                            canvas.blend(
                                left + (u - u0) as i64,
                                top + (v - v0) as i64,
                                color,
                                f32::from(a) / 255.0,
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 方塊裡、外有沒有被畫到的像素（底色是黑的）
    fn lit(c: &Canvas, x0: usize, y0: usize, x1: usize, y1: usize) -> (usize, usize) {
        let (mut inside, mut outside) = (0, 0);
        for y in 0..c.h {
            for x in 0..c.w {
                if c.pixel(x, y).unwrap() != [0, 0, 0] {
                    if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                        inside += 1;
                    } else {
                        outside += 1;
                    }
                }
            }
        }
        (inside, outside)
    }

    #[test]
    fn draws_digits() {
        let mut t = TextPainter::new();
        let (w, h) = t.measure("12:34", 20.0);
        assert!(w > 20.0 && h >= 20.0, "{w}×{h}");
        let mut c = Canvas::new(120, 60, [0, 0, 0]);
        t.draw(&mut c, 10.0, 10.0, "12:34", 20.0, [255, 255, 255]);
        // 字在量出來的方塊裡（寬鬆 1 像素：四捨五入），外面一個像素都沒畫到
        let (x1, y1) = ((10.0 + w).ceil() as usize + 1, (10.0 + h).ceil() as usize + 1);
        let (inside, outside) = lit(&c, 9, 9, x1, y1);
        assert!(inside > 30, "方塊裡只畫了 {inside} 個像素");
        assert_eq!(outside, 0, "畫到方塊外面了");
        // 有完全不透明的筆畫（不是糊成一片灰）
        assert!(c.rgb.contains(&255));
    }

    #[test]
    fn measure_grows() {
        let mut t = TextPainter::new();
        let (w1, h1) = t.measure("00:01", 14.0);
        let (w2, _) = t.measure("00:01:00", 14.0);
        let (w3, h3) = t.measure("00:01", 28.0);
        assert!(w2 > w1, "字多的比較寬：{w1} {w2}");
        assert!(w3 > w1 * 1.8 && h3 > h1 * 1.8, "字大的比較大：{w1}×{h1} {w3}×{h3}");
        assert_eq!(t.measure("", 14.0).0, 0.0);
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        let mut t = TextPainter::new();
        let long = "a_very_long_file_name_that_does_not_fit.mkv";
        let full = t.measure(long, 16.0).0;
        assert_eq!(t.fit(long, 16.0, full + 1.0), long, "放得下時照原樣");
        let cut = t.fit(long, 16.0, full / 2.0);
        assert!(cut.ends_with('…') && cut.len() < long.len(), "{cut}");
        assert!(t.measure(&cut, 16.0).0 <= full / 2.0);
        assert!(long.starts_with(cut.trim_end_matches('…')));
        // 一個字都放不下：只剩「…」
        assert_eq!(t.fit(long, 16.0, 1.0), "…");
    }

    #[test]
    fn cjk_glyphs_when_font_present() {
        if !crate::fonts::has_cjk() {
            eprintln!("略過：這台電腦沒有中日韓字型");
            return;
        }
        let mut t = TextPainter::new();
        let mut c = Canvas::new(200, 60, [0, 0, 0]);
        t.draw(&mut c, 4.0, 4.0, "第1集", 24.0, [230, 230, 230]);
        let (w, _) = t.measure("第1集", 24.0);
        // 有中文字型時是真的字（不是缺字的方塊「◻」：方塊也差不多一個字寬，看寬度分不出來）：
        // 排版用的字型族裡要有這些字
        assert!(t.fonts.has_glyphs(&FontId::proportional(24.0), "第集時間標記"));
        assert!(w > 50.0, "{w}");
        let (inside, outside) = lit(&c, 3, 3, (5.0 + w).ceil() as usize + 1, 60);
        assert!(inside > 80 && outside == 0, "{inside} {outside}");
    }

    #[test]
    fn canvas_blit_fill_and_rounded_box() {
        let mut c = Canvas::new(10, 8, [20, 20, 20]);
        let img = Image {
            w: 4,
            h: 3,
            rgba: [[200u8, 100, 50, 7]; 12].concat(),
        };
        // 貼到右下角：超出的部分不貼，不當掉
        c.blit(8, 6, &img);
        assert_eq!(c.pixel(9, 7), Some([200, 100, 50]));
        assert_eq!(c.pixel(7, 7), Some([20, 20, 20]));
        c.fill_rect(0, 0, 2, 2, [1, 2, 3]);
        assert_eq!(c.pixel(1, 1), Some([1, 2, 3]));
        assert_eq!(c.pixel(2, 2), Some([20, 20, 20]));
        // 半透明的黑色圓角方塊：中間變暗，角落沒畫
        let mut c = Canvas::new(20, 20, [200, 200, 200]);
        c.blend_rounded(0, 0, 20, 20, [0, 0, 0, 128], 6.0);
        assert!(c.pixel(10, 10).unwrap()[0] < 110);
        assert_eq!(c.pixel(0, 0), Some([200, 200, 200]));
        assert_eq!(c.pixel(19, 19), Some([200, 200, 200]));
    }
}
