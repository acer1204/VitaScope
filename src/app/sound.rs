//! 音效設定（輸出裝置、獨佔模式、轉成立體聲、音訊直通）的邏輯：右鍵選單「音效」、設定頁都走這裡。
//! 設定整個程式共用、改了馬上存檔；對應到哪些 mpv 選項由 `sound::mpv_options` 決定，套用時只送有變的
//!（改裝置、獨佔、聲道都會重新開啟音訊輸出，聲音中斷一下）。

use super::VitascopeApp;
use crate::player::{AsyncKey, TrackKind};
use crate::sound::{self, AUTO_DEVICE, AudioDevice, AudioSettings, Passthrough};
use crate::{tf, tr};

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
            self.devices_seen = self.player.state.audio_devices.clone();
            if let Some(list) = self.devices_seen.clone() {
                self.check_saved_device(&list, true);
            }
        }
        let spdif = self.player.state.audio_spdif.clone();
        if spdif != self.spdif_seen {
            if let Some(format) = &spdif {
                let name = sound::spdif_label(format);
                self.osd(tf!(
                    "音訊直通：{name} → 擴大機（音量請用擴大機調整）",
                    "Passthrough: {name} → amplifier (use the amplifier's volume)"
                ));
            }
            self.spdif_seen = spdif;
        }
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
        let Some(id) = self.audio_restore.filter(|_| self.player.state.loaded) else {
            return;
        };
        self.set_option_async(AsyncKey::AudioDevice, "aid", &id.to_string());
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

    /// 設定改了之後：非同步送出有變的選項
    pub(super) fn apply_sound(&mut self) {
        let opts = self.sound_options();
        for (name, key, result) in self.player.apply_sound(&opts, false) {
            match result {
                Ok(Some(id)) => {
                    self.async_pending.insert(id, name.to_owned());
                }
                Ok(None) => {}
                Err(e) => self.async_failed(key, name, &e.to_string()),
            }
        }
    }

    /// 改音效設定：有變才套用、存檔
    fn change_sound(&mut self, change: impl FnOnce(&mut AudioSettings)) {
        let before = self.settings.audio.clone();
        change(&mut self.settings.audio);
        if self.settings.audio != before {
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
