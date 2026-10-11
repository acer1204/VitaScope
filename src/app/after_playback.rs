//! 播完後的動作（右鍵選單「播完後：… ▸」、「設定 → 播放」、控制列的 ⏳）：倒數、取消、執行。
//!
//! - 只對這次有效（不存檔）；執行、失敗或取消之後回到「不做任何事」。
//! - 播放結束（[`VitascopeApp::playback_finished`]：清單播完、沒開自動播放下一個、剩下的都開不起來）才開始倒數；
//!   還在匯出時等匯出做完。倒數畫面是 `egui::Modal`：底下的按鈕、畫面都點不到，點哪裡都是取消。
//! - 取消倒數：任何按鍵（`handle_keys` 的最前面，輸入文字時也一樣；按鍵被吃掉，空白鍵不會順便暫停）、點一下、
//!   捲動滾輪、移動滑鼠（超過幾點）、整個系統有鍵盤或滑鼠的動作（[`SystemPower::last_input_age`]，Wayland 查不到）、
//!   開了檔案對話框、開檔、停止、任何操作。
//! - 倒數時視窗還原、移到最上層、暫時置頂（`Launch.countdown_raise`，main.rs 打開；自動測試不動視窗）。
//! - 睡眠、休眠、關機要有 `Launch.system`：沒有時（自動測試、`--shot`）選單不列出。系統能做哪些事在背景問：
//!   啟動時、每次打開「播完後 ▸」或設定頁的選單、選了動作時（選不了也一樣）再問一次（休眠開關、別的使用者登入、polkit 規則可能變）。
//!   選了動作時也先取得要的權限（[`SystemPower::prepare`]：macOS 的關機在這時跳出系統的詢問視窗，人還在電腦前面）。
//!
//! [`SystemPower::prepare`]: crate::system_power::SystemPower::prepare

use super::{Action, INHIBIT_RELEASE_WAIT, VitascopeApp};
use crate::keymap::Command;
use crate::playlist::Repeat;
use crate::system_power::{AfterPlayback, Avail, Countdown, PowerAction, PowerError, Tick, countdown_len};
use crate::theme::{self, Palette};
use crate::{tf, tr};
use eframe::egui::{self, Id, Pos2, RichText, Vec2, ViewportCommand};
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

/// 滑鼠移動超過這麼多點才算（手碰到桌子的抖動不算）
const POINTER_SLOP: f32 = 8.0;
/// 倒數開始（或把視窗叫到前面）後這段時間內不看滑鼠的位置：視窗還原、移到最上層時，滑鼠相對於視窗的位置會變
/// （不是使用者動了滑鼠）。叫過視窗時修飾鍵、系統的輸入也一樣（見 [`Running::raised`]）。
/// 很短的倒數（測試）用倒數的四分之一
const POINTER_GRACE: Duration = Duration::from_millis(400);
/// 有 `SystemPower` 時，倒數期間至少這麼久看一次系統的輸入（倒數畫面才不會在使用者回來之後還停留一秒）
const SYSTEM_INPUT_POLL: Duration = Duration::from_millis(250);

/// 播完之後、執行之前
pub(super) enum Pending {
    /// 播完了，但還在匯出：做完才開始倒數
    WaitExport(AfterPlayback),
    /// 倒數中（畫面上有倒數）
    Running(Running),
}

pub(super) struct Running {
    timer: Countdown,
    /// 開始（或叫視窗）之後這段時間內不看滑鼠（見 [`POINTER_GRACE`]）
    grace: Duration,
    /// 倒數開始、或最後一次把視窗叫到前面的時間（`grace` 從這裡算）
    settle_from: Instant,
    /// 把視窗叫到前面過：Windows 的 winit 叫視窗（`ViewportCommand::Focus`）時會模擬按一下 Alt（`SendInput`），
    /// 系統會把它當成鍵盤的動作（[`SystemPower::last_input_age`]），視窗也可能收到 Alt 的修飾鍵變化。
    /// 所以叫過視窗時，`grace` 內的修飾鍵、系統的輸入都不算（真的有人的話，之後的輸入照樣取消）
    ///
    /// [`SystemPower::last_input_age`]: crate::system_power::SystemPower::last_input_age
    raised: bool,
    /// 還原縮到最小的視窗之後要再叫一次到前面（macOS 還原有動畫，同一幀的 `Focus` 叫不到前面）
    refocus: bool,
    /// 過了 `grace` 之後記下的滑鼠位置（外層 None = 還沒記；裡面 None = 滑鼠不在視窗上）
    anchor: Option<Option<Pos2>>,
    /// 過了 `grace` 之後滑鼠實際移動的淨位移（裝置回報的，不受視窗位置影響）。
    /// 加的是向量：來回的抖動互相抵掉，倒數再久也不會累積成「移動」
    raw_motion: Vec2,
}

impl Running {
    fn new(action: AfterPlayback, len: Duration, now: Instant) -> Self {
        Self {
            timer: Countdown::new(action, len, now),
            grace: POINTER_GRACE.min(len / 4),
            settle_from: now,
            raised: false,
            refocus: false,
            anchor: None,
            raw_motion: Vec2::ZERO,
        }
    }

    /// 剛開始或剛叫過視窗（`grace` 內）
    fn settling(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.settle_from) < self.grace
    }

    /// 現在把視窗叫到前面了：`grace` 重新算，滑鼠的位置重新記
    fn raised_at(&mut self, now: Instant) {
        self.raised = true;
        self.settle_from = now;
        self.anchor = None;
        self.raw_motion = Vec2::ZERO;
    }

    /// 這一幀滑鼠有沒有移動（`pos` = 滑鼠在視窗裡的位置，`motion` = 這一幀裝置回報的移動量）
    fn pointer_moved(&mut self, pos: Option<Pos2>, motion: Vec2, now: Instant) -> bool {
        if self.settling(now) {
            return false;
        }
        self.raw_motion += motion;
        let anchor = *self.anchor.get_or_insert(pos);
        self.raw_motion.length() > POINTER_SLOP || pointer_moved(anchor, pos)
    }

    /// 倒數開始之後系統有沒有鍵盤、滑鼠的動作（`idle` = 系統多久沒有輸入）。
    /// 叫過視窗時從最後一次叫視窗之後的 `grace` 算起（叫視窗模擬的 Alt 不算）
    fn system_input(&self, idle: Duration, now: Instant) -> bool {
        if !self.raised {
            return self.timer.input_since_start(idle, now);
        }
        let from = self.settle_from + self.grace;
        now.checked_sub(idle).is_some_and(|last_input| last_input >= from)
    }

    /// 這個事件算不算使用者的輸入（叫過視窗、還在 `grace` 內時，修飾鍵的變化可能是叫視窗模擬的 Alt：不算）
    fn counts(&self, e: &egui::Event, now: Instant) -> bool {
        match e {
            egui::Event::ModifiersChanged(_) if self.raised && self.settling(now) => false,
            e => is_user_input(e),
        }
    }
}

/// 滑鼠從 `from` 移到 `to` 算不算移動：超過幾點，或移進、移出視窗
fn pointer_moved(from: Option<Pos2>, to: Option<Pos2>) -> bool {
    match (from, to) {
        (Some(a), Some(b)) => a.distance(b) > POINTER_SLOP,
        (None, None) => false,
        _ => true,
    }
}

/// 倒數時算「使用者在」的輸入事件（會被吃掉，不再給其他地方）。
/// 只按修飾鍵（Shift、Ctrl…）沒有按鍵事件：看修飾鍵變成有按著。視窗得到焦點時也會送修飾鍵的狀態（都沒按著），那不算
fn is_user_input(e: &egui::Event) -> bool {
    match e {
        egui::Event::Key { pressed, .. } => *pressed,
        egui::Event::ModifiersChanged(m) => !m.is_none(),
        egui::Event::Text(_)
        | egui::Event::Copy
        | egui::Event::Cut
        | egui::Event::Paste(_)
        | egui::Event::Ime(_)
        | egui::Event::PointerButton { .. }
        | egui::Event::MouseWheel { .. }
        | egui::Event::Zoom(_)
        | egui::Event::Rotate(_)
        | egui::Event::Touch { .. } => true,
        _ => false,
    }
}

/// 取消倒數的原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cancel {
    /// 使用者的動作（按鍵、點擊、滑鼠、系統其他地方的輸入、開了檔案對話框、媒體鍵）：提示「已取消」
    Input,
    /// 開了別的檔案、停止：不提示（新檔案自己的提示比較有用）
    Quiet,
}

impl VitascopeApp {
    // ───────────── 選擇 ─────────────

    /// 播完後的動作（介面測試用）
    #[doc(hidden)]
    pub fn after_playback(&self) -> AfterPlayback {
        self.after_playback
    }

    /// 倒數還剩幾秒（沒有在倒數時 None；介面測試用）
    #[doc(hidden)]
    pub fn countdown_remaining(&self) -> Option<u64> {
        match &self.countdown {
            Some(Pending::Running(run)) => Some(match run.timer.tick(Instant::now()) {
                Tick::Wait { remaining_secs, .. } => remaining_secs,
                Tick::Fire(_) => 0,
            }),
            _ => None,
        }
    }

    /// 播完了、等匯出做完才倒數（介面測試用）
    #[doc(hidden)]
    pub fn countdown_waiting_for_export(&self) -> bool {
        matches!(self.countdown, Some(Pending::WaitExport(_)))
    }

    /// 還在背景問系統能做哪些事（結果還沒處理；介面測試用：等它問完再改假系統的回答）
    #[doc(hidden)]
    pub fn power_probe_pending(&self) -> bool {
        self.caps_probe.is_some()
    }

    /// 選了動作之後還在取得權限（[`SystemPower::prepare`] 的結果還沒處理；介面測試用）
    ///
    /// [`SystemPower::prepare`]: crate::system_power::SystemPower::prepare
    #[doc(hidden)]
    pub fn power_prepare_pending(&self) -> bool {
        self.prepare_job.is_some()
    }

    /// 播完後的「關閉影戲」送出了關閉視窗（介面測試用：測試的視窗不會真的關）
    #[doc(hidden)]
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// 這個動作在選單裡的樣子：None = 不列出（睡眠、休眠、關機要有 `Launch.system`；系統不支援休眠時也不列出），
    /// 其他 = 能不能選（不能的話停用，滑鼠移上去說明原因）
    pub(super) fn after_avail(&self, a: AfterPlayback) -> Option<Avail> {
        let Some(power) = a.power() else {
            return Some(Avail::Yes);
        };
        self.system.as_ref()?;
        match self.power_caps.of(power) {
            // 休眠：系統沒有（Windows 沒開休眠、macOS、logind 說 no / na）就不列出。
            // 只是要密碼的（logind 的 challenge）照樣列出、停用並說明：使用者可能想知道為什麼不能選
            Avail::No(e) if power == PowerAction::Hibernate && *e != PowerError::NeedsPassword => None,
            avail => Some(avail.clone()),
        }
    }

    /// 選不了這個動作的原因（None = 可以選）
    fn after_refusal(&self, a: AfterPlayback) -> Option<String> {
        match self.after_avail(a) {
            Some(Avail::Yes) => None,
            Some(Avail::No(e)) => Some(e.message()),
            Some(Avail::Unknown) => {
                Some(tr!("還在檢查系統是否支援", "Still checking whether the system supports it").into())
            }
            // 不列出的（沒有 SystemPower、系統沒有休眠）：系統說的原因（用快捷鍵選的時候看得到）
            None => Some(
                match a
                    .power()
                    .filter(|_| self.system.is_some())
                    .map(|p| self.power_caps.of(p))
                {
                    Some(Avail::No(e)) => e.message(),
                    _ => PowerError::NotSupported.message(),
                },
            ),
        }
    }

    /// 選擇播完後的動作（右鍵選單、設定頁、快捷鍵、控制列的 ⏳ 取消都走這裡）：只對這次有效、不存檔
    pub fn set_after_playback(&mut self, a: AfterPlayback) {
        if let Some(why) = self.after_refusal(a) {
            self.osd(refused(a, &why));
            // 說不行用的是上次問的結果（可能過時了，例如剛打開休眠）：再問一次，下次選的時候用新的
            if a.power().is_some() {
                self.probe_power_caps();
            }
            return;
        }
        // 倒數中不會走到這裡（按鍵、點擊先取消了倒數）；保險起見先不出聲地取消
        if matches!(self.countdown, Some(Pending::Running(_))) {
            self.cancel_countdown(Cancel::Quiet);
        }
        self.after_playback = a;
        // 等匯出時改了：照新的做（改成「不做任何事」就不等了）
        if let Some(Pending::WaitExport(waiting)) = &mut self.countdown {
            if a == AfterPlayback::Nothing {
                self.countdown = None;
            } else {
                *waiting = a;
            }
        }
        let mut text = tf!("播完後：{}", "When playback ends: {}", a.noun());
        if a != AfterPlayback::Nothing && self.shown_repeat() != Repeat::Off {
            text.push_str(repeating_note());
        }
        self.osd(text);
        // 睡眠、休眠、關機：人在電腦前面的時候先取得權限（macOS 的關機會跳出系統的詢問），
        // 也再問一次系統現在能不能做（之前問的可能過時了）。結果在 `poll_power` 處理
        if let Some(power) = a.power() {
            self.prepare_power(a, power);
            self.probe_power_caps();
        }
    }

    /// 在背景先取得執行 `power` 要的權限（[`SystemPower::prepare`]，可能等使用者回答系統的詢問）
    ///
    /// [`SystemPower::prepare`]: crate::system_power::SystemPower::prepare
    fn prepare_power(&mut self, a: AfterPlayback, power: PowerAction) {
        let Some(system) = self.system.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let wake = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("vitascope-power-prepare".into())
            .spawn(move || {
                let _ = tx.send(system.prepare(power));
                wake.request_repaint();
            });
        // 開不了執行緒：不擋（執行時系統還會再說一次）
        if spawned.is_ok() {
            self.prepare_job = Some((a, rx));
        }
    }

    /// 選好的動作現在做不到了（取得權限被拒、再問一次系統說不行）：說明原因，回到「不做任何事」
    fn after_no_longer_possible(&mut self, a: AfterPlayback, e: &PowerError) {
        if self.after_playback != a {
            return;
        }
        let pending = match &self.countdown {
            Some(Pending::WaitExport(w)) => Some(*w),
            Some(Pending::Running(run)) => Some(run.timer.action()),
            None => None,
        };
        if pending == Some(a) {
            self.countdown = None;
        }
        self.after_playback = AfterPlayback::Nothing;
        self.osd(refused(a, &e.message()));
    }

    // ───────────── 播放結束 → 倒數 ─────────────

    /// 介面測試用：現在就當成播放結束（下一幀就是倒數開始的那一幀：倒數畫面還沒畫過，
    /// 底下的東西還點得到、`modal_open` 還沒設，測這一幀的按鍵、點擊）
    #[doc(hidden)]
    pub fn finish_playback_now(&mut self) {
        self.playback_finished();
    }

    /// 播放結束了（清單播完、沒開自動播放下一個、剩下的都開不起來）：有選播完後的動作就開始倒數
    pub(super) fn playback_finished(&mut self) {
        self.finished_count += 1;
        let a = self.after_playback;
        if a == AfterPlayback::Nothing || self.countdown.is_some() {
            return;
        }
        // 現在做不到（系統說不行、還在問）：說明原因，回到不做任何事（不等，人可能已經不在了）
        if let Some(power) = a.power()
            && self.after_avail(a) != Some(Avail::Yes)
        {
            let e = match self.after_avail(a) {
                Some(Avail::No(e)) => e,
                _ => PowerError::NotSupported,
            };
            self.osd(power.failed(&e));
            self.after_playback = AfterPlayback::Nothing;
            return;
        }
        self.begin_countdown(a);
    }

    /// 播完了（或等到匯出做完了）：開著檔案對話框就取消，還在匯出就等，不然開始倒數
    fn begin_countdown(&mut self, a: AfterPlayback) {
        // 開著檔案對話框：使用者就在電腦前面（Windows 的主視窗這時點不到，倒數畫面也按不到「取消」）。
        // 等匯出的時候才開的也一樣：不能先倒數、把視窗叫到對話框前面
        if self.dialog.is_some() {
            self.osd(a.cancelled());
            self.after_playback = AfterPlayback::Nothing;
            return;
        }
        if self.export_busy() {
            self.countdown = Some(Pending::WaitExport(a));
            self.osd(a.after_export());
            return;
        }
        self.start_countdown(a);
    }

    fn start_countdown(&mut self, a: AfterPlayback) {
        let len = self.countdown_override.unwrap_or_else(|| countdown_len(a));
        let now = Instant::now();
        let mut run = Running::new(a, len, now);
        let ctx = self.egui_ctx.clone();
        if self.countdown_raise {
            run.refocus = ctx.input(|i| i.viewport().minimized) == Some(true);
            self.raise_window(&ctx);
            run.raised_at(now);
        }
        self.countdown = Some(Pending::Running(run));
        ctx.request_repaint();
    }

    /// 倒數時把視窗叫出來：縮到最小的還原、叫到前面；系統不讓搶前景時閃工作列 / 跳 Dock。
    /// 置頂見 `countdown_on_top`（`on_top_now`）
    fn raise_window(&mut self, ctx: &egui::Context) {
        // macOS：整個程式被隱藏（⌘H）時視窗看不到，winit 的 `Focus` 也不做事（只對看得到的視窗）
        #[cfg(target_os = "macos")]
        unhide_app();
        self.bring_to_front(ctx);
    }

    /// 正在倒數（畫面上有倒數）
    pub(super) fn countdown_running(&self) -> bool {
        matches!(self.countdown, Some(Pending::Running(_)))
    }

    /// 倒數時視窗暫時置頂（Wayland 不能設置頂）
    pub(super) fn countdown_on_top(&self) -> bool {
        self.countdown_raise && !self.wayland && self.countdown_running()
    }

    /// 取消倒數（或等匯出），回到「不做任何事」；視窗的層級在這一幀的 `keep_window_level` 還原
    pub(super) fn cancel_countdown(&mut self, why: Cancel) {
        let Some(pending) = self.countdown.take() else {
            return;
        };
        let a = match pending {
            Pending::WaitExport(a) => a,
            Pending::Running(run) => run.timer.action(),
        };
        self.after_playback = AfterPlayback::Nothing;
        if why == Cancel::Input {
            self.osd(a.cancelled());
        }
    }

    /// 倒數中的按鍵、點擊、滑鼠（`handle_keys` 的最前面，正在輸入文字、對話框開著時也一樣）：
    /// 有的話取消倒數、把那些事件吃掉（不會再變成快捷鍵、打進輸入框），回傳 true（這一幀不再處理按鍵）
    pub(super) fn countdown_input(&mut self, ctx: &egui::Context) -> bool {
        let Some(Pending::Running(run)) = &mut self.countdown else {
            return false;
        };
        let now = Instant::now();
        let pressed = ctx.input_mut(|i| {
            let before = i.events.len();
            i.events.retain(|e| !run.counts(e, now));
            i.events.len() != before
        });
        let (pos, motion) = ctx.input(|i| {
            let motion = i.events.iter().fold(Vec2::ZERO, |sum, e| match e {
                egui::Event::MouseMoved(d) => sum + *d,
                _ => sum,
            });
            (i.pointer.latest_pos(), motion)
        });
        let moved = run.pointer_moved(pos, motion, now);
        if !(pressed || moved) {
            return false;
        }
        self.cancel_countdown(Cancel::Input);
        // 這一幀的點擊、滾輪不要再給影片畫面（倒數畫面出現的第一幀，底下還點得到）。
        // 按下的鍵還按著的話，放開那一幀也不給（egui 放開時才算點擊，見 `logic` 的開頭）
        self.countdown_ate_input = true;
        self.countdown_press_held = ctx.input(|i| i.pointer.any_down());
        self.last_activity = now;
        true
    }

    /// 倒數或剛被輸入取消：影片畫面這一幀不處理點擊、滾輪
    pub(super) fn countdown_blocks_video(&self) -> bool {
        self.countdown_ate_input || self.countdown_running()
    }

    /// 每一幀（按鍵處理之後）：等匯出、看系統的輸入、檔案對話框、時間到了沒
    pub(super) fn countdown_tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(Pending::WaitExport(a)) = &self.countdown {
            let a = *a;
            // 等的時候又播起來了（從頭播放、跳回前面）：不等了，選的動作留著，下次播完再說
            let st = &self.player.state;
            if st.loaded && !st.eof {
                self.countdown = None;
                return;
            }
            // 匯出的進度、結果會叫醒介面。做完了：跟剛播完一樣（檔案對話框開著就取消）
            if !self.export_busy() {
                self.countdown = None;
                self.begin_countdown(a);
            }
            return;
        }
        let idle = self.system.as_ref().and_then(|s| s.last_input_age());
        let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        let dialog = self.dialog.is_some();
        let Some(Pending::Running(run)) = &mut self.countdown else {
            return;
        };
        // 開了檔案對話框（使用者在）；整個系統有鍵盤、滑鼠的動作（在別的程式裡打字、移動滑鼠）
        if dialog || idle.is_some_and(|idle| run.system_input(idle, now)) {
            self.cancel_countdown(Cancel::Input);
            return;
        }
        // 縮到最小的視窗還原好了：再叫一次到前面
        let refocus = run.refocus && !minimized;
        if refocus {
            run.refocus = false;
            run.raised_at(now);
        }
        let tick = run.timer.tick(now);
        if refocus {
            self.raise_window(ctx);
        }
        match tick {
            Tick::Wait { repaint_in, .. } => {
                let wait = if self.system.is_some() {
                    repaint_in.min(SYSTEM_INPUT_POLL)
                } else {
                    repaint_in
                };
                ctx.request_repaint_after(wait);
            }
            Tick::Fire(a) => self.fire(ctx, a),
        }
    }

    /// 時間到：先存檔（Windows、logind 關機時可能來不及跑 `on_exit`），放掉讓螢幕不關閉的要求，再做
    fn fire(&mut self, ctx: &egui::Context, a: AfterPlayback) {
        self.countdown = None;
        self.after_playback = AfterPlayback::Nothing;
        self.remember_position();
        self.save_settings();
        self.persist_playlist();
        // 自己的要求不能擋住自己的睡眠、關機：先放掉（背景執行緒執行前等它做完）
        let inhibitor = self.release_inhibit();
        match a {
            AfterPlayback::Nothing => {}
            AfterPlayback::Close => {
                self.close_requested = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            AfterPlayback::Sleep | AfterPlayback::Hibernate | AfterPlayback::Shutdown => {
                let (Some(power), Some(system)) = (a.power(), self.system.clone()) else {
                    return;
                };
                let (tx, rx) = mpsc::channel();
                let wake = ctx.clone();
                // Windows 的睡眠醒來後才回傳、Linux 可能跳出 polkit 的密碼視窗：不能卡住介面
                let spawned = std::thread::Builder::new()
                    .name("vitascope-power-action".into())
                    .spawn(move || {
                        if let Some(inhibitor) = inhibitor
                            && !inhibitor.settle(INHIBIT_RELEASE_WAIT)
                        {
                            eprintln!("[vitascope] 讓螢幕不關閉的要求還沒放掉就執行{power:?}");
                        }
                        let _ = tx.send(system.run(power));
                        wake.request_repaint();
                    });
                match spawned {
                    Ok(_) => self.power_job = Some((power, rx)),
                    Err(e) => self.osd(power.failed(&PowerError::Failed(e.to_string()))),
                }
            }
        }
    }

    /// 在背景問系統能做哪些事（有 `SystemPower` 時）：啟動時、打開「播完後」的選單時、選了動作時。
    /// 已經在問就不再開；問好之前沿用上次的結果（選單不會一直閃「檢查中…」）
    pub(super) fn probe_power_caps(&mut self) {
        let Some(system) = self.system.clone() else {
            return;
        };
        if self.caps_probe.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = self.egui_ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("vitascope-power-caps".into())
            .spawn(move || {
                let _ = tx.send(system.caps());
                wake.request_repaint();
            });
        if spawned.is_ok() {
            self.caps_probe = Some(rx);
        }
    }

    /// 每一幀：系統能做哪些事問好了沒、睡眠 / 關機的結果（失敗時提示原因）
    pub(super) fn poll_power(&mut self) {
        if let Some(rx) = &self.caps_probe {
            match rx.try_recv() {
                Ok(caps) => {
                    self.power_caps = caps;
                    self.caps_probe = None;
                    // 選好的動作現在不能做了（例如休眠被關掉、改成要密碼）：現在就說，不要等到播完
                    let a = self.after_playback;
                    if let Some(power) = a.power()
                        && let Avail::No(e) = self.power_caps.of(power).clone()
                    {
                        self.after_no_longer_possible(a, &e);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.caps_probe = None,
            }
        }
        if let Some((a, rx)) = &self.prepare_job {
            let a = *a;
            match rx.try_recv() {
                Ok(result) => {
                    self.prepare_job = None;
                    if let Err(e) = result {
                        self.after_no_longer_possible(a, &e);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.prepare_job = None,
            }
        }
        if let Some((power, rx)) = &self.power_job {
            let power = *power;
            match rx.try_recv() {
                Ok(result) => {
                    self.power_job = None;
                    if let Err(e) = result {
                        self.osd(power.failed(&e));
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.power_job = None,
            }
        }
    }

    // ───────────── 介面 ─────────────

    /// 倒數畫面（每一幀最後畫，蓋在所有視窗、對話框上面）。點哪裡都是取消（點擊在 `countdown_input` 就取消了；
    /// 這裡的「取消」按鈕是給螢幕閱讀器、介面測試找的）
    pub(super) fn countdown_window(&mut self, ctx: &egui::Context) {
        let Some(Pending::Running(run)) = &self.countdown else {
            return;
        };
        let a = run.timer.action();
        let secs = match run.timer.tick(Instant::now()) {
            Tick::Wait { remaining_secs, .. } => remaining_secs,
            Tick::Fire(_) => 0,
        };
        let id = Id::new("after_playback_countdown");
        // 最後畫的、而且移到最上層：其他對話框（關於、開啟網址…）開著時也蓋在它們上面
        ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
        // 蓋在影片上：淺色主題也是深色的
        let dark = ctx.style_of(egui::Theme::Dark);
        let mut cancel = false;
        let modal = egui::Modal::new(id).frame(egui::Frame::popup(&dark)).show(ctx, |ui| {
            theme::dark_overlay(ui);
            // 迷你播放器（約 320 寬）也放得下
            ui.set_max_width(280.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(tr!("播放完畢", "Playback finished")).strong());
                ui.add_space(4.0);
                let accent = Palette::of(ui.visuals()).accent;
                ui.label(RichText::new(a.countdown_line(secs)).size(22.0).color(accent));
                ui.add_space(6.0);
                cancel = ui.button(tr!("取消（Esc）", "Cancel (Esc)")).clicked();
                ui.add_space(2.0);
                ui.weak(tr!(
                    "按任何鍵、移動滑鼠或點一下就取消",
                    "Press any key, move the mouse or click to cancel"
                ));
            });
        });
        if cancel || modal.backdrop_response.clicked() {
            self.cancel_countdown(Cancel::Input);
        }
        self.modal_open = true;
    }

    /// 右鍵選單「播完後：… ▸」（標題寫著選好的動作，每次打開選單都看得到）。回傳要做的動作
    pub(super) fn after_playback_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let current = self.after_playback;
        let repeating = self.shown_repeat() != Repeat::Off;
        let mut action = None;
        let title = tf!("播完後：{}", "When playback ends: {}", current.noun());
        let shown = ui.menu_button(title, |ui| {
            for a in AfterPlayback::ALL {
                let Some(avail) = self.after_avail(a) else { continue };
                // 使用者指定了按鍵的話寫在右邊（跟其他指令一樣從 keymap 來）
                let hint = self.keymap.hint(command_of(a));
                let mut r = super::with_hint(ui, &hint, |ui| {
                    ui.add_enabled(avail == Avail::Yes, egui::RadioButton::new(current == a, a.label()))
                });
                r = match &avail {
                    Avail::Yes => r,
                    Avail::Unknown => r.on_disabled_hover_text(tr!("檢查中…", "Checking…")),
                    Avail::No(e) => r.on_disabled_hover_text(e.message()),
                };
                if repeating && a != AfterPlayback::Nothing {
                    r = r.on_hover_text(repeating_hover());
                }
                if r.clicked() && a != current {
                    action = Some(Action::SetAfterPlayback(a));
                }
            }
            ui.separator();
            ui.weak(tr!("只對這次有效", "For this session only"));
        });
        self.recheck_power_when_opened(shown.inner.is_some(), false);
        action
    }

    /// 「播完後」的選單（`combo` = 設定頁的下拉選單，否則右鍵選單）剛打開：再問一次系統能做哪些事
    fn recheck_power_when_opened(&mut self, open: bool, combo: bool) {
        let was = if combo {
            std::mem::replace(&mut self.after_combo_open, open)
        } else {
            std::mem::replace(&mut self.after_menu_open, open)
        };
        if open && !was {
            self.probe_power_caps();
        }
    }

    /// 「設定 → 播放」的「播完後」。不算改了設定（不存檔、不會讓設定視窗寫設定檔）：回傳要做的動作
    pub(super) fn after_playback_row(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let current = self.after_playback;
        let mut action = None;
        let mut open = false;
        ui.horizontal(|ui| {
            let label = ui.label(tr!("播完後", "When playback ends"));
            let shown = egui::ComboBox::from_id_salt("settings_after_playback")
                .selected_text(current.label())
                .show_ui(ui, |ui| {
                    for a in AfterPlayback::ALL {
                        let Some(avail) = self.after_avail(a) else { continue };
                        let r = ui.add_enabled(avail == Avail::Yes, egui::Button::selectable(current == a, a.label()));
                        let r = match &avail {
                            Avail::Yes => r,
                            Avail::Unknown => r.on_disabled_hover_text(tr!("檢查中…", "Checking…")),
                            Avail::No(e) => r.on_disabled_hover_text(e.message()),
                        };
                        if r.clicked() && a != current {
                            action = Some(Action::SetAfterPlayback(a));
                        }
                    }
                });
            open = shown.inner.is_some();
            let mut combo = shown.response.labelled_by(label.id);
            if self.shown_repeat() != Repeat::Off {
                combo = combo.on_hover_text(repeating_hover());
            }
            let _ = combo;
        });
        self.recheck_power_when_opened(open, true);
        ui.indent("after_playback_note", |ui| {
            ui.weak(tr!(
                "只對這次有效：下次開啟影戲時是「不做任何事」",
                "For this session only: VitaScope starts with “Do nothing”"
            ));
        });
        action
    }

    /// 控制列的 ⏳（選了播完後的動作時）：（文字, 滑鼠移上去的說明）
    pub(super) fn after_indicator(&self) -> Option<(&'static str, &'static str)> {
        let a = self.after_playback;
        (a != AfterPlayback::Nothing).then(|| (a.indicator(), tr!("點一下取消", "Click to cancel")))
    }
}

/// 選這個動作的快捷鍵指令
fn command_of(a: AfterPlayback) -> Command {
    match a {
        AfterPlayback::Nothing => Command::AfterPlaybackNone,
        AfterPlayback::Close => Command::AfterPlaybackClose,
        AfterPlayback::Sleep => Command::AfterPlaybackSleep,
        AfterPlayback::Hibernate => Command::AfterPlaybackHibernate,
        AfterPlayback::Shutdown => Command::AfterPlaybackShutdown,
    }
}

/// 選不了這個動作（OSD）
fn refused(a: AfterPlayback, why: &str) -> String {
    tf!("播完後無法{}：{why}", "Can't {} when playback ends: {why}", a.noun())
}

/// macOS：程式被隱藏（⌘H）時取消隱藏（倒數要看得到）。winit 只會把看得到的視窗叫到前面
#[cfg(target_os = "macos")]
fn unhide_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    // 介面的執行緒才是主執行緒；不是的話不做（不會發生）
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if app.isHidden() {
        app.unhide(None);
    }
}

/// 重複播放開著時（選單、設定頁的提示）
fn repeating_hover() -> &'static str {
    tr!("重複播放開著時，播放不會結束", "Playback never ends while repeat is on")
}

/// 重複播放開著時選了動作（接在提示後面）
fn repeating_note() -> &'static str {
    tr!(
        "（重複播放開著時，播放不會結束）",
        " (playback never ends while repeat is on)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Modifiers, pos2, vec2};

    #[test]
    fn pointer_movement_needs_more_than_a_few_points() {
        let a = pos2(100.0, 100.0);
        assert!(!pointer_moved(Some(a), Some(pos2(103.0, 104.0))), "5 點的抖動不算");
        assert!(pointer_moved(Some(a), Some(pos2(109.0, 100.0))));
        // 移進、移出視窗都算；一直不在視窗上不算
        assert!(pointer_moved(None, Some(a)));
        assert!(pointer_moved(Some(a), None));
        assert!(!pointer_moved(None, None));
    }

    #[test]
    fn pointer_is_ignored_right_after_the_start_then_anchored() {
        let t0 = Instant::now();
        let none = Vec2::ZERO;
        let mut run = Running::new(AfterPlayback::Close, Duration::from_secs(10), t0);
        let far = pos2(500.0, 500.0);
        // 視窗剛還原、移到最上層：滑鼠的位置變了不算
        assert!(!run.pointer_moved(Some(pos2(10.0, 10.0)), none, t0));
        assert!(!run.pointer_moved(Some(far), vec2(50.0, 0.0), t0 + Duration::from_millis(399)));
        // 之後記下位置，離開它超過幾點才算
        let later = t0 + Duration::from_millis(500);
        assert!(!run.pointer_moved(Some(far), none, later));
        assert!(!run.pointer_moved(Some(pos2(504.0, 503.0)), none, later));
        assert!(run.pointer_moved(Some(pos2(520.0, 500.0)), none, later));
        // 裝置回報的移動（不看視窗裡的位置）往同一個方向累積起來也算
        let mut run = Running::new(AfterPlayback::Close, Duration::from_secs(10), t0);
        assert!(!run.pointer_moved(None, vec2(3.0, 0.0), later));
        assert!(!run.pointer_moved(None, vec2(3.0, 0.0), later));
        assert!(run.pointer_moved(None, vec2(3.0, 0.0), later));
        // 很短的倒數（測試用）：不看滑鼠的時間是倒數的四分之一
        let mut run = Running::new(AfterPlayback::Close, Duration::from_millis(400), t0);
        assert!(!run.pointer_moved(Some(far), none, t0 + Duration::from_millis(99)));
        assert!(!run.pointer_moved(Some(far), none, t0 + Duration::from_millis(101)));
        assert!(run.pointer_moved(None, none, t0 + Duration::from_millis(102)));
    }

    /// 桌子被碰到、感應器的雜訊：來回的小移動倒數再久也不會加起來變成「移動」
    #[test]
    fn jitter_back_and_forth_never_adds_up() {
        let t0 = Instant::now();
        let mut run = Running::new(AfterPlayback::Shutdown, Duration::from_secs(60), t0);
        let later = t0 + Duration::from_secs(1);
        for i in 0..1000 {
            let d = if i % 2 == 0 { vec2(3.0, -2.0) } else { vec2(-3.0, 2.0) };
            assert!(!run.pointer_moved(None, d, later), "第 {i} 下");
        }
        assert!(run.pointer_moved(None, vec2(9.0, 0.0), later), "真的移動照樣算");
    }

    /// 叫過視窗（Windows 的 winit 叫視窗時模擬按一下 Alt）：剛叫的那一下子，修飾鍵、系統的輸入不算；之後照樣取消
    #[test]
    fn raising_the_window_is_not_mistaken_for_the_user() {
        let t0 = Instant::now();
        let alt = egui::Event::ModifiersChanged(Modifiers::ALT);
        let key = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        // 系統最後一次有輸入是 `at`：從 `now` 看，閒置了多久
        let idle = |at: Instant, now: Instant| now - at;
        let len = Duration::from_secs(10);
        // 沒叫視窗（自動測試、--shot）：開始之後的都算
        let run = Running::new(AfterPlayback::Close, len, t0);
        let now = t0 + Duration::from_millis(20);
        assert!(run.counts(&alt, now));
        assert!(run.system_input(idle(t0 + Duration::from_millis(10), now), now));
        // 叫了視窗：grace 內的修飾鍵、系統的輸入不算（按鍵照樣算）
        let mut run = Running::new(AfterPlayback::Close, len, t0);
        run.raised_at(t0);
        assert!(!run.counts(&alt, now), "叫視窗模擬的 Alt");
        assert!(run.counts(&key, now), "真的按鍵");
        assert!(!run.system_input(idle(t0 + Duration::from_millis(10), now), now));
        let after = t0 + Duration::from_millis(700);
        assert!(
            !run.system_input(idle(t0 + Duration::from_millis(10), after), after),
            "之後再看，叫視窗時的輸入還是不算"
        );
        assert!(run.counts(&alt, after), "grace 之後的修飾鍵算");
        assert!(
            run.system_input(idle(t0 + Duration::from_millis(450), after), after),
            "grace 之後的輸入算"
        );
        // 還原縮到最小的視窗之後再叫一次：從那時重新算
        let again = t0 + Duration::from_secs(2);
        run.raised_at(again);
        let now = again + Duration::from_millis(100);
        assert!(!run.counts(&alt, now));
        assert!(!run.system_input(idle(again + Duration::from_millis(5), now), now));
        assert!(run.system_input(
            idle(again + Duration::from_millis(450), now + Duration::from_millis(400)),
            now + Duration::from_millis(400)
        ));
    }

    #[test]
    fn keys_clicks_wheel_and_held_modifiers_are_input_but_focus_is_not() {
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(is_user_input(&key(true)));
        assert!(!is_user_input(&key(false)), "放開不算（按下時已經取消了）");
        assert!(is_user_input(&egui::Event::Text("a".into())));
        assert!(is_user_input(&egui::Event::Copy));
        assert!(is_user_input(&egui::Event::ModifiersChanged(Modifiers::SHIFT)));
        assert!(
            !is_user_input(&egui::Event::ModifiersChanged(Modifiers::NONE)),
            "視窗得到焦點時送的（都沒按著）不算"
        );
        assert!(is_user_input(&egui::Event::PointerButton {
            pos: pos2(1.0, 1.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }));
        assert!(is_user_input(&egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }));
        assert!(!is_user_input(&egui::Event::WindowFocused(true)));
        assert!(
            !is_user_input(&egui::Event::PointerMoved(pos2(1.0, 1.0))),
            "移動另外看距離"
        );
    }
}
