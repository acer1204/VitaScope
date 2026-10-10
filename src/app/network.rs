//! 網路（開啟網址）：
//! - 設定的套用：HLS / DASH 畫質、重新連線、快取、逾時、憑證、標頭、proxy。對應到哪些 mpv 選項由 `net::mpv_options` 決定；
//!   啟動時同步設定（第一個網址就生效），之後非同步、只送有變的。
//! - 「開啟網址」對話框（Ctrl+U、右鍵選單、🗁 按右鍵）：`egui::Modal`，按一次 Esc 就關（輸入框有焦點也是）、Enter 開啟；
//!   打開時讀剪貼簿，是網址就先填好（Windows、Linux；macOS 讀剪貼簿會跳出系統的提示，不讀）。
//! - 在播放器上貼上（Ctrl+V、Shift+Insert）：網址就開，看起來是完整路徑的也開（不先檢查在不在），其他的提示一下。
//! - 網路串流載入完成時記下標題、加進最近開啟；能跳轉、一分鐘以上的影片續播（依續播的代號，`net::resume_key`）。
//! - 播放中：連線中（300 毫秒後才出現，本機檔案不會閃一下）、緩衝中的畫面，取消正在連線的網址
//!   （Esc 只在一般視窗；全螢幕時 Esc 照樣是離開全螢幕）、直播的「直播」標示與結束的提示。
//! - 網路上的播放清單（IPTV 的 .m3u 網址、yt-dlp 解析出來的網站播放清單）：展開成影戲的播放清單，項目照一般的開檔重新開。
//! - 網站影片（yt-dlp，播放器的 `on_load` hook）：等 yt-dlp 時顯示「正在取得網站影片」，記下網站給的標題，
//!   續播、書籤用同一部影片的代號；播不了時起始畫面寫出原因、提醒與建議（每次畫的時候才轉成文字，換語言也跟著換）。
//! - 「設定 → 網路」頁：串流的設定（HLS / DASH 畫質、重新連線、快取、逾時、憑證、記住網址）與進階（標頭、proxy）。

use super::control_panel::adjust_locked_hover;
use super::{VitascopeApp, is_fullscreen, recent_label};
use crate::history::History;
use crate::net::{self, HlsBitrate, NetSettings};
use crate::playlist::Playlist;
use crate::theme::{self, Palette};
use crate::ytdl::plan::NetInfo;
use crate::{tf, tr};
use eframe::egui::{self, Align, Color32, Frame, Id, Key, Layout, Rect, ViewportCommand};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 開網址多久之後才顯示「正在連線」（本機檔案、很快就連上的網址不會閃一下）
const CONNECTING_AFTER: Duration = Duration::from_millis(300);

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
        self.site_config();
    }

    /// 網站影片（yt-dlp）用的網路設定（proxy、逾時、憑證…）：下一次解析就用
    fn site_config(&mut self) {
        self.player
            .set_net_config(&self.settings.net, &crate::ytdl::SitePrefs::default());
    }

    /// 設定改了之後：非同步送出有變的選項（播放中改不會卡住介面；正在播的串流到下一次連線才用新的值），
    /// 網站影片（yt-dlp 的逾時、憑證、proxy，網站影片的重新連線、標頭）也從下一次解析起用新的設定
    fn apply_net(&mut self) {
        self.site_config();
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

/// 播放紀錄（續播）、書籤用的代號：本機檔案是路徑本身，網路串流是 `net::resume_key`（去掉 `#` 之後的部分）。
/// 網站影片（`site` = yt-dlp 解析出來的資料，是這個網址的時候）是同一部影片的代號 `ytdl://擷取器/代號`：
/// `youtu.be/x`、`watch?v=x&t=90` 是同一個。mpv 自己的網址（`av://` 之類）沒有代號：不續播
pub(super) fn history_key(path: &str, site: Option<&NetInfo>) -> Option<String> {
    if !super::is_url(path) {
        return Some(path.to_owned());
    }
    if let Some(key) = site.filter(|s| s.page_url == path).and_then(NetInfo::resume_key) {
        return Some(key);
    }
    net::resume_key(path, None)
}

/// 直播結束（或連線中斷、重新連線也失敗）的提示
fn live_ended_text() -> &'static str {
    tr!(
        "直播已結束或連線中斷",
        "The live stream ended or the connection dropped"
    )
}

/// 「檢查網站憑證」的說明（Windows 另外說明查不到撤銷清單時的情況）
fn tls_hover() -> String {
    let mut text = tr!(
        "確認網站的身分（HTTPS 憑證）。關閉後不確認，比較不安全。",
        "Verify the site's identity (its HTTPS certificate). Turning this off is less safe."
    )
    .to_owned();
    if cfg!(windows) {
        text += tr!(
            "Windows 上如果出現「無法確認網站憑證是否已被撤銷」，可能是網路擋住了憑證檢查，可以暫時關閉。",
            " On Windows, if you see \"Couldn't check whether the site's certificate was revoked\", \
             your network may be blocking the check; you can turn this off for now."
        );
    }
    text
}

/// 「設定 → 網路 → 進階」正在編輯的文字。打字時先放在這裡，離開欄位（或關掉設定視窗、換頁）才整理、套用、存檔：
/// 每打一個字就整理的話，前後的空白會被拿掉、打不出空白
pub(super) struct NetDraft {
    /// 開始編輯時的設定：只有改過的欄位才寫回去（別的視窗同時改了其他欄位不會被蓋掉）
    base: NetSettings,
    user_agent: String,
    referrer: String,
    /// 一行一個標頭
    headers: String,
    proxy: String,
}

impl NetDraft {
    fn of(n: &NetSettings) -> Self {
        Self {
            base: n.clone(),
            user_agent: n.user_agent.clone(),
            referrer: n.referrer.clone(),
            headers: n.headers.join("\n"),
            proxy: n.proxy.clone(),
        }
    }

    fn header_lines(&self) -> Vec<String> {
        self.headers
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

impl VitascopeApp {
    // ───────────── 載入中、緩衝中、直播 ─────────────

    /// 網路串流載入完成：記下是不是直播（`net::is_live`），能跳轉的影片從上次的位置繼續
    pub(super) fn net_file_loaded(&mut self, url: &str) {
        // 直接問 mpv（總長度、能不能跳轉的通知可能還沒到）
        let duration = self
            .player
            .get_f64("duration")
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0);
        let seekable = self.player.get_string("seekable").is_ok_and(|v| v == "yes");
        // 直播：沒有總長度，或是不能跳轉的 HLS / DASH / RTSP 之類。直播的 HLS 也有「總長度」：FFmpeg 用開頭讀到的那一段估的
        // （不到一秒），mpv 之後跟著已經讀到的長度變長。伺服器不支援 Range 的一般檔案也不能跳轉，但不是直播（見 `net::is_live`）
        let format = self.player.get_string("file-format").ok();
        let site = self.player.state.net.clone().filter(|s| s.page_url == url);
        // 網站影片：yt-dlp 說是直播就是（EDL 合成的影片看不出來）
        self.net_live =
            site.as_ref().is_some_and(|s| s.live) || net::is_live(url, duration, seekable, format.as_deref());
        // 不記網址時也不續播（續播的位置跟最近開啟一樣，是記下來的網址）；自動截圖要固定的畫面
        if !self.settings.resume || self.autoshot.is_some() || !self.settings.net.remember_urls || !net::storable(url) {
            return;
        }
        // 網站影片從指定的位置開始（換畫質時接著播、網址的 &t=90）：不跳到上次的位置
        if site.as_ref().is_some_and(|s| s.start_at.is_some()) {
            return;
        }
        let Some(key) = history_key(url, site.as_deref()) else {
            return;
        };
        let Some(t) = self.history.resume_point(&key) else {
            return;
        };
        // 不能跳轉（伺服器不支援 Range 之類）、直播：這次不跳，紀錄留著
        let Some(d) = duration.filter(|_| seekable) else {
            return;
        };
        if crate::history::worth_resuming(t, d) {
            let _ = self.player.seek_to(t, true);
            self.resume_target = Some((t, Instant::now()));
            self.osd(self.keymap.resume_osd(&super::fmt_time(t)));
        } else {
            // 同一個網址換成了較短的影片：位置已經不合理
            self.update_history(|h| h.forget(&key));
        }
    }

    /// 網站影片載入完成：網站給的標題（比之前記的、網址的最後一段準）記下來，最近開啟、播放清單都顯示它
    pub(super) fn site_file_loaded(&mut self, url: &str) {
        let Some(site) = self.player.state.net.clone().filter(|s| s.page_url == url) else {
            return;
        };
        if let Some(t) = site.title.as_deref().and_then(|t| net::useful_title(url, t)) {
            self.titles.insert(url.to_owned(), t);
        }
    }

    /// 網站影片播得了、但 yt-dlp 警告裡看得出問題（沒有 deno 時 YouTube 只有部分畫質之類）：提示一次。
    /// 已經有續播的提示時不蓋掉它
    pub(super) fn site_hint_osd(&mut self) {
        if self.resume_target.is_some() {
            return;
        }
        if let Some(hint) = self.player.state.net_hints.first() {
            self.osd(hint.message());
        }
    }

    /// 起始畫面上網站影片播不了的說明：原因（現在的語言）、警告裡看得出的提醒、建議怎麼做
    pub(super) fn site_failure_lines(&self) -> Option<(String, Vec<&'static str>)> {
        let f = self.player.state.net_failure.as_ref()?;
        let mut notes: Vec<&'static str> = f.hints.iter().map(|h| h.message()).collect();
        if let Some(r) = f.remedy() {
            notes.push(r.advice());
        }
        notes.dedup();
        Some((f.error.message(), notes))
    }

    /// 正在播的是直播（網路串流，載入時沒有總長度，或是不能跳轉的 HLS / DASH / RTSP 之類）：時間顯示「直播」，播完時提示
    pub(super) fn live_now(&self) -> bool {
        self.net_live && self.player.state.loaded
    }

    /// 每一幀：直播播到結尾（mpv 停在最後一格）時提示一次
    pub(super) fn live_tick(&mut self) {
        if self.live_now() && self.player.state.eof && !self.live_end_shown {
            self.live_end_shown = true;
            self.osd(live_ended_text());
        }
    }

    /// 直播播完、mpv 卸載了檔案（沒有停在最後一格的時候）
    pub(super) fn live_file_ended(&mut self) {
        if self.net_live && !self.live_end_shown {
            self.live_end_shown = true;
            self.osd(live_ended_text());
        }
    }

    /// 取消正在進行的開檔（Esc、連線中畫面的「取消」、載入中按停止）
    pub(super) fn cancel_loading(&mut self) {
        if !self.player.loading_now() {
            return;
        }
        if let Err(e) = self.player.cancel_loading() {
            eprintln!("[vitascope] 無法取消：{e}");
            return;
        }
        self.osd(tr!("已取消", "Cancelled"));
    }

    /// 影片畫面上的「正在連線」（網路串流開了 300 毫秒還沒載入完成）與「緩衝中」（快取不夠、等待中）。
    /// 畫在影片上：兩種主題都是深色。回傳按了「取消」
    pub(super) fn loading_overlay(&mut self, ui: &mut egui::Ui, rect: Rect) -> bool {
        let fullscreen = is_fullscreen(ui.ctx());
        let st = &self.player.state;
        let (line, cancel) = if let Some((url, since)) = self.player.net_loading() {
            let waited = since.elapsed();
            if waited < CONNECTING_AFTER {
                ui.ctx().request_repaint_after(CONNECTING_AFTER - waited);
                return false;
            }
            // 秒數每秒更新
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            let host = net::host(url).unwrap_or_else(|| net::display_name(url, None));
            let secs = waited.as_secs();
            // 等 yt-dlp 解析網頁（第一次啟動 yt-dlp 要幾秒）；整個播放清單更久（最多兩分鐘）
            let line = match st.net_busy.as_ref().map(|b| b.playlist) {
                Some(true) if secs >= 1 => tf!(
                    "正在讀取播放清單（yt-dlp）…（{secs} 秒）",
                    "Reading the playlist (yt-dlp)… ({secs} s)"
                ),
                Some(true) => tr!("正在讀取播放清單（yt-dlp）…", "Reading the playlist (yt-dlp)…").to_owned(),
                Some(false) if secs >= 1 => tf!(
                    "正在取得網站影片（yt-dlp）…（{secs} 秒）",
                    "Getting the website video (yt-dlp)… ({secs} s)"
                ),
                Some(false) => tr!("正在取得網站影片（yt-dlp）…", "Getting the website video (yt-dlp)…").to_owned(),
                None if secs >= 1 => tf!("正在連線：{host}…（{secs} 秒）", "Connecting to {host}… ({secs} s)"),
                None => tf!("正在連線：{host}…", "Connecting to {host}…"),
            };
            (line, true)
        } else if st.loaded && st.paused_for_cache {
            let line = match st.cache_buffering {
                Some(pct) => tf!("緩衝中… {pct}%", "Buffering… {pct}%"),
                None => tr!("緩衝中…", "Buffering…").to_owned(),
            };
            (line, false)
        } else {
            return false;
        };
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink(20.0))
                .layout(Layout::top_down(Align::Center)),
        );
        theme::dark_overlay(&mut ui);
        let height = if cancel { 96.0 } else { 44.0 };
        ui.add_space((rect.height() / 2.0 - 20.0 - height / 2.0).max(0.0));
        let mut clicked = false;
        Frame::NONE
            .fill(Color32::from_black_alpha(170))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(18, 10))
            .show(&mut ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(line).size(18.0).color(Color32::from_gray(230)));
                    if cancel {
                        // 全螢幕時 Esc 是離開全螢幕，只能按「取消」
                        if !fullscreen {
                            ui.label(
                                egui::RichText::new(tr!("按 Esc 取消", "Press Esc to cancel"))
                                    .size(13.0)
                                    .color(Color32::from_gray(160)),
                            );
                        }
                        clicked = ui.button(tr!("取消", "Cancel")).clicked();
                    }
                });
            });
        clicked
    }

    // ───────────── 網路上的播放清單 ─────────────

    /// 網路上的播放清單（IPTV 的 .m3u 網址）讀完了：展開成影戲的播放清單（清單本身在目前的播放清單上時，換成裡面的項目），
    /// 記下清單寫的標題，照一般的開檔重新開要播的那一個（換檔時的設定都照常做：取消暫停、A-B、字幕延遲、音訊直通的預測…）。
    /// 開檔用 `loadfile replace`，mpv 自己的播放清單又回到只有一個
    /// `capped` = yt-dlp 解析出來的網站播放清單、只讀了前 200 個項目（清單可能更長）
    pub(super) fn adopt_remote_playlist(&mut self, list: net::RemotePlaylist, capped: bool) {
        if self.settings.net.remember_urls && net::storable(&list.source) {
            let source = list.source.clone();
            self.update_history(|h| h.add_recent(&source));
        }
        if list.entries.is_empty() {
            // mpv 接著會開它自己選的項目：停下來（清單裡只有不能開的項目）
            let _ = self.player.stop();
            self.player.state.last_error = Some(
                tr!(
                    "播放清單裡沒有可以播放的網址",
                    "The playlist has no URL that can be played"
                )
                .to_owned(),
            );
            return;
        }
        for (url, title) in &list.entries {
            if let Some(t) = title.as_deref().and_then(|t| net::useful_title(url, t)) {
                self.titles.entry(url.clone()).or_insert(t);
            }
        }
        let n = list.entries.len();
        let paths: Vec<PathBuf> = list.entries.into_iter().map(|(u, _)| PathBuf::from(u)).collect();
        let source = Path::new(&list.source);
        let spliced = self
            .playlist
            .as_mut()
            .filter(|l| l.current().is_some_and(|c| crate::playlist::same_file(c, source)))
            .and_then(|l| l.replace_current(paths.clone()));
        let first = match spliced {
            Some(at) => at,
            None => {
                self.playlist_scan = None;
                self.playlist = Some(Playlist::from_files(paths.clone()).manual());
                0
            }
        };
        let start = list.start.min(n - 1);
        self.open_at(&paths[start], Some(first + start));
        let mut msg = tf!("播放清單：{n} 個項目", "Playlist: {n} items");
        // 網站的播放清單只讀前 200 個（頻道可能有上萬部影片）
        if capped {
            let max = crate::ytdl::PLAYLIST_MAX;
            msg += &tf!("（只載入前 {max} 個）", " (only the first {max} were loaded)");
        }
        if list.dropped > 0 {
            msg += &tf!(
                "（略過 {} 個不能開的項目）",
                " ({} entries that can't be opened were skipped)",
                list.dropped
            );
        }
        self.osd(msg);
    }

    // ───────────── 媒體資訊 ─────────────

    /// 媒體資訊的「來源」（網路串流才有）
    pub(super) fn info_source(&self) -> Option<crate::mediainfo::Source> {
        let url = self.player.state.path.clone().filter(|p| net::is_network(p))?;
        Some(crate::mediainfo::Source {
            title: self.titles.get(&url).cloned(),
            live: self.live_now(),
            url,
        })
    }

    // ───────────── 設定 → 網路 ─────────────

    /// 把「進階」正在編輯的文字寫回設定（整理、套用、存檔）。離開欄位、換頁、關掉設定視窗時呼叫
    pub(super) fn commit_net_draft(&mut self) {
        let Some(d) = self.net_draft.take() else { return };
        let headers = d.header_lines();
        self.change_net(|n| {
            if d.user_agent != d.base.user_agent {
                n.user_agent = d.user_agent.clone();
            }
            if d.referrer != d.base.referrer {
                n.referrer = d.referrer.clone();
            }
            if headers != d.base.headers {
                n.headers = headers;
            }
            if d.proxy != d.base.proxy {
                n.proxy = d.proxy.clone();
            }
        });
    }

    /// 「設定 → 網路」頁：串流（HLS / DASH 畫質、快取、逾時、重新連線、憑證、記住網址），進階（User-Agent、Referer、標頭、proxy）。
    /// 改了馬上套用、存檔（逾時拖曳時先套用，放開才存檔；文字離開欄位才套用）。VITASCOPE_MPV_OPTS 指定的選項停用
    pub(super) fn network_page(&mut self, ui: &mut egui::Ui) {
        let locked = |app: &Self, names: &[&str]| names.iter().any(|n| app.player.user_overrides().contains(*n));
        let mut n = self.settings.net.clone();
        let mut save = false;
        ui.strong(tr!("串流", "Streams"));
        ui.weak(tr!(
            "開啟網址（HTTP、HLS、DASH）用的設定；改了之後，下一次連線就用新的設定。",
            "Settings for URLs (HTTP, HLS, DASH); changes apply from the next connection."
        ));
        ui.add_space(4.0);
        let (hls_locked, cache_locked, timeout_locked) = (
            locked(self, &["hls-bitrate"]),
            locked(self, &["demuxer-max-bytes", "demuxer-max-back-bytes"]),
            locked(self, &["network-timeout"]),
        );
        egui::Grid::new("settings_network")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let name = ui.label(tr!("HLS / DASH 畫質", "HLS / DASH quality"));
                let r = ui
                    .add_enabled_ui(!hls_locked, |ui| {
                        egui::ComboBox::from_id_salt("settings_net_hls")
                            .selected_text(n.hls_bitrate.label())
                            .show_ui(ui, |ui| {
                                for b in HlsBitrate::ALL {
                                    save |= ui.selectable_value(&mut n.hls_bitrate, b, b.label()).changed();
                                }
                            })
                            .response
                    })
                    .inner
                    .labelled_by(name.id)
                    .on_hover_text(tr!(
                        "有好幾種畫質的串流，一開始選哪一個（網路慢的時候選最低）",
                        "Which quality to start with when a stream has several (choose the lowest on a slow connection)"
                    ));
                if hls_locked {
                    r.on_disabled_hover_text(adjust_locked_hover());
                }
                ui.end_row();

                let name = ui.label(tr!("網路快取", "Network cache"));
                let r = ui
                    .add_enabled_ui(!cache_locked, |ui| {
                        egui::ComboBox::from_id_salt("settings_net_cache")
                            .selected_text(format!("{} MB", n.cache_mb))
                            .show_ui(ui, |ui| {
                                for mb in net::CACHE_SIZES {
                                    save |= ui.selectable_value(&mut n.cache_mb, mb, format!("{mb} MB")).changed();
                                }
                            })
                            .response
                    })
                    .inner
                    .labelled_by(name.id)
                    .on_hover_text(tr!(
                        "最多預先讀多少（往回跳轉用的另外保留三分之一）",
                        "How much to read ahead (a third more is kept for seeking back)"
                    ));
                if cache_locked {
                    r.on_disabled_hover_text(adjust_locked_hover());
                }
                ui.end_row();

                let name = ui.label(tr!("連線逾時", "Connection timeout"));
                let r = ui
                    .add_enabled(
                        !timeout_locked,
                        egui::DragValue::new(&mut n.timeout_secs)
                            .range(net::TIMEOUT_RANGE)
                            .speed(0.5)
                            .suffix(tr!(" 秒", " s")),
                    )
                    .labelled_by(name.id)
                    .on_hover_text(tr!(
                        "伺服器多久沒有回應就放棄",
                        "Give up when the server doesn't respond for this long"
                    ))
                    .on_disabled_hover_text(adjust_locked_hover());
                // 拖曳、打字時馬上套用，放開滑鼠或離開欄位時才存檔
                save |= r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
                ui.end_row();
            });
        ui.add_space(4.0);
        save |= ui
            .add_enabled(
                !locked(self, &[net::LAVF]),
                egui::Checkbox::new(
                    &mut n.reconnect,
                    tr!("連線中斷時自動重新連線", "Reconnect when the connection drops"),
                ),
            )
            .on_hover_text(tr!(
                "直播之類的串流斷線時重新連線（重試之間最多等 5 秒）；一開始就連不上的網址不重試，馬上說明原因",
                "Reconnect when a stream (a live one too) drops (waiting up to 5 s between tries); \
                 a URL that can't be reached at all isn't retried, so you see why at once"
            ))
            .on_disabled_hover_text(adjust_locked_hover())
            .changed();
        save |= ui
            .add_enabled(
                !locked(self, &["tls-verify"]),
                egui::Checkbox::new(
                    &mut n.tls_verify,
                    tr!("檢查網站憑證（建議）", "Check website certificates (recommended)"),
                ),
            )
            .on_hover_text(tls_hover())
            .on_disabled_hover_text(adjust_locked_hover())
            .changed();
        save |= ui
            .checkbox(
                &mut n.remember_urls,
                tr!(
                    "記住開啟過的網址（最近開啟、續播）",
                    "Remember URLs I open (recent list, resume)"
                ),
            )
            .on_hover_text(tr!(
                "網址裡有帳號密碼、token 之類的一律不記；已經記下的可以在「最近開啟的檔案」清除",
                "URLs with a user name, password or token are never remembered; \
                 clear the ones already remembered from \"Recent files\""
            ))
            .changed();
        if n != self.settings.net {
            self.change_net_now(n, save);
        } else if save {
            self.save_settings();
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new(tr!("進階", "Advanced"))
            .id_salt("settings_net_advanced")
            .default_open(false)
            .show(ui, |ui| self.network_advanced(ui));
    }

    /// 改網路設定：整理過再套用（只送有變的），`save` 時存檔
    fn change_net_now(&mut self, n: NetSettings, save: bool) {
        self.settings.net = n.sanitized();
        self.apply_net();
        if save {
            self.save_settings();
        }
    }

    /// 「設定 → 網路 → 進階」：User-Agent、Referer、其他標頭、proxy（文字，離開欄位才套用）
    fn network_advanced(&mut self, ui: &mut egui::Ui) {
        let locked = |app: &Self, name: &str| app.player.user_overrides().contains(name);
        let (ua_locked, headers_locked, proxy_locked) = (
            locked(self, "user-agent"),
            locked(self, net::HEADERS),
            locked(self, "http-proxy"),
        );
        let engine_ua = self.net_defaults.user_agent.clone();
        let problem = Palette::of(ui.visuals()).problem;
        let settings = &self.settings.net;
        let d = self.net_draft.get_or_insert_with(|| NetDraft::of(settings));
        let mut commit = false;
        egui::Grid::new("settings_net_advanced_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let fields: [(&str, &mut String, bool, String, bool); 4] = [
                    (
                        "User-Agent",
                        &mut d.user_agent,
                        ua_locked,
                        tf!(
                            "空白 = 播放引擎預設（{engine_ua}）",
                            "Empty = engine default ({engine_ua})"
                        ),
                        false,
                    ),
                    (
                        "Referer",
                        &mut d.referrer,
                        headers_locked,
                        tr!("空白 = 不送", "Empty = not sent").to_owned(),
                        false,
                    ),
                    (
                        tr!("其他 HTTP 標頭", "Other HTTP headers"),
                        &mut d.headers,
                        headers_locked,
                        tr!("一行一個「名稱: 值」", "One \"Name: value\" per line").to_owned(),
                        true,
                    ),
                    (
                        "Proxy",
                        &mut d.proxy,
                        proxy_locked,
                        tr!("http://主機:埠", "http://host:port").to_owned(),
                        false,
                    ),
                ];
                for (label, text, is_locked, hint, multiline) in fields {
                    let name = ui.label(label);
                    let edit = if multiline {
                        egui::TextEdit::multiline(text).desired_rows(3)
                    } else {
                        egui::TextEdit::singleline(text)
                    };
                    let r = ui
                        .add_enabled(!is_locked, edit.hint_text(hint).desired_width(280.0))
                        .labelled_by(name.id);
                    commit |= r.lost_focus();
                    if is_locked {
                        r.on_disabled_hover_text(adjust_locked_hover());
                    }
                    ui.end_row();
                }
            });
        // 不會送出的標頭：哪幾行
        let bad: Vec<String> = d
            .headers
            .lines()
            .enumerate()
            .filter(|(_, l)| !net::header_line_ok(l))
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        if !bad.is_empty() {
            let lines = bad.join(tr!("、", ", "));
            let text = if bad.len() == 1 {
                tf!(
                    "第 {} 行不是「名稱: 值」，不會送出",
                    "Line {} isn't \"Name: value\" and won't be sent",
                    lines
                )
            } else {
                tf!(
                    "第 {} 行不是「名稱: 值」，不會送出",
                    "Lines {} aren't \"Name: value\" and won't be sent",
                    lines
                )
            };
            ui.colored_label(problem, text);
        }
        if net::is_socks_proxy(&d.proxy) {
            ui.colored_label(
                problem,
                tr!(
                    "播放引擎只支援 HTTP proxy，SOCKS 只有 yt-dlp 會用",
                    "The playback engine supports HTTP proxies only; SOCKS is used by yt-dlp only"
                ),
            );
        }
        ui.weak(tr!(
            "Proxy 空白時用系統的 http_proxy 環境變數；Windows、macOS 系統設定裡的 proxy，播放引擎不會用。",
            "With no proxy, the http_proxy environment variable is used; \
             the playback engine doesn't use the proxy set in Windows or macOS settings."
        ));
        if commit {
            self.commit_net_draft();
        }
    }
}
