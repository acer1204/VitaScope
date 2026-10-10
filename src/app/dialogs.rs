//! 檔案對話框（開檔、載入字幕／音軌、播放清單、截圖資料夾、片段資料夾、GIF 資料夾、像素著色器、yt-dlp 的執行檔）。
//!
//! 以前在介面的執行緒上直接開（同步），對話框開著時 eframe 整個停住：mpv 照樣播聲音，影像卻停在那裡。
//! 現在 Windows、Linux 在背景執行緒開（擁有者照樣是主視窗：Windows 上主視窗按不到，跟以前一樣），
//! 介面照樣畫、影片照樣播，選好的結果用 channel 送回來，在 `logic()` 裡處理（`poll_dialog`）。
//! macOS 的 NSOpenPanel 一定要在主執行緒開，開著時先暫停播放，選完再繼續。
//! 同時只開一個：Linux 的對話框（xdg portal；沒有 portal 時 rfd 改用 zenity，完全沒有擁有者）
//! 不一定擋得住主視窗，可能躲在主視窗後面，再按一次開檔只提示已經開著。
//!
//! 「另存截圖」照樣在介面的執行緒上開、開著時暫停（存的是選「另存新檔」時的畫面），見 `capture.rs`。

use super::{VitascopeApp, file_name};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

/// 哪一個對話框：決定選好之後做什麼（`on_dialog_result`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// 開啟影片（或播放清單檔）
    Open,
    LoadSubtitle,
    LoadAudio,
    /// 播放清單「加入檔案…」
    PlaylistAddFiles,
    /// 播放清單「加入資料夾…」
    PlaylistAddFolder,
    /// 播放清單「開啟清單…」
    PlaylistOpen,
    /// 播放清單「儲存清單…」
    PlaylistSave,
    /// 變更截圖資料夾
    ScreenshotDir,
    /// 像素著色器的組合（編號）「加入檔案…」
    ShaderFiles(u32),
    /// 擷取畫面「另存新檔…」：只在介面的執行緒上開（開著時暫停，見 `capture.rs`）
    ScreenshotSaveAs,
    /// 「設定 → 網路」選擇 yt-dlp 的執行檔
    YtdlPath,
    /// 變更片段的資料夾（匯出視窗、「設定 → 截圖與匯出」）
    ExportClipDir,
    /// 變更 GIF 的資料夾（匯出視窗、「設定 → 截圖與匯出」）
    ExportImageDir,
}

impl DialogKind {
    /// 選好的檔案是給開對話框時的那個影片的（字幕、音軌）
    fn for_current_file(self) -> bool {
        matches!(self, DialogKind::LoadSubtitle | DialogKind::LoadAudio)
    }
}

/// 對話框要選什麼
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    File,
    Files,
    Folder,
    Save,
}

impl Pick {
    /// 開對話框、等使用者選好（會卡住呼叫的執行緒）；取消是 None
    fn run(self, dialog: rfd::FileDialog) -> Option<Vec<PathBuf>> {
        match self {
            Pick::File => dialog.pick_file().map(|p| vec![p]),
            Pick::Files => dialog.pick_files(),
            Pick::Folder => dialog.pick_folder().map(|p| vec![p]),
            Pick::Save => dialog.save_file().map(|p| vec![p]),
        }
    }
}

/// 對話框只能在主執行緒開（macOS 的 NSOpenPanel）：開著時暫停播放
const MAIN_THREAD_ONLY: bool = cfg!(target_os = "macos");

/// 介面測試用：取代真的對話框。在開對話框的那個執行緒上呼叫（Windows、Linux 是背景執行緒），選好才回傳
pub(super) type DialogStub = Arc<dyn Fn(DialogKind, Pick) -> Option<Vec<PathBuf>> + Send + Sync>;

/// `stub_dialogs` 的對話框最多等測試回覆這麼久（測試寫錯、在介面的執行緒上等時不會永遠卡住），逾時當成取消
const STUB_WAIT: Duration = Duration::from_secs(20);

/// 開著的對話框：選好的結果從這裡送回來
pub(super) struct PendingDialog {
    kind: DialogKind,
    rx: Receiver<Option<Vec<PathBuf>>>,
    /// 開對話框時的檔案（`file_gen`）：字幕、音軌只能加到那個影片
    file_gen: u64,
}

/// 介面測試用（`stub_dialogs`）：要開的對話框，測試回覆選了什麼（None = 取消；丟掉 = 開對話框的執行緒出錯結束）
#[doc(hidden)]
pub struct DialogRequest {
    pub kind: DialogKind,
    pub pick: Pick,
    pub reply: Sender<Option<Vec<PathBuf>>>,
}

impl VitascopeApp {
    /// 開對話框（`dialog` 已經設好標題、篩選條件、擁有者）。已經有一個開著就不開
    pub(super) fn show_dialog(&mut self, kind: DialogKind, pick: Pick, dialog: rfd::FileDialog) {
        if self.refuse_second_dialog(kind) {
            return;
        }
        let run = self.dialog_runner(kind, pick, dialog);
        if MAIN_THREAD_ONLY {
            // 開著時介面不會畫：與其影像停住、聲音繼續，不如先暫停
            let was_playing = self.pause_for_dialog();
            let chosen = run();
            self.resume_after_dialog(was_playing);
            if let Some(paths) = chosen.filter(|p| !p.is_empty()) {
                self.on_dialog_result(kind, paths);
            }
            return;
        }
        // rfd::FileDialog 可以送到別的執行緒（擁有者的 handle 在主視窗存在期間都有效，主視窗跟程式一樣久）
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("file-dialog".to_owned())
            .spawn(move || {
                let chosen = run();
                // 選好了：叫醒介面馬上處理（暫停中介面不會自己重畫）
                if tx.send(chosen).is_ok() {
                    ctx.request_repaint();
                }
            });
        match spawned {
            Ok(_) => {
                self.dialog = Some(PendingDialog {
                    kind,
                    rx,
                    file_gen: self.file_gen,
                })
            }
            Err(e) => eprintln!("[vitascope] 無法開啟對話框：{e}"),
        }
    }

    /// 已經開著一個對話框：不再開，提示一下（Linux 的對話框可能躲在主視窗後面，不然看起來像按鈕壞了）
    pub(super) fn refuse_second_dialog(&mut self, kind: DialogKind) -> bool {
        let Some(open) = &self.dialog else { return false };
        eprintln!("[vitascope] 已經開著對話框（{:?}），不再開 {kind:?}", open.kind);
        self.osd(crate::tr!("檔案對話框已經開著", "A file dialog is already open"));
        true
    }

    /// 開對話框、等選好的動作（呼叫的執行緒會卡住）；介面測試換成測試給的（`stub_dialogs`）
    pub(super) fn dialog_runner(
        &self,
        kind: DialogKind,
        pick: Pick,
        dialog: rfd::FileDialog,
    ) -> impl FnOnce() -> Option<Vec<PathBuf>> + Send + 'static {
        let stub = self.dialog_stub.clone();
        move || match stub {
            Some(stub) => stub(kind, pick),
            None => pick.run(dialog),
        }
    }

    /// 每一幀：對話框選好了的話處理結果（取消就什麼都不做）
    pub(super) fn poll_dialog(&mut self) {
        let Some(open) = &self.dialog else { return };
        let chosen = match open.rx.try_recv() {
            Ok(chosen) => chosen,
            Err(TryRecvError::Empty) => return,
            // 背景執行緒出錯結束了：當成取消
            Err(TryRecvError::Disconnected) => None,
        };
        let (kind, file_gen) = (open.kind, open.file_gen);
        self.dialog = None;
        let Some(paths) = chosen.filter(|p| !p.is_empty()) else {
            return;
        };
        // 對話框開著時影片照樣播：可能已經播完換到清單的下一個（Linux 上也可能自己換了檔案）。
        // 字幕、音軌是給原來那個影片選的，不能加到別的影片上
        if kind.for_current_file() && (file_gen != self.file_gen || !self.player.state.loaded) {
            self.osd(crate::tf!(
                "影片已經換了，沒有載入 {}",
                "The video has changed; {} was not loaded",
                file_name(&paths[0])
            ));
            return;
        }
        self.on_dialog_result(kind, paths);
    }

    /// 對話框選好了（`paths` 至少一個）：跟以前選完之後做的一樣（介面測試直接呼叫）
    #[doc(hidden)]
    pub fn on_dialog_result(&mut self, kind: DialogKind, mut paths: Vec<PathBuf>) {
        let Some(first) = paths.first().cloned() else { return };
        match kind {
            DialogKind::Open => self.open(&first),
            DialogKind::LoadSubtitle => self.load_extra_file(&first, true),
            DialogKind::LoadAudio => self.load_extra_file(&first, false),
            DialogKind::PlaylistAddFiles => {
                crate::playlist::sort_by_name(&mut paths);
                self.add_to_playlist(paths);
            }
            DialogKind::PlaylistAddFolder => self.add_folder(first),
            DialogKind::PlaylistOpen => self.open_playlist_file(&first),
            DialogKind::PlaylistSave => self.save_playlist(first),
            DialogKind::ScreenshotDir => self.set_screenshot_dir(first),
            DialogKind::ShaderFiles(preset) => {
                self.add_shader_files(preset, &paths);
            }
            DialogKind::ScreenshotSaveAs => self.save_screenshot_as(first),
            DialogKind::YtdlPath => self.set_ytdl_path(first),
            DialogKind::ExportClipDir => self.set_export_dir(super::export_panel::ExportDir::Clip, first),
            DialogKind::ExportImageDir => self.set_export_dir(super::export_panel::ExportDir::Image, first),
        }
    }

    /// 開著的對話框（介面測試用）
    #[doc(hidden)]
    pub fn dialog_pending(&self) -> Option<DialogKind> {
        self.dialog.as_ref().map(|d| d.kind)
    }

    /// 介面測試用：之後的對話框不真的打開，改呼叫 `stub`（跟真的對話框一樣在開對話框的執行緒上，選好才回傳）
    #[doc(hidden)]
    pub fn stub_dialogs_with(
        &mut self,
        stub: impl Fn(DialogKind, Pick) -> Option<Vec<PathBuf>> + Send + Sync + 'static,
    ) {
        self.dialog_stub = Some(Arc::new(stub));
    }

    /// 介面測試用：之後的對話框不真的打開，要求送到回傳的 channel，開對話框的執行緒等測試回覆（見 `DialogRequest`）
    #[doc(hidden)]
    pub fn stub_dialogs(&mut self) -> Receiver<DialogRequest> {
        let (requests, rx) = mpsc::channel();
        self.stub_dialogs_with(move |kind, pick| {
            let (reply, answer) = mpsc::channel();
            requests.send(DialogRequest { kind, pick, reply }).ok()?;
            match answer.recv_timeout(STUB_WAIT) {
                Ok(chosen) => chosen,
                Err(RecvTimeoutError::Timeout) => None,
                // 測試丟掉了要求：當成開對話框的執行緒出錯結束（不經過 panic hook，不印訊息）
                Err(RecvTimeoutError::Disconnected) => std::panic::resume_unwind(Box::new("測試：對話框的執行緒結束")),
            }
        });
        rx
    }

    /// 對話框開著時介面不會畫（macOS 的對話框、另存截圖）：播放中的話先暫停，回傳之前是不是在播
    pub(super) fn pause_for_dialog(&mut self) -> bool {
        let st = &self.player.state;
        let was_playing = st.loaded && !st.paused;
        if was_playing {
            let _ = self.player.set_pause(true);
        }
        was_playing
    }

    /// 對話框關了：之前在播的話繼續播
    pub(super) fn resume_after_dialog(&mut self, was_playing: bool) {
        if was_playing {
            let _ = self.player.set_pause(false);
        }
    }
}
