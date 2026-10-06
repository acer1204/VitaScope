//! 進度條預覽縮圖的介面部分：要縮圖、收縮圖、快取材質、畫在時間標籤上面。產生縮圖見 `thumbs.rs`。

use super::VitascopeApp;
use crate::thumbs::{self, Thumbnailer};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, TextureHandle, TextureOptions, vec2};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// 新的縮圖多久還沒好，就不再拿上一張頂著（避免顯示錯的時間的畫面）
const STALE_AFTER: Duration = Duration::from_secs(3);

/// 最多留幾張縮圖的材質（一張 240×135 約 130 KB）
const CACHE: usize = 128;
/// 縮圖最寬顯示多少點
const MAX_WIDTH: f32 = 200.0;

/// 這個檔案已經做好的縮圖
#[derive(Default)]
pub(super) struct PreviewCache {
    file: u64,
    textures: HashMap<u32, TextureHandle>,
    /// 最近用過的在後面（超過上限時刪最前面的）
    order: VecDeque<u32>,
    /// 上一次畫的區段（新的還沒好時先顯示舊的，不會一直閃）
    last: Option<u32>,
    /// 上一次要的區段與時間（同一個不重複要求；等太久就不再顯示舊的）
    requested: Option<(u32, Instant)>,
}

impl PreviewCache {
    fn reset(&mut self, file: u64) {
        if self.file != file {
            *self = Self {
                file,
                ..Self::default()
            };
        }
    }

    fn touch(&mut self, bucket: u32) {
        self.order.retain(|b| *b != bucket);
        self.order.push_back(bucket);
    }
}

impl VitascopeApp {
    /// 這個檔案能不能做縮圖：要有影像（不是專輯封面）、本機或網路磁碟上的檔案（串流不做，會多連一條線）
    fn preview_allowed(&self) -> bool {
        let st = &self.player.state;
        // 本機的 HLS .m3u8 也是串流（片段在網路上）
        st.loaded
            && st.has_video()
            && self.autoshot.is_none()
            && st
                .path
                .as_deref()
                .is_some_and(|p| !super::is_url(p) && !crate::formats::is_playlist(std::path::Path::new(p)))
    }

    /// 新檔案載入了：縮圖產生器也換檔（還沒建立就等第一次停在進度條上再說）
    pub(super) fn preview_file_loaded(&mut self) {
        if let Some(t) = &self.thumbs
            && self.preview_allowed()
            && let Some(path) = self.player.state.path.clone()
        {
            t.open(self.file_gen, &path);
        }
    }

    /// 收做好的縮圖，變成材質
    pub(super) fn poll_previews(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.thumbs else { return };
        let mut got = Vec::new();
        while let Some(thumb) = t.try_recv() {
            got.push(thumb);
        }
        for thumb in got {
            self.preview.reset(self.file_gen);
            if thumb.file != self.file_gen {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([thumb.w, thumb.h], &thumb.rgba);
            let texture = ctx.load_texture(format!("preview-{}", thumb.bucket), image, TextureOptions::LINEAR);
            self.preview.textures.insert(thumb.bucket, texture);
            self.preview.touch(thumb.bucket);
            while self.preview.order.len() > CACHE {
                if let Some(old) = self.preview.order.pop_front() {
                    self.preview.textures.remove(&old);
                }
            }
        }
    }

    /// 滑鼠停在進度條的 `time`：要縮圖，畫在 `label_top`（時間標籤的上緣）上面、以 `x` 為中心
    pub(super) fn paint_preview(&mut self, ctx: &egui::Context, time: f64, x: f32, label_top: f32, bar: Rect) {
        if !self.preview_allowed() {
            return;
        }
        let Some(duration) = self.player.state.duration.filter(|d| *d > 0.0) else {
            return;
        };
        self.preview.reset(self.file_gen);
        // 第一次用到才建立（另一個 mpv，約 10 毫秒）
        if self.thumbs.is_none() {
            let repaint_ctx = ctx.clone();
            let t = Thumbnailer::new(move || repaint_ctx.request_repaint());
            if let Some(path) = &self.player.state.path {
                t.open(self.file_gen, path);
            }
            self.thumbs = Some(t);
        }
        let (bucket, target) = thumbs::bucket_of(time, duration);
        let ready = self.preview.textures.contains_key(&bucket);
        // 每一幀都重送同一個要求的話，做好之後會再做一次
        let new_request = self.preview.requested.is_none_or(|(b, _)| b != bucket);
        if !ready
            && new_request
            && let Some(t) = &self.thumbs
        {
            t.request(self.file_gen, bucket, target);
            self.preview.requested = Some((bucket, Instant::now()));
        }
        // 這個區段還沒好：先畫上一張（滑鼠移動中不會一直閃）；等太久就不畫（不要一直顯示錯的時間的畫面）
        let waited_long = self
            .preview
            .requested
            .is_some_and(|(b, at)| b == bucket && at.elapsed() > STALE_AFTER);
        let shown = if ready {
            Some(bucket)
        } else if waited_long {
            None
        } else {
            self.preview.last
        };
        let Some(texture) = shown.and_then(|b| self.preview.textures.get(&b)).cloned() else {
            return;
        };
        if let Some(b) = shown {
            self.preview.last = Some(b);
            self.preview.touch(b);
        }
        let [w, h] = texture.size();
        let ppp = ctx.pixels_per_point();
        let mut size = vec2(w as f32, h as f32) / ppp;
        if size.x > MAX_WIDTH {
            size *= MAX_WIDTH / size.x;
        }
        let x = x.clamp(bar.left() + size.x / 2.0 + 2.0, bar.right() - size.x / 2.0 - 2.0);
        let rect = Rect::from_min_size(Pos2::new(x - size.x / 2.0, label_top - size.y - 6.0), size);
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("seek_preview"));
        let painter = ctx.layer_painter(layer);
        painter.rect_filled(rect.expand(2.0), CornerRadius::same(4), Color32::from_black_alpha(220));
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// 測試用：上一次畫出來的縮圖（區段, 像素大小）
    #[doc(hidden)]
    pub fn preview_shown(&self) -> Option<(u32, [usize; 2])> {
        let b = self.preview.last?;
        Some((b, self.preview.textures.get(&b)?.size()))
    }
}
