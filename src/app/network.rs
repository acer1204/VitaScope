//! 網路（開啟網址）：
//! - 設定的套用：HLS / DASH 畫質、重新連線、快取、逾時、憑證、標頭、proxy。對應到哪些 mpv 選項由 `net::mpv_options` 決定；
//!   啟動時同步設定（第一個網址就生效），之後非同步、只送有變的。
//! - 「開啟網址」對話框（Ctrl+U、右鍵選單、🗁 按右鍵）：`egui::Modal`，按一次 Esc 就關（輸入框有焦點也是）、Enter 開啟；
//!   打開時讀剪貼簿，是網址就先填好（Windows、Linux；macOS 讀剪貼簿會跳出系統的提示，不讀）。
//! - 在播放器上貼上（Ctrl+V、Shift+Insert）：網址就開，看起來是完整路徑的也開（不先檢查在不在），其他的提示一下。
//! - 網路串流載入完成時記下標題、加進最近開啟。
//!
//! （「設定 → 網路」頁之後加在這裡）

use super::{VitascopeApp, recent_label};
use crate::history::History;
use crate::net::{self, NetSettings};
use crate::theme::Palette;
use eframe::egui::{self, Id, Key, ViewportCommand};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 打開對話框時讀剪貼簿（Windows、Linux）。macOS 15.4 起程式自己讀剪貼簿會跳出系統的隱私提示（之後的版本會要求同意），
/// 所以不讀；使用者自己按 ⌘V 貼上
const PREFILL: bool = !cfg!(target_os = "macos");
/// 讀剪貼簿的結果最多等多久（eframe 在下一幀送來；剪貼簿是空的、不是文字時什麼都不會來）
pub(super) const PREFILL_WAIT: Duration = Duration::from_millis(500);
/// 對話框列出幾個最近輸入的網址
const RECENT_IN_DIALOG: usize = 10;
/// 好幾個網址時列出前幾個
const LISTED: usize = 5;

/// 「開啟網址」對話框的狀態
pub(super) struct UrlDialog {
    /// 輸入框的文字
    text: String,
    /// 等讀剪貼簿的結果到什麼時候（None = 不等了）。這段期間的「貼上」是讀剪貼簿的結果：不是網址就不填，
    /// 而且在輸入框收到之前就拿走（不會先出現一幀不相干的文字）
    prefill_until: Option<Instant>,
    /// 下一次畫的時候把焦點放到輸入框（剛打開、點了最近的網址）
    focus: bool,
}

/// 對話框裡按了什麼
enum UrlOp {
    /// 開啟（Enter、「開啟」）
    Open,
    /// 加入播放清單
    Add,
    /// 點了最近的網址：填進輸入框
    Fill(String),
    /// 雙擊最近的網址：直接開
    OpenRecent(String),
    /// 清除最近輸入的網址
    ClearRecent,
    Cancel,
}

impl VitascopeApp {
    /// 啟動時（還沒開檔）同步套用：系統的 libmpv 0.37 預設不檢查網站憑證，從第一個網址就要檢查
    pub(super) fn net_startup(&mut self) {
        self.net_defaults = self.player.net_defaults();
        let opts = net::mpv_options(&self.settings.net, &self.net_defaults);
        for (name, _, result) in self.player.apply_net(&opts, true) {
            if let Err(e) = result {
                eprintln!("[vitascope] 無法套用 {name}：{e}");
            }
        }
    }

    /// 設定改了之後：非同步送出有變的選項（播放中改不會卡住介面；正在播的串流到下一次連線才用新的值）
    fn apply_net(&mut self) {
        let opts = net::mpv_options(&self.settings.net, &self.net_defaults);
        for (name, key, result) in self.player.apply_net(&opts, false) {
            match result {
                Ok(Some(id)) => {
                    self.async_pending.insert(id, name.to_owned());
                }
                Ok(None) => {}
                Err(e) => self.async_failed(key, name, &e.to_string()),
            }
        }
    }

    /// 改網路設定：整理過（去掉換行之類）、有變才套用、存檔。「設定 → 網路」頁用；測試也直接呼叫
    #[doc(hidden)]
    pub fn change_net(&mut self, change: impl FnOnce(&mut NetSettings)) {
        let before = self.settings.net.clone();
        change(&mut self.settings.net);
        self.settings.net = std::mem::take(&mut self.settings.net).sanitized();
        if self.settings.net != before {
            self.apply_net();
            self.save_settings();
        }
    }

    // ───────────── 開啟網址 ─────────────

    /// 打開「開啟網址」對話框（已經開著就不動）
    pub(super) fn open_url_dialog(&mut self, ctx: &egui::Context) {
        if self.url_dialog.is_some() {
            return;
        }
        if PREFILL {
            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
        }
        self.url_dialog = Some(UrlDialog {
            text: String::new(),
            prefill_until: PREFILL.then(|| Instant::now() + self.url_prefill_wait),
            focus: true,
        });
    }

    /// 測試用：打開「開啟網址」時等讀剪貼簿的結果多久（0 = 不等，之後的貼上都交給輸入框；很長 = 一定等得到測試送的貼上）
    #[doc(hidden)]
    pub fn set_url_prefill_wait(&mut self, wait: Duration) {
        self.url_prefill_wait = wait;
    }

    /// 「開啟網址」對話框開著（介面測試用）
    #[doc(hidden)]
    pub fn url_dialog_open(&self) -> bool {
        self.url_dialog.is_some()
    }

    /// 網址的標題（介面測試用）
    #[doc(hidden)]
    pub fn url_title(&self, url: &str) -> Option<&str> {
        self.titles.get(url).map(String::as_str)
    }

    /// 「開啟網址」對話框
    pub(super) fn url_dialog_window(&mut self, ctx: &egui::Context) {
        if self.url_dialog.is_none() {
            return;
        }
        let recent: Vec<(String, String)> = self
            .history
            .urls
            .iter()
            .take(RECENT_IN_DIALOG)
            .map(|u| (u.clone(), recent_label(&self.titles, u)))
            .collect();
        let Some(d) = &mut self.url_dialog else { return };
        // 讀剪貼簿的結果：在輸入框收到之前拿走，是網址才填（打開時輸入框一定是空的）
        if let Some(until) = d.prefill_until {
            let pasted = ctx.input_mut(|i| {
                let at = i.events.iter().position(|e| matches!(e, egui::Event::Paste(_)))?;
                match i.events.remove(at) {
                    egui::Event::Paste(text) => Some(text),
                    _ => None,
                }
            });
            if let Some(text) = pasted {
                d.prefill_until = None;
                // 是程式要的，不是按著 Ctrl+V（按著 Ctrl+U 時收到的也是）：下一次按 Ctrl+V 不算自動重複
                self.paste_held = false;
                if d.text.is_empty() && net::parse_input(&text).is_ok() {
                    d.text = prefill_text(&text);
                }
            } else if Instant::now() >= until {
                d.prefill_until = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
        }
        let mut op = None;
        // 固定在上方（不是置中）：對話框變高時（出現「會開啟：…」、好幾個網址）上面的東西不會跟著移動，
        // 最近的網址第一下點完填入、第二下（雙擊）才點得到同一列
        let top = (ctx.content_rect().height() * 0.12).clamp(8.0, 120.0);
        let area =
            egui::Modal::default_area(Id::new("open_url")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top));
        let modal = egui::Modal::new(Id::new("open_url")).area(area).show(ctx, |ui| {
            let width = (ctx.content_rect().width() - 80.0).clamp(280.0, 560.0);
            ui.set_width(width);
            let problem = Palette::of(ui.visuals()).problem;
            ui.heading(crate::tr!("開啟網址", "Open URL"));
            ui.add_space(6.0);
            let label = ui.label(crate::tr!("網址", "URL"));
            let edit = egui::TextEdit::singleline(&mut d.text)
                .hint_text("https://…")
                .desired_width(f32::INFINITY);
            let field = ui.add(edit).labelled_by(label.id);
            if std::mem::take(&mut d.focus) {
                field.request_focus();
            }
            ui.weak(crate::tr!(
                "影片或串流的網址（HTTP、HLS 的 .m3u8、DASH 的 .mpd）；好幾個網址用空白分開，會變成播放清單",
                "A video or stream URL (HTTP, HLS .m3u8, DASH .mpd); separate several URLs with spaces to make a playlist"
            ));
            let parsed = net::parse_input(&d.text);
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                if parsed.is_ok() {
                    op = Some(UrlOp::Open);
                } else {
                    // 單行輸入框按 Enter 會交出焦點：打不開時留在輸入框，接著打的字才不會不見
                    field.request_focus();
                }
            }

            if !recent.is_empty() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.strong(crate::tr!("最近開啟的網址", "Recent URLs"));
                    if ui.small_button(crate::tr!("清除記錄", "Clear history")).clicked() {
                        op = Some(UrlOp::ClearRecent);
                    }
                });
                for (url, name) in &recent {
                    let r = ui
                        .add(egui::Button::new(name.as_str()).frame(false).truncate())
                        .on_hover_text(url);
                    if r.double_clicked() {
                        op = Some(UrlOp::OpenRecent(url.clone()));
                    } else if r.clicked() {
                        op = Some(UrlOp::Fill(url.clone()));
                    }
                }
            }

            // 開之前先讓人看到實際會開的網址：空白換成 %20、補上的 https://、拆成好幾個的。
            // 放在最近的網址下面：這幾行變多變少時，上面的清單不會移動
            ui.add_space(8.0);
            match &parsed {
                Ok(urls) if urls.len() == 1 => url_line(ui, &crate::tf!("會開啟：{}", "Will open: {}", urls[0])),
                Ok(urls) => {
                    ui.label(crate::tf!(
                        "{} 個網址，會變成播放清單：",
                        "{} URLs — they will become a playlist:",
                        urls.len()
                    ));
                    for (i, u) in urls.iter().take(LISTED).enumerate() {
                        url_line(ui, &format!("{}. {u}", i + 1));
                    }
                    if urls.len() > LISTED {
                        ui.weak(crate::tf!("…還有 {} 個", "…and {} more", urls.len() - LISTED));
                    }
                }
                Err(net::InputError::Empty) => {}
                Err(e) => {
                    ui.colored_label(problem, e.message());
                }
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(crate::tr!("取消", "Cancel")).clicked() {
                    op = Some(UrlOp::Cancel);
                }
                let open = ui.add_enabled(parsed.is_ok(), egui::Button::new(crate::tr!("開啟", "Open")));
                let add = ui.add_enabled(
                    parsed.is_ok(),
                    egui::Button::new(crate::tr!("加入播放清單", "Add to playlist")),
                );
                if open.clicked() {
                    op = Some(UrlOp::Open);
                }
                if add.clicked() {
                    op = Some(UrlOp::Add);
                }
                if let Err(e) = &parsed
                    && *e != net::InputError::Empty
                {
                    open.on_disabled_hover_text(e.message());
                    add.on_disabled_hover_text(e.message());
                }
            });
        });
        let close = modal.should_close();
        match op {
            Some(op @ (UrlOp::Open | UrlOp::Add)) => {
                let urls = self
                    .url_dialog
                    .take()
                    .and_then(|d| net::parse_input(&d.text).ok())
                    .unwrap_or_default();
                self.open_typed_urls(urls, matches!(op, UrlOp::Add));
            }
            Some(UrlOp::OpenRecent(url)) => {
                self.url_dialog = None;
                self.open_typed_urls(vec![url], false);
            }
            Some(UrlOp::Fill(url)) => {
                if let Some(d) = &mut self.url_dialog {
                    d.text = url;
                    d.focus = true;
                }
            }
            Some(UrlOp::ClearRecent) => self.update_history(History::clear_urls),
            Some(UrlOp::Cancel) => self.url_dialog = None,
            None => {}
        }
        if close {
            self.url_dialog = None;
        }
        // 對話框開著：下一幀的按鍵都交給它（Enter 不會切換全螢幕、Esc 只關對話框、貼上不會開別的網址）
        self.modal_open |= self.url_dialog.is_some();
    }

    /// 「開啟網址」對話框開的網址：記進對話框的清單（「記住開啟過的網址」打開、網址裡沒有密碼之類的），
    /// 然後開啟（好幾個 = 照順序的播放清單）或加入播放清單
    fn open_typed_urls(&mut self, urls: Vec<String>, add: bool) {
        if urls.is_empty() {
            return;
        }
        if self.settings.net.remember_urls {
            let keep: Vec<&String> = urls.iter().filter(|u| net::storable(u)).collect();
            if !keep.is_empty() {
                // 倒著加：第一個在最前面
                self.update_history(|h| keep.iter().rev().for_each(|u| h.add_url(u)));
            }
        }
        let paths: Vec<PathBuf> = urls.into_iter().map(PathBuf::from).collect();
        if add {
            self.add_to_playlist(paths);
        } else {
            self.open_paths(paths, false);
        }
    }

    /// 在播放器上貼上（Ctrl+V、Shift+Insert）：網址就開（播放清單開著時加到最後，跟拖放一樣；字幕的網址加到正在播的影片）；
    /// 看起來是完整路徑的也開（不先檢查在不在：網路磁碟連不上時會卡住畫面，打不開時 mpv 會說明原因）；
    /// 其他的提示一下。
    ///
    /// 複製的「檔案」：Windows 的檔案總管只放檔案、不放文字，egui 收不到「貼上」，不會到這裡（README 寫了要用「複製路徑」）；
    /// macOS 的 Finder、一些 Linux 的檔案管理員另外放了檔名的文字（沒有資料夾），提示剪貼簿裡只有檔名
    pub(super) fn paste_text(&mut self, text: &str) {
        if let Ok(urls) = net::parse_input(text) {
            let msg = match urls.as_slice() {
                [one] => crate::tf!(
                    "開啟剪貼簿的網址：{}",
                    "Opening the URL from the clipboard: {}",
                    net::display_name(one, self.titles.get(one).map(String::as_str))
                ),
                many => crate::tf!(
                    "開啟剪貼簿的網址：{} 個",
                    "Opening {} URLs from the clipboard",
                    many.len()
                ),
            };
            // 先提示：加到播放清單（加了幾個）、載入字幕時，那邊自己的提示蓋過這個
            self.osd(msg);
            self.open_paths(urls.into_iter().map(PathBuf::from).collect(), true);
        } else if let Some(paths) = net::pasted_paths(text) {
            self.open_paths(paths, true);
        } else if net::bare_file_name(text) {
            self.osd(crate::tr!(
                "剪貼簿裡只有檔名，沒有完整路徑（請複製路徑或拖放）",
                "The clipboard has only a file name, not the full path (copy the path, or drag the file in)"
            ));
        } else {
            self.osd(crate::tr!(
                "剪貼簿裡沒有網址或檔案路徑",
                "The clipboard has no URL or file path"
            ));
        }
    }

    /// 網路串流載入完成：記下標題（mpv 讀到真的標題時；已經知道的，例如 m3u 寫的，優先），
    /// 加進最近開啟（「記住開啟過的網址」打開、網址裡沒有密碼、token 之類的）
    pub(super) fn remember_url(&mut self, url: &str) {
        // 沒有標題時 media-title 是網址的最後一段（mpv 的 filename），那不算標題
        let filename = self.player.get_string("filename").unwrap_or_default();
        let tag = |key: &str| self.player.get_string(&format!("metadata/by-key/{key}")).ok();
        if !self.titles.contains_key(url)
            && let Ok(t) = self.player.get_string("media-title")
            && t != filename
            && let Some(t) = net::stream_title(&t, tag("icy-title").as_deref(), tag("icy-name").as_deref())
            && let Some(t) = net::useful_title(url, t)
        {
            self.titles.insert(url.to_owned(), t);
        }
        if self.settings.net.remember_urls && net::storable(url) {
            let title = self.titles.get(url).cloned();
            self.update_history(|h| {
                h.add_recent(url);
                if let Some(t) = &title {
                    h.set_title(url, t);
                }
            });
        }
    }
}

/// 讀剪貼簿填進輸入框的文字（已經確定是網址）。一行的照原樣，只去掉前後的空白：路徑裡的全形空白、連續的空白
/// 照樣換成各自的 %xx，跟直接貼進輸入框的結果一樣。好幾行的換行換成空白：只有每一段都是網址時才會到這裡，
/// 合成一行之後 parse_input 拆出來的網址不變
fn prefill_text(text: &str) -> String {
    if text.trim().contains(['\n', '\r']) {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        text.trim().to_owned()
    }
}

/// 對話框裡的一個網址：一行，太長的截掉（滑鼠停在上面看完整的）
fn url_line(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak()).truncate())
        .on_hover_text(text);
}
