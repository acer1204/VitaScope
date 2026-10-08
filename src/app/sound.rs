//! 音效設定（輸出裝置、獨佔模式、轉成立體聲、等化器、音量平衡、音量上限、音訊直通）的邏輯：
//! 右鍵選單「音效」、控制面板、設定頁都走這裡。
//! 設定整個程式共用、改了馬上存檔；對應到哪些 mpv 選項由 `sound::mpv_options` 決定，套用時只送有變的
//!（改裝置、獨佔、聲道都會重新開啟音訊輸出，聲音中斷一下）。
//! 等化器、音量平衡、音量超過 100% 的放大在影戲自己的 af 濾鏡鏈（`sound::af_chain`）：字串是唯一的依據，
//! 拖滑桿、調音量時先送 af-command 馬上聽得到，停下來再改寫字串（見 `af_live`）。
//! 音訊直通的資料不能過濾鏡，預測會直通時先把 af 清空（開檔前、選音軌前、播放中打開直通時），直通結束再設回來

use super::VitascopeApp;
use crate::player::{AsyncKey, TrackKind};
use crate::sound::{self, AUTO_DEVICE, AudioDevice, AudioSettings, EqPreset, Leveling, Passthrough};
use crate::{tf, tr};
use eframe::egui;
use std::time::{Duration, Instant};

/// 預測會直通、音訊輸出卻一直是一般的 PCM（擴大機、輸出方式不支援，mpv 改回 PCM）：等這麼久就不再當成直通，
/// 濾鏡鏈設回來
const SPDIF_GRACE: Duration = Duration::from_millis(1500);

/// 等直通時多久問一次 mpv 解碼的格式（見 `spdif_refused`）
const SPDIF_POLL: Duration = Duration::from_millis(100);

/// 改用 null 之後重開音訊輸出（`retry_audio_output`）：等這麼久（而且檔案載入了）再看結果
const AO_RETRY_SETTLE: Duration = Duration::from_secs(1);

/// 音訊直通中：音量交給擴大機（音量鍵、滑桿不動作時的提示、停用的說明）
pub(super) fn spdif_volume_hover() -> &'static str {
    tr!(
        "音訊直通中：聲音由擴大機處理",
        "Passthrough is on: the amplifier handles the sound"
    )
}

/// 「預設裝置（跟隨系統）」
pub(super) fn auto_device_label() -> &'static str {
    tr!("預設裝置（跟隨系統）", "Default device (follow the system)")
}

/// 開 / 關
fn on_off(on: bool) -> &'static str {
    if on { tr!("開", "on") } else { tr!("關", "off") }
}

/// 引擎沒有等化器用的濾鏡（舊的引擎）：停用時的說明
pub(super) fn eq_missing_hover() -> &'static str {
    tr!(
        "播放引擎缺少等化器濾鏡，請更新影戲",
        "The playback engine lacks the equalizer filters; please update VitaScope"
    )
}

/// 音量上限的說明：有限幅器時不會破音；沒有時（舊的引擎、VITASCOPE_MPV_OPTS 指定了 af）可能破音
pub(super) fn volume_max_hover(limiter: bool) -> &'static str {
    if limiter {
        tr!(
            "超過 100% 會經過限幅器，不會爆音",
            "Above 100% the sound goes through a limiter, so it won't clip"
        )
    } else {
        tr!(
            "沒有限幅器（播放引擎太舊，或 VITASCOPE_MPV_OPTS 指定了 af）：超過 100% 可能會破音",
            "No limiter (old playback engine, or af set by VITASCOPE_MPV_OPTS): above 100% the sound may clip"
        )
    }
}

/// 「音量平衡」的說明（右鍵選單、控制面板、設定頁共用）
pub(super) fn leveling_hover() -> &'static str {
    tr!(
        "夜間模式：小聲變大、大聲變小，晚上不吵到別人；人聲平衡：讓說話的音量一致；音量平均：整部片的音量差不多大",
        "Night mode: quiet parts louder and loud parts quieter, for late-night viewing. Voice leveling: keeps speech \
         at an even volume. Normalize: evens out the volume across the whole video."
    )
}

/// 音量的提示：超過 100% 註明「放大」
pub(super) fn volume_osd(v: f64) -> String {
    if v > 100.0 {
        tf!("音量 {v:.0}%（放大）", "Volume {v:.0}% (boosted)")
    } else {
        tf!("音量 {v:.0}%", "Volume {v:.0}%")
    }
}

/// 等化器某一段的增益（「+5」「-2.5」「0」）
pub(super) fn fmt_gain(db: f32) -> String {
    let n = sound::fmt_num(f64::from(db));
    if db > 0.0 { format!("+{n}") } else { n }
}

/// 音量上限的選項（「150%」）
pub(super) fn volume_max_label(v: u32) -> &'static str {
    match v {
        130 => "130%",
        150 => "150%",
        200 => "200%",
        _ => "100%",
    }
}

/// `restore`（最後選的音軌）還能選回來：有檔案、而且這個檔案還有這條音軌（外掛的音軌可能被移除了）
fn restorable_track(restore: Option<i64>, st: &crate::player::State) -> Option<i64> {
    restore.filter(|id| st.loaded && st.tracks_of(TrackKind::Audio).any(|t| t.id == *id))
}

impl VitascopeApp {
    /// 啟動時（還沒開檔）同步套用。存了指定的裝置時先同步讀一次裝置清單，裝置不在就暫時用預設裝置
    ///（沒存的話不讀：列舉裝置可能很慢，等第一個畫面出來之後再開始觀察，見 `sound_tick`）
    pub(super) fn sound_startup(&mut self) {
        let saved = self.saved_device().map(str::to_owned);
        if saved.is_some()
            && !self.sound_locked("audio-device")
            && let Some(list) = self.player.read_audio_devices()
        {
            let list = list.to_vec();
            self.devices_seen = Some(list.clone());
            // 下面一起同步套用
            self.check_saved_device(&list, false);
        }
        let opts = self.sound_options();
        for (name, _, result) in self.player.apply_sound(&opts, true) {
            if let Err(e) = result {
                eprintln!("[vitascope] 無法套用 {name}：{e}");
            }
        }
        // 等化器、音量平衡、音量上限超過 100% 的濾鏡鏈：接在其他音效選項後面同步設定，第一個檔案就生效
        //（什麼都沒開時是空的，跟 mpv 原本一樣，不用送）
        if let Some(chain) = self.wanted_af() {
            if chain.is_empty() {
                self.af_applied = self.player.get_string("af").ok().filter(String::is_empty);
            } else {
                match self.player.mpv().set_property("af", chain.as_str()) {
                    Ok(()) => self.af_applied = Some(chain),
                    Err(e) => {
                        eprintln!("[vitascope] 無法套用 af：{e}");
                        self.af_failed = Some(chain);
                    }
                }
            }
        }
    }

    /// 每一幀：第一個畫面出來之後開始觀察裝置清單；清單變了就看存下的裝置拔掉了沒、插回來了沒；
    /// 開始音訊直通時提示
    pub(super) fn sound_tick(&mut self) {
        if self.frames >= 1 {
            self.player.watch_audio_devices();
        }
        // 記下最後選的音軌（mpv 因為裝置開不起來關掉音軌時不算，換裝置之後要選回來）
        let audio = self.player.state.selected(TrackKind::Audio).map(|t| t.id);
        if audio != self.audio_seen {
            self.audio_seen = audio;
            if audio.is_some() {
                self.audio_restore = audio;
            }
        }
        if self.player.state.audio_devices != self.devices_seen {
            // 第一次拿到清單不算變了（不是插拔）
            let changed = self.devices_seen.is_some();
            self.devices_seen = self.player.state.audio_devices.clone();
            if let Some(list) = self.devices_seen.clone() {
                self.check_saved_device(&list, true);
            }
            // 改用 null 中（例如預設裝置的電視關掉又打開）：裝置可能回來了，重開一次
            //（上次重開還沒有結果的也再重開：那次可能比清單變早）
            if changed && (self.player.audio_fell_back() || self.ao_retry.is_some()) {
                self.retry_audio_output();
            }
        }
        self.af_tick();
        let spdif = self.player.state.audio_spdif.clone();
        if spdif != self.spdif_seen {
            if let Some(format) = &spdif {
                self.spdif_started(format);
            }
            self.spdif_seen = spdif;
        }
        // 音訊輸出開不起來、mpv 改用 null 繼續播放（沒有聲音）：提示一次。改裝置、獨佔模式之後會照新的設定重開。
        // 重開了（`retry_audio_output`）的話等一下、直接問 mpv 結果：觀察到的值可能還是重開前的 null
        let fell_back = match self.ao_retry {
            Some(at) if at.elapsed() < AO_RETRY_SETTLE || !self.player.state.loaded => return,
            Some(_) => {
                self.ao_retry = None;
                self.player.audio_fell_back_now()
            }
            None => self.player.audio_fell_back(),
        };
        if fell_back != self.ao_fallback_seen {
            self.ao_fallback_seen = fell_back;
            if fell_back {
                let msg = if self.settings.audio.exclusive && !self.sound_locked("audio-exclusive") {
                    tr!(
                        "無法開啟音訊裝置（可能不允許獨佔模式），暫時沒有聲音",
                        "Can't open the audio device (exclusive mode may not be allowed); no sound for now"
                    )
                } else {
                    tr!(
                        "無法開啟音訊裝置，暫時沒有聲音",
                        "Can't open the audio device; no sound for now"
                    )
                };
                self.osd(msg);
            }
        }
    }

    /// 音訊輸出開不起來、mpv 改用了 null：照設定重開一次（再試真正的裝置）。mpv 換檔時沿用同一個輸出
    ///（gapless-audio 預設 weak），預設裝置的清單變了也不會自己重開，不重開的話之後一直沒有聲音。
    /// 還是開不起來的話 mpv 又改用 null，等一下看結果、再提示一次（見 `sound_tick`）
    pub(super) fn retry_audio_output(&mut self) {
        self.command_async_keyed(AsyncKey::AudioDevice, &["ao-reload"]);
        self.ao_retry = Some(Instant::now());
        self.ao_fallback_seen = false;
        self.ao_retries += 1;
    }

    /// 改用 null 之後重開音訊輸出的次數（介面測試用）
    #[doc(hidden)]
    pub fn audio_output_retries(&self) -> u32 {
        self.ao_retries
    }

    /// 開始音訊直通：提示。靜音、音量 0% 對直通沒有作用（擴大機照樣出聲），提示裡註明；
    /// 不是正常速度的話改回 1×（直通的資料不能變速，mpv 會丟掉或重複整個封包，擴大機的聲音斷斷續續）
    fn spdif_started(&mut self, format: &str) {
        let name = sound::spdif_label(format);
        let st = &self.player.state;
        let mut msg = if st.muted || self.player.volume_total() <= 0.0 {
            tf!(
                "音訊直通：{name} → 擴大機（靜音、音量請用擴大機調整）",
                "Passthrough: {name} → amplifier (use the amplifier for mute and volume)"
            )
        } else {
            tf!(
                "音訊直通：{name} → 擴大機（音量請用擴大機調整）",
                "Passthrough: {name} → amplifier (use the amplifier's volume)"
            )
        };
        // 直接問 mpv：速度的通知可能還沒到
        let speed = self.player.get_f64("speed").unwrap_or(st.speed);
        if (speed - 1.0).abs() > 1e-9 {
            match self.player.set_speed(1.0) {
                Ok(()) => msg.push_str(tr!(
                    "（直通時不能變速，已改回 1×）",
                    " (the speed can't change during passthrough; set back to 1×)"
                )),
                Err(e) => eprintln!("[vitascope] 無法改回正常速度：{e}"),
            }
        }
        self.osd(msg);
    }

    /// 存下的輸出裝置（None = 預設裝置）
    fn saved_device(&self) -> Option<&str> {
        self.settings.audio.device.as_deref().filter(|d| *d != AUTO_DEVICE)
    }

    /// 裝置清單（`devices_seen`）變了：存下的裝置不在就暫時改用預設裝置（提示一次），回來了就切回去。
    /// `apply`：馬上（非同步）套用；啟動時由 `sound_startup` 一起同步套用
    fn check_saved_device(&mut self, list: &[AudioDevice], apply: bool) {
        if self.sound_locked("audio-device") {
            return;
        }
        let a = &self.settings.audio;
        let choice = sound::resolve_device(self.saved_device(), a.device_label.as_deref(), list);
        match (choice.missing, self.device_fallback) {
            (Some(name), false) => {
                self.device_fallback = true;
                if apply {
                    self.apply_sound();
                    self.reopen_audio();
                }
                self.osd(tf!(
                    "找不到音訊裝置「{name}」，改用預設裝置",
                    "Can't find the audio device \"{name}\"; using the default device"
                ));
            }
            (None, true) => {
                self.device_fallback = false;
                if apply {
                    self.apply_sound();
                    self.reopen_audio();
                }
                let a = &self.settings.audio;
                let name = a.device_label.clone().or_else(|| a.device.clone()).unwrap_or_default();
                self.osd(tf!("已切換回 {name}", "Switched back to {name}"));
            }
            _ => {}
        }
    }

    /// 播放中換了輸出裝置（拔掉、插回來）之後把音軌選回來。拔掉時 mpv 自己會先重開音訊輸出，
    /// 用的還是拔掉的裝置、開不起來就把音軌關掉（沒有聲音）；音訊輸出關了之後再改 audio-device 也不會重開，
    /// 要重新選音軌才會用新的裝置開。音軌還開著的話選同一條不會有動作。
    /// 接在改 audio-device 後面送（非同步指令照順序執行）
    fn reopen_audio(&mut self) {
        let Some(id) = self.restorable_audio() else {
            return;
        };
        // 重新選音軌會重新建立音訊（照設定試直通）：會直通的話濾鏡鏈先清空
        self.sound_before_reopen(Some(id));
        self.set_option_async(AsyncKey::AudioDevice, "aid", &id.to_string());
    }

    /// 要選回來的音軌：最後選的那一條（使用者自己關掉音軌時沒有），而且是這個檔案的
    fn restorable_audio(&self) -> Option<i64> {
        restorable_track(self.audio_restore, &self.player.state)
    }

    /// 交給 mpv 的裝置：存下的裝置；對照過裝置清單、它不在時暫時用預設裝置（還不知道清單時照存下的）
    fn device_to_apply(&self) -> String {
        let a = &self.settings.audio;
        match &self.devices_seen {
            Some(list) if !self.sound_locked("audio-device") => {
                sound::resolve_device(self.saved_device(), a.device_label.as_deref(), list).apply
            }
            _ => self.saved_device().unwrap_or(AUTO_DEVICE).to_owned(),
        }
    }

    /// 目前的設定對應的 mpv 選項（VITASCOPE_MPV_OPTS 指定的不列）
    fn sound_options(&self) -> Vec<(&'static str, String)> {
        sound::mpv_options(
            &self.settings.audio,
            &self.device_to_apply(),
            self.player.user_overrides(),
        )
    }

    /// 設定改了之後：非同步送出有變的選項。濾鏡鏈先送：播放中打開直通時，af 要在 audio-spdif 之前清空
    ///（非同步指令照順序執行）；關掉直通時 af 要等直通真的結束才設回來（`af_tick`）
    pub(super) fn apply_sound(&mut self) {
        self.sync_af();
        let opts = self.sound_options();
        // 換裝置、獨佔模式、聲道會重新開啟音訊輸出，mpv 會再試一次直通（之前不支援、改回 PCM 的也會）：
        // 目前的音軌會直通的話濾鏡鏈先清空（同步，排在這些非同步的設定之前）
        let reopens = self
            .player
            .pending_sound(&opts)
            .any(|n| sound::REOPENS_OUTPUT.contains(&n));
        // 之前音訊輸出開不起來、mpv 把音軌關掉了（沒有聲音）：音訊輸出已經關了，光改這些選項 mpv 不會重開，
        // 送完之後把音軌選回來才會照新的設定開（跟拔掉裝置之後一樣，見 `reopen_audio`）
        let selected = self.player.state.selected(TrackKind::Audio).map(|t| t.id);
        let restore = if reopens && selected.is_none() {
            self.restorable_audio()
        } else {
            None
        };
        if reopens {
            self.sound_before_reopen(selected.or(restore));
        }
        for (name, key, result) in self.player.apply_sound(&opts, false) {
            match result {
                Ok(Some(id)) => {
                    self.async_pending.insert(id, name.to_owned());
                }
                Ok(None) => {}
                Err(e) => self.async_failed(key, name, &e.to_string()),
            }
        }
        if let Some(id) = restore {
            self.set_option_async(AsyncKey::AudioDevice, "aid", &id.to_string());
        }
    }

    /// 改音效設定：有變才套用、存檔
    fn change_sound(&mut self, change: impl FnOnce(&mut AudioSettings)) {
        let before = self.settings.audio.clone();
        change(&mut self.settings.audio);
        if self.settings.audio != before {
            if self.settings.audio.passthrough != before.passthrough {
                self.passthrough_changed();
            }
            self.apply_sound();
            self.save_settings();
        }
    }

    /// 使用者用 VITASCOPE_MPV_OPTS（或測試的 `Options.extra`）指定了這個 mpv 選項：介面上不能改
    pub(super) fn sound_locked(&self, name: &str) -> bool {
        super::mpv_opts_override(&self.player, name)
    }

    /// 選項停用的原因（VITASCOPE_MPV_OPTS 指定了 `name`）；可以改時 None
    pub(super) fn sound_disabled(&self, name: &str) -> Option<&'static str> {
        self.sound_locked(name).then(super::control_panel::adjust_locked_hover)
    }

    // ───────────── 輸出裝置、獨佔模式 ─────────────

    /// 目前輸出方式的裝置（選單、設定頁列出的；不含預設裝置）。還沒開始觀察清單的話現在開始（之後幾幀才會有）
    pub(super) fn device_choices(&mut self) -> Vec<AudioDevice> {
        self.player.watch_audio_devices();
        let Some(list) = &self.player.state.audio_devices else {
            return Vec::new();
        };
        let ao = self.player.get_string("current-ao").ok();
        sound::family_devices(list, ao.as_deref(), self.caps.macos)
            .into_iter()
            .cloned()
            .collect()
    }

    /// 獨佔模式要不要顯示（Linux 只有 PipeWire 支援）
    pub(super) fn exclusive_shown(&self) -> bool {
        let ao = self.player.get_string("current-ao").ok();
        let list = self.player.state.audio_devices.as_deref().unwrap_or_default();
        sound::exclusive_shown(sound::device_family(list, ao.as_deref(), self.caps.macos).as_deref())
    }

    /// 存下的裝置拔掉了（暫時用預設裝置）
    pub(super) fn device_missing(&self) -> bool {
        self.device_fallback
    }

    /// 選輸出裝置；None = 預設裝置（跟隨系統）。選的都在清單上，不用再等拔掉的裝置
    pub(super) fn select_audio_device(&mut self, device: Option<&AudioDevice>) {
        self.device_fallback = false;
        let (name, label) = match device {
            Some(d) => (Some(d.name.clone()), Some(d.label().to_owned())),
            None => (None, None),
        };
        self.change_sound(|a| {
            a.device = name;
            a.device_label = label;
        });
        let shown = device.map_or_else(|| auto_device_label().to_owned(), |d| d.label().to_owned());
        self.osd(tf!("音訊輸出：{shown}", "Audio output: {shown}"));
    }

    pub(super) fn set_exclusive(&mut self, on: bool) {
        self.change_sound(|a| a.exclusive = on);
        self.osd(tf!("獨佔模式：{}", "Exclusive mode: {}", on_off(on)));
    }

    // ───────────── 轉成立體聲 ─────────────

    /// 「多聲道轉成立體聲」不能改的原因：VITASCOPE_MPV_OPTS 指定了 audio-channels，或音訊直通中
    ///（直通的資料不經過混音，改了也沒作用，還會重開音訊輸出、打斷直通）
    pub(super) fn downmix_disabled(&self) -> Option<&'static str> {
        self.sound_disabled("audio-channels")
            .or_else(|| self.spdif_active().is_some().then(spdif_volume_hover))
    }

    pub(super) fn set_downmix(&mut self, on: bool) {
        self.change_sound(|a| a.downmix = on);
        self.osd(tf!("轉成立體聲：{}", "Downmix to stereo: {}", on_off(on)));
    }

    pub(super) fn set_normalize_downmix(&mut self, on: bool) {
        self.change_sound(|a| a.normalize_downmix = on);
        self.osd(tf!(
            "混音時避免破音：{}",
            "Avoid clipping when downmixing: {}",
            on_off(on)
        ));
    }

    // ───────────── 音訊直通 ─────────────

    /// 音訊直通中：直通的格式（"ac3"…）
    pub(super) fn spdif_active(&self) -> Option<&str> {
        self.player.state.audio_spdif.as_deref()
    }

    pub(super) fn set_passthrough(&mut self, on: bool) {
        self.change_sound(|a| a.passthrough.enabled = on);
        // 舊的引擎（系統的 libmpv 0.40 以前）播放中改了不會重新開啟音訊解碼器：說明下一個檔案才生效
        if self.player.state.loaded && !self.caps.spdif_live {
            self.osd(tf!(
                "音訊直通：{}（下一個檔案開始生效）",
                "Passthrough: {} (from the next file)",
                on_off(on)
            ));
        } else {
            self.osd(tf!("音訊直通：{}", "Passthrough: {}", on_off(on)));
        }
    }

    /// 直通的格式（設定頁的勾選）
    pub(super) fn set_passthrough_codecs(&mut self, p: Passthrough) {
        self.change_sound(|a| a.passthrough = p);
    }

    /// 音訊直通中不能變速（直通的資料不能重新取樣）：改成正常速度以外的值時提示並回傳 true
    pub(super) fn speed_blocked(&mut self, speed: f64) -> bool {
        if self.spdif_active().is_none() || (speed - 1.0).abs() < 1e-9 {
            return false;
        }
        self.osd(tr!("音訊直通中無法變速", "Can't change the speed during passthrough"));
        true
    }

    /// 測試用：當成 mpv 的裝置清單是這些（見 `Player::set_fake_audio_devices`）
    #[doc(hidden)]
    pub fn fake_audio_devices(&mut self, list: Vec<AudioDevice>) {
        self.player.set_fake_audio_devices(list);
    }

    /// async 回覆說音效選項失敗：忘掉記下的值，下次改設定時再送
    pub(super) fn sound_reply_failed(&mut self, k: AsyncKey, name: &str) {
        if matches!(
            k,
            AsyncKey::AudioDevice | AsyncKey::Exclusive | AsyncKey::Downmix | AsyncKey::Spdif
        ) {
            self.player.forget_sound(name);
        }
    }
}

// ───────────── 等化器、音量平衡、音量放大：af 濾鏡鏈 ─────────────

impl VitascopeApp {
    /// 使用者用 VITASCOPE_MPV_OPTS（或 profile=、include=）指定了 af：影戲不改它
    pub(super) fn af_locked(&self) -> bool {
        self.sound_locked("af")
    }

    /// 現在該有的 af；使用者自己指定了 af 時 None（不管）
    fn wanted_af(&self) -> Option<String> {
        if self.af_locked() {
            return None;
        }
        let spdif = self.spdif_active().is_some() || self.spdif_expect;
        Some(sound::af_chain(
            &self.settings.audio,
            &self.caps.af,
            spdif,
            sound::boost_level(self.player.volume_total()),
        ))
    }

    /// 送出一條 af（非同步）。送出的就當成 mpv 現在的值（之後的非同步指令照順序執行）；mpv 不接受時見 `af_reply`
    fn send_af(&mut self, value: &str) {
        match self.player.set_async(AsyncKey::Af, "af", value) {
            Ok(id) => {
                self.async_pending.insert(id, "af".to_owned());
                self.af_inflight.insert(id, value.to_owned());
                self.af_applied = Some(value.to_owned());
            }
            Err(e) => {
                self.af_applied = None;
                self.async_failed(AsyncKey::Af, "af", &e.to_string());
            }
        }
    }

    /// 結構改變（等化器開關、預設、音量平衡、音量上限、進出直通…）或即時調整停下來：整條 af 改成現在該有的。
    /// 跟上次送的一樣、或是 mpv 拒絕過的就不送
    pub(super) fn sync_af(&mut self) {
        self.af_debounce.cancel();
        let Some(want) = self.wanted_af() else { return };
        if self.af_applied.as_deref() == Some(want.as_str()) || self.af_failed.as_deref() == Some(want.as_str()) {
            return;
        }
        self.send_af(&want);
    }

    /// 直通結束（或預測的直通沒發生）：先送「af ""」再送整條，濾鏡一定重新建立。
    /// 直通時濾鏡碰到直通的資料會失敗、被 mpv 停用；mpv 重設 af 時會留下參數沒變的濾鏡，
    /// 先清空就不會留下失敗的那一個。兩個非同步指令照順序執行
    fn revive_af(&mut self) {
        let Some(want) = self.wanted_af() else { return };
        self.af_debounce.cancel();
        if want.is_empty() || self.af_failed.as_deref() == Some(want.as_str()) {
            self.sync_af();
            return;
        }
        if self.af_applied.as_deref() != Some("") {
            self.send_af("");
        }
        self.send_af(&want);
    }

    /// 即時調整（拖等化器的滑桿、調超過 100% 的音量、改前級）：播放中而且濾鏡鏈的結構沒變（只有參數不一樣）時，
    /// 先送 af-command 馬上聽得到，停下來之後再改寫整條 af（`flush` = 放開滑桿，下一幀就改寫；不然等 300 毫秒）。
    /// af-command 的值在跳轉、換音軌時會被 mpv 用字串重建濾鏡蓋掉，所以一定要改寫字串。
    /// 沒開檔、或濾鏡鏈裡還沒有 `label` 那一段（結構改變）時直接改寫
    fn af_live(&mut self, label: &str, commands: &[[String; 5]], flush: bool) {
        let Some(want) = self.wanted_af() else { return };
        let applied = self.af_applied.clone().unwrap_or_default();
        let same_shape = [sound::EQ_LABEL, sound::LEVEL_LABEL, sound::LIMIT_LABEL]
            .into_iter()
            .all(|l| sound::has_stage(&applied, l) == sound::has_stage(&want, l));
        if !(self.player.state.loaded && same_shape && sound::has_stage(&want, label)) {
            self.sync_af();
            return;
        }
        for c in commands {
            let args: Vec<&str> = c.iter().map(String::as_str).collect();
            self.command_async_keyed(AsyncKey::AfCommand, &args);
        }
        let now = Instant::now();
        if flush {
            self.af_debounce.flush(now);
            self.egui_ctx.request_repaint();
        } else {
            self.af_debounce.touch(now);
            self.egui_ctx.request_repaint_after(sound::AF_DEBOUNCE);
        }
    }

    /// 限幅器的輸入增益（音量放大 × 前級）即時改
    fn limiter_live(&mut self, flush: bool) {
        let level = sound::limiter_level(
            &self.settings.audio,
            &self.caps.af,
            sound::boost_level(self.player.volume_total()),
        );
        self.af_live(sound::LIMIT_LABEL, &[sound::limit_command(level)], flush);
    }

    /// mpv 回覆了 af 的設定：失敗的話記下來（同一條不再送；mpv 留著原本的值，不確定是哪一條）
    pub(super) fn af_reply(&mut self, id: u64, failed: bool) {
        let Some(value) = self.af_inflight.remove(&id) else {
            return;
        };
        if failed {
            self.af_failed = Some(value);
            self.af_applied = None;
        }
    }

    /// 每一幀：調整停下來 300 毫秒了就改寫 af；直通開始、結束（實際的或預測的）時換濾鏡鏈
    fn af_tick(&mut self) {
        let now = Instant::now();
        if self.af_debounce.take(now) {
            self.sync_af();
        } else if let Some(due) = self.af_debounce.due() {
            self.egui_ctx.request_repaint_after(due.saturating_duration_since(now));
        }
        if self.spdif_expect {
            let waited = self.spdif_expect_at.elapsed();
            if self.player.state.audio_spdif.is_some() {
                // 真的開始直通了：之後看實際的狀態
                self.spdif_expect = false;
            } else if waited >= SPDIF_GRACE && self.spdif_refused() {
                // 預測會直通，卻是用一般的 PCM 播放（不支援直通，mpv 改回 PCM）：濾鏡鏈設回來
                self.spdif_expect = false;
            } else {
                self.egui_ctx
                    .request_repaint_after(SPDIF_GRACE.saturating_sub(waited).max(SPDIF_POLL));
            }
        }
        let spdif = self.player.state.audio_spdif.is_some() || self.spdif_expect;
        if spdif != self.af_spdif_seen {
            self.af_spdif_seen = spdif;
            if spdif {
                self.sync_af();
            } else {
                self.revive_af();
            }
        }
    }

    /// 預測的直通沒有發生（mpv 用 PCM 播放這個檔案的聲音）。輸出的格式是觀察的，解碼的格式直接問 mpv
    ///（只在等直通、輸出是 PCM 時才問，最多每 `SPDIF_POLL` 一次）
    fn spdif_refused(&mut self) -> bool {
        let (loaded, out_pcm) = (self.player.state.loaded, self.player.state.audio_out_pcm);
        if !loaded || !out_pcm || self.spdif_polled_at.elapsed() < SPDIF_POLL {
            return false;
        }
        self.spdif_polled_at = Instant::now();
        let decoded = self.player.get_string("audio-params/format").ok();
        sound::spdif_refused(out_pcm, decoded.as_deref())
    }

    /// 重新開啟音訊輸出、重新選音軌之前（mpv 會照設定再試一次直通）：這條音軌（`id`）會直通的話先清空濾鏡鏈。
    /// 不會直通的話不動（預測中的直通照舊等）
    fn sound_before_reopen(&mut self, id: Option<i64>) {
        if self.player.state.loaded && self.track_predicts_spdif(id) {
            self.expect_spdif_now();
        }
    }

    /// 這條音軌（編號）照目前的設定會不會直通
    fn track_predicts_spdif(&self, id: Option<i64>) -> bool {
        id.and_then(|id| self.player.state.tracks_of(TrackKind::Audio).find(|t| t.id == id))
            .and_then(|t| t.codec.as_deref())
            .is_some_and(|codec| sound::predict_spdif(codec, &self.settings.audio.passthrough))
    }

    /// 預測接下來的音訊會直通：記下來；濾鏡鏈還有東西的話馬上（同步）清空，
    /// 直通的資料才不會碰到濾鏡（同步設定排在接下來的開檔、選音軌之前）
    fn expect_spdif_now(&mut self) {
        self.spdif_expect = true;
        self.spdif_expect_at = Instant::now();
        self.af_spdif_seen = true;
        self.af_debounce.cancel();
        if self.af_locked() || self.af_applied.as_deref().is_none_or(str::is_empty) {
            return;
        }
        match self.player.mpv().set_property("af", "") {
            Ok(()) => {
                self.af_applied = Some(String::new());
                // 還沒執行的非同步 af（例如剛改寫的整條）可能排在這個同步設定之後：再排一個空的在它們後面
                if !self.af_inflight.is_empty() {
                    self.send_af("");
                }
            }
            Err(e) => eprintln!("[vitascope] 無法清空 af：{e}"),
        }
    }

    /// 照目前的設定預測選上的音軌（`id`）會不會直通
    fn predict_track(&mut self, id: Option<i64>) {
        if self.track_predicts_spdif(id) {
            self.expect_spdif_now();
        } else {
            self.spdif_expect = false;
        }
    }

    /// 開新檔之前：開了音訊直通的話，還不知道新檔案的音軌會不會直通，先當成會（清空濾鏡鏈）；
    /// 載入完成看了音軌再決定（`sound_file_loaded`）。沒開直通時什麼都不做
    pub(super) fn sound_before_open(&mut self) {
        if !sound::spdif_value(&self.settings.audio.passthrough).is_empty() && !self.af_locked() {
            self.expect_spdif_now();
        }
    }

    /// 檔案載入完成：看選上的音軌會不會直通
    pub(super) fn sound_file_loaded(&mut self) {
        let id = self.player.state.selected(TrackKind::Audio).map(|t| t.id);
        self.predict_track(id);
    }

    /// 開檔失敗：不會有音訊，不再當成會直通
    pub(super) fn sound_open_failed(&mut self) {
        self.spdif_expect = false;
    }

    /// 選音軌之前：新的音軌會直通的話先清空濾鏡鏈；不會的話之後（`af_tick`）設回來
    pub(super) fn sound_before_track(&mut self, id: Option<i64>) {
        self.predict_track(id);
    }

    /// 播放中改了直通的設定：新的引擎（mpv 0.41 起）馬上重新開音訊，照新的設定預測目前的音軌；
    /// 舊的引擎要到下一個檔案才生效，目前的檔案不變
    fn passthrough_changed(&mut self) {
        if !self.player.state.loaded || !self.caps.spdif_live {
            return;
        }
        let id = self.player.state.selected(TrackKind::Audio).map(|t| t.id);
        self.predict_track(id);
    }

    // ───────────── 等化器 ─────────────

    /// 等化器不能用的原因：VITASCOPE_MPV_OPTS 指定了 af、引擎沒有濾鏡、音訊直通中
    pub(super) fn eq_disabled(&self) -> Option<&'static str> {
        self.sound_disabled("af")
            .or_else(|| (!sound::eq_available(&self.caps.af)).then(eq_missing_hover))
            .or_else(|| self.spdif_active().is_some().then(spdif_volume_hover))
    }

    /// 「等化器：搖滾」「等化器：關」
    fn eq_osd(&mut self) {
        let eq = self.settings.audio.eq;
        if eq.enabled {
            self.osd(tf!("等化器：{}", "Equalizer: {}", eq.preset.label()));
        } else {
            self.osd(tr!("等化器：關", "Equalizer: off"));
        }
    }

    pub(super) fn set_eq_enabled(&mut self, on: bool) {
        if self.eq_disabled().is_some() {
            return;
        }
        self.change_sound(|a| a.eq.enabled = on);
        self.eq_osd();
    }

    /// 選預設（順便打開等化器：選了預設就是要用）
    pub(super) fn set_eq_preset(&mut self, preset: EqPreset) {
        if self.eq_disabled().is_some() {
            return;
        }
        self.change_sound(|a| {
            a.eq.enabled = true;
            a.eq.preset = preset;
        });
        self.eq_osd();
    }

    /// 拖某一段（`band` 從 0 開始）：預設變成「自訂」（從目前的值開始改），馬上聽得到；`commit` = 放開滑桿，存檔
    pub(super) fn set_eq_band(&mut self, band: usize, db: f32, commit: bool) {
        if self.eq_disabled().is_some() || band >= sound::EQ_BANDS.len() {
            return;
        }
        let db = db.clamp(-sound::EQ_MAX_GAIN, sound::EQ_MAX_GAIN);
        let eq = &mut self.settings.audio.eq;
        let before = eq.effective_gains();
        let mut gains = before;
        gains[band] = db;
        let changed = gains != before;
        eq.gains = gains;
        eq.preset = EqPreset::Custom;
        if changed && eq.enabled {
            let mut commands = vec![sound::band_command(band, db)];
            // 自動防止破音：最高的那一段變了，前級（限幅器的輸入增益）也跟著變
            let a = &self.settings.audio;
            if sound::preamp(&before, a.eq.auto_preamp) != sound::preamp(&gains, a.eq.auto_preamp) {
                let level = sound::limiter_level(a, &self.caps.af, sound::boost_level(self.player.volume_total()));
                commands.push(sound::limit_command(level));
            }
            self.af_live(sound::EQ_LABEL, &commands, commit);
        } else if commit {
            self.sync_af();
        }
        if commit {
            self.save_settings();
        }
    }

    /// 「還原」：全部回到 0（平坦）
    pub(super) fn reset_eq(&mut self) {
        if self.eq_disabled().is_some() {
            return;
        }
        self.change_sound(|a| {
            a.eq.preset = EqPreset::Flat;
            a.eq.gains = [0.0; 10];
        });
        self.eq_osd();
    }

    /// 自動防止破音（前級）：只改限幅器的輸入增益
    pub(super) fn set_auto_preamp(&mut self, on: bool) {
        if self.eq_disabled().is_some() || self.settings.audio.eq.auto_preamp == on {
            return;
        }
        self.settings.audio.eq.auto_preamp = on;
        self.limiter_live(true);
        self.save_settings();
        self.osd(tf!("自動防止破音：{}", "Prevent clipping: {}", on_off(on)));
    }

    /// 前級（dB，負數）：等化器開著、自動防止破音、有段落調高時才有
    pub(super) fn eq_preamp_db(&self) -> Option<f64> {
        let eq = &self.settings.audio.eq;
        let pre = sound::preamp(&eq.effective_gains(), eq.auto_preamp);
        (eq.enabled && pre < 1.0).then(|| 20.0 * pre.log10())
    }

    /// 等化器目前的狀態（設定頁）：「搖滾（開啟）」「平坦（關閉）」
    pub(super) fn eq_summary(&self) -> String {
        let eq = &self.settings.audio.eq;
        if eq.enabled {
            tf!("{}（開啟）", "{} (on)", eq.preset.label())
        } else {
            tf!("{}（關閉）", "{} (off)", eq.preset.label())
        }
    }

    /// 打開控制面板的「音效」分頁（右鍵選單、設定頁的「等化器…」）；已經開著就換到這一頁
    pub(super) fn show_equalizer(&mut self) {
        self.panel_open = true;
        self.panel_tab = super::control_panel::PanelTab::Sound;
    }

    // ───────────── 音量平衡 ─────────────

    /// 音量平衡整個不能改的原因：VITASCOPE_MPV_OPTS 指定了 af、音訊直通中
    pub(super) fn leveling_disabled(&self) -> Option<&'static str> {
        self.sound_disabled("af")
            .or_else(|| self.spdif_active().is_some().then(spdif_volume_hover))
    }

    /// 某一種音量平衡不能選的原因：引擎沒有它的濾鏡
    pub(super) fn leveling_missing(&self, mode: Leveling) -> Option<String> {
        (!mode.available(&self.caps.af)).then(|| {
            let filter = mode.filter().unwrap_or_default();
            tf!(
                "播放引擎缺少 {filter} 濾鏡，請更新影戲",
                "The playback engine lacks the {filter} filter; please update VitaScope"
            )
        })
    }

    /// 左邊名稱「音量平衡」+ 下拉選單（控制面板、設定頁）；引擎沒有濾鏡的那一種停用並說明。選了不一樣的回傳它
    pub(super) fn leveling_combo(&self, ui: &mut egui::Ui, id: &str) -> Option<Leveling> {
        let label = ui.label(tr!("音量平衡", "Volume leveling"));
        let current = self.settings.audio.leveling;
        let disabled = self.leveling_disabled();
        let mut chosen = None;
        let r = ui
            .add_enabled_ui(disabled.is_none(), |ui| {
                egui::ComboBox::from_id_salt(id)
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for mode in Leveling::ALL {
                            let missing = self.leveling_missing(mode);
                            let r = ui.add_enabled(
                                missing.is_none(),
                                egui::Button::selectable(current == mode, mode.menu_label()),
                            );
                            let r = match &missing {
                                Some(why) => r.on_disabled_hover_text(why),
                                None => r,
                            };
                            if r.clicked() && current != mode {
                                chosen = Some(mode);
                            }
                        }
                    })
                    .response
            })
            .inner
            .labelled_by(label.id)
            .on_hover_text(leveling_hover());
        if let Some(why) = disabled {
            r.on_disabled_hover_text(why);
        }
        chosen
    }

    /// 左邊名稱「音量上限」+ 下拉選單（控制面板、設定頁）。選了不一樣的回傳它
    pub(super) fn volume_max_combo(&self, ui: &mut egui::Ui, id: &str) -> Option<u32> {
        let limiter = !self.af_locked() && self.caps.af.alimiter;
        let label = ui.label(tr!("音量上限", "Volume limit"));
        let current = self.settings.audio.volume_max;
        let mut chosen = None;
        egui::ComboBox::from_id_salt(id)
            .selected_text(volume_max_label(current))
            .show_ui(ui, |ui| {
                for v in sound::VOLUME_MAX_CHOICES {
                    if ui.selectable_label(current == v, volume_max_label(v)).clicked() && current != v {
                        chosen = Some(v);
                    }
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(volume_max_hover(limiter));
        chosen
    }

    pub(super) fn set_leveling(&mut self, mode: Leveling) {
        if self.leveling_disabled().is_some() || self.leveling_missing(mode).is_some() {
            return;
        }
        self.change_sound(|a| a.leveling = mode);
        self.osd(tf!("音量平衡：{}", "Volume leveling: {}", mode.label()));
    }

    // ───────────── 音量（可以超過 100%） ─────────────

    /// 濾鏡鏈有限幅器：超過 100% 的音量在限幅器放大（不然用 mpv 自己的音量，可能破音）
    pub(super) fn limiter_on(&self) -> bool {
        !self.af_locked() && sound::has_limiter(&self.settings.audio, &self.caps.af)
    }

    /// 音量上限（%）
    pub(super) fn volume_cap(&self) -> f64 {
        f64::from(self.settings.audio.volume_max)
    }

    /// 設定總音量（0…音量上限）。超過 100% 而且有限幅器時改限幅器的輸入增益：先送 af-command，
    /// `flush`（放開滑桿）時下一幀就改寫 af，不然停下來 300 毫秒之後
    pub(super) fn set_volume_total(&mut self, v: f64, flush: bool) {
        let limiter = self.limiter_on();
        let before = self.player.boost_pct();
        let cap = self.volume_cap();
        if let Err(e) = self.player.set_volume_total(v, cap, limiter) {
            eprintln!("[vitascope] 無法設定音量：{e}");
        }
        if limiter && (self.player.boost_pct() != before || (flush && self.af_debounce.due().is_some())) {
            self.limiter_live(flush);
        }
    }

    /// 音量上限：100 / 130 / 150 / 200%。調低時總音量也拉下來
    pub(super) fn set_volume_max(&mut self, max: u32) {
        let max = sound::snap_volume_max(max);
        if max == self.settings.audio.volume_max {
            return;
        }
        let total = self.player.volume_total();
        self.settings.audio.volume_max = max;
        if total > f64::from(max) {
            // 先把音量拉下來（放大變小或歸零），濾鏡鏈再跟著新的上限改
            let limiter = self.limiter_on();
            if let Err(e) = self.player.set_volume_total(f64::from(max), f64::from(max), limiter) {
                eprintln!("[vitascope] 無法設定音量：{e}");
            }
        }
        self.apply_sound();
        self.save_settings();
        self.osd(tf!("音量上限：{max}%", "Volume limit: {max}%"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{State, Track};

    fn track(id: i64, kind: &str) -> Track {
        serde_json::from_value(serde_json::json!({ "id": id, "type": kind })).unwrap()
    }

    #[test]
    fn only_tracks_of_this_file_are_restored() {
        let mut st = State {
            loaded: true,
            tracks: vec![track(1, "video"), track(1, "audio"), track(2, "audio")],
            ..Default::default()
        };
        assert_eq!(restorable_track(Some(2), &st), Some(2));
        assert_eq!(restorable_track(None, &st), None, "使用者自己關掉音軌");
        assert_eq!(
            restorable_track(Some(3), &st),
            None,
            "不在這個檔案（例如移除了的外掛音軌）"
        );
        st.tracks = vec![track(3, "video")];
        assert_eq!(restorable_track(Some(3), &st), None, "同編號的不是音軌");
        st.tracks = vec![track(1, "audio")];
        st.loaded = false;
        assert_eq!(restorable_track(Some(1), &st), None, "沒有檔案");
    }
}
