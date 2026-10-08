//! 媒體資訊面板（Ctrl+F1 / Ctrl+I）：疊在影片左上角，打開時每秒更新一次。資料見 `mediainfo.rs`。

use super::VitascopeApp;
use crate::mediainfo::{self, LiveStats, MediaInfo};
use eframe::egui::{self, Color32, FontId, Id, Rect, RichText, vec2};
use std::time::{Duration, Instant};

/// 多久更新一次（跟 mpv 自己的 stats.lua 一樣）
const REFRESH: Duration = Duration::from_secs(1);

/// 面板的資料與讀取時間
pub(super) struct InfoCache {
    info: MediaInfo,
    live: LiveStats,
    read_at: Instant,
    /// 讀的時候的介面語言（換語言時重讀，裡面的說明文字才會跟著換）
    lang: crate::i18n::Lang,
}

impl VitascopeApp {
    pub(super) fn toggle_info(&mut self) {
        self.info_open = !self.info_open;
        self.info_cache = None;
        if self.info_open {
            // 裝置清單第一次查要十幾毫秒，面板打開時查一次就好
            self.audio_device = mediainfo::audio_device_name(&self.player);
        }
    }

    /// 目前檔案的資訊（面板關著時也能用，例如「複製媒體資訊」）
    pub(super) fn info_sections(&mut self) -> Vec<mediainfo::Section> {
        let lang = crate::i18n::lang();
        let lang_changed = self.info_cache.as_ref().is_some_and(|c| c.lang != lang);
        if lang_changed && self.audio_device.is_some() {
            // 「預設裝置」也要換成新的語言
            self.audio_device = mediainfo::audio_device_name(&self.player);
        }
        let stale = lang_changed || self.info_cache.as_ref().is_none_or(|c| c.read_at.elapsed() >= REFRESH);
        if stale {
            let live = mediainfo::read_live(&self.player);
            // 「使用中」的說明（每格幾次更新、影片快多少）也用這次讀到的數字
            self.set_sync_numbers(&live);
            self.info_cache = Some(InfoCache {
                info: mediainfo::read(&self.player, self.audio_device.clone()),
                live,
                read_at: Instant::now(),
                lang,
            });
        }
        let cache = self.info_cache.as_ref().expect("剛讀過");
        let mut sections = mediainfo::sections(&cache.info, &cache.live);
        // 螢幕更新率、電源、流暢播放的狀態（跟著每一幀更新，不用快取），加上 mpv 的顯示同步數字（每秒讀一次）
        let mut smooth = self.pacing_status().info_lines();
        smooth.extend(mediainfo::sync_lines(&cache.live));
        sections.push((crate::tr!("播放流暢度", "Smoothness"), smooth));
        sections
    }

    /// 畫在影片畫面的左上角（不接收滑鼠，點下去還是暫停 / 播放）
    pub(super) fn paint_info(&mut self, ctx: &egui::Context, rect: Rect) {
        if !self.info_open || !self.player.state.loaded {
            return;
        }
        let sections = self.info_sections();
        ctx.request_repaint_after(REFRESH);
        egui::Area::new(Id::new("media_info"))
            .fixed_pos(rect.left_top() + vec2(12.0, 12.0))
            .interactable(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_black_alpha(190))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_max_width((rect.width() - 48.0).max(200.0));
                        let font = FontId::proportional(13.0);
                        egui::Grid::new("media_info_grid")
                            .num_columns(2)
                            .spacing(vec2(12.0, 2.0))
                            .show(ui, |ui| {
                                for (title, lines) in &sections {
                                    for (i, line) in lines.iter().enumerate() {
                                        let head = if i == 0 { *title } else { "" };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(head).font(font.clone()).color(super::ACCENT),
                                            )
                                            .selectable(false),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line).font(font.clone()).color(Color32::from_gray(230)),
                                            )
                                            .selectable(false)
                                            .wrap(),
                                        );
                                        ui.end_row();
                                    }
                                }
                            });
                    });
            });
    }

    /// 「複製媒體資訊」：純文字放到剪貼簿（回報問題時好用）
    pub(super) fn copy_info(&mut self, ctx: &egui::Context) {
        if !self.player.state.loaded {
            return;
        }
        self.info_cache = None;
        let text = mediainfo::to_text(&self.info_sections());
        ctx.copy_text(text);
        self.osd(crate::tr!("已複製媒體資訊", "Media info copied"));
    }
}
