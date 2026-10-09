//! 外觀：深色 / 淺色 / 跟隨系統，以及介面自己畫的顏色（控制列、播放清單、進度條…）。
//!
//! - 視窗、選單、設定視窗跟著主題；自己畫的顏色從 [`Palette`] 拿，不要寫死 `Color32::from_gray`。
//! - 影片畫面和蓋在影片上的東西（OSD、媒體資訊、進度條的提示與預覽、全螢幕的控制列、純音訊的歌名）
//!   兩種主題都是深色：有 `Ui` 的用 [`dark_overlay`]，直接畫的用固定的深色。
//! - egui 的 `set_visuals` 只改「目前」那個主題的樣式，而 eframe 預設跟隨系統：
//!   以前啟動時設深色，系統是淺色的 Windows、macOS 第一幀就換成 egui 的淺色樣式，蓋在寫死的深色面板上。
//!   所以一定要明確設定主題偏好（[`apply`]）。

use eframe::egui::{self, Color32, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

/// 使用者選的外觀（「設定 → 一般 → 外觀」）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// 預設深色（影片播放器多半是深色；升級後外觀不變）
    #[default]
    Dark,
    Light,
    /// 跟隨系統：Windows、macOS 由 egui 依系統回報；Linux 回報不了，暫時用深色
    System,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::Dark => crate::tr!("深色", "Dark"),
            ThemeChoice::Light => crate::tr!("淺色", "Light"),
            ThemeChoice::System => crate::tr!("跟隨系統", "Follow the system"),
        }
    }

    /// 對應的 egui 主題偏好
    pub fn preference(self) -> ThemePreference {
        match self {
            ThemeChoice::Dark => ThemePreference::Dark,
            ThemeChoice::Light => ThemePreference::Light,
            ThemeChoice::System => ThemePreference::System,
        }
    }

    /// 「切換深色／淺色」：深色 ↔ 淺色；跟隨系統時換成跟現在看到的（`shown`）相反的那個
    pub fn toggled(self, shown: Theme) -> Self {
        let now = match self {
            ThemeChoice::Dark => Theme::Dark,
            ThemeChoice::Light => Theme::Light,
            ThemeChoice::System => shown,
        };
        match now {
            Theme::Dark => ThemeChoice::Light,
            Theme::Light => ThemeChoice::Dark,
        }
    }
}

/// 跟隨系統卻問不到系統的主題時（Linux、沒有視窗的自動測試）用的主題
pub const FALLBACK: Theme = Theme::Dark;

/// 啟動時兩種主題都放 egui 的標準樣式（`set_visuals` 只會改到目前那一個）
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, Visuals::dark());
    ctx.set_visuals_of(Theme::Light, Visuals::light());
}

/// 套用使用者選的外觀（啟動時、改設定時）。標題列由 egui 送 `SetTheme` 跟著換，但還有已知的缺口（F4 處理）：
/// Windows 收到系統設定的廣播時，winit 會把標題列改回系統的顏色；X11 的跟隨系統一律是深色
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.options_mut(|o| {
        o.theme_preference = choice.preference();
        o.fallback_theme = FALLBACK;
    });
    ctx.request_repaint();
}

/// 選了跟隨系統、但作業系統沒有回報深淺色（Linux 目前都是這樣）：設定頁提示暫時用深色
pub fn system_unknown(ctx: &egui::Context, choice: ThemeChoice) -> bool {
    choice == ThemeChoice::System && ctx.system_theme().is_none()
}

/// 蓋在影片上的 `Ui`：不管目前的主題，一律用深色的樣式（影片是黑的，淺色的字和按鈕底色看不清楚）
pub fn dark_overlay(ui: &mut egui::Ui) {
    let dark = ui.ctx().style_of(Theme::Dark).visuals.clone();
    *ui.visuals_mut() = dark;
}

/// 介面自己畫的顏色（依目前 `Ui` 的樣式選深色或淺色的一組）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// 影片畫面的底色：兩種主題都是黑色
    pub video: Color32,
    /// 視窗模式的控制列
    pub panel: Color32,
    /// 播放清單面板
    pub side: Color32,
    /// 進度條的底
    pub track: Color32,
    /// 進度條上的章節間隔（跟控制列同色，看起來像切開）
    pub tick: Color32,
    /// 強調色：已播放的進度、播放中的項目、播放速度
    pub accent: Color32,
    /// 進度條上的 A-B 重播區段（半透明疊在進度條上）、只設了起點時的那條線
    pub ab: Color32,
    /// 進度條上方的書籤標記（小三角形）
    pub bookmark: Color32,
    /// 有問題的檔案、錯誤訊息
    pub problem: Color32,
    /// 次要的小字（例如「關於」的授權說明）
    pub faint: Color32,
}

impl Palette {
    /// 深色：跟以前寫死的顏色一樣
    const DARK: Palette = Palette {
        video: Color32::BLACK,
        panel: Color32::from_gray(24),
        side: Color32::from_gray(28),
        track: Color32::from_gray(70),
        tick: Color32::from_gray(24),
        accent: Color32::from_rgb(0x4f, 0x9d, 0xff),
        ab: Color32::from_rgb(0xff, 0xc1, 0x07),
        bookmark: Color32::from_rgb(0x66, 0xbb, 0x6a),
        problem: Color32::from_rgb(0xff, 0x8a, 0x80),
        faint: Color32::from_gray(150),
    };

    /// 淺色：淺底上的強調色、錯誤色要深一點才看得清楚
    fn light(v: &Visuals) -> Palette {
        Palette {
            video: Color32::BLACK,
            panel: v.panel_fill,
            side: Color32::from_gray(240),
            track: Color32::from_gray(200),
            tick: v.panel_fill,
            accent: Color32::from_rgb(0x1a, 0x6f, 0xd8),
            // 原本的琥珀色（#FFC107）跟淺灰的進度條底差不多亮，淺色主題看不出區段：換深一點的琥珀色
            ab: Color32::from_rgb(0xa6, 0x6f, 0x00),
            // 淺底上原本的綠色太淡：深一點的綠
            bookmark: Color32::from_rgb(0x2e, 0x7d, 0x32),
            problem: Color32::from_rgb(0xc6, 0x28, 0x28),
            faint: Color32::from_gray(110),
        }
    }

    pub fn of(visuals: &Visuals) -> Palette {
        if visuals.dark_mode {
            Self::DARK
        } else {
            Self::light(visuals)
        }
    }
}

/// 介面上用到的圖示字元（按鈕、選單、提示）。只能用 egui 內建字型（一般文字的字體）有的字，
/// 不然在沒有那些字的系統字型上會變成方塊（豆腐字）：新增圖示時加在這裡，`icons_have_glyphs` 會檢查。
/// - ⧉ ⤢ ✎ ⏻ ⏯ ＋（全形）內建字型沒有，不要用。
/// - 像素著色器組合裡的 ✕ ↑ ↓ 也不在內建字型裡（現在靠系統的中文字型畫出來），以後換圖示時再加進來；
///   ← → ↑ ↓ 在快捷鍵說明裡是按鍵名稱（文字），不算圖示。
pub const ICONS: &[&str] = &[
    "⏮", "⏸", "▶", "⏭", "⏹", "⛶", "🗁", "ℹ", "☰", "🔇", "🔊", "↺", "◀", "⚠", "♪", "×", "•",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 亮度（粗略）：判斷顏色是深還是淺
    fn luma(c: Color32) -> u32 {
        (u32::from(c.r()) * 299 + u32::from(c.g()) * 587 + u32::from(c.b()) * 114) / 1000
    }

    #[test]
    fn serialized_names() {
        for (choice, name) in [
            (ThemeChoice::Dark, "\"dark\""),
            (ThemeChoice::Light, "\"light\""),
            (ThemeChoice::System, "\"system\""),
        ] {
            assert_eq!(serde_json::to_string(&choice).unwrap(), name);
            assert_eq!(serde_json::from_str::<ThemeChoice>(name).unwrap(), choice);
        }
        assert_eq!(ThemeChoice::default(), ThemeChoice::Dark, "預設深色");
    }

    #[test]
    fn labels_in_both_languages() {
        let zh: Vec<&str> = ThemeChoice::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(zh, ["深色", "淺色", "跟隨系統"]);
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let en: Vec<&str> = ThemeChoice::ALL.iter().map(|c| c.label()).collect();
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(en, ["Dark", "Light", "Follow the system"]);
    }

    #[test]
    fn toggled_switches_between_dark_and_light() {
        assert_eq!(ThemeChoice::Dark.toggled(Theme::Dark), ThemeChoice::Light);
        assert_eq!(ThemeChoice::Light.toggled(Theme::Light), ThemeChoice::Dark);
        // 跟隨系統：換成跟現在看到的相反
        assert_eq!(ThemeChoice::System.toggled(Theme::Light), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::System.toggled(Theme::Dark), ThemeChoice::Light);
    }

    #[test]
    fn apply_sets_the_preference_and_the_fallback() {
        let ctx = egui::Context::default();
        ctx.options_mut(|o| o.fallback_theme = Theme::Light);
        for choice in ThemeChoice::ALL {
            apply(&ctx, choice);
            let (pref, fallback) = ctx.options(|o| (o.theme_preference, o.fallback_theme));
            assert_eq!(pref, choice.preference());
            assert_eq!(fallback, Theme::Dark, "問不到系統時用深色");
        }
        // 問不到系統（沒有回報）：跟隨系統 = 深色
        apply(&ctx, ThemeChoice::System);
        assert_eq!(ctx.theme(), Theme::Dark);
        assert!(system_unknown(&ctx, ThemeChoice::System));
        assert!(!system_unknown(&ctx, ThemeChoice::Light));
        apply(&ctx, ThemeChoice::Light);
        assert_eq!(ctx.theme(), Theme::Light);
    }

    #[test]
    fn install_keeps_both_styles_standard() {
        let ctx = egui::Context::default();
        // 以前的寫法：只改到目前的主題
        ctx.set_visuals_of(Theme::Light, Visuals::dark());
        install(&ctx);
        assert!(ctx.style_of(Theme::Dark).visuals.dark_mode);
        assert!(!ctx.style_of(Theme::Light).visuals.dark_mode);
    }

    #[test]
    fn palettes_differ_but_the_video_stays_black() {
        let dark = Palette::of(&Visuals::dark());
        let light = Palette::of(&Visuals::light());
        assert_eq!(dark.video, Color32::BLACK);
        assert_eq!(light.video, Color32::BLACK, "淺色主題的影片畫面也是黑的");
        assert_ne!(dark.panel, light.panel);
        assert_ne!(dark.track, light.track);
        assert_ne!(dark.problem, light.problem);
        // 深色跟以前寫死的顏色一樣（升級後外觀不變）
        assert_eq!(dark.panel, Color32::from_gray(24));
        assert_eq!(dark.side, Color32::from_gray(28));
        assert_eq!(dark.track, Color32::from_gray(70));
        assert_eq!(dark.problem, Color32::from_rgb(0xff, 0x8a, 0x80));
        assert_eq!(dark.accent, Color32::from_rgb(0x4f, 0x9d, 0xff));
        assert_eq!(dark.ab, Color32::from_rgb(0xff, 0xc1, 0x07));
        // 深色的控制列是深的、淺色是淺的；章節間隔跟控制列同色
        assert!(luma(dark.panel) < 60 && luma(dark.side) < 60);
        assert!(luma(light.panel) > 200 && luma(light.side) > 200);
        assert_eq!(dark.tick, dark.panel);
        assert_eq!(light.tick, light.panel);
        // 進度條的底跟控制列看得出差別；淺底上的錯誤色、強調色要夠深
        for p in [dark, light] {
            assert!(luma(p.panel).abs_diff(luma(p.track)) >= 40, "{p:?}");
        }
        // A-B 區段（六成不透明疊在進度條的底上）要看得出來：疊上去後亮度至少差 40
        for p in [dark, light] {
            let over = luma(p.ab) * 6 / 10 + luma(p.track) * 4 / 10;
            assert!(over.abs_diff(luma(p.track)) >= 40, "{p:?}");
        }
        assert!(luma(light.problem) < 110 && luma(light.accent) < 130, "{light:?}");
        // 書籤標記畫在控制列的底上：深色是亮的綠、淺色是深的綠，跟底色看得出差別
        assert_eq!(dark.bookmark, Color32::from_rgb(0x66, 0xbb, 0x6a));
        for p in [dark, light] {
            assert!(luma(p.panel).abs_diff(luma(p.bookmark)) >= 80, "{p:?}");
        }
        assert!(luma(light.faint) < luma(light.panel) - 100);
        assert!(luma(dark.faint) > luma(dark.panel) + 100);
    }

    #[test]
    fn dark_overlay_forces_dark_visuals_in_the_light_theme() {
        let ctx = egui::Context::default();
        install(&ctx);
        apply(&ctx, ThemeChoice::Light);
        let mut inside = None;
        let mut outside = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            outside = Some(ui.visuals().dark_mode);
            ui.scope(|ui| {
                dark_overlay(ui);
                inside = Some((ui.visuals().dark_mode, Palette::of(ui.visuals())));
            });
        });
        output.textures_delta.clear();
        assert_eq!(outside, Some(false), "外面是淺色");
        let (dark_mode, palette) = inside.unwrap();
        assert!(dark_mode, "蓋在影片上的是深色");
        assert_eq!(palette, Palette::DARK);
    }

    #[test]
    fn icons_have_glyphs() {
        use egui::epaint::text::{Fonts, TextOptions};
        // egui 內建的字型（不含系統的中文字型）：圖示要在這裡面，不然在沒有那些字型的電腦上會是方塊。
        // 不能用 `Fonts::has_glyphs`：跟「替代字元」在同一個字型裡的字（例如 ▶ ← ✕）它也回答沒有。
        // 改成實際排版，看畫出來的是不是替代字元的那一格
        let mut fonts = Fonts::new(TextOptions::default(), egui::FontDefinitions::default());
        let mut drawn_as = |c: char| {
            let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
                c.to_string(),
                egui::FontId::proportional(16.0),
                Color32::WHITE,
            );
            galley.rows[0].row.glyphs[0].uv_rect
        };
        // 一定沒有的字（私人使用區）畫出來就是替代字元
        let replacement = drawn_as('\u{10FFFD}');
        let mut missing: Vec<char> = ICONS
            .iter()
            .flat_map(|s| s.chars())
            .filter(|&c| drawn_as(c) == replacement)
            .collect();
        missing.dedup();
        assert!(missing.is_empty(), "內建字型沒有這些圖示：{missing:?}");
        // 檢查本身有效：已知沒有的字要被找出來
        for absent in ['⧉', '⤢', '✎', '⏻', '⏯', '＋', '✕'] {
            assert_eq!(drawn_as(absent), replacement, "{absent}");
        }
    }
}
