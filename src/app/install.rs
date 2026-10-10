//! 下載、更新、移除 yt-dlp 與 deno 的介面（主人的決定 Q2 (a)）：
//! - 只在使用者按下時才連網：起始畫面（網站影片播不了時）、「設定 → 網路」、「網站影片 ▸ 更新 yt-dlp」。不在背景自動更新。
//! - 下載前先問（`egui::Modal`）：來源、大小（deno 寫下載與解開後的大小）、放在哪裡。更新不另外問（按的就是「更新」）；
//!   移除先確認。
//! - 進度在設定頁與起始畫面，可以取消；做完了用提示說結果。給使用者看的文字都在介面執行緒產生（換語言也跟著換）。
//! - 從起始畫面按的（網站影片播不了）：做完再開一次那個網址（使用者在等的時候開了別的就不搶）。
//! - 影戲下載的 yt-dlp 超過 30 天沒更新（[`locate::managed_stale_days`]）：網站影片播不了時、設定頁提醒，不自動連網。
//!
//! 自動測試、`--shot` 沒有下載的方法（`Launch.installer` 是 None）：不顯示下載、更新、移除的按鈕。

use super::{PlaceholderOp, VitascopeApp};
use crate::theme::Palette;
use crate::web::WebError;
use crate::ytdl::install::{InstallError, Installer, Job, Op, Outcome, Stage, Tool};
use crate::ytdl::{Failure, Hint, Remedy, YtdlError, locate};
use crate::{tf, tr};
use eframe::egui::{self, Color32, Id};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// 起始畫面、設定頁、選單上按的
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolAction {
    /// 下載（先問）
    Download(Tool),
    /// 更新影戲下載的 yt-dlp（不另外問）
    UpdateYtdl,
    /// 移除影戲下載的那一份（先確認）
    Remove(Tool),
}

impl ToolAction {
    /// 起始畫面上的按鈕文字（從那裡按的，做完會再開一次網址）
    fn retry_label(self) -> String {
        match self {
            ToolAction::Download(t) => {
                let name = t.name();
                tf!("下載 {name}…", "Download {name}…")
            }
            ToolAction::UpdateYtdl => tr!("更新 yt-dlp 再試一次", "Update yt-dlp and try again").to_owned(),
            ToolAction::Remove(t) => {
                let name = t.name();
                tf!("移除 {name}…", "Remove {name}…")
            }
        }
    }
}

/// 下載、更新、移除的狀態
pub(super) struct InstallUi {
    /// 下載的方法；None = 不能下載（自動測試、`--shot`）
    installer: Option<Arc<Installer>>,
    /// 正在背景做的
    job: Option<Running>,
    /// 同意下載的對話框開著：(哪一個, 做完要不要再開一次播不了的網址)
    ask: Option<(Tool, bool)>,
    /// 確認移除的對話框開著
    remove: Option<Tool>,
    /// 上一次失敗的（設定頁顯示原因；開始下一件時清掉）：(做什麼, 原因, 從起始畫面按的話是哪個網址播不了)。
    /// 起始畫面只在那個網址還是播不了的時候顯示（開了別的就不再顯示）
    failed: Option<(Op, InstallError, Option<String>)>,
}

impl InstallUi {
    pub(super) fn new(installer: Option<Arc<Installer>>) -> Self {
        Self {
            installer,
            job: None,
            ask: None,
            remove: None,
            failed: None,
        }
    }
}

struct Running {
    job: Job,
    /// 從起始畫面按的（進度也畫在起始畫面）
    from_start: bool,
    /// 做完再開一次：(網址, 播放清單的位置)
    retry: Option<(String, Option<usize>)>,
}

/// 找到的 yt-dlp 是影戲下載的那一份
pub(super) struct Managed {
    /// 多久沒更新（天）；不用提醒時 None
    pub stale_days: Option<i64>,
}

/// 「已經 N 天沒更新」的提醒對這個錯誤有沒有意義（沒有 yt-dlp、關掉了、取消、沒有網路、缺 deno：更新也沒用）
fn stale_matters(f: &Failure) -> bool {
    !matches!(
        f.error,
        YtdlError::Missing | YtdlError::Disabled | YtdlError::Cancelled | YtdlError::Offline
    ) && f.remedy() != Some(Remedy::GetDeno)
}

impl VitascopeApp {
    /// 這個工具能由影戲下載嗎（有下載的方法、這個系統有可以下載的版本）
    pub(super) fn can_install(&self, tool: Tool) -> bool {
        self.install.installer.as_ref().is_some_and(|i| i.available(tool))
    }

    /// 正在下載、更新、移除
    pub(super) fn install_busy(&self) -> bool {
        self.install.job.is_some()
    }

    /// 目前找到的 yt-dlp 是影戲下載的那一份：多久沒更新。不是（或還沒找過）時 None
    pub(super) fn managed_ytdl(&self) -> Option<Managed> {
        let t = self.ytdl.get()?;
        t.ytdl.as_ref().filter(|y| y.managed())?;
        let stale_days = t
            .ytdl_version
            .and_then(|v| locate::managed_stale_days(v, self.ytdl.managed_modified(), SystemTime::now()));
        Some(Managed { stale_days })
    }

    /// 能在影戲裡更新 yt-dlp（找到的是影戲下載的那一份，而且有下載的方法）
    pub(super) fn can_update_ytdl(&self) -> bool {
        self.can_install(Tool::Ytdl) && self.managed_ytdl().is_some()
    }

    /// 網站影片播不了時，起始畫面多寫的提醒：影戲下載的 yt-dlp 很久沒更新
    pub(super) fn stale_note(&self, f: &Failure) -> Option<String> {
        let days = self.managed_ytdl()?.stale_days?;
        stale_matters(f).then(|| {
            tf!(
                "影戲下載的 yt-dlp 已經 {days} 天沒更新，網站改版後可能播不了",
                "The yt-dlp VitaScope downloaded hasn't been updated for {days} days and may not work after site changes"
            )
        })
    }

    /// 錯誤附帶的建議改成按鈕的（起始畫面就不再寫那段文字）。從設定頁、選單開始的下載還在做時，
    /// 起始畫面不畫按鈕也不畫進度：照舊寫建議
    pub(super) fn remedy_has_button(&self, r: Remedy) -> bool {
        if self.install.job.as_ref().is_some_and(|j| !j.from_start) {
            return false;
        }
        let action = match r {
            Remedy::GetYtdl => ToolAction::Download(Tool::Ytdl),
            Remedy::GetDeno => ToolAction::Download(Tool::Deno),
            Remedy::UpdateYtdl => ToolAction::UpdateYtdl,
            _ => return false,
        };
        self.site_tool_actions().contains(&action)
    }

    /// 網站影片播不了時，起始畫面上的按鈕（下載 yt-dlp、下載 deno、更新 yt-dlp 再試一次）
    pub(super) fn site_tool_actions(&self) -> Vec<ToolAction> {
        let st = &self.player.state;
        let mut out = Vec::new();
        if (st.net_need_ytdl || st.net_page_needs_ytdl) && self.can_install(Tool::Ytdl) {
            out.push(ToolAction::Download(Tool::Ytdl));
        }
        if let Some(f) = &st.net_failure {
            let remedy = f.remedy();
            let no_deno = self.ytdl.get().is_none_or(|t| t.deno.is_none());
            if remedy == Some(Remedy::GetDeno) && self.can_install(Tool::Deno) && no_deno {
                out.push(ToolAction::Download(Tool::Deno));
            }
            // 錯誤本身說太舊，或是錯誤看不出原因、警告裡說 yt-dlp 可能需要更新（解不開網站的驗證）
            let outdated =
                remedy == Some(Remedy::UpdateYtdl) || (f.hints.contains(&Hint::Outdated) && stale_matters(f));
            if self.can_update_ytdl() && (outdated || self.stale_note(f).is_some()) {
                out.push(ToolAction::UpdateYtdl);
            }
        }
        out
    }

    /// 起始畫面：正在下載的進度，或是網站影片播不了時能按的（下載、更新）；上一次下載失敗的原因
    pub(super) fn placeholder_tools(&self, ui: &mut egui::Ui) -> Option<PlaceholderOp> {
        let mut chosen = None;
        if let Some(r) = &self.install.job {
            // 從設定頁、選單開始的：進度在設定頁（起始畫面不重複畫）
            if !r.from_start {
                return None;
            }
            ui.add_space(8.0);
            if self.install_progress(ui) {
                chosen = Some(PlaceholderOp::CancelInstall);
            }
            return chosen;
        }
        if let Some((op, e, Some(url))) = &self.install.failed
            && self.site_failure_showing(url)
        {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(e.message(op))
                    .size(13.0)
                    .color(Palette::of(ui.visuals()).problem),
            );
        }
        let actions = self.site_tool_actions();
        if actions.is_empty() {
            return None;
        }
        ui.add_space(8.0);
        for a in actions {
            if ui.button(a.retry_label()).clicked() {
                chosen = Some(PlaceholderOp::Tool(a));
            }
        }
        chosen
    }

    /// 起始畫面上 `url` 播不了的說明還在（沒有開別的）
    fn site_failure_showing(&self, url: &str) -> bool {
        let st = &self.player.state;
        let failed = st.net_failure.is_some() || st.net_need_ytdl || st.net_page_needs_ytdl;
        failed && !st.loaded && self.player.opening() == Some(url)
    }

    /// 按了下載、更新、移除。`retry` = 從起始畫面按的：做完再開一次播不了的網址
    pub(super) fn tool_action(&mut self, a: ToolAction, retry: bool) {
        if self.install.job.is_some() {
            return;
        }
        match a {
            ToolAction::Download(t) => self.install.ask = Some((t, retry)),
            ToolAction::UpdateYtdl => self.update_ytdl(retry),
            ToolAction::Remove(t) => self.install.remove = Some(t),
        }
    }

    /// 更新影戲下載的 yt-dlp（已經是最新版就不下載）
    pub(super) fn update_ytdl(&mut self, retry: bool) {
        let installed = self.ytdl.get().and_then(|t| t.ytdl_version).map(|v| v.to_string());
        self.start_install(
            Op::Update {
                tool: Tool::Ytdl,
                installed,
            },
            retry,
        );
    }

    /// 在背景開始做（已經有一件在做時不做）
    fn start_install(&mut self, op: Op, retry: bool) {
        let Some(installer) = self.install.installer.clone() else {
            return;
        };
        if self.install.job.is_some() {
            return;
        }
        let from_start = retry;
        let retry = retry
            .then(|| self.player.opening().filter(|u| crate::m3u::is_url(u)))
            .flatten()
            .map(|u| (u.to_owned(), self.playlist.as_ref().and_then(|l| l.current_index())));
        let ctx = self.egui_ctx.clone();
        let wake: crate::instance::Wake = Arc::new(move || ctx.request_repaint());
        self.install.failed = None;
        self.install.job = Some(Running {
            job: Job::start(installer, op, Some(wake)),
            from_start,
            retry,
        });
    }

    /// 取消正在下載的（暫存檔刪掉，原本的檔案不動）
    pub(super) fn cancel_install(&mut self) {
        if let Some(r) = &self.install.job {
            r.job.cancel();
        }
    }

    /// 每一幀：做完了沒。做完就重新找 yt-dlp、deno（播放器解析用的是同一個尋找的結果），提示結果，
    /// 從起始畫面按的再開一次那個網址
    pub(super) fn install_tick(&mut self, ctx: &egui::Context) {
        let Some(running) = &self.install.job else {
            return;
        };
        let Some(result) = running.job.poll() else {
            // 進度條：做完時背景執行緒會叫醒介面，這裡是更新進度
            ctx.request_repaint_after(Duration::from_millis(200));
            return;
        };
        let Some(running) = self.install.job.take() else {
            return;
        };
        self.ytdl.refresh();
        self.ytdl.get();
        match result {
            Ok(outcome) => {
                self.osd(outcome.message());
                if let Some((url, index)) = running.retry
                    && !matches!(outcome, Outcome::Removed(_))
                {
                    // 等的時候使用者開了別的、正在播：不搶
                    let st = &self.player.state;
                    let still = !st.loaded && !self.player.loading_now() && self.player.opening() == Some(url.as_str());
                    if still {
                        self.open_at(Path::new(&url), index);
                    }
                }
            }
            Err(InstallError::Web(WebError::Cancelled)) => {
                let osd = match running.job.op {
                    Op::Update { .. } => tr!("已取消更新", "Update cancelled"),
                    _ => tr!("已取消下載", "Download cancelled"),
                };
                self.osd(osd);
            }
            Err(e) => {
                self.osd(e.message(&running.job.op));
                let url = running.from_start.then(|| running.retry.map(|(u, _)| u)).flatten();
                self.install.failed = Some((running.job.op, e, url));
            }
        }
    }

    /// 正在做的事與進度（設定頁、起始畫面）；回傳按了「取消下載」
    pub(super) fn install_progress(&self, ui: &mut egui::Ui) -> bool {
        let Some(r) = &self.install.job else {
            return false;
        };
        let name = r.job.op.tool().name();
        let stage = r.job.status.stage();
        let (done, total) = r.job.status.bytes.get();
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        let text = match (&r.job.op, stage) {
            (Op::Remove(_), _) => tf!("正在移除 {name}…", "Removing {name}…"),
            (_, Stage::Downloading) => match total {
                Some(t) => tf!(
                    "下載 {name}：{:.1} / {:.1} MB",
                    "Downloading {name}: {:.1} / {:.1} MB",
                    mb(done),
                    mb(t)
                ),
                None => tf!("下載 {name}：{:.1} MB", "Downloading {name}: {:.1} MB", mb(done)),
            },
            (_, s) => {
                let label = s.label();
                tf!("{name}：{label}", "{name}: {label}")
            }
        };
        let mut cancel = false;
        ui.horizontal(|ui| {
            if let (Stage::Downloading, Some(t)) = (stage, total) {
                ui.add(egui::ProgressBar::new(done as f32 / t.max(1) as f32).desired_width(160.0));
            } else {
                ui.spinner();
            }
            ui.label(text);
            if !matches!(r.job.op, Op::Remove(_)) && !r.job.cancelled() {
                cancel = ui.button(tr!("取消下載", "Cancel download")).clicked();
            }
        });
        cancel
    }

    /// 上一次失敗的原因（設定頁）
    pub(super) fn install_failure(&self, ui: &mut egui::Ui, problem: Color32) {
        if let Some((op, e, _)) = &self.install.failed {
            ui.colored_label(problem, e.message(op));
        }
    }

    /// 同意下載、確認移除的對話框（蓋住整個畫面；Esc、點外面 = 取消）
    pub(super) fn install_modals(&mut self, ctx: &egui::Context) {
        if let Some((tool, retry)) = self.install.ask {
            let mut answer = None;
            let dir = self
                .install
                .installer
                .as_ref()
                .map(|i| i.tools_dir().display().to_string())
                .unwrap_or_default();
            let modal = egui::Modal::new(Id::new("install_ask")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                let name = tool.name();
                ui.heading(tf!("下載 {name}？", "Download {name}?"));
                ui.add_space(6.0);
                ui.label(consent_text(tool));
                ui.add_space(4.0);
                ui.label(tr!("放在：", "It goes to:"));
                ui.monospace(dir.as_str());
                ui.add_space(4.0);
                ui.weak(tr!(
                    "下載後會核對發佈附的檢查碼。影戲不會在背景自動更新它；網站改版播不了時，可以按「更新 yt-dlp 再試一次」，\
                     或在「設定 → 網路」更新、移除。",
                    "The download is checked against the checksum published with the release. VitaScope never updates it \
                     in the background; when a site change breaks playback, use \"Update yt-dlp and try again\", or update \
                     or remove it in Settings → Network."
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr!("下載", "Download")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr!("取消", "Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if answer.is_some() || modal.should_close() {
                self.install.ask = None;
            }
            if answer == Some(true) {
                self.start_install(Op::Install(tool), retry);
            }
        }
        if let Some(tool) = self.install.remove {
            let mut answer = None;
            let path = self
                .install
                .installer
                .as_ref()
                .map(|i| i.path(tool).display().to_string())
                .unwrap_or_default();
            let modal = egui::Modal::new(Id::new("install_remove")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                let name = tool.name();
                ui.label(tf!(
                    "移除影戲下載的 {name}？",
                    "Remove the {name} VitaScope downloaded?"
                ));
                ui.monospace(path.as_str());
                if tool == Tool::Ytdl {
                    ui.weak(tr!(
                        "之後網站影片要用另外安裝的 yt-dlp，或再下載一次。",
                        "Website videos will then need a separately installed yt-dlp, or a new download."
                    ));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr!("移除", "Remove")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr!("取消", "Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if answer.is_some() || modal.should_close() {
                self.install.remove = None;
            }
            if answer == Some(true) {
                self.start_install(Op::Remove(tool), false);
            }
        }
        // 對話框開著：下一幀的按鍵都交給它（空白鍵不會暫停、Esc 只關對話框）
        self.modal_open |= self.install.ask.is_some() || self.install.remove.is_some();
    }

    /// 測試用：正在下載、更新、移除
    #[doc(hidden)]
    pub fn install_running(&self) -> bool {
        self.install_busy()
    }
}

/// 同意下載的說明：來源、授權、大小
fn consent_text(tool: Tool) -> String {
    let (download, disk) = tool.sizes_mb(crate::paths::Os::current());
    match tool {
        Tool::Ytdl => tf!(
            "從 GitHub（github.com/yt-dlp/yt-dlp）下載最新版的 yt-dlp，約 {download} MB。\
             yt-dlp 是獨立的開放原始碼程式，不是影戲的一部分（執行檔內含 GPLv3+ 等授權的元件）。",
            "Downloads the latest yt-dlp from GitHub (github.com/yt-dlp/yt-dlp), about {download} MB. \
             yt-dlp is a separate open-source program, not part of VitaScope (the program includes parts \
             under GPLv3+ and other licenses)."
        ),
        Tool::Deno => tf!(
            "deno 是 YouTube 需要的 JavaScript 執行環境（MIT 授權），沒有它 YouTube 只有部分畫質。\
             從 GitHub（github.com/denoland/deno）下載約 {download} MB，解開後約 {disk} MB。",
            "deno is the JavaScript runtime YouTube needs (MIT license); without it YouTube offers only some \
             qualities. Downloads about {download} MB from GitHub (github.com/denoland/deno), about {disk} MB \
             once unpacked."
        ),
    }
}
