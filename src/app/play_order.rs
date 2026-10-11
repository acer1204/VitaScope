//! 播放順序接上播放器：隨機播放、重複播放、一個檔案播完時接下來做什麼（[`end_step`]），
//! 上一個 / 下一個檔案，自動接下一個時開不起來的檔案跳過。
//!
//! mpv 自己的清單永遠只有一項：接下來播哪一個都由 [`Playlist`] 決定。重複單一檔案用 mpv 的 `loop-file`
//! （幾乎無縫，而且根本不會「播完」）；VITASCOPE_MPV_OPTS 自己指定了 loop-file 時不動它，播完時由這裡重播。
//! 選單、清單面板的按鈕、設定頁在之後的步驟。

use super::{VitascopeApp, display_label, mpv_opts_override};
use crate::player::AsyncKey;
use crate::playlist::{EndStep, Playlist, Repeat, end_step, skip_step};
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
        self.osd(if on {
            crate::tr!("隨機播放：開啟", "Shuffle: on")
        } else {
            crate::tr!("隨機播放：關閉", "Shuffle: off")
        });
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

    /// 實際的重複方式：「全部」要開著「播完自動播放下一個」才算數（手動的上一個 / 下一個繞不繞回去）
    fn effective_repeat(&self) -> Repeat {
        match self.settings.play_order.repeat {
            Repeat::All if !self.settings.auto_next => Repeat::Off,
            r => r,
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

    /// 播放結束了（清單播完、沒開自動播放下一個、剩下的都開不起來）。「播完後」的動作之後從這裡開始
    pub(super) fn playback_finished(&mut self) {
        self.finished_count += 1;
    }

    /// 測試用：播放結束（[`Self::playback_finished`]）了幾次
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
