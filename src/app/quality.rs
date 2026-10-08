//! 畫質設定（去交錯、去色帶、銳化、縮放演算法、HDR 色調映射）的邏輯：右鍵選單、控制面板、設定頁都走這裡。
//! 設定整個程式共用、改了馬上存檔；對應到哪些 mpv 選項由 `picture::mpv_options` 決定，套用時只送有變的。

use super::{VitascopeApp, mpv_opts_override};
use crate::picture::{
    self, ChromaScaler, Deinterlace, Downscaler, Gamut, Quality, Strength, ToneCurve, Upscaler, VideoSettings,
};
use crate::{tf, tr};
use eframe::egui;

/// 軟體繪圖的簡化流程（gpu-dumb-mode）不支援的項目：停用時的說明
pub(super) fn dumb_hover() -> &'static str {
    tr!("軟體繪圖模式不支援", "Not available with software rendering")
}

/// 縮放演算法「跟隨畫質」（不個別指定）
pub(super) fn follow_quality() -> &'static str {
    tr!("跟隨畫質", "Follow quality")
}

/// 個別指定的縮放演算法的名稱；None = 跟隨畫質
pub(super) fn scaler_choice(label: Option<&'static str>) -> &'static str {
    label.unwrap_or_else(follow_quality)
}

/// 開 / 關
fn on_off(on: bool) -> &'static str {
    if on { tr!("開", "on") } else { tr!("關", "off") }
}

/// 左邊名稱 + 下拉選單（控制面板、設定頁用）；名稱也是下拉選單的無障礙標籤。
/// `disabled` 有值時停用，滑鼠移上去顯示原因。選了不一樣的值時回傳它
pub(super) fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    name: &str,
    id: &str,
    current: T,
    all: &[T],
    text: impl Fn(T) -> &'static str,
    disabled: Option<&str>,
) -> Option<T> {
    let label = ui.label(name);
    let mut chosen = None;
    let r = ui
        .add_enabled_ui(disabled.is_none(), |ui| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(text(current))
                .show_ui(ui, |ui| {
                    for v in all {
                        if ui.selectable_label(current == *v, text(*v)).clicked() && current != *v {
                            chosen = Some(*v);
                        }
                    }
                })
                .response
        })
        .inner
        .labelled_by(label.id);
    if let Some(why) = disabled {
        r.on_disabled_hover_text(why);
    }
    chosen
}

impl VitascopeApp {
    /// 啟動時（還沒開檔）同步套用：去交錯預設「自動」從這裡開始生效
    pub(super) fn video_startup(&mut self) {
        let opts = self.video_options();
        for (name, _, result) in self.player.apply_picture(&opts, true) {
            if let Err(e) = result {
                eprintln!("[vitascope] 無法套用 {name}：{e}");
            }
        }
    }

    /// 目前的設定對應的 mpv 選項（VITASCOPE_MPV_OPTS 指定的不列）
    fn video_options(&self) -> Vec<(&'static str, String)> {
        picture::mpv_options(
            &self.settings.video,
            &self.caps,
            &self.picture_defaults,
            self.player.user_overrides(),
        )
    }

    /// 設定改了之後：非同步送出有變的選項（畫面輸出的選項同步設定要等畫面輸出執行緒，會卡住介面）
    pub(super) fn apply_video(&mut self) {
        let opts = self.video_options();
        for (name, key, result) in self.player.apply_picture(&opts, false) {
            match result {
                Ok(Some(id)) => {
                    self.async_pending.insert(id, name.to_owned());
                }
                Ok(None) => {}
                Err(e) => self.async_failed(key, name, &e.to_string()),
            }
        }
    }

    /// 改畫質設定：有變才套用、存檔
    fn change_video(&mut self, change: impl FnOnce(&mut VideoSettings)) {
        let before = self.settings.video.clone();
        change(&mut self.settings.video);
        if self.settings.video != before {
            self.apply_video();
            self.save_settings();
        }
    }

    /// 使用者用 VITASCOPE_MPV_OPTS（或測試的 `Options.extra`）指定了這個 mpv 選項：介面上不能改
    pub(super) fn video_locked(&self, name: &str) -> bool {
        mpv_opts_override(&self.player, name)
    }

    /// 選項停用的原因：軟體繪圖不支援（`needs_gpu`）、或 VITASCOPE_MPV_OPTS 指定了 `name`；可以改時 None
    pub(super) fn video_disabled(&self, needs_gpu: bool, name: &str) -> Option<&'static str> {
        if needs_gpu && self.caps.dumb {
            Some(dumb_hover())
        } else if self.video_locked(name) {
            Some(super::control_panel::adjust_locked_hover())
        } else {
            None
        }
    }

    // ───────────── 去交錯 ─────────────

    /// mpv 實際用的去交錯設定：VITASCOPE_MPV_OPTS 指定的話看 mpv 的值，不然是設定（引擎不支援自動時當成關閉）
    pub(super) fn deint_effective(&self) -> Deinterlace {
        if self.video_locked("deinterlace") {
            return match self.player.get_string("deinterlace").as_deref() {
                Ok("auto") => Deinterlace::Auto,
                Ok("yes") => Deinterlace::On,
                _ => Deinterlace::Off,
            };
        }
        self.settings.video.deinterlace.effective(&self.caps)
    }

    /// 目前有沒有在去交錯（「已去交錯」「逐行影片」）；沒有影片、引擎看不到（沒有 deinterlace-active）時 None
    pub(super) fn deint_status(&self) -> Option<&'static str> {
        let st = &self.player.state;
        (self.caps.deint_status && st.loaded && st.has_video())
            .then(|| picture::deinterlace_status(self.deint_effective(), st.deinterlace_active))
    }

    /// 「去交錯：自動（目前：逐行影片）」
    fn deint_osd_text(&self) -> String {
        let setting = self.deint_effective().label();
        match self.deint_status() {
            Some(now) => tf!(
                "去交錯：{setting}（目前：{now}）",
                "Deinterlacing: {setting} (now: {now})"
            ),
            None => tf!("去交錯：{setting}", "Deinterlacing: {setting}"),
        }
    }

    /// 改了去交錯之後，提示還在畫面上時跟著 mpv 的狀態更新（換濾鏡要等下一個影格）
    pub(super) fn refresh_deint_osd(&mut self) {
        let Some(at) = self.deint_osd else { return };
        if self.osd.as_ref().is_some_and(|(_, t)| *t == at) {
            let text = self.deint_osd_text();
            if let Some((current, _)) = &mut self.osd {
                *current = text;
            }
        } else {
            self.deint_osd = None;
        }
    }

    pub(super) fn set_deinterlace(&mut self, d: Deinterlace) {
        self.change_video(|v| v.deinterlace = d);
        let text = self.deint_osd_text();
        self.osd(text);
        self.deint_osd = self.osd.as_ref().map(|(_, at)| *at);
    }

    // ───────────── 去色帶、銳化、縮放演算法 ─────────────

    pub(super) fn set_deband(&mut self, s: Strength) {
        self.change_video(|v| v.deband = s);
        self.osd(tf!("去色帶：{}", "Debanding: {}", s.label()));
    }

    pub(super) fn set_sharpen(&mut self, s: Strength) {
        self.change_video(|v| v.sharpen = s);
        self.osd(tf!("銳化：{}", "Sharpening: {}", s.label()));
    }

    pub(super) fn set_quality(&mut self, q: Quality) {
        self.change_video(|v| v.quality = q);
        self.osd(tf!("縮放：{}", "Scaling: {}", q.label()));
    }

    pub(super) fn set_upscaler(&mut self, s: Option<Upscaler>) {
        self.change_video(|v| v.scale = s);
        let name = scaler_choice(s.map(Upscaler::label));
        self.osd(tf!("放大：{name}", "Upscaling: {name}"));
    }

    pub(super) fn set_downscaler(&mut self, s: Option<Downscaler>) {
        self.change_video(|v| v.dscale = s);
        let name = scaler_choice(s.map(Downscaler::label));
        self.osd(tf!("縮小：{name}", "Downscaling: {name}"));
    }

    pub(super) fn set_chroma_scaler(&mut self, s: Option<ChromaScaler>) {
        self.change_video(|v| v.cscale = s);
        let name = scaler_choice(s.map(ChromaScaler::label));
        self.osd(tf!("色度：{name}", "Chroma: {name}"));
    }

    // ───────────── HDR 色調映射 ─────────────

    pub(super) fn set_tone(&mut self, c: ToneCurve) {
        self.change_video(|v| v.tone.curve = c);
        self.osd(tf!("HDR 色調映射：{}", "HDR tone mapping: {}", c.label()));
    }

    pub(super) fn set_target_peak(&mut self, peak: Option<u32>) {
        let peak = peak.map(|p| p.clamp(picture::ToneSettings::MIN_PEAK, picture::ToneSettings::MAX_PEAK));
        self.change_video(|v| v.tone.target_peak = peak);
        let name = picture::peak_label(peak);
        self.osd(tf!("HDR 目標亮度：{name}", "HDR target brightness: {name}"));
    }

    pub(super) fn set_gamut(&mut self, g: Gamut) {
        self.change_video(|v| v.tone.gamut = g);
        self.osd(tf!("HDR 色域對應：{}", "HDR gamut mapping: {}", g.label()));
    }

    pub(super) fn set_compute_peak(&mut self, on: bool) {
        self.change_video(|v| v.tone.compute_peak = on);
        self.osd(tf!(
            "依畫面動態調整亮度：{}",
            "Adjust brightness to each scene: {}",
            on_off(on)
        ));
    }

    /// 去交錯可以選的值（引擎不支援自動時不列）
    pub(super) fn deint_choices(&self) -> Vec<Deinterlace> {
        Deinterlace::ALL
            .into_iter()
            .filter(|d| *d != Deinterlace::Auto || self.caps.deint_auto)
            .collect()
    }

    /// 目前的影片不是 HDR（有影片的時候才算；HDR 選項對它沒有作用）
    pub(super) fn video_not_hdr(&self) -> bool {
        let st = &self.player.state;
        st.loaded && st.has_video() && !st.video_hdr
    }
}
