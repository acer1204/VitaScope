//! 播放順序接上播放器：隨機播放、重複播放、一個檔案播完時接下來做什麼（[`end_step`]），
//! 上一個 / 下一個檔案，自動接下一個時開不起來的檔案跳過。
//!
//! mpv 自己的清單永遠只有一項：接下來播哪一個都由 [`Playlist`] 決定。重複單一檔案用 mpv 的 `loop-file`
//! （幾乎無縫，而且根本不會「播完」）；VITASCOPE_MPV_OPTS 自己指定了 loop-file 時不動它，播完時由這裡重播。
//!
//! 介面：右鍵選單「播放順序：… ▸」（「播完自動播放下一個」也在裡面）、播放清單分頁標題列下面的 🔀 / 🔁 按鈕、
//! 控制列的 🔀 / 🔂、「設定 → 播放」；快捷鍵 `toggle-shuffle`、`cycle-repeat`（預設沒有按鍵）。
//! 顯示的都是實際的重複方式（[`VitascopeApp::shown_repeat`]）：存的是「全部」、但「播完自動播放下一個」關著時顯示不重複；
//! VITASCOPE_MPV_OPTS 指定的 loop-file 讓 mpv 一直重播時顯示單一檔案，重複播放停用（[`VitascopeApp::repeat_locked`]）

use super::control_panel::adjust_locked_hover;
use super::{Action, VitascopeApp, display_label, mpv_opts_override, with_hint};
use crate::keymap::Command;
use crate::player::AsyncKey;
use crate::playlist::{EndStep, Playlist, Repeat, end_step, skip_step};
use eframe::egui::{self, WidgetInfo, WidgetType, vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 自動接下一個（或重複全部繞回去）開的檔案：開不起來時跳過，不是停在錯誤畫面（「播完後關機」才不會因為
/// 一個壞掉的檔案永遠等不到）。使用者自己開的檔案（雙擊、PgDn、開啟檔案…）開不起來就照常顯示原因
#[derive(Debug, Default)]
pub(super) struct AutoAdvance {
    /// 上次開成功之後開不起來的檔案（開成功就整個清掉）：清單上每一項都在裡面才停
    failed_files: HashSet<PathBuf>,
    /// 這個檔案的 StartFile 收到了：之後的 EndFile 才是它的（在它之前的是上一個檔案收尾的）
    started: bool,
    /// 開不起來（EndFile 帶著錯誤）：下一次 `auto_next` 跳過它
    failed: bool,
}

/// 換到清單上的另一項時的提示
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Next,
    Prev,
    /// 重複全部：清單播完、從頭（隨機時是新的一輪）
    Wrap,
}

/// mpv 的 `loop-file`：重複單一檔案時是 inf。
/// 目前的檔案不能跳轉（`unseekable`，例如伺服器不支援 Range 的網路串流、結束了的直播）時是 no：
/// mpv 播到結尾時跳不回開頭（快取裡沒有開頭時「Cannot seek in this stream」），
/// 會一直重試那個跳轉、佔滿 CPU、永遠不算播完；改由播完時（[`end_step`] → 重播）重新開
fn loop_file_value(repeat: Repeat, unseekable: bool) -> &'static str {
    if repeat == Repeat::One && !unseekable {
        "inf"
    } else {
        "no"
    }
}

/// 重複播放的名稱（選單、提示）
pub(super) fn repeat_label(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => crate::tr!("關閉", "off"),
        Repeat::One => crate::tr!("單一檔案", "this file"),
        Repeat::All => crate::tr!("全部", "all"),
    }
}

/// 「設定 → 播放」重複播放的選項（英文開頭大寫）
pub(super) fn repeat_option(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => crate::tr!("關閉", "Off"),
        Repeat::One => crate::tr!("單一檔案", "This file"),
        Repeat::All => crate::tr!("全部", "All"),
    }
}

/// 重複播放依序切換：關閉 → 全部 → 單一檔案 → 關閉（播放清單的按鈕、快捷鍵 `cycle-repeat`）
pub(super) fn next_repeat(repeat: Repeat) -> Repeat {
    match repeat {
        Repeat::Off => Repeat::All,
        Repeat::All => Repeat::One,
        Repeat::One => Repeat::Off,
    }
}

/// 右鍵選單「播放順序 ▸」裡重複播放的三個選項
fn repeat_choice_label(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => crate::tr!("不重複", "No repeat"),
        Repeat::One => crate::tr!("重複單一檔案", "Repeat this file"),
        Repeat::All => crate::tr!("重複全部", "Repeat all"),
    }
}

/// 播放順序的簡短說明（右鍵選單「播放順序：… ▸」的標題，每次打開選單都看得到現在的狀態）
fn order_summary(shuffle: bool, repeat: Repeat) -> &'static str {
    match (shuffle, repeat) {
        (false, Repeat::Off) => crate::tr!("依序", "in order"),
        (true, Repeat::Off) => crate::tr!("隨機", "shuffle"),
        (false, Repeat::One) => crate::tr!("重複單一", "repeat one"),
        (false, Repeat::All) => crate::tr!("重複全部", "repeat all"),
        (true, Repeat::One) => crate::tr!("隨機・重複單一", "shuffle · repeat one"),
        (true, Repeat::All) => crate::tr!("隨機・重複全部", "shuffle · repeat all"),
    }
}

/// 隨機播放開著 / 關著的說明（OSD、按鈕的提示）
fn shuffle_state(on: bool) -> &'static str {
    if on {
        crate::tr!("隨機播放：開啟", "Shuffle: on")
    } else {
        crate::tr!("隨機播放：關閉", "Shuffle: off")
    }
}

impl VitascopeApp {
    // ───────────── 換清單 ─────────────

    /// 換成另一份清單（所有 `self.playlist = Some(…)` 都經過這裡或 [`Self::replace_playlist_whole`]）：
    /// 開著隨機播放時套用上去，目前的檔案（還沒開始播時是「下一個」）排第一個。
    /// 單一檔案（同資料夾掃描出來的）、雙擊、還原的清單都從那個檔案開始
    pub(super) fn replace_playlist(&mut self, mut list: Playlist) {
        list.set_shuffle(self.settings.play_order.shuffle, &mut self.shuffler);
        self.playlist = Some(list);
    }

    /// 整份清單一起開（.m3u、一次拖放好幾個檔案、加入檔案到空的播放器）：開著隨機播放時整份洗牌，
    /// 連第一個也是隨機的。回傳要先播的那一項（清單是空的時 None）；之後照一般的開檔開它
    pub(super) fn replace_playlist_whole(&mut self, mut list: Playlist) -> Option<usize> {
        let first = if self.settings.play_order.shuffle {
            list.shuffle_whole_list(&mut self.shuffler)
        } else {
            (!list.is_empty()).then_some(0)
        };
        self.playlist = Some(list);
        first
    }

    // ───────────── 隨機、重複 ─────────────

    /// 目前的播放順序（設定裡的；介面測試、之後的選單用）
    pub fn play_order(&self) -> crate::playlist::PlayOrder {
        self.settings.play_order
    }

    /// 開關隨機播放（存檔）。打開時目前的檔案排第一個、其餘打散，每一項這一輪播一次；
    /// 關掉時回到清單的順序，從目前的檔案接下去
    pub fn set_shuffle(&mut self, on: bool) {
        self.settings.play_order.shuffle = on;
        if let Some(list) = &mut self.playlist {
            list.set_shuffle(on, &mut self.shuffler);
        }
        self.save_settings();
        self.osd(shuffle_state(on));
    }

    /// 重複播放：關閉 / 單一檔案 / 全部（存檔）。
    /// 選「全部」時一併打開「播完自動播放下一個」：不然清單播完也不會從頭（系統的媒體控制改的也一樣）
    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.settings.play_order.repeat = repeat;
        let auto_next_on = repeat == Repeat::All && !self.settings.auto_next;
        if auto_next_on {
            self.settings.auto_next = true;
        }
        self.apply_loop_file();
        self.save_settings();
        let label = repeat_label(repeat);
        self.osd(if auto_next_on {
            crate::tf!(
                "重複播放：{label}，並開啟「播完自動播放下一個」",
                "Repeat: {label}, and “Play the next file automatically” is on"
            )
        } else {
            crate::tf!("重複播放：{label}", "Repeat: {label}")
        });
    }

    /// VITASCOPE_MPV_OPTS（或 include=、profile=）自己指定了 loop-file / loop：影戲不動它
    pub(super) fn loop_file_overridden(&self) -> bool {
        mpv_opts_override(&self.player, "loop-file")
    }

    /// 重複單一檔案 → mpv 的 loop-file。它是全域選項（換檔後照樣有效），播放中改了對目前的檔案馬上生效。
    /// 跟上次送出的一樣就不再送
    fn apply_loop_file(&mut self) {
        if self.loop_file_overridden() {
            return;
        }
        let value = loop_file_value(self.settings.play_order.repeat, self.unseekable_file);
        if self.loop_file_sent != value {
            self.loop_file_sent = value;
            self.set_option_async(AsyncKey::LoopFile, "loop-file", value);
        }
    }

    /// 啟動時（還沒開任何檔案）同步設定：第一個檔案就要重複
    pub(super) fn play_order_startup(&mut self) {
        if self.settings.play_order.repeat != Repeat::One || self.loop_file_overridden() {
            return;
        }
        for (name, e) in self.player.apply_sync(&[("loop-file", "inf".to_owned())]) {
            eprintln!("[vitascope] 無法套用 {name}：{e}");
        }
        self.loop_file_sent = "inf";
    }

    /// 檔案載入完成：這個檔案能不能跳回開頭（直接問 mpv，屬性通知可能還沒到），loop-file 跟著調整
    pub(super) fn play_order_file_loaded(&mut self) {
        self.unseekable_file = self.player.get_string("seekable").is_ok_and(|v| v == "no");
        self.apply_loop_file();
    }

    /// mpv 現在會不會自己重播目前的檔案（直接問 mpv：設定失敗、使用者自己指定了都算在內）
    fn loop_file_active(&self) -> bool {
        self.player
            .get_string("loop-file")
            .is_ok_and(|v| !v.is_empty() && v != "no")
    }

    /// 實際的重複方式：「全部」要開著「播完自動播放下一個」才算數（手動的上一個 / 下一個繞不繞回去）。
    /// 選單、按鈕、設定頁顯示的也是這個（存的「全部」等「播完自動播放下一個」打開後才又有效）
    pub(super) fn effective_repeat(&self) -> Repeat {
        match self.settings.play_order.repeat {
            Repeat::All if !self.settings.auto_next => Repeat::Off,
            r => r,
        }
    }

    /// VITASCOPE_MPV_OPTS 指定的 loop-file（inf、次數…）讓 mpv 自己重播目前的檔案：影戲的重複播放沒有作用，
    /// 選單、播放清單的按鈕、設定頁的重複播放停用（滑鼠移上去說明原因），快捷鍵只提示。
    /// 指定 loop-file=no 時照常能選：重複單一檔案改由播完時重播（[`end_step`] 的 RestartSame）
    pub(super) fn repeat_locked(&self) -> bool {
        self.loop_file_overridden() && self.loop_file_active()
    }

    /// 介面上顯示的重複方式（選單、按鈕、設定頁、控制列的 🔂）：mpv 被指定一直重播時是單一檔案，
    /// 其他時候是 [`Self::effective_repeat`]
    pub(super) fn shown_repeat(&self) -> Repeat {
        if self.repeat_locked() {
            Repeat::One
        } else {
            self.effective_repeat()
        }
    }

    /// 快捷鍵 `cycle-repeat`：從顯示的重複方式換到下一個；被 VITASCOPE_MPV_OPTS 鎖住時只提示原因
    pub(super) fn cycle_repeat(&mut self) {
        if self.repeat_locked() {
            self.osd(crate::tf!("重複播放：{}", "Repeat: {}", adjust_locked_hover()));
        } else {
            self.set_repeat(next_repeat(self.effective_repeat()));
        }
    }

    // ───────────── 上一個 / 下一個 ─────────────

    /// 上一個 / 下一個檔案能不能按（控制列、右鍵選單；之後的系統媒體控制也用這個）。
    /// 重複全部時最後一個的下一個是第一個（隨機時是新的一輪）、第一個的上一個是最後一個（只有照清單順序時）
    pub(super) fn can_step(&self, forward: bool) -> bool {
        let Some(list) = &self.playlist else {
            return false;
        };
        let wrap = self.effective_repeat() == Repeat::All;
        if forward {
            list.next_index().is_some() || (wrap && list.can_wrap())
        } else {
            list.prev_index().is_some() || (wrap && list.last_index().is_some())
        }
    }

    /// 播放清單的上一個 / 下一個檔案（PgUp / PgDn、控制列的按鈕）。隨機時照洗好的順序，PgUp 沿著播過的往回走
    pub(super) fn step_file(&mut self, forward: bool) {
        let wrap = self.effective_repeat() == Repeat::All;
        let target = self.playlist.as_mut().and_then(|l| {
            let step = if forward { l.next_index() } else { l.prev_index() };
            match step {
                Some(i) => Some((i, if forward { StepKind::Next } else { StepKind::Prev })),
                None if wrap && forward => l.wrap_index(&mut self.shuffler).map(|i| (i, StepKind::Wrap)),
                None if wrap => l.last_index().map(|i| (i, StepKind::Prev)),
                None => None,
            }
        });
        match target {
            Some((i, kind)) => {
                self.open_list_item(i);
                // 沒送出開檔（空的播放清單檔之類）時，開檔自己的提示、錯誤畫面比較有用
                if self.switching_file {
                    self.step_osd(kind);
                }
            }
            None => self.osd(if forward {
                crate::tr!("已經是最後一個檔案", "This is the last file")
            } else {
                crate::tr!("已經是第一個檔案", "This is the first file")
            }),
        }
    }

    /// 開清單上的第 `i` 項（回傳開的路徑）
    fn open_list_item(&mut self, i: usize) -> Option<PathBuf> {
        let path = self.playlist.as_ref()?.items().get(i)?.clone();
        self.open_at(&path, Some(i));
        Some(path)
    }

    /// 換檔的提示。位置是播放順序裡的第幾個：隨機時是「這一輪的第幾個」
    fn step_osd(&mut self, kind: StepKind) {
        let Some(list) = &self.playlist else { return };
        let Some(path) = list.current() else { return };
        let (pos, len) = (list.order_position(), list.len());
        let name = display_label(&self.titles, path);
        let text = match kind {
            StepKind::Wrap => crate::tf!(
                "從頭重播清單（{pos}/{len}）：{name}",
                "Playlist restarted ({pos}/{len}): {name}"
            ),
            StepKind::Next if list.is_shuffled() => crate::tf!(
                "下一個（隨機，{pos}/{len}）：{name}",
                "Next (shuffle, {pos}/{len}): {name}"
            ),
            StepKind::Next => crate::tf!("下一個（{pos}/{len}）：{name}", "Next ({pos}/{len}): {name}"),
            StepKind::Prev if list.is_shuffled() => crate::tf!(
                "上一個（隨機，{pos}/{len}）：{name}",
                "Previous (shuffle, {pos}/{len}): {name}"
            ),
            StepKind::Prev => crate::tf!("上一個（{pos}/{len}）：{name}", "Previous ({pos}/{len}): {name}"),
        };
        self.osd(text);
    }

    // ───────────── 播完時 ─────────────

    /// 一個檔案自然播完（[`Self::auto_next`] 偵測到的）：照播放順序接下來做什麼。
    /// `scanning`：同資料夾的檔案還在背景掃描（不知道有沒有下一個）
    pub(super) fn file_ended(&mut self, scanning: bool) {
        let step = end_step(
            self.playlist.as_ref(),
            scanning,
            self.settings.auto_next,
            self.settings.play_order.repeat,
            self.loop_file_active(),
        );
        match step {
            EndStep::Open(i) => {
                self.advance(i, StepKind::Next, HashSet::new());
            }
            EndStep::Wrap => {
                let first = self.playlist.as_mut().and_then(|l| l.wrap_index(&mut self.shuffler));
                match first {
                    Some(i) => {
                        self.advance(i, StepKind::Wrap, HashSet::new());
                    }
                    None => self.restart_same(),
                }
            }
            EndStep::RestartSame => self.restart_same(),
            EndStep::WaitScan => self.pending_auto_next = true,
            EndStep::Finished => self.playback_finished(),
        }
    }

    /// 自動換到清單上的第 `i` 項（播完接下一個、從頭、跳過開不起來的）：開不起來時會再跳過。
    /// `failed_files` = 上次開成功之後開不起來的檔案。回傳開的路徑
    fn advance(&mut self, i: usize, kind: StepKind, failed_files: HashSet<PathBuf>) -> Option<PathBuf> {
        let path = self.open_list_item(i)?;
        // open_at 清掉了（使用者開的檔案不跳過）：開完才記下這是自動換的。
        // 根本沒送出開檔（mpv 不接受這個路徑、空的播放清單檔）也算開不起來，下一幀跳過
        let sent = self.switching_file;
        self.auto_advance = Some(AutoAdvance {
            failed_files,
            started: !sent,
            failed: !sent,
        });
        if sent {
            self.step_osd(kind);
        }
        Some(path)
    }

    /// 重播目前的檔案（重複單一檔案但 mpv 沒有自己重播、重複全部但清單只有它）：跳到開頭繼續播，不重開檔案。
    /// 不能跳轉的（網路串流）重新開
    fn restart_same(&mut self) {
        let st = &self.player.state;
        if !st.loaded {
            return;
        }
        let Some(path) = st.path.clone().map(PathBuf::from) else {
            return;
        };
        if st.seekable {
            let _ = self.player.seek_to(0.0, true);
            // 播完停在最後一格（keep-open 暫停了）
            let _ = self.player.set_pause(false);
            self.frame_stepping = false;
        } else {
            let index = self.playlist.as_ref().and_then(Playlist::current_index);
            self.open_at(&path, index);
        }
        let name = display_label(&self.titles, &path);
        self.osd(crate::tf!("重複播放：{name}", "Repeating: {name}"));
    }

    /// 測試用：播放結束（[`Self::playback_finished`]，「播完後」的動作從那裡開始）了幾次
    #[doc(hidden)]
    pub fn playback_finished_count(&self) -> u64 {
        self.finished_count
    }

    // ───────────── 自動接下一個時開不起來 ─────────────

    /// 收到 StartFile：自動換的檔案開始了
    pub(super) fn auto_advance_started(&mut self) {
        if let Some(a) = &mut self.auto_advance {
            a.started = true;
        }
    }

    /// 收到帶著錯誤的 EndFile：自動換的檔案開不起來的話，下一次 [`Self::auto_next`] 跳過它
    pub(super) fn auto_advance_failed(&mut self) {
        if let Some(a) = &mut self.auto_advance
            && a.started
        {
            a.failed = true;
        }
    }

    /// 自動換的項目讀得到、但沒有東西可以播（網路上的播放清單裡沒有能開的網址）：一樣當成開不起來，
    /// 下一次 [`Self::auto_next`] 跳過它（這種結束沒有帶錯誤，[`Self::auto_advance_failed`] 收不到）
    pub(super) fn auto_advance_nothing_to_play(&mut self) {
        if let Some(a) = &mut self.auto_advance {
            a.started = true;
            a.failed = true;
        }
    }

    /// 自動換的檔案開不起來：跳到下一個（提示是哪個檔案開不起來）；整輪都開不起來、或後面沒有了，播放結束。
    /// 回傳有沒有處理
    pub(super) fn skip_failed(&mut self) -> bool {
        if !self.auto_advance.as_ref().is_some_and(|a| a.failed) {
            return false;
        }
        let mut failed_files = self.auto_advance.take().map(|a| a.failed_files).unwrap_or_default();
        let failed = self
            .playlist
            .as_ref()
            .and_then(Playlist::current)
            .map(Path::to_path_buf);
        failed_files.extend(failed.clone());
        let step = skip_step(
            self.playlist.as_ref(),
            &failed_files,
            self.settings.auto_next,
            self.settings.play_order.repeat,
        );
        let opened = match step {
            EndStep::Open(i) => self.advance(i, StepKind::Next, failed_files),
            EndStep::Wrap => {
                let first = self.playlist.as_mut().and_then(|l| l.wrap_index(&mut self.shuffler));
                first.and_then(|i| self.advance(i, StepKind::Wrap, failed_files))
            }
            _ => None,
        };
        match (opened, failed) {
            (Some(_), Some(failed)) => {
                let name = display_label(&self.titles, &failed);
                self.osd(crate::tf!(
                    "無法開啟：{name}，跳到下一個",
                    "Couldn't open {name}; skipping to the next file"
                ));
            }
            (Some(_), None) => {}
            (None, _) => self.playback_finished(),
        }
        true
    }

    // ───────────── 介面 ─────────────

    /// 右鍵選單「播放順序：… ▸」：播完自動播放下一個、隨機播放、重複（不重複 / 單一檔案 / 全部）。
    /// 「播完自動播放下一個」在這裡直接改、存檔；其他的回傳要做的動作
    pub(super) fn play_order_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let order = self.settings.play_order;
        let repeat = self.shown_repeat();
        let locked = self.repeat_locked();
        let auto_next = self.settings.auto_next;
        let shuffle_hint = self.keymap.hint(Command::ToggleShuffle);
        let cycle_hint = self.keymap.hint(Command::CycleRepeat);
        let mut auto = auto_next;
        let mut action = None;
        let title = crate::tf!("播放順序：{}", "Play order: {}", order_summary(order.shuffle, repeat));
        ui.menu_button(title, |ui| {
            ui.checkbox(
                &mut auto,
                crate::tr!("播完自動播放下一個", "Play the next file automatically"),
            );
            let mut shuffle = order.shuffle;
            let r = with_hint(ui, &shuffle_hint, |ui| {
                ui.checkbox(&mut shuffle, crate::tr!("隨機播放", "Shuffle"))
            });
            if r.changed() {
                action = Some(Action::SetShuffle(shuffle));
            }
            ui.separator();
            for choice in [Repeat::Off, Repeat::One, Repeat::All] {
                let mut r = ui.add_enabled(
                    !locked,
                    egui::RadioButton::new(repeat == choice, repeat_choice_label(choice)),
                );
                r = r.on_disabled_hover_text(adjust_locked_hover());
                if choice == Repeat::All && !auto_next {
                    r = r.on_hover_text(crate::tr!(
                        "會一併打開「播完自動播放下一個」",
                        "This also turns on “Play the next file automatically”"
                    ));
                }
                // 顯示的是實際的重複方式：存的「全部」沒有作用時，點「不重複」也要真的改掉
                if r.clicked() && (choice != repeat || choice != order.repeat) {
                    action = Some(Action::SetRepeat(choice));
                }
            }
            if !cycle_hint.is_empty() {
                ui.separator();
                ui.weak(crate::tf!(
                    "依序切換重複播放：{cycle_hint}",
                    "Cycle repeat mode: {cycle_hint}"
                ));
            }
        });
        if auto != auto_next {
            self.settings.auto_next = auto;
            self.save_settings();
        }
        action
    }

    /// 播放清單分頁標題列下面的 🔀（隨機播放）、🔁 / 🔂（重複播放：關閉 → 全部 → 單一檔案），
    /// 後面寫現在的播放順序；回傳按了哪一個
    pub(super) fn play_order_buttons(&self, ui: &mut egui::Ui) -> Option<Action> {
        let shuffle = self.settings.play_order.shuffle;
        let repeat = self.shown_repeat();
        let locked = self.repeat_locked();
        let mut action = None;
        // 開著 / 關著用選取的樣子表示（不是只靠顏色深淺，淺色主題也看得出來）
        let button = |ui: &mut egui::Ui, enabled: bool, on: bool, glyph: &str, name: &str| {
            let r = ui.add_enabled(enabled, egui::Button::selectable(on, glyph).min_size(vec2(26.0, 20.0)));
            // 螢幕閱讀器、介面測試讀到的是名稱，不是圖示
            r.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, name));
            r
        };
        let name = crate::tr!("隨機播放", "Shuffle");
        let hover = self.keymap.labeled(shuffle_state(shuffle), Command::ToggleShuffle);
        if button(ui, true, shuffle, "🔀", name).on_hover_text(hover).clicked() {
            action = Some(Action::SetShuffle(!shuffle));
        }
        let next = next_repeat(repeat);
        let name = crate::tf!("重複播放：{}", "Repeat: {}", repeat_label(repeat));
        let glyph = if repeat == Repeat::One { "🔂" } else { "🔁" };
        let hover = crate::tf!(
            "{}\n點一下改成：{}",
            "{}\nClick for: {}",
            self.keymap.labeled(&name, Command::CycleRepeat),
            repeat_label(next)
        );
        if button(ui, !locked, repeat != Repeat::Off, glyph, &name)
            .on_hover_text(hover)
            .on_disabled_hover_text(adjust_locked_hover())
            .clicked()
        {
            action = Some(Action::SetRepeat(next));
        }
        ui.weak(order_summary(shuffle, repeat));
        action
    }

    /// 控制列上的狀態圖示（圖示, 說明）：🔀 隨機播放開著、🔂 重複單一檔案（檔案會一直重播，看得出來比較好）。
    /// 重複全部不放（清單面板、選單看得到）
    pub(super) fn order_indicators(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.settings.play_order.shuffle {
            out.push(("🔀", self.keymap.labeled(shuffle_state(true), Command::ToggleShuffle)));
        }
        if self.shown_repeat() == Repeat::One {
            let name = crate::tf!("重複播放：{}", "Repeat: {}", repeat_label(Repeat::One));
            out.push(("🔂", self.keymap.labeled(&name, Command::CycleRepeat)));
        }
        out
    }

    /// 播放清單上「下一個」的那一項（只在隨機播放時標出來：照清單順序時就是下一列）
    pub(super) fn shuffled_next(&self) -> Option<usize> {
        let list = self.playlist.as_ref().filter(|l| l.is_shuffled())?;
        list.next_index().filter(|&i| Some(i) != list.current_index())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_cycles_off_all_one() {
        assert_eq!(next_repeat(Repeat::Off), Repeat::All);
        assert_eq!(next_repeat(Repeat::All), Repeat::One);
        assert_eq!(next_repeat(Repeat::One), Repeat::Off);
    }

    #[test]
    fn order_summary_names_every_combination() {
        let mut seen = HashSet::new();
        for shuffle in [false, true] {
            for repeat in [Repeat::Off, Repeat::One, Repeat::All] {
                assert!(seen.insert(order_summary(shuffle, repeat)), "{shuffle} {repeat:?}");
            }
        }
        assert_eq!(order_summary(false, Repeat::Off), "依序");
        assert_eq!(order_summary(true, Repeat::All), "隨機・重複全部");
    }

    #[test]
    fn loop_file_follows_repeat_one_only() {
        assert_eq!(loop_file_value(Repeat::One, false), "inf");
        assert_eq!(loop_file_value(Repeat::Off, false), "no");
        assert_eq!(
            loop_file_value(Repeat::All, false),
            "no",
            "重複全部是清單的事，不是 mpv 的"
        );
        // 不能跳轉的：mpv 跳不回開頭，由影戲重新開
        for repeat in [Repeat::Off, Repeat::One, Repeat::All] {
            assert_eq!(loop_file_value(repeat, true), "no", "{repeat:?}");
        }
    }
}
