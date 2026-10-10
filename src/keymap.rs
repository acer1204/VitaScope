//! 快捷鍵對照表：所有使用者能操作的指令只有這一份清單（[`Command`]）。
//!
//! - 鍵盤：`handle_keys` → [`Keymap::lookup`] → `Command` → 介面的操作。
//! - 選單、提示上的按鍵說明一律從這裡產生（[`Keymap::hint`] 等），改了按鍵說明跟著變；
//!   影戲的預設組產生的字跟以前寫死的一模一樣。
//! - 指令的編號（[`Command::id`]）存在使用者的設定檔裡，發佈之後就不能改名。
//!
//! 按鍵的寫法（[`Chord`]）：
//! - **Cmd** 是 egui 的 `COMMAND`：Windows、Linux 是 Ctrl，macOS 是 ⌘。
//! - **Ctrl** 在 Windows、Linux 跟 Cmd 是同一個鍵；macOS 是 Control 鍵（裁切用 Control+Q，因為 ⌘Q 是結束程式）。
//!
//! 比對規則跟 egui 的 `consume_key`（`Modifiers::matches_logically`）一樣：多按的 Shift / Alt 不影響，
//! 指定了 Cmd 或 Ctrl 的按鍵多按另一個也算，沒有指定 Cmd / Ctrl 的按鍵按了就不算。
//! 不一樣的是不看登記的順序：符合的按鍵裡修飾鍵最多的優先，一樣多時 `Command::ALL` 裡排前面的優先。

use eframe::egui::{self, Key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 一個指令最多幾組按鍵
pub const MAX_CHORDS: usize = 4;

/// 按鍵說明、對照表要照哪個作業系統（測試可以在任何平台上檢查每個平台的表）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    pub const ALL: [Platform; 3] = [Platform::Windows, Platform::Mac, Platform::Linux];

    /// 目前執行的平台
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    fn is_mac(self) -> bool {
        self == Platform::Mac
    }
}

/// 修飾鍵（已依平台整理過：Windows、Linux 的 Ctrl 一律算 `cmd`，`ctrl` 只有 macOS 的 Control 鍵）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        cmd: false,
        ctrl: false,
        alt: false,
        shift: false,
    };
    pub const CMD: Mods = Mods {
        cmd: true,
        ..Mods::NONE
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        ..Mods::NONE
    };
    pub const ALT: Mods = Mods {
        alt: true,
        ..Mods::NONE
    };
    pub const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };

    /// egui 收到的修飾鍵 → 這個平台的寫法
    pub fn from_egui(m: egui::Modifiers, platform: Platform) -> Self {
        if platform.is_mac() {
            Mods {
                cmd: m.mac_cmd || m.command,
                ctrl: m.ctrl,
                alt: m.alt,
                shift: m.shift,
            }
        } else {
            Mods {
                cmd: m.ctrl || m.command || m.mac_cmd,
                ctrl: false,
                alt: m.alt,
                shift: m.shift,
            }
        }
    }

    /// Windows、Linux 沒有分開的 Control：併進 Cmd
    fn normalized(self, platform: Platform) -> Self {
        if platform.is_mac() {
            self
        } else {
            Mods {
                cmd: self.cmd || self.ctrl,
                ctrl: false,
                ..self
            }
        }
    }

    /// 按了幾個修飾鍵（越多越優先）
    fn count(self) -> u32 {
        u32::from(self.cmd) + u32::from(self.ctrl) + u32::from(self.alt) + u32::from(self.shift)
    }

    /// 按下的修飾鍵（`pressed`）符不符合這組：指定的都要按；多按的 Shift、Alt 不管；
    /// 指定了 Cmd 或 Ctrl 時多按另一個也算，都沒指定時兩個都不能按（跟 egui 的 `matches_logically` 一樣）
    fn accepts(self, pressed: Mods) -> bool {
        if (self.shift && !pressed.shift) || (self.alt && !pressed.alt) {
            return false;
        }
        if !self.cmd && !self.ctrl {
            return !pressed.cmd && !pressed.ctrl;
        }
        (!self.cmd || pressed.cmd) && (!self.ctrl || pressed.ctrl)
    }
}

/// 一組按鍵：一個鍵 + 修飾鍵
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub mods: Mods,
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Self {
        Self { key, mods }
    }

    /// 讀設定檔裡的寫法（`"Cmd+Shift+K"`、`"Space"`），也看得懂選單上的寫法（`"Ctrl+PgUp"`、`"Option+←"`）。
    /// 修飾鍵不分大小寫；修飾鍵本身（Shift…）不能當成按鍵
    pub fn parse(text: &str, platform: Platform) -> Option<Self> {
        let text = text.trim();
        // 「+」鍵本身：「Cmd++」、「+」
        let (mods_part, key_part) = match text.strip_suffix("++") {
            Some(rest) => (Some(rest), "+"),
            None if text == "+" => (None, "+"),
            None => match text.rsplit_once('+') {
                Some((m, k)) => (Some(m), k),
                None => (None, text),
            },
        };
        let mut mods = Mods::NONE;
        for part in mods_part.into_iter().flat_map(|m| m.split('+')) {
            match part.trim().to_ascii_lowercase().as_str() {
                "cmd" | "command" | "⌘" => mods.cmd = true,
                // Super／Meta 只在 macOS 是 ⌘；Windows、Linux 的 egui 收不到這個修飾鍵（Linux 的 Meta 通常是 Alt），
                // 不能悄悄變成 Ctrl
                "super" | "meta" if platform.is_mac() => mods.cmd = true,
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" | "opt" | "⌥" => mods.alt = true,
                "shift" | "⇧" => mods.shift = true,
                _ => return None,
            }
        }
        let key = parse_key(key_part.trim())?;
        if is_modifier_key(key) {
            return None;
        }
        Some(Self {
            key,
            mods: mods.normalized(platform),
        })
    }

    /// 存進設定檔的寫法（每個平台一樣：Cmd = Windows、Linux 的 Ctrl、macOS 的 ⌘；Ctrl 只用在 macOS 的 Control）
    pub fn to_config(&self) -> String {
        let mut s = String::new();
        for (on, name) in [
            (self.mods.cmd, "Cmd+"),
            (self.mods.ctrl, "Ctrl+"),
            (self.mods.alt, "Alt+"),
            (self.mods.shift, "Shift+"),
        ] {
            if on {
                s.push_str(name);
            }
        }
        s.push_str(self.key.name());
        s
    }

    /// 選單、提示上的寫法：Windows、Linux 是「Ctrl+O」「Alt+G」，macOS 是「Cmd+O」「Option+G」「Control+Q」
    pub fn display(&self, platform: Platform) -> String {
        let mut s = String::new();
        if self.mods.cmd {
            s.push_str(if platform.is_mac() { "Cmd+" } else { "Ctrl+" });
        }
        if self.mods.ctrl {
            s.push_str("Control+");
        }
        if self.mods.alt {
            s.push_str(if platform.is_mac() { "Option+" } else { "Alt+" });
        }
        if self.mods.shift {
            s.push_str("Shift+");
        }
        s.push_str(key_display(self.key));
        s
    }

    /// 這組按鍵不會以按鍵事件送到程式：egui 把 Cmd+C 變成「複製」（`Event::Copy`），只能從那裡觸發
    fn arrives_as_copy(&self) -> bool {
        self.key == Key::C && self.mods.cmd
    }
}

/// 按鍵名稱：egui 的名稱（`Key::from_name`，含單一字元），加上選單上的寫法
fn parse_key(name: &str) -> Option<Key> {
    let alias = match name {
        "←" => Some(Key::ArrowLeft),
        "→" => Some(Key::ArrowRight),
        "↑" => Some(Key::ArrowUp),
        "↓" => Some(Key::ArrowDown),
        "PgUp" => Some(Key::PageUp),
        "PgDn" => Some(Key::PageDown),
        "空白鍵" => Some(Key::Space),
        _ => None,
    };
    alias.or_else(|| Key::from_name(name)).or_else(|| {
        // 大小寫寫錯（「space」「pageup」）
        Key::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(name))
    })
}

/// 左右的 Shift、Ctrl、Alt、Super：egui 也會送這些按鍵事件，但只能當修飾鍵
fn is_modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// 按鍵在選單上的寫法
fn key_display(key: Key) -> &'static str {
    match key {
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Space => crate::tr!("空白鍵", "Space"),
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        Key::Escape => "Esc",
        // egui 的符號是數學的減號（U+2212）
        Key::Minus => "-",
        _ => key.symbol_or_name(),
    }
}

/// 系統保留、不能指定的按鍵：
/// - Cmd+X、Cmd+V（egui 變成剪下、貼上，不是按鍵事件；Cmd+C 例外，從「複製」觸發）；
///   Cmd+C 再加別的修飾鍵（Cmd+Shift+C）也會變成「複製」，分不出來，所以也保留；
/// - Windows：Shift+Delete、Shift+Insert、Ctrl+Insert（同上），Alt+F4（關閉視窗）；
/// - macOS：⌘Q、⌘H、⌘⌥H（系統選單）；
/// - Esc（固定用來關視窗、離開全螢幕）。
pub fn reserved(chord: Chord, platform: Platform) -> bool {
    let m = chord.mods;
    let only = |mods: Mods| m == mods;
    match chord.key {
        Key::Escape | Key::Cut | Key::Copy | Key::Paste => true,
        Key::X | Key::V if m.cmd => true,
        Key::C if m.cmd && m != Mods::CMD => true,
        Key::Delete if platform == Platform::Windows && m.shift => true,
        Key::Insert if platform == Platform::Windows && (m.shift || m.cmd) => true,
        Key::F4 if platform == Platform::Windows && only(Mods::ALT) => true,
        Key::Q if platform.is_mac() && only(Mods::CMD) => true,
        Key::H if platform.is_mac() && (only(Mods::CMD) || only(Mods { alt: true, ..Mods::CMD })) => true,
        _ => false,
    }
}

/// 指令的分組（設定頁、說明的順序）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Playback,
    Sound,
    Subtitles,
    Picture,
    Quality,
    Window,
    Files,
    Bookmarks,
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::Playback,
        Group::Sound,
        Group::Subtitles,
        Group::Picture,
        Group::Quality,
        Group::Window,
        Group::Files,
        Group::Bookmarks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Playback => crate::tr!("播放", "Playback"),
            Group::Sound => crate::tr!("音量與音效", "Volume and sound"),
            Group::Subtitles => crate::tr!("字幕", "Subtitles"),
            // 跟右鍵選單一樣：「畫面」是 Picture，「畫質」是 Video quality
            Group::Picture => crate::tr!("畫面", "Picture"),
            Group::Quality => crate::tr!("畫質", "Video quality"),
            Group::Window => crate::tr!("視窗", "Window"),
            Group::Files => crate::tr!("檔案與截圖", "Files and screenshots"),
            Group::Bookmarks => crate::tr!("書籤", "Bookmarks"),
        }
    }
}

/// 定義所有指令：名稱 => 編號, 分組, 按住時重複（repeat / once）, (中文, English)
macro_rules! commands {
    ($( $(#[$doc:meta])* $name:ident => $id:literal, $group:ident, $repeat:ident, ($zh:literal, $en:literal); )*) => {
        /// 使用者能操作的指令（快捷鍵、選單、之後的滑鼠按鍵與系統媒體控制都走這裡）
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Command {
            $( $(#[$doc])* $name, )*
        }

        impl Command {
            /// 全部的指令；順序 = 設定頁的順序、按鍵衝突時的優先順序
            pub const ALL: &'static [Command] = &[ $( Command::$name, )* ];

            /// 存在設定檔裡的編號（發佈之後不能改名）
            pub fn id(self) -> &'static str {
                match self { $( Command::$name => $id, )* }
            }

            pub fn label(self) -> &'static str {
                match self { $( Command::$name => crate::tr!($zh, $en), )* }
            }

            pub fn group(self) -> Group {
                match self { $( Command::$name => Group::$group, )* }
            }

            /// 按住不放（鍵盤自動重複）時要不要一直做：跳轉、音量這類要；開關類的不要（按住空白鍵不會一直切換）
            pub fn repeatable(self) -> bool {
                match self { $( Command::$name => commands!(@repeat $repeat), )* }
            }
        }
    };
    (@repeat repeat) => { true };
    (@repeat once) => { false };
}

commands! {
    // 編號一經發佈就不能改名、不能刪（存在使用者的設定裡）；新指令加在各組最後，也加進測試的 GOLDEN_IDS
    TogglePause => "toggle-pause", Playback, once, ("播放／暫停", "Play / pause");
    Stop => "stop", Playback, once, ("停止", "Stop");
    Restart => "restart", Playback, once, ("從頭播放", "Play from the start");
    SeekBack => "seek-back", Playback, repeat, ("後退", "Seek backward");
    SeekForward => "seek-forward", Playback, repeat, ("前進", "Seek forward");
    SeekBackLong => "seek-back-long", Playback, repeat, ("大幅後退", "Seek further backward");
    SeekForwardLong => "seek-forward-long", Playback, repeat, ("大幅前進", "Seek further forward");
    FrameNext => "frame-next", Playback, repeat, ("逐格前進", "Next frame");
    FramePrev => "frame-prev", Playback, repeat, ("逐格後退", "Previous frame");
    SpeedUp => "speed-up", Playback, repeat, ("加快", "Faster");
    SpeedDown => "speed-down", Playback, repeat, ("減慢", "Slower");
    SpeedReset => "speed-reset", Playback, once, ("正常速度", "Normal speed");
    AbLoop => "ab-loop", Playback, once, ("A-B 重播", "A-B loop");
    PrevFile => "prev-file", Playback, once, ("上一個檔案", "Previous file");
    NextFile => "next-file", Playback, once, ("下一個檔案", "Next file");
    PrevChapter => "prev-chapter", Playback, once, ("上一章", "Previous chapter");
    NextChapter => "next-chapter", Playback, once, ("下一章", "Next chapter");
    AbSetStart => "ab-set-start", Playback, once, ("A-B 重播：設定起點", "A-B loop: set start");
    AbSetEnd => "ab-set-end", Playback, once, ("A-B 重播：設定終點", "A-B loop: set end");
    AbClear => "ab-clear", Playback, once, ("取消 A-B 重播", "Cancel A-B loop");
    VolumeUp => "volume-up", Sound, repeat, ("音量 +", "Volume up");
    VolumeDown => "volume-down", Sound, repeat, ("音量 −", "Volume down");
    ToggleMute => "toggle-mute", Sound, once, ("靜音", "Mute");
    AudioDelayDown => "audio-delay-down", Sound, repeat, ("聲音提早", "Audio earlier");
    AudioDelayUp => "audio-delay-up", Sound, repeat, ("聲音延後", "Audio later");
    AudioDelayReset => "audio-delay-reset", Sound, once, ("音訊延遲歸零", "Reset audio delay");
    NextAudioTrack => "next-audio-track", Sound, once, ("下一條音軌", "Next audio track");
    ToggleEq => "toggle-eq", Sound, once, ("等化器開／關", "Equalizer on/off");
    SubDelayDown => "sub-delay-down", Subtitles, repeat, ("字幕提早", "Subtitles earlier");
    SubDelayUp => "sub-delay-up", Subtitles, repeat, ("字幕延後", "Subtitles later");
    SubDelayReset => "sub-delay-reset", Subtitles, once, ("字幕延遲歸零", "Reset subtitle delay");
    NextSubtitle => "next-subtitle", Subtitles, once, ("下一個字幕", "Next subtitle");
    AspectCycle => "aspect-cycle", Picture, once, ("畫面比例", "Aspect ratio");
    CropCycle => "crop-cycle", Picture, once, ("裁切", "Crop");
    ZoomIn => "zoom-in", Picture, repeat, ("放大", "Zoom in");
    ZoomOut => "zoom-out", Picture, repeat, ("縮小", "Zoom out");
    ZoomReset => "zoom-reset", Picture, once, ("縮放 100%", "Zoom 100%");
    PanLeft => "pan-left", Picture, repeat, ("畫面左移", "Move the picture left");
    PanRight => "pan-right", Picture, repeat, ("畫面右移", "Move the picture right");
    PanUp => "pan-up", Picture, repeat, ("畫面上移", "Move the picture up");
    PanDown => "pan-down", Picture, repeat, ("畫面下移", "Move the picture down");
    PanCenter => "pan-center", Picture, once, ("畫面置中", "Center the picture");
    Rotate => "rotate", Picture, once, ("旋轉 90°", "Rotate 90°");
    FlipH => "flip-h", Picture, once, ("左右翻轉", "Flip horizontally");
    FlipV => "flip-v", Picture, once, ("上下翻轉", "Flip vertically");
    ResetView => "reset-view", Picture, once, ("重設畫面", "Reset the picture");
    FillWindow => "fill-window", Picture, once, ("填滿視窗", "Fill window");
    BrightnessDown => "brightness-down", Quality, repeat, ("亮度 −", "Brightness −");
    BrightnessUp => "brightness-up", Quality, repeat, ("亮度 +", "Brightness +");
    ContrastDown => "contrast-down", Quality, repeat, ("對比 −", "Contrast −");
    ContrastUp => "contrast-up", Quality, repeat, ("對比 +", "Contrast +");
    SaturationDown => "saturation-down", Quality, repeat, ("飽和度 −", "Saturation −");
    SaturationUp => "saturation-up", Quality, repeat, ("飽和度 +", "Saturation +");
    HueDown => "hue-down", Quality, repeat, ("色相 −", "Hue −");
    HueUp => "hue-up", Quality, repeat, ("色相 +", "Hue +");
    AdjustReset => "adjust-reset", Quality, once, ("影像調整還原", "Reset image adjustments");
    GammaDown => "gamma-down", Quality, repeat, ("Gamma −", "Gamma −");
    GammaUp => "gamma-up", Quality, repeat, ("Gamma +", "Gamma +");
    ToggleSmooth => "toggle-smooth", Quality, once, ("流暢播放開／關", "Smooth playback on/off");
    Fullscreen => "fullscreen", Window, once, ("全螢幕", "Fullscreen");
    OnTop => "on-top", Window, once, ("切換視窗置頂模式", "Cycle always-on-top mode");
    ControlPanel => "control-panel", Window, once, ("控制面板", "Control panel");
    Playlist => "playlist", Window, once, ("播放清單", "Playlist");
    MediaInfo => "media-info", Window, once, ("媒體資訊", "Media info");
    Settings => "settings", Window, once, ("設定", "Settings");
    About => "about", Window, once, ("關於", "About");
    CycleTheme => "cycle-theme", Window, once, ("切換深色／淺色", "Switch dark/light");
    OpenFile => "open-file", Files, once, ("開啟檔案", "Open file");
    Screenshot => "screenshot", Files, once, ("擷取畫面（存檔）", "Save screenshot");
    CopyFrame => "copy-frame", Files, once, ("擷取畫面（剪貼簿）", "Copy frame");
    ScreenshotAs => "screenshot-as", Files, once, ("另存截圖…", "Save screenshot as…");
    LoadSubtitle => "load-subtitle", Files, once, ("載入字幕檔…", "Load subtitle file…");
    OpenUrl => "open-url", Files, once, ("開啟網址…", "Open URL…");
    /// 匯出視窗的「片段」（預設沒有按鍵）
    ExportClip => "export-clip", Files, once, ("儲存片段…", "Save clip…");
    BookmarkAdd => "bookmark-add", Bookmarks, once, ("新增書籤", "Add bookmark");
    BookmarkPrev => "bookmark-prev", Bookmarks, once, ("上一個書籤", "Previous bookmark");
    BookmarkNext => "bookmark-next", Bookmarks, once, ("下一個書籤", "Next bookmark");
    /// 側邊面板的書籤分頁（英文不叫 "Bookmarks"：跟群組、右鍵選單的「書籤 ▸」同名）
    BookmarkList => "bookmark-list", Bookmarks, once, ("書籤清單", "Bookmark list");
}

impl Command {
    /// 設定檔裡的編號 → 指令（新版加的、打錯的：None）
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.id() == id)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// 內建的按鍵組合
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyPreset {
    /// 影戲原本的按鍵
    #[default]
    Vitascope,
    /// 比照 PotPlayer 的按鍵（只換幾個 PotPlayer 使用者手會記得的鍵，其他跟影戲一樣）
    Potplayer,
}

impl KeyPreset {
    pub const ALL: [KeyPreset; 2] = [KeyPreset::Vitascope, KeyPreset::Potplayer];

    pub fn label(self) -> &'static str {
        match self {
            KeyPreset::Vitascope => crate::tr!("影戲", "VitaScope"),
            KeyPreset::Potplayer => crate::tr!("PotPlayer 風格", "PotPlayer style"),
        }
    }

    /// 這個預設組在 `platform` 上的按鍵（同一個指令的第一組是選單上顯示的）
    pub fn bindings(self, platform: Platform) -> Vec<(Command, Chord)> {
        match self {
            KeyPreset::Vitascope => vitascope_preset(platform),
            KeyPreset::Potplayer => potplayer_preset(platform),
        }
    }
}

/// 影戲的預設組（跟 v0.3.0 寫死的按鍵一樣）
fn vitascope_preset(platform: Platform) -> Vec<(Command, Chord)> {
    use Command as C;
    const N: Mods = Mods::NONE;
    const CMD: Mods = Mods::CMD;
    const ALT: Mods = Mods::ALT;
    const SHIFT: Mods = Mods::SHIFT;
    let mut list = vec![
        (C::TogglePause, Chord::new(N, Key::Space)),
        (C::Restart, Chord::new(N, Key::Home)),
        (C::SeekBack, Chord::new(N, Key::ArrowLeft)),
        (C::SeekForward, Chord::new(N, Key::ArrowRight)),
        (C::SeekBackLong, Chord::new(CMD, Key::ArrowLeft)),
        (C::SeekForwardLong, Chord::new(CMD, Key::ArrowRight)),
        (C::FrameNext, Chord::new(N, Key::Period)),
        (C::FramePrev, Chord::new(N, Key::Comma)),
        (C::SpeedUp, Chord::new(N, Key::C)),
        (C::SpeedDown, Chord::new(N, Key::X)),
        (C::SpeedReset, Chord::new(N, Key::Z)),
        (C::AbLoop, Chord::new(N, Key::L)),
        (C::PrevFile, Chord::new(N, Key::PageUp)),
        (C::NextFile, Chord::new(N, Key::PageDown)),
        (C::PrevChapter, Chord::new(CMD, Key::PageUp)),
        (C::NextChapter, Chord::new(CMD, Key::PageDown)),
        // 書籤：檔案、章節、書籤用同一組鍵、不同的修飾鍵。以前 Shift+PgUp / PgDn 是換檔（多按的 Shift 不影響），
        // 現在修飾鍵多的優先，變成跳書籤
        (C::BookmarkPrev, Chord::new(SHIFT, Key::PageUp)),
        (C::BookmarkNext, Chord::new(SHIFT, Key::PageDown)),
        (C::BookmarkAdd, Chord::new(N, Key::P)),
        (C::BookmarkList, Chord::new(N, Key::H)),
        (C::VolumeUp, Chord::new(N, Key::ArrowUp)),
        (C::VolumeDown, Chord::new(N, Key::ArrowDown)),
        (C::ToggleMute, Chord::new(N, Key::M)),
        (C::AudioDelayDown, Chord::new(N, Key::Minus)),
        (C::AudioDelayUp, Chord::new(N, Key::Equals)),
        // 數字鍵盤的 +、德文鍵盤的 + 鍵是 Plus，不是 Equals
        (C::AudioDelayUp, Chord::new(N, Key::Plus)),
        (C::SubDelayDown, Chord::new(N, Key::OpenBracket)),
        (C::SubDelayUp, Chord::new(N, Key::CloseBracket)),
        (C::AspectCycle, Chord::new(N, Key::A)),
        (C::AspectCycle, Chord::new(CMD, Key::F6)),
        // 裁切用 Ctrl（macOS 是 Control 鍵）：⌘Q 是結束程式
        (C::CropCycle, Chord::new(Mods::CTRL, Key::Q)),
        (C::ZoomIn, Chord::new(N, Key::Num9)),
        (C::ZoomOut, Chord::new(N, Key::Num1)),
        (C::ZoomReset, Chord::new(N, Key::Num5)),
        (C::PanLeft, Chord::new(ALT, Key::ArrowLeft)),
        (C::PanRight, Chord::new(ALT, Key::ArrowRight)),
        (C::PanUp, Chord::new(ALT, Key::ArrowUp)),
        (C::PanDown, Chord::new(ALT, Key::ArrowDown)),
        (C::PanCenter, Chord::new(CMD, Key::Num5)),
        (C::Rotate, Chord::new(ALT, Key::K)),
        (C::FlipH, Chord::new(CMD, Key::Z)),
        (C::FlipV, Chord::new(CMD, Key::P)),
        (C::ResetView, Chord::new(ALT, Key::Backspace)),
        // 影像調整（PotPlayer 的按鍵）
        (C::BrightnessDown, Chord::new(N, Key::W)),
        (C::BrightnessUp, Chord::new(N, Key::E)),
        (C::ContrastDown, Chord::new(N, Key::R)),
        (C::ContrastUp, Chord::new(N, Key::T)),
        (C::SaturationDown, Chord::new(N, Key::Y)),
        (C::SaturationUp, Chord::new(N, Key::U)),
        (C::HueDown, Chord::new(N, Key::I)),
        (C::HueUp, Chord::new(N, Key::O)),
        (C::AdjustReset, Chord::new(N, Key::Q)),
        (C::Fullscreen, Chord::new(N, Key::F)),
        (C::Fullscreen, Chord::new(N, Key::Enter)),
        (C::OnTop, Chord::new(CMD, Key::T)),
        (C::ControlPanel, Chord::new(ALT, Key::G)),
        (C::Playlist, Chord::new(N, Key::F6)),
    ];
    // macOS 的 Ctrl+F1 是系統的「鍵盤操作」快捷鍵，選單上寫 Cmd+I（QuickTime 的「影片檢閱器」）
    let info = [Chord::new(CMD, Key::F1), Chord::new(CMD, Key::I)];
    if platform.is_mac() {
        list.extend(info.iter().rev().map(|c| (C::MediaInfo, *c)));
    } else {
        list.extend(info.iter().map(|c| (C::MediaInfo, *c)));
    }
    list.extend([
        (C::Settings, Chord::new(N, Key::F5)),
        (C::About, Chord::new(N, Key::F1)),
        (C::OpenFile, Chord::new(CMD, Key::O)),
        // 開啟網址：Ctrl+U（macOS ⌘U），跟 PotPlayer 一樣。只按 U 是飽和度 +，有 Ctrl 的不會混在一起
        (C::OpenUrl, Chord::new(CMD, Key::U)),
        (C::Screenshot, Chord::new(CMD, Key::E)),
        // egui 送的是「複製」（Event::Copy），不是按鍵事件
        (C::CopyFrame, Chord::new(CMD, Key::C)),
    ]);
    list.into_iter()
        .map(|(c, chord)| (c, Chord::new(chord.mods.normalized(platform), chord.key)))
        .collect()
}

/// PotPlayer 風格：以影戲的為底，換掉這些指令的按鍵（主人對照 PotPlayer 確認後只改這張表）：
/// - Backspace 從頭播放（取代 Home）；F / D 逐格前進 / 後退（`.` `,` 空出來）；
/// - 全螢幕 Enter、Alt+Enter（F 給了逐格）；F3 也是開啟檔案；
/// - `[` `]` `\` A-B 重播的起點、終點、取消；字幕提早 / 延後改成 Shift+, / Shift+.（`<` `>`），`/` 字幕延遲歸零。
///   `<` `>` 不是 egui 的按鍵，美式鍵盤上收到的是 Shift + 逗號 / 句號（其他配置的 `<` 可能是別的鍵，使用者自己改）；
/// - L（A-B 依序切換）沒有按鍵：PotPlayer 用 `[` `]` `\`（設計 P§2.1 的表）
fn potplayer_preset(platform: Platform) -> Vec<(Command, Chord)> {
    use Command as C;
    const N: Mods = Mods::NONE;
    const CMD: Mods = Mods::CMD;
    const ALT: Mods = Mods::ALT;
    const SHIFT: Mods = Mods::SHIFT;
    let changed: [(Command, &[Chord]); 12] = [
        (C::Restart, &[Chord::new(N, Key::Backspace)]),
        (C::AbLoop, &[]),
        (C::FrameNext, &[Chord::new(N, Key::F)]),
        (C::FramePrev, &[Chord::new(N, Key::D)]),
        (C::Fullscreen, &[Chord::new(N, Key::Enter), Chord::new(ALT, Key::Enter)]),
        (C::OpenFile, &[Chord::new(CMD, Key::O), Chord::new(N, Key::F3)]),
        (C::AbSetStart, &[Chord::new(N, Key::OpenBracket)]),
        (C::AbSetEnd, &[Chord::new(N, Key::CloseBracket)]),
        (C::AbClear, &[Chord::new(N, Key::Backslash)]),
        (C::SubDelayDown, &[Chord::new(SHIFT, Key::Comma)]),
        (C::SubDelayUp, &[Chord::new(SHIFT, Key::Period)]),
        (C::SubDelayReset, &[Chord::new(N, Key::Slash)]),
    ];
    let mut list: Vec<(Command, Chord)> = vitascope_preset(platform)
        .into_iter()
        .filter(|(cmd, _)| !changed.iter().any(|(c, _)| c == cmd))
        .collect();
    for (cmd, chords) in changed {
        list.extend(
            chords
                .iter()
                .map(|c| (cmd, Chord::new(c.mods.normalized(platform), c.key))),
        );
    }
    list
}

/// 可以改的滑鼠按鍵（在影片畫面上；滾輪另外是 [`WheelMode`]）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseInput {
    Click,
    DoubleClick,
    Middle,
    /// 側鍵「上一頁」（egui 的 Extra1）
    Back,
    /// 側鍵「下一頁」（egui 的 Extra2）
    Forward,
}

impl MouseInput {
    pub const ALL: [MouseInput; 5] = [
        MouseInput::Click,
        MouseInput::DoubleClick,
        MouseInput::Middle,
        MouseInput::Back,
        MouseInput::Forward,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MouseInput::Click => crate::tr!("單擊畫面", "Click on the video"),
            MouseInput::DoubleClick => crate::tr!("雙擊畫面", "Double-click on the video"),
            MouseInput::Middle => crate::tr!("中鍵", "Middle button"),
            MouseInput::Back => crate::tr!("側鍵（上一頁）", "Back button"),
            MouseInput::Forward => crate::tr!("側鍵（下一頁）", "Forward button"),
        }
    }

    /// 可以選的指令（另外都可以選「不動作」）；None = 任何指令。
    /// 單擊只能是開關（雙擊時要把第一下做的切回來）。
    /// F1 / F2 要接手：雙擊加上迷你播放器、子母畫面；在迷你播放器、子母畫面裡雙擊「全螢幕」是回到一般視窗
    /// （app.rs 的 video_mouse 要改）
    pub fn choices(self) -> Option<&'static [Command]> {
        match self {
            MouseInput::Click => Some(&[Command::TogglePause, Command::ToggleMute]),
            MouseInput::DoubleClick => Some(&[Command::Fullscreen]),
            MouseInput::Middle | MouseInput::Back | MouseInput::Forward => None,
        }
    }
}

/// 在影片畫面上捲動滾輪做什麼（Ctrl / ⌘ + 滾輪固定是縮放）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WheelMode {
    /// 音量 ±5（比照 PotPlayer）
    #[default]
    Volume,
    /// 每格跳轉「跳轉秒數」，往上捲 = 前進（舊版 mpv 的預設方向；現在的 mpv 預設滾輪是音量）
    Seek,
    None,
}

impl WheelMode {
    pub const ALL: [WheelMode; 3] = [WheelMode::Volume, WheelMode::Seek, WheelMode::None];

    /// `seek_secs` 是每格跳幾秒（「設定 → 播放」的跳轉秒數）
    pub fn label(self, seek_secs: f64) -> String {
        match self {
            WheelMode::Volume => crate::tr!("音量", "Volume").to_owned(),
            WheelMode::Seek => crate::tf!("跳轉（每格 {seek_secs} 秒）", "Seek ({seek_secs} s per notch)"),
            WheelMode::None => no_action_label().to_owned(),
        }
    }
}

/// 滑鼠按鍵指定為「不動作」
pub fn no_action_label() -> &'static str {
    crate::tr!("不動作", "Do nothing")
}

/// 滑鼠按鍵的設定（`settings.json` 的 `keys.mouse`）。按鍵存指令編號（跟 `custom` 一樣）；"" = 不動作。
/// 認不得的編號（新版的指令）照樣保留在檔案裡，這版當成不動作
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MouseSettings {
    pub click: String,
    pub double_click: String,
    pub middle: String,
    pub back: String,
    pub forward: String,
    pub wheel: WheelMode,
}

impl Default for MouseSettings {
    /// 兩個預設組一樣：單擊播放／暫停、雙擊全螢幕、滾輪音量（跟 v0.3.0 一樣），其他不動作
    fn default() -> Self {
        Self {
            click: Command::TogglePause.id().to_owned(),
            double_click: Command::Fullscreen.id().to_owned(),
            middle: String::new(),
            back: String::new(),
            forward: String::new(),
            wheel: WheelMode::Volume,
        }
    }
}

impl MouseSettings {
    /// 這個按鍵存的指令編號（"" = 不動作）
    pub fn id(&self, input: MouseInput) -> &str {
        match input {
            MouseInput::Click => &self.click,
            MouseInput::DoubleClick => &self.double_click,
            MouseInput::Middle => &self.middle,
            MouseInput::Back => &self.back,
            MouseInput::Forward => &self.forward,
        }
    }

    fn id_mut(&mut self, input: MouseInput) -> &mut String {
        match input {
            MouseInput::Click => &mut self.click,
            MouseInput::DoubleClick => &mut self.double_click,
            MouseInput::Middle => &mut self.middle,
            MouseInput::Back => &mut self.back,
            MouseInput::Forward => &mut self.forward,
        }
    }

    /// 這個按鍵做的指令；不動作、認不得的編號是 None
    pub fn command(&self, input: MouseInput) -> Option<Command> {
        Command::from_id(self.id(input))
    }

    /// 改這個按鍵的指令（None = 不動作）
    pub fn set(&mut self, input: MouseInput, cmd: Option<Command>) {
        *self.id_mut(input) = cmd.map(|c| c.id().to_owned()).unwrap_or_default();
    }

    /// 選單上的寫法：指令名稱、「不動作」；認不得的編號照原樣寫出來（新版的指令）
    pub fn label(&self, input: MouseInput) -> String {
        let id = self.id(input);
        match Command::from_id(id) {
            Some(cmd) => cmd.label().to_owned(),
            None if id.is_empty() => no_action_label().to_owned(),
            None => id.to_owned(),
        }
    }

    /// 這個按鍵跟預設的不一樣
    pub fn changed(&self, input: MouseInput) -> bool {
        self.id(input) != Self::default().id(input)
    }
}

/// 快捷鍵的設定（`settings.json` 的 `keys`）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeySettings {
    /// 用哪一組內建的按鍵
    pub preset: KeyPreset,
    /// 自己改過的：指令編號（例如 "toggle-pause"）→ 按鍵（例如 ["Space", "Shift+K"]）；空陣列 = 不指定。
    /// 用字串存：認不得的指令、按鍵（新版寫的、打錯的）照樣保留在檔案裡，只是這版不用
    pub custom: BTreeMap<String, Vec<String>>,
    /// 滑鼠按鍵（兩個預設組一樣）
    pub mouse: MouseSettings,
}

impl KeySettings {
    /// 讀檔後整理：每個指令的按鍵去掉重複的、最多 `MAX_CHORDS` 組（認不得的照樣保留）
    pub fn sanitized(mut self) -> Self {
        for chords in self.custom.values_mut() {
            let mut seen = Vec::new();
            chords.retain(|c| {
                let keep = seen.len() < MAX_CHORDS && !seen.contains(c);
                if keep {
                    seen.push(c.clone());
                }
                keep
            });
        }
        self
    }

    /// 這個指令的按鍵改過（跟預設組不一樣；也包含改成「不指定」）
    pub fn overridden(&self, cmd: Command) -> bool {
        self.custom.contains_key(cmd.id())
    }

    /// 改過幾個指令（認不得的編號不算：這版看不到、也改不到）
    pub fn override_count(&self) -> usize {
        self.custom.keys().filter(|id| Command::from_id(id).is_some()).count()
    }

    /// 這個指令還原成預設組的按鍵
    pub fn reset_command(&self, cmd: Command) -> Self {
        let mut keys = self.clone();
        keys.custom.remove(cmd.id());
        keys
    }

    /// 「還原成預設組…」：預設組不變，自己改過的按鍵全部拿掉，滑鼠也回到預設。
    /// 認不得的編號（新版的指令）留著：這版看不到、也沒有算在「你改過的」裡，回到新版時照樣有效
    pub fn reset_all(&self) -> Self {
        let mut keys = self.clone();
        keys.custom.retain(|id, _| Command::from_id(id).is_none());
        keys.mouse = MouseSettings::default();
        keys
    }
}

/// 錄到的按鍵要怎麼處理（[`Keymap::try_assign`]）
#[derive(Debug, Clone, PartialEq)]
pub enum Assign {
    /// 系統保留的按鍵，不能指定
    Reserved,
    /// 本來就是這樣，不用改
    Unchanged,
    /// 已經用在別的指令：問使用者要不要改到這裡（[`Keymap::assign_stealing`]）
    Conflict(Command),
    /// 可以直接改：改好的設定
    Changed(KeySettings),
}

/// 設定頁錄到的按鍵 → 要存的按鍵；修飾鍵本身（只按了 Shift…）是 None。
/// 要按 Shift 才打得出來的符號（美式鍵盤的 Shift+= 是 `+`、Shift+/ 是 `?`）不記 Shift：
/// egui 收到的是符號本身，比對時多按的 Shift 不影響，記了 Shift 反而在不用按 Shift 的鍵盤配置上按不到
pub fn recorded_chord(key: Key, modifiers: egui::Modifiers, platform: Platform) -> Option<Chord> {
    if is_modifier_key(key) {
        return None;
    }
    let mut mods = Mods::from_egui(modifiers, platform);
    if matches!(
        key,
        Key::Plus
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::Colon
            | Key::Pipe
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
    ) {
        mods.shift = false;
    }
    Some(Chord::new(mods, key))
}

/// 實際用的對照表：預設組 + 自己改過的
#[derive(Debug, Clone)]
pub struct Keymap {
    platform: Platform,
    /// 每個指令的按鍵（依 `Command::ALL` 的順序）；第一組是選單上顯示的
    chords: Vec<Vec<Chord>>,
    /// 被別的指令先拿走的按鍵：(指令, 按鍵, 拿走的指令)
    shadowed: Vec<(Command, Chord, Command)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::build(&KeySettings::default(), Platform::current())
    }
}

impl Keymap {
    /// 依設定建立：自己改過的指令取代預設組的按鍵；同一組按鍵只給一個指令
    /// （先給改過的指令，再給預設的，各自依 `Command::ALL` 的順序；拿不到的記在 `shadowed`）
    pub fn build(settings: &KeySettings, platform: Platform) -> Self {
        let mut preset: Vec<Vec<Chord>> = vec![Vec::new(); Command::ALL.len()];
        for (cmd, chord) in settings.preset.bindings(platform) {
            preset[cmd.index()].push(chord);
        }
        let mut custom: Vec<Option<Vec<Chord>>> = vec![None; Command::ALL.len()];
        for (id, list) in &settings.custom {
            let Some(cmd) = Command::from_id(id) else { continue };
            let mut chords = Vec::new();
            for chord in list.iter().filter_map(|s| Chord::parse(s, platform)) {
                if !reserved(chord, platform) && !chords.contains(&chord) && chords.len() < MAX_CHORDS {
                    chords.push(chord);
                }
            }
            custom[cmd.index()] = Some(chords);
        }
        let mut map = Self {
            platform,
            chords: vec![Vec::new(); Command::ALL.len()],
            shadowed: Vec::new(),
        };
        let mut owner: Vec<(Chord, Command)> = Vec::new();
        for overridden in [true, false] {
            for &cmd in Command::ALL {
                let list = match (&custom[cmd.index()], overridden) {
                    (Some(list), true) => list,
                    (None, false) => &preset[cmd.index()],
                    _ => continue,
                };
                for &chord in list {
                    match owner.iter().find(|(c, _)| *c == chord) {
                        Some(&(_, other)) => {
                            if other != cmd {
                                map.shadowed.push((cmd, chord, other));
                            }
                        }
                        None => {
                            owner.push((chord, cmd));
                            map.chords[cmd.index()].push(chord);
                        }
                    }
                }
            }
        }
        map
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// 這個指令的按鍵（第一組是選單上顯示的）
    pub fn chords(&self, cmd: Command) -> &[Chord] {
        &self.chords[cmd.index()]
    }

    /// 被別的指令先拿走、所以沒有作用的按鍵：(指令, 按鍵, 拿走的指令)
    pub fn shadowed(&self) -> &[(Command, Chord, Command)] {
        &self.shadowed
    }

    /// 按鍵事件 → 指令；Cmd+C 不會是按鍵事件（見 [`Keymap::on_copy`]）
    pub fn lookup(&self, key: Key, modifiers: egui::Modifiers) -> Option<Command> {
        self.lookup_chord(key, modifiers).map(|(cmd, _)| cmd)
    }

    /// 同 [`Keymap::lookup`]，也回傳對到的是哪一組按鍵
    pub fn lookup_chord(&self, key: Key, modifiers: egui::Modifiers) -> Option<(Command, Chord)> {
        let pressed = Mods::from_egui(modifiers, self.platform);
        let mut best: Option<(u32, Command, Chord)> = None;
        for &cmd in Command::ALL {
            for &chord in self.chords(cmd) {
                if chord.key != key || chord.arrives_as_copy() || !chord.mods.accepts(pressed) {
                    continue;
                }
                let n = chord.mods.count();
                if best.is_none_or(|(b, _, _)| n > b) {
                    best = Some((n, cmd, chord));
                }
            }
        }
        best.map(|(_, cmd, chord)| (cmd, chord))
    }

    /// egui 的「複製」事件（Cmd+C；Windows 的 Ctrl+Insert 也是）→ Cmd+C 的指令
    pub fn on_copy(&self) -> Option<Command> {
        let copy = Chord::new(Mods::CMD, Key::C);
        Command::ALL
            .iter()
            .copied()
            .find(|&cmd| self.chords(cmd).contains(&copy))
    }

    /// 選單上顯示的按鍵（第一組）；沒有指定時是空字串
    pub fn hint(&self, cmd: Command) -> String {
        self.chords(cmd)
            .first()
            .map(|c| c.display(self.platform))
            .unwrap_or_default()
    }

    /// 所有的按鍵，用 `sep` 隔開
    pub fn hints(&self, cmd: Command, sep: &str) -> String {
        self.chords(cmd)
            .iter()
            .map(|c| c.display(self.platform))
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// 這組按鍵現在是哪個指令的（被別的指令先拿走、沒有作用的不算）
    pub fn owner(&self, chord: Chord) -> Option<Command> {
        Command::ALL.iter().copied().find(|&c| self.chords(c).contains(&chord))
    }

    /// `cmd` 換掉第 `slot` 組按鍵（None = 新增一組），改成 `chord`：
    /// 系統保留的不行；已經是別的指令的要先問（[`Assign::Conflict`]）
    pub fn try_assign(&self, keys: &KeySettings, cmd: Command, slot: Option<usize>, chord: Chord) -> Assign {
        if reserved(chord, self.platform) {
            return Assign::Reserved;
        }
        match self.owner(chord) {
            Some(other) if other != cmd => return Assign::Conflict(other),
            _ => {}
        }
        let list = self.with_chord(cmd, slot, chord);
        if list == self.chords(cmd) {
            return Assign::Unchanged;
        }
        let mut keys = keys.clone();
        keys.custom.insert(cmd.id().to_owned(), config_list(&list));
        Assign::Changed(keys)
    }

    /// 衝突時選「改到這裡」：從原本的指令拿掉（那個指令也記成改過的，設定裡不會有重複的按鍵），再指定給 `cmd`
    pub fn assign_stealing(&self, keys: &KeySettings, cmd: Command, slot: Option<usize>, chord: Chord) -> KeySettings {
        let mut keys = keys.clone();
        if let Some(other) = self.owner(chord).filter(|&o| o != cmd) {
            let rest: Vec<Chord> = self.chords(other).iter().copied().filter(|&c| c != chord).collect();
            keys.custom.insert(other.id().to_owned(), config_list(&rest));
        }
        if !reserved(chord, self.platform) {
            let list = self.with_chord(cmd, slot, chord);
            keys.custom.insert(cmd.id().to_owned(), config_list(&list));
        }
        keys
    }

    /// 拿掉 `cmd` 的第 `index` 組按鍵（全部拿掉 = 不指定，存成空陣列）
    pub fn unassign(&self, keys: &KeySettings, cmd: Command, index: usize) -> KeySettings {
        let mut list = self.chords(cmd).to_vec();
        if index < list.len() {
            list.remove(index);
        }
        let mut keys = keys.clone();
        keys.custom.insert(cmd.id().to_owned(), config_list(&list));
        keys
    }

    /// `cmd` 現在的按鍵換掉第 `slot` 組（或加在最後），去掉重複的、最多 `MAX_CHORDS` 組
    fn with_chord(&self, cmd: Command, slot: Option<usize>, chord: Chord) -> Vec<Chord> {
        let mut list = self.chords(cmd).to_vec();
        match slot {
            Some(i) if i < list.len() => list[i] = chord,
            _ => list.push(chord),
        }
        let mut unique: Vec<Chord> = Vec::new();
        for c in list {
            if !unique.contains(&c) {
                unique.push(c);
            }
        }
        unique.truncate(MAX_CHORDS);
        unique
    }

    // ───────────── 選單、提示上的按鍵說明（沒有指定按鍵時，括號整個不寫） ─────────────

    /// 「標題（按鍵）」；中文用全形括號
    pub fn labeled(&self, label: &str, cmd: Command) -> String {
        paren(label, &self.hint(cmd))
    }

    /// 「標題（所有的按鍵）」：「全螢幕（F / Enter）」
    pub fn labeled_all(&self, label: &str, cmd: Command) -> String {
        paren(label, &self.hints(cmd, " / "))
    }

    /// 兩個相關的指令：「Ctrl+PgUp / PgDn」（修飾鍵一樣時後面那個省略修飾鍵）
    pub fn pair(&self, a: Command, b: Command) -> String {
        match (self.chords(a).first(), self.chords(b).first()) {
            (Some(x), Some(y)) if x.mods == y.mods => {
                format!("{} / {}", x.display(self.platform), key_display(y.key))
            }
            (Some(x), Some(y)) => format!("{} / {}", x.display(self.platform), y.display(self.platform)),
            (Some(x), None) | (None, Some(x)) => x.display(self.platform),
            (None, None) => String::new(),
        }
    }

    /// 依序切換的說明：「A 或 Ctrl+F6」
    pub fn any_of(&self, cmd: Command) -> String {
        self.hints(cmd, crate::tr!(" 或 ", " or "))
    }

    /// 播放速度的按鍵說明：「C 加快、X 減慢、Z 恢復正常」
    pub fn speed_keys(&self) -> String {
        let parts = [
            (Command::SpeedUp, crate::tr!("加快", "faster")),
            (Command::SpeedDown, crate::tr!("減慢", "slower")),
            (Command::SpeedReset, crate::tr!("恢復正常", "normal")),
        ];
        self.segments(&parts, crate::tr!("、", ", "))
    }

    /// 音量的按鍵說明：「↑ ↓」
    pub fn volume_keys(&self) -> String {
        [Command::VolumeUp, Command::VolumeDown]
            .into_iter()
            .map(|c| self.hint(c))
            .filter(|h| !h.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 影像調整的按鍵說明：「W/E 亮度・R/T 對比・Y/U 飽和度・I/O 色相」
    pub fn adjust_keys(&self) -> String {
        let rows = [
            (
                Command::BrightnessDown,
                Command::BrightnessUp,
                crate::tr!("亮度", "brightness"),
            ),
            (
                Command::ContrastDown,
                Command::ContrastUp,
                crate::tr!("對比", "contrast"),
            ),
            (
                Command::SaturationDown,
                Command::SaturationUp,
                crate::tr!("飽和度", "saturation"),
            ),
            (Command::HueDown, Command::HueUp, crate::tr!("色相", "hue")),
        ];
        rows.iter()
            .filter_map(|&(minus, plus, name)| {
                let keys = [self.hint(minus), self.hint(plus)]
                    .into_iter()
                    .filter(|h| !h.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                (!keys.is_empty()).then(|| format!("{keys} {name}"))
            })
            .collect::<Vec<_>>()
            .join(crate::tr!("・", " · "))
    }

    /// 開檔後跳到上次的位置：「從 1:23 繼續播放（Home 從頭播放）」
    pub fn resume_osd(&self, time: &str) -> String {
        let key = self.hint(Command::Restart);
        if key.is_empty() {
            crate::tf!("從 {time} 繼續播放", "Resuming from {time}")
        } else {
            crate::tf!(
                "從 {time} 繼續播放（{key} 從頭播放）",
                "Resuming from {time} ({key} plays from the start)"
            )
        }
    }

    /// 開檔時有影像調整：「影像調整中：亮度 +3（Q 還原）」
    pub fn adjust_osd(&self, summary: &str) -> String {
        let key = self.hint(Command::AdjustReset);
        if key.is_empty() {
            crate::tf!("影像調整中：{summary}", "Image adjusted: {summary}")
        } else {
            crate::tf!(
                "影像調整中：{summary}（{key} 還原）",
                "Image adjusted: {summary} ({key} to reset)"
            )
        }
    }

    /// 字幕、音訊延遲子選單的說明：「快捷鍵 [ / ]。正數 = 字幕晚一點出現」；
    /// 歸零有按鍵時也寫出來（PotPlayer 風格：「快捷鍵 Shift+, / Shift+.，歸零 /。…」）
    pub fn delay_note(&self, subtitle: bool) -> String {
        // 兩個都寫完整（PotPlayer 風格的「Shift+, / Shift+.」省略成「Shift+, / .」看不懂）
        let both = |a: Command, b: Command| {
            [self.hint(a), self.hint(b)]
                .into_iter()
                .filter(|h| !h.is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        };
        let (keys, reset, what) = if subtitle {
            (
                both(Command::SubDelayDown, Command::SubDelayUp),
                self.hint(Command::SubDelayReset),
                crate::tr!("正數 = 字幕晚一點出現", "Positive = subtitles appear later"),
            )
        } else {
            (
                both(Command::AudioDelayDown, Command::AudioDelayUp),
                self.hint(Command::AudioDelayReset),
                crate::tr!("正數 = 聲音晚一點", "Positive = sound plays later"),
            )
        };
        match (keys.is_empty(), reset.is_empty()) {
            (true, true) => what.to_owned(),
            (false, true) => crate::tf!("快捷鍵 {keys}。{what}", "Keys {keys}. {what}"),
            (true, false) => crate::tf!("歸零 {reset}。{what}", "Reset {reset}. {what}"),
            (false, false) => crate::tf!(
                "快捷鍵 {keys}，歸零 {reset}。{what}",
                "Keys {keys}, reset {reset}. {what}"
            ),
        }
    }

    /// 「設定 → 播放」跳轉秒數的標題：「← / → 跳轉」「Ctrl+← / → 跳轉」；沒有按鍵時只寫「跳轉」「大幅跳轉」
    pub fn seek_label(&self, long: bool) -> String {
        let keys = if long {
            self.pair(Command::SeekBackLong, Command::SeekForwardLong)
        } else {
            self.pair(Command::SeekBack, Command::SeekForward)
        };
        match (keys.is_empty(), long) {
            (true, false) => crate::tr!("跳轉", "Seek").to_owned(),
            (true, true) => crate::tr!("大幅跳轉", "Seek further").to_owned(),
            (false, _) => crate::tf!("{keys} 跳轉", "{keys} seek"),
        }
    }

    /// 這個檔案沒有書籤時按了上一個 / 下一個：「這個檔案還沒有書籤（按 P 新增）」
    pub fn no_bookmarks_osd(&self) -> String {
        let key = self.hint(Command::BookmarkAdd);
        if key.is_empty() {
            crate::tr!("這個檔案還沒有書籤", "No bookmarks in this file yet").to_owned()
        } else {
            crate::tf!(
                "這個檔案還沒有書籤（按 {key} 新增）",
                "No bookmarks in this file yet (press {key} to add one)"
            )
        }
    }

    /// 書籤分頁沒有書籤時：「按 P 在目前的位置新增書籤」
    pub fn bookmarks_empty_hint(&self) -> String {
        let key = self.hint(Command::BookmarkAdd);
        if key.is_empty() {
            crate::tr!("這個檔案還沒有書籤", "No bookmarks in this file yet").to_owned()
        } else {
            crate::tf!(
                "按 {key} 在目前的位置新增書籤",
                "Press {key} to bookmark the current position"
            )
        }
    }

    /// 起始畫面：「把影片拖放到這裡，或按 Ctrl+O 開啟檔案」
    pub fn drop_hint(&self) -> String {
        let key = self.hint(Command::OpenFile);
        if key.is_empty() {
            crate::tr!("把影片拖放到這裡", "Drop a video here").to_owned()
        } else {
            crate::tf!(
                "把影片拖放到這裡，或按 {key} 開啟檔案",
                "Drop a video here, or press {key} to open a file"
            )
        }
    }

    /// 起始畫面的第二行：「按 Ctrl+U 開啟網址，或按 Ctrl+V 貼上網址」。貼上是固定的按鍵（不在對照表裡）；
    /// 開啟網址沒有按鍵時只寫貼上
    pub fn url_hint(&self) -> String {
        let paste = Chord::new(Mods::CMD, Key::V).display(self.platform);
        let key = self.hint(Command::OpenUrl);
        if key.is_empty() {
            crate::tf!("按 {paste} 貼上網址", "Press {paste} to paste a URL")
        } else {
            crate::tf!(
                "按 {key} 開啟網址，或按 {paste} 貼上網址",
                "Press {key} to open a URL, or {paste} to paste one"
            )
        }
    }

    /// 「按鍵 動作」的片段，沒有按鍵的片段不寫
    fn segments(&self, parts: &[(Command, &str)], sep: &str) -> String {
        parts
            .iter()
            .filter_map(|&(cmd, what)| {
                let key = self.hint(cmd);
                (!key.is_empty()).then(|| format!("{key} {what}"))
            })
            .collect::<Vec<_>>()
            .join(sep)
    }
}

/// 存進設定檔的按鍵清單
fn config_list(chords: &[Chord]) -> Vec<String> {
    chords.iter().map(Chord::to_config).collect()
}

/// 「標題（說明）」；說明是空的就只有標題
pub fn paren(label: &str, inner: &str) -> String {
    if inner.is_empty() {
        label.to_owned()
    } else if crate::i18n::is_en() {
        format!("{label} ({inner})")
    } else {
        format!("{label}（{inner}）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_lang};
    use egui::Modifiers;

    fn keymap(p: Platform) -> Keymap {
        Keymap::build(&KeySettings::default(), p)
    }

    fn custom(entries: &[(&str, &[&str])]) -> KeySettings {
        KeySettings {
            custom: entries
                .iter()
                .map(|(id, keys)| (id.to_string(), keys.iter().map(|k| k.to_string()).collect()))
                .collect(),
            ..Default::default()
        }
    }

    /// 發佈過的編號（存在使用者的設定檔裡）：不能改名、不能刪；新指令也加進來（放哪裡都可以，順序看 `Command::ALL`）
    const GOLDEN_IDS: &[&str] = &[
        "toggle-pause",
        "stop",
        "restart",
        "seek-back",
        "seek-forward",
        "seek-back-long",
        "seek-forward-long",
        "frame-next",
        "frame-prev",
        "speed-up",
        "speed-down",
        "speed-reset",
        "ab-loop",
        "prev-file",
        "next-file",
        "prev-chapter",
        "next-chapter",
        "volume-up",
        "volume-down",
        "toggle-mute",
        "audio-delay-down",
        "audio-delay-up",
        "sub-delay-down",
        "sub-delay-up",
        "aspect-cycle",
        "crop-cycle",
        "zoom-in",
        "zoom-out",
        "zoom-reset",
        "pan-left",
        "pan-right",
        "pan-up",
        "pan-down",
        "pan-center",
        "rotate",
        "flip-h",
        "flip-v",
        "reset-view",
        "brightness-down",
        "brightness-up",
        "contrast-down",
        "contrast-up",
        "saturation-down",
        "saturation-up",
        "hue-down",
        "hue-up",
        "adjust-reset",
        "fullscreen",
        "on-top",
        "control-panel",
        "playlist",
        "media-info",
        "settings",
        "about",
        "cycle-theme",
        "open-file",
        "screenshot",
        "copy-frame",
        // A3
        "ab-set-start",
        "ab-set-end",
        "ab-clear",
        "audio-delay-reset",
        "next-audio-track",
        "toggle-eq",
        "sub-delay-reset",
        "next-subtitle",
        "fill-window",
        "gamma-down",
        "gamma-up",
        "toggle-smooth",
        "screenshot-as",
        "load-subtitle",
        // B1
        "bookmark-add",
        "bookmark-prev",
        "bookmark-next",
        // B2
        "bookmark-list",
        // C3
        "open-url",
        // D3
        "export-clip",
    ];

    #[test]
    fn ids_are_stable_unique_and_kebab_case() {
        let ids: Vec<&str> = Command::ALL.iter().map(|c| c.id()).collect();
        for id in GOLDEN_IDS {
            assert!(ids.contains(id), "編號 {id} 不見了（發佈過的編號不能改名或刪掉）");
        }
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "編號重複：{id}");
            assert!(
                !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && !id.starts_with('-')
                    && !id.ends_with('-')
                    && !id.contains("--"),
                "不是 kebab-case：{id}"
            );
            assert_eq!(Command::from_id(id), Some(Command::ALL[i]));
        }
        assert_eq!(Command::from_id("toggle-pause "), None);
        assert_eq!(Command::from_id("future-command"), None);
        // 新指令也要加進上面的清單（清單就是「發佈過的編號」）；順序不比，新指令是加在各組最後
        for id in &ids {
            assert!(GOLDEN_IDS.contains(id), "新指令 {id} 也加到 GOLDEN_IDS");
        }
        assert_eq!(ids.len(), GOLDEN_IDS.len(), "GOLDEN_IDS 有重複的編號");
    }

    #[test]
    fn labels_are_unique_per_language() {
        for lang in Lang::ALL {
            set_lang(lang);
            let labels: Vec<&str> = Command::ALL.iter().map(|c| c.label()).collect();
            for (i, l) in labels.iter().enumerate() {
                assert!(!l.trim().is_empty());
                assert!(!labels[..i].contains(l), "{lang:?} 名稱重複：{l}");
            }
            for g in Group::ALL {
                assert!(!g.label().is_empty());
            }
            for p in KeyPreset::ALL {
                assert!(!p.label().is_empty());
            }
            assert_ne!(KeyPreset::Vitascope.label(), KeyPreset::Potplayer.label());
        }
        set_lang(Lang::ZhTw);
    }

    #[test]
    fn repeatable_flags() {
        use Command as C;
        // 每個指令都要寫（沒有 `_`）：新指令一定要決定按住時要不要重複
        for &c in Command::ALL {
            let expected = match c {
                C::SeekBack
                | C::SeekForward
                | C::SeekBackLong
                | C::SeekForwardLong
                | C::FrameNext
                | C::FramePrev
                | C::SpeedUp
                | C::SpeedDown
                | C::VolumeUp
                | C::VolumeDown
                | C::AudioDelayDown
                | C::AudioDelayUp
                | C::SubDelayDown
                | C::SubDelayUp
                | C::ZoomIn
                | C::ZoomOut
                | C::PanLeft
                | C::PanRight
                | C::PanUp
                | C::PanDown
                | C::BrightnessDown
                | C::BrightnessUp
                | C::ContrastDown
                | C::ContrastUp
                | C::SaturationDown
                | C::SaturationUp
                | C::HueDown
                | C::HueUp
                | C::GammaDown
                | C::GammaUp => true,
                C::TogglePause
                | C::Stop
                | C::Restart
                | C::SpeedReset
                | C::AbLoop
                | C::PrevFile
                | C::NextFile
                | C::PrevChapter
                | C::NextChapter
                | C::ToggleMute
                | C::AspectCycle
                | C::CropCycle
                | C::ZoomReset
                | C::PanCenter
                | C::Rotate
                | C::FlipH
                | C::FlipV
                | C::ResetView
                | C::AdjustReset
                | C::Fullscreen
                | C::OnTop
                | C::ControlPanel
                | C::Playlist
                | C::MediaInfo
                | C::Settings
                | C::About
                | C::CycleTheme
                | C::OpenFile
                | C::Screenshot
                | C::CopyFrame
                | C::AbSetStart
                | C::AbSetEnd
                | C::AbClear
                | C::AudioDelayReset
                | C::NextAudioTrack
                | C::ToggleEq
                | C::SubDelayReset
                | C::NextSubtitle
                | C::FillWindow
                | C::ToggleSmooth
                | C::ScreenshotAs
                | C::LoadSubtitle
                | C::OpenUrl
                | C::ExportClip
                | C::BookmarkAdd
                | C::BookmarkPrev
                | C::BookmarkNext
                | C::BookmarkList => false,
            };
            assert_eq!(c.repeatable(), expected, "{c:?}");
        }
    }

    #[test]
    fn chord_config_round_trips_for_every_key() {
        for p in Platform::ALL {
            for &key in Key::ALL {
                if is_modifier_key(key) {
                    assert_eq!(Chord::parse(key.name(), p), None, "{key:?} 只能當修飾鍵");
                    continue;
                }
                for mods in [
                    Mods::NONE,
                    Mods::CMD,
                    Mods::ALT,
                    Mods::SHIFT,
                    Mods {
                        cmd: true,
                        alt: true,
                        shift: true,
                        ctrl: false,
                    },
                    Mods {
                        ctrl: true,
                        ..Mods::SHIFT
                    },
                ] {
                    let chord = Chord::new(mods.normalized(p), key);
                    let text = chord.to_config();
                    assert_eq!(Chord::parse(&text, p), Some(chord), "{p:?} {text}");
                    // 選單上的寫法也讀得回來
                    for lang in Lang::ALL {
                        set_lang(lang);
                        let shown = chord.display(p);
                        assert_eq!(Chord::parse(&shown, p), Some(chord), "{p:?} {shown}");
                    }
                    set_lang(Lang::ZhTw);
                }
            }
        }
    }

    #[test]
    fn parse_spellings() {
        let w = Platform::Windows;
        let m = Platform::Mac;
        let l = Platform::Linux;
        // Windows、Linux：Ctrl 就是 Cmd；macOS 不一樣
        for p in [w, l] {
            assert_eq!(Chord::parse("Ctrl+X", p), Chord::parse("Cmd+X", p));
            assert_eq!(Chord::parse("ctrl+q", p), Some(Chord::new(Mods::CMD, Key::Q)));
        }
        assert_ne!(Chord::parse("Ctrl+X", m), Chord::parse("Cmd+X", m));
        assert_eq!(Chord::parse("Control+Q", m), Some(Chord::new(Mods::CTRL, Key::Q)));
        assert_eq!(Chord::parse("Option+G", m), Some(Chord::new(Mods::ALT, Key::G)));
        assert_eq!(Chord::parse("Shift+K", w), Some(Chord::new(Mods::SHIFT, Key::K)));
        assert_eq!(Chord::parse(" Space ", w), Some(Chord::new(Mods::NONE, Key::Space)));
        assert_eq!(Chord::parse("space", w), Some(Chord::new(Mods::NONE, Key::Space)));
        assert_eq!(Chord::parse("pageup", w), Some(Chord::new(Mods::NONE, Key::PageUp)));
        assert_eq!(Chord::parse("+", w), Some(Chord::new(Mods::NONE, Key::Plus)));
        assert_eq!(Chord::parse("Cmd++", w), Some(Chord::new(Mods::CMD, Key::Plus)));
        assert_eq!(Chord::parse("Cmd+Plus", w), Some(Chord::new(Mods::CMD, Key::Plus)));
        assert_eq!(Chord::parse("-", w), Some(Chord::new(Mods::NONE, Key::Minus)));
        assert_eq!(Chord::parse("Alt+←", w), Some(Chord::new(Mods::ALT, Key::ArrowLeft)));
        // Super／Meta 只在 macOS 是 ⌘；其他平台讀不懂就不要（不能變成 Ctrl）
        assert_eq!(Chord::parse("Super+K", m), Some(Chord::new(Mods::CMD, Key::K)));
        assert_eq!(Chord::parse("Meta+K", m), Some(Chord::new(Mods::CMD, Key::K)));
        for p in [w, l] {
            assert_eq!(Chord::parse("Super+K", p), None);
            assert_eq!(Chord::parse("Meta+K", p), None);
        }
        for bad in [
            "",
            "Cmd+",
            "Hyper+K",
            "K+Cmd",
            "NoSuchKey",
            "Shift",
            "ShiftLeft",
            "Cmd+ControlLeft",
        ] {
            assert_eq!(Chord::parse(bad, w), None, "{bad:?}");
        }
    }

    #[test]
    fn display_names() {
        set_lang(Lang::ZhTw);
        let w = Platform::Windows;
        let m = Platform::Mac;
        assert_eq!(Chord::new(Mods::CMD, Key::O).display(w), "Ctrl+O");
        assert_eq!(Chord::new(Mods::CMD, Key::O).display(m), "Cmd+O");
        assert_eq!(Chord::new(Mods::CTRL, Key::Q).display(m), "Control+Q");
        assert_eq!(Chord::new(Mods::ALT, Key::G).display(m), "Option+G");
        assert_eq!(Chord::new(Mods::ALT, Key::G).display(Platform::Linux), "Alt+G");
        assert_eq!(Chord::new(Mods::NONE, Key::Space).display(w), "空白鍵");
        assert_eq!(Chord::new(Mods::NONE, Key::PageUp).display(w), "PgUp");
        assert_eq!(Chord::new(Mods::NONE, Key::ArrowLeft).display(w), "←");
        assert_eq!(Chord::new(Mods::NONE, Key::Num5).display(w), "5");
        assert_eq!(Chord::new(Mods::NONE, Key::Minus).display(w), "-");
        assert_eq!(
            Chord::new(
                Mods {
                    cmd: true,
                    shift: true,
                    ..Mods::NONE
                },
                Key::M
            )
            .display(w),
            "Ctrl+Shift+M"
        );
        set_lang(Lang::En);
        assert_eq!(Chord::new(Mods::NONE, Key::Space).display(w), "Space");
        set_lang(Lang::ZhTw);
    }

    #[test]
    fn reserved_chords_per_platform() {
        let parse = |s: &str, p| Chord::parse(s, p).unwrap();
        for p in Platform::ALL {
            // Cmd+C 加別的修飾鍵也會變成「複製」（egui-winit 只看 command），只有 Cmd+C 本身能指定
            for s in [
                "Cmd+X",
                "Cmd+V",
                "Cmd+Shift+V",
                "Esc",
                "Shift+Esc",
                "Cmd+Shift+C",
                "Cmd+Alt+C",
            ] {
                assert!(reserved(parse(s, p), p), "{p:?} {s}");
            }
            for s in ["Cmd+C", "Space", "Ctrl+Q", "Alt+G", "Delete", "F12"] {
                assert!(!reserved(parse(s, p), p), "{p:?} {s}");
            }
        }
        let w = Platform::Windows;
        for s in ["Shift+Insert", "Shift+Delete", "Ctrl+Insert", "Alt+F4"] {
            assert!(reserved(parse(s, w), w), "{s}");
            assert!(!reserved(parse(s, Platform::Linux), Platform::Linux), "{s}");
        }
        let m = Platform::Mac;
        for s in ["Cmd+Q", "Cmd+H", "Cmd+Option+H"] {
            assert!(reserved(parse(s, m), m), "{s}");
            assert!(!reserved(parse(s, w), w), "{s}");
        }
        assert!(!reserved(parse("Control+Q", m), m), "裁切");
        assert!(reserved(parse("Cmd+Control+C", m), m));
        assert!(!reserved(parse("Control+C", m), m), "macOS 的 Control+C 是一般按鍵");
    }

    #[test]
    fn preset_has_no_duplicates_and_keeps_owner_keys() {
        for preset in KeyPreset::ALL {
            for p in Platform::ALL {
                let list = preset.bindings(p);
                for (i, (cmd, chord)) in list.iter().enumerate() {
                    assert!(
                        !list[..i].iter().any(|(_, c)| c == chord),
                        "{preset:?} {p:?} 重複的按鍵：{chord:?}（{cmd:?}）"
                    );
                    assert!(!reserved(*chord, p), "{preset:?} {p:?} {chord:?}");
                    assert_eq!(Chord::parse(&chord.to_config(), p), Some(*chord));
                }
                let map = Keymap::build(
                    &KeySettings {
                        preset,
                        ..Default::default()
                    },
                    p,
                );
                assert!(map.shadowed().is_empty());
                // 主人定的按鍵：Q、W…O、Alt+G（兩個預設組都一樣）
                set_lang(Lang::ZhTw);
                assert_eq!(map.hint(Command::AdjustReset), "Q");
                assert_eq!(map.adjust_keys(), "W/E 亮度・R/T 對比・Y/U 飽和度・I/O 色相");
                assert_eq!(
                    map.hint(Command::ControlPanel),
                    if p == Platform::Mac { "Option+G" } else { "Alt+G" }
                );
            }
        }
    }

    /// PotPlayer 風格跟影戲只差這些指令（§1.3 的清單；主人對照 PotPlayer 後只改這裡和 `potplayer_preset`）
    #[test]
    fn potplayer_preset_differs_only_in_the_listed_keys() {
        use Command as C;
        let changed = [
            C::Restart,
            C::AbLoop,
            C::FrameNext,
            C::FramePrev,
            C::Fullscreen,
            C::OpenFile,
            C::AbSetStart,
            C::AbSetEnd,
            C::AbClear,
            C::SubDelayDown,
            C::SubDelayUp,
            C::SubDelayReset,
        ];
        for p in Platform::ALL {
            let vita = keymap(p);
            let pot = Keymap::build(
                &KeySettings {
                    preset: KeyPreset::Potplayer,
                    ..Default::default()
                },
                p,
            );
            for &cmd in Command::ALL {
                if !changed.contains(&cmd) {
                    assert_eq!(pot.chords(cmd), vita.chords(cmd), "{p:?} {cmd:?}");
                }
            }
            let n = Modifiers::NONE;
            let shift = Modifiers::SHIFT;
            let alt = Modifiers::ALT;
            for (key, mods, want) in [
                (Key::Backspace, n, Some(C::Restart)),
                (Key::F, n, Some(C::FrameNext)),
                (Key::D, n, Some(C::FramePrev)),
                (Key::Enter, n, Some(C::Fullscreen)),
                (Key::Enter, alt, Some(C::Fullscreen)),
                (Key::F3, n, Some(C::OpenFile)),
                (Key::OpenBracket, n, Some(C::AbSetStart)),
                (Key::CloseBracket, n, Some(C::AbSetEnd)),
                (Key::Backslash, n, Some(C::AbClear)),
                (Key::Comma, shift, Some(C::SubDelayDown)),
                (Key::Period, shift, Some(C::SubDelayUp)),
                (Key::Slash, n, Some(C::SubDelayReset)),
                // 空出來的鍵：逐格的 . ,、從頭播放的 Home、A-B 依序切換的 L（P§2.1 的表）
                (Key::Period, n, None),
                (Key::Comma, n, None),
                (Key::Home, n, None),
                (Key::L, n, None),
                // 沒換的照舊：Alt+Backspace 重設畫面、Ctrl+T 切換視窗置頂模式
                (Key::Backspace, alt, Some(C::ResetView)),
            ] {
                assert_eq!(pot.lookup(key, mods), want, "{p:?} {mods:?} {key:?}");
            }
            let cmd_t = if p == Platform::Mac {
                Modifiers::MAC_CMD | Modifiers::COMMAND
            } else {
                Modifiers::COMMAND
            };
            assert_eq!(pot.lookup(Key::T, cmd_t), Some(C::OnTop));
            assert_eq!(pot.lookup(Key::O, cmd_t), Some(C::OpenFile));
            // 選單、提示上顯示的是 PotPlayer 的鍵
            set_lang(Lang::ZhTw);
            assert_eq!(pot.hint(C::Restart), "Backspace");
            assert_eq!(pot.resume_osd("1:23"), "從 1:23 繼續播放（Backspace 從頭播放）");
            let alt_name = if p == Platform::Mac { "Option" } else { "Alt" };
            assert_eq!(
                pot.labeled_all("全螢幕", C::Fullscreen),
                format!("全螢幕（Enter / {alt_name}+Enter）")
            );
            assert_eq!(
                pot.delay_note(true),
                "快捷鍵 Shift+, / Shift+.，歸零 /。正數 = 字幕晚一點出現"
            );
        }
    }

    /// v0.3.0 的 `handle_keys`：依登記的順序用 egui 的 `consume_key`（`matches_logically`）比對，先符合的先拿走。
    /// （Delete、macOS 的 Backspace、Esc 是固定的按鍵，不在對照表裡）
    fn old_lookup(mods: Modifiers, key: Key) -> Option<Command> {
        use Command as C;
        use Modifiers as M;
        let table: &[(Modifiers, Key, Command)] = &[
            (M::ALT, Key::ArrowLeft, C::PanLeft),
            (M::ALT, Key::ArrowRight, C::PanRight),
            (M::ALT, Key::ArrowUp, C::PanUp),
            (M::ALT, Key::ArrowDown, C::PanDown),
            (M::ALT, Key::K, C::Rotate),
            (M::ALT, Key::Backspace, C::ResetView),
            (M::ALT, Key::G, C::ControlPanel),
            (M::CTRL, Key::Q, C::CropCycle),
            (M::COMMAND, Key::Z, C::FlipH),
            (M::COMMAND, Key::P, C::FlipV),
            (M::COMMAND, Key::T, C::OnTop),
            (M::COMMAND, Key::Num5, C::PanCenter),
            (M::COMMAND, Key::F6, C::AspectCycle),
            (M::NONE, Key::A, C::AspectCycle),
            (M::NONE, Key::Num9, C::ZoomIn),
            (M::NONE, Key::Num1, C::ZoomOut),
            (M::NONE, Key::Num5, C::ZoomReset),
            (M::COMMAND, Key::O, C::OpenFile),
            (M::COMMAND, Key::ArrowLeft, C::SeekBackLong),
            (M::COMMAND, Key::ArrowRight, C::SeekForwardLong),
            (M::COMMAND, Key::PageUp, C::PrevChapter),
            (M::COMMAND, Key::PageDown, C::NextChapter),
            (M::NONE, Key::PageUp, C::PrevFile),
            (M::NONE, Key::PageDown, C::NextFile),
            (M::NONE, Key::C, C::SpeedUp),
            (M::NONE, Key::X, C::SpeedDown),
            (M::NONE, Key::Z, C::SpeedReset),
            (M::NONE, Key::Period, C::FrameNext),
            (M::NONE, Key::Comma, C::FramePrev),
            (M::NONE, Key::L, C::AbLoop),
            (M::NONE, Key::Home, C::Restart),
            (M::NONE, Key::OpenBracket, C::SubDelayDown),
            (M::NONE, Key::CloseBracket, C::SubDelayUp),
            (M::NONE, Key::Minus, C::AudioDelayDown),
            (M::NONE, Key::Equals, C::AudioDelayUp),
            (M::NONE, Key::Plus, C::AudioDelayUp),
            (M::NONE, Key::ArrowLeft, C::SeekBack),
            (M::NONE, Key::ArrowRight, C::SeekForward),
            (M::NONE, Key::ArrowUp, C::VolumeUp),
            (M::NONE, Key::ArrowDown, C::VolumeDown),
            (M::NONE, Key::Space, C::TogglePause),
            (M::NONE, Key::M, C::ToggleMute),
            (M::NONE, Key::F, C::Fullscreen),
            (M::NONE, Key::Enter, C::Fullscreen),
            (M::COMMAND, Key::E, C::Screenshot),
            (M::COMMAND, Key::F1, C::MediaInfo),
            (M::COMMAND, Key::I, C::MediaInfo),
            (M::NONE, Key::F1, C::About),
            (M::NONE, Key::F6, C::Playlist),
            (M::NONE, Key::F5, C::Settings),
            (M::NONE, Key::Q, C::AdjustReset),
            (M::NONE, Key::W, C::BrightnessDown),
            (M::NONE, Key::E, C::BrightnessUp),
            (M::NONE, Key::R, C::ContrastDown),
            (M::NONE, Key::T, C::ContrastUp),
            (M::NONE, Key::Y, C::SaturationDown),
            (M::NONE, Key::U, C::SaturationUp),
            (M::NONE, Key::I, C::HueDown),
            (M::NONE, Key::O, C::HueUp),
        ];
        table
            .iter()
            .find(|(m, k, _)| *k == key && mods.matches_logically(*m))
            .map(|(_, _, c)| *c)
    }

    /// 這個平台上 egui 收到的修飾鍵：{Shift, Alt, Ctrl, Cmd} 的 16 種組合。
    /// macOS 的 ⌘ 同時是 `mac_cmd` 和 `command`（egui-winit 這樣送）
    fn combos(p: Platform) -> Vec<Modifiers> {
        (0..16u8)
            .map(|bits| Modifiers {
                shift: bits & 1 != 0,
                alt: bits & 2 != 0,
                ctrl: bits & 4 != 0,
                command: bits & 8 != 0,
                mac_cmd: p == Platform::Mac && bits & 8 != 0,
            })
            .collect()
    }

    /// v0.3.0 之後刻意改的按鍵（批次二 B1、B2）：P 新增書籤、H 書籤清單（以前都沒有作用）；Shift+PgUp / PgDn 上一個 / 下一個書籤
    /// （以前多按的 Shift 不影響，是換檔）。Ctrl+P、Ctrl+Shift+PgUp 之類有 Ctrl 的照舊。
    /// C3：Ctrl（⌘）+ U 開啟網址（以前沒有作用；只按 U 照舊是飽和度 +）
    fn changed_after_v030(mods: Modifiers, key: Key) -> Option<Option<Command>> {
        let no_ctrl = !mods.ctrl && !mods.command && !mods.mac_cmd;
        match key {
            Key::U if mods.command => Some(Some(Command::OpenUrl)),
            Key::P if no_ctrl => Some(Some(Command::BookmarkAdd)),
            Key::H if no_ctrl => Some(Some(Command::BookmarkList)),
            Key::PageUp if no_ctrl && mods.shift => Some(Some(Command::BookmarkPrev)),
            Key::PageDown if no_ctrl && mods.shift => Some(Some(Command::BookmarkNext)),
            _ => None,
        }
    }

    #[test]
    fn preset_matches_the_old_hard_coded_keys_exactly() {
        for p in Platform::ALL {
            let map = keymap(p);
            for &key in Key::ALL {
                for mods in combos(p) {
                    let old = changed_after_v030(mods, key).unwrap_or_else(|| old_lookup(mods, key));
                    let new = map.lookup(key, mods);
                    // Windows、Linux 的 egui-winit 一律送 command = ctrl。只按其中一個（介面測試的
                    // `Modifiers::COMMAND`、`Modifiers::CTRL`）時舊版分得出兩者，新版都當成 Ctrl：
                    // 舊版有作用的按法新版一樣，舊版沒作用的（例如只有 ctrl 的 Ctrl+Z）新版也會翻轉
                    if p != Platform::Mac && mods.ctrl != mods.command {
                        if old.is_some() {
                            assert_eq!(new, old, "{p:?} {mods:?} {key:?}");
                        }
                        continue;
                    }
                    assert_eq!(new, old, "{p:?} {mods:?} {key:?}");
                }
            }
        }
    }

    #[test]
    fn lookup_specificity_and_tolerance() {
        let w = keymap(Platform::Windows);
        // 多按的 Shift 不影響（Shift+E 照樣調亮度）
        assert_eq!(w.lookup(Key::E, Modifiers::SHIFT), Some(Command::BrightnessUp));
        // Alt+← 是移動畫面，不是跳轉
        assert_eq!(w.lookup(Key::ArrowLeft, Modifiers::ALT), Some(Command::PanLeft));
        assert_eq!(w.lookup(Key::ArrowLeft, Modifiers::NONE), Some(Command::SeekBack));
        // Ctrl+F1 是媒體資訊，F1 是關於
        assert_eq!(w.lookup(Key::F1, Modifiers::COMMAND), Some(Command::MediaInfo));
        assert_eq!(w.lookup(Key::F1, Modifiers::NONE), Some(Command::About));
        // 介面測試在 Windows 上只送 ctrl 或只送 command：兩種都是 Ctrl
        assert_eq!(w.lookup(Key::Q, Modifiers::CTRL), Some(Command::CropCycle));
        assert_eq!(w.lookup(Key::Q, Modifiers::COMMAND), Some(Command::CropCycle));
        assert_eq!(w.lookup(Key::Z, Modifiers::CTRL), Some(Command::FlipH));
        // macOS：Control+Q 是裁切，⌘Q 不是
        let m = keymap(Platform::Mac);
        assert_eq!(m.lookup(Key::Q, Modifiers::CTRL), Some(Command::CropCycle));
        assert_eq!(m.lookup(Key::Q, Modifiers::MAC_CMD | Modifiers::COMMAND), None);
        // Cmd+C 只從「複製」事件來
        assert_eq!(w.lookup(Key::C, Modifiers::COMMAND), None);
        assert_eq!(w.on_copy(), Some(Command::CopyFrame));
        // 修飾鍵最多的優先：Shift+PgUp 指定給別的指令時，贏過 PgUp
        let s = custom(&[("cycle-theme", &["Shift+PageUp"])]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.lookup(Key::PageUp, Modifiers::SHIFT), Some(Command::CycleTheme));
        assert_eq!(map.lookup(Key::PageUp, Modifiers::NONE), Some(Command::PrevFile));
        // 一樣多時 Command::ALL 前面的優先（Alt+E、Shift+E 都有指定，按 Alt+Shift+E）
        let s = custom(&[("cycle-theme", &["Alt+E"]), ("stop", &["Shift+E"])]);
        let map = Keymap::build(&s, Platform::Windows);
        let both = Modifiers::ALT | Modifiers::SHIFT;
        assert_eq!(
            map.lookup(Key::E, both),
            Some(Command::Stop),
            "stop 排在 cycle-theme 前面"
        );
        assert_eq!(map.lookup(Key::E, Modifiers::ALT), Some(Command::CycleTheme));
        assert_eq!(map.lookup(Key::E, Modifiers::SHIFT), Some(Command::Stop));
    }

    #[test]
    fn overrides_replace_steal_and_ignore_unknown() {
        let p = Platform::Windows;
        // 改過的取代預設組；M 從靜音那裡拿走（靜音就沒有按鍵了）
        let s = custom(&[
            ("toggle-pause", &["Space", "Shift+K"]),
            ("next-file", &["M"]),
            ("future-command", &["F9"]),
        ]);
        let map = Keymap::build(&s, p);
        let chords = |c| map.chords(c).to_vec();
        assert_eq!(
            chords(Command::TogglePause),
            vec![Chord::new(Mods::NONE, Key::Space), Chord::new(Mods::SHIFT, Key::K)]
        );
        assert_eq!(chords(Command::NextFile), vec![Chord::new(Mods::NONE, Key::M)]);
        assert!(chords(Command::ToggleMute).is_empty());
        assert_eq!(
            map.shadowed(),
            &[(Command::ToggleMute, Chord::new(Mods::NONE, Key::M), Command::NextFile)]
        );
        assert_eq!(map.lookup(Key::M, Modifiers::NONE), Some(Command::NextFile));
        assert_eq!(map.lookup(Key::PageDown, Modifiers::NONE), None);
        assert_eq!(map.lookup(Key::F9, Modifiers::NONE), None, "認不得的指令不用");
        // 空陣列 = 不指定；讀不懂、保留的、重複的拿掉；最多 4 組
        let s = custom(&[
            ("restart", &[]),
            (
                "stop",
                &["Nope", "Cmd+V", "S", "S", "Shift+S", "Alt+S", "Cmd+S", "Alt+Shift+S"],
            ),
        ]);
        let map = Keymap::build(&s, p);
        assert!(map.chords(Command::Restart).is_empty());
        assert_eq!(map.hint(Command::Restart), "");
        assert_eq!(map.chords(Command::Stop).len(), MAX_CHORDS);
        assert_eq!(map.hints(Command::Stop, " / "), "S / Shift+S / Alt+S / Ctrl+S");
        // 兩個改過的指令要同一組按鍵：Command::ALL 前面的拿到
        let s = custom(&[("cycle-theme", &["F9"]), ("stop", &["F9"])]);
        let map = Keymap::build(&s, p);
        assert_eq!(map.lookup(Key::F9, Modifiers::NONE), Some(Command::Stop));
        assert!(map.chords(Command::CycleTheme).is_empty());
        // Cmd+C 可以指定給別的指令（從「複製」事件觸發）
        let s = custom(&[("toggle-pause", &["Cmd+C"])]);
        let map = Keymap::build(&s, p);
        assert_eq!(map.on_copy(), Some(Command::TogglePause));
        assert!(map.chords(Command::CopyFrame).is_empty());
        // Cmd+Shift+C 也會變成「複製」：不能指定（不然按了會做 Cmd+C 的指令）
        let s = custom(&[("toggle-pause", &["Cmd+Shift+C", "Cmd+Alt+C"])]);
        let map = Keymap::build(&s, p);
        assert!(map.chords(Command::TogglePause).is_empty());
        assert_eq!(map.on_copy(), Some(Command::CopyFrame));
    }

    #[test]
    fn sanitized_caps_and_dedupes_custom_lists() {
        let s = custom(&[("stop", &["A", "A", "B", "C", "D", "E"]), ("future", &["X", "X"])]).sanitized();
        assert_eq!(s.custom["stop"], ["A", "B", "C", "D"]);
        assert_eq!(s.custom["future"], ["X"], "認不得的指令也整理，但保留");
    }

    /// 選單、提示上的按鍵說明：預設組跟 v0.3.0 寫死的字一模一樣（每個平台、兩種語言）
    #[test]
    fn hints_are_byte_identical_to_v030() {
        use Command as C;
        for p in Platform::ALL {
            let mac = p == Platform::Mac;
            let map = keymap(p);
            let (cmd, alt) = if mac { ("Cmd", "Option") } else { ("Ctrl", "Alt") };
            for lang in Lang::ALL {
                set_lang(lang);
                let en = lang == Lang::En;
                let pick = |zh: &str, e: &str| if en { e.to_owned() } else { zh.to_owned() };
                // 右鍵選單右側的按鍵
                let menu = [
                    (C::OpenFile, format!("{cmd}+O")),
                    (C::TogglePause, pick("空白鍵", "Space")),
                    (C::PrevFile, "PgUp".to_owned()),
                    (C::NextFile, "PgDn".to_owned()),
                    (C::FrameNext, ".".to_owned()),
                    (C::FramePrev, ",".to_owned()),
                    (C::AbLoop, "L".to_owned()),
                    (C::Fullscreen, "F".to_owned()),
                    (C::Playlist, "F6".to_owned()),
                    (C::MediaInfo, if mac { "Cmd+I" } else { "Ctrl+F1" }.to_owned()),
                    (C::OnTop, format!("{cmd}+T")),
                    (C::Settings, "F5".to_owned()),
                    (C::About, "F1".to_owned()),
                    (C::ZoomIn, "9".to_owned()),
                    (C::ZoomOut, "1".to_owned()),
                    (C::ZoomReset, "5".to_owned()),
                    (C::PanLeft, format!("{alt}+←")),
                    (C::PanRight, format!("{alt}+→")),
                    (C::PanUp, format!("{alt}+↑")),
                    (C::PanDown, format!("{alt}+↓")),
                    (C::PanCenter, format!("{cmd}+5")),
                    (C::FlipH, format!("{cmd}+Z")),
                    (C::FlipV, format!("{cmd}+P")),
                    (C::ResetView, format!("{alt}+Backspace")),
                    (C::Screenshot, format!("{cmd}+E")),
                    (C::CopyFrame, format!("{cmd}+C")),
                    (C::ControlPanel, format!("{alt}+G")),
                    (C::AdjustReset, "Q".to_owned()),
                    (C::Stop, String::new()),
                    (C::CycleTheme, String::new()),
                ];
                for (c, want) in menu {
                    assert_eq!(map.hint(c), want, "{p:?} {lang:?} {c:?}");
                }
                let crop = if mac { "Control+Q" } else { "Ctrl+Q" };
                // 提示文字
                let texts = [
                    (
                        map.drop_hint(),
                        pick(
                            &format!("把影片拖放到這裡，或按 {cmd}+O 開啟檔案"),
                            &format!("Drop a video here, or press {cmd}+O to open a file"),
                        ),
                    ),
                    (
                        map.labeled(crate::tr!("上一個檔案", "Previous file"), C::PrevFile),
                        pick("上一個檔案（PgUp）", "Previous file (PgUp)"),
                    ),
                    (
                        map.labeled(crate::tr!("播放 / 暫停", "Play / pause"), C::TogglePause),
                        pick("播放 / 暫停（空白鍵）", "Play / pause (Space)"),
                    ),
                    (
                        map.labeled(crate::tr!("下一個檔案", "Next file"), C::NextFile),
                        pick("下一個檔案（PgDn）", "Next file (PgDn)"),
                    ),
                    (
                        paren(crate::tr!("播放速度", "Playback speed"), &map.speed_keys()),
                        pick(
                            "播放速度（C 加快、X 減慢、Z 恢復正常）",
                            "Playback speed (C faster, X slower, Z normal)",
                        ),
                    ),
                    (
                        map.speed_keys(),
                        pick("C 加快、X 減慢、Z 恢復正常", "C faster, X slower, Z normal"),
                    ),
                    (
                        map.labeled_all(crate::tr!("全螢幕", "Fullscreen"), C::Fullscreen),
                        pick("全螢幕（F / Enter）", "Fullscreen (F / Enter)"),
                    ),
                    (
                        map.labeled(crate::tr!("開啟檔案", "Open file"), C::OpenFile),
                        pick(&format!("開啟檔案（{cmd}+O）"), &format!("Open file ({cmd}+O)")),
                    ),
                    (
                        map.labeled(crate::tr!("關於影戲", "About VitaScope"), C::About),
                        pick("關於影戲（F1）", "About VitaScope (F1)"),
                    ),
                    (
                        map.labeled(crate::tr!("播放清單", "Playlist"), C::Playlist),
                        pick("播放清單（F6）", "Playlist (F6)"),
                    ),
                    (
                        map.labeled(crate::tr!("關閉", "Close"), C::Playlist),
                        pick("關閉（F6）", "Close (F6)"),
                    ),
                    (
                        paren(&crate::tf!("音量 {:.0}%", "Volume {:.0}%", 80.0), &map.volume_keys()),
                        pick("音量 80%（↑ ↓）", "Volume 80% (↑ ↓)"),
                    ),
                    (
                        map.labeled(crate::tr!("靜音", "Mute"), C::ToggleMute),
                        pick("靜音（M）", "Mute (M)"),
                    ),
                    (
                        map.any_of(C::AspectCycle),
                        pick(&format!("A 或 {cmd}+F6"), &format!("A or {cmd}+F6")),
                    ),
                    (map.hint(C::CropCycle), crop.to_owned()),
                    (map.hint(C::Rotate), format!("{alt}+K")),
                    (map.pair(C::PrevChapter, C::NextChapter), format!("{cmd}+PgUp / PgDn")),
                    (
                        map.labeled(crate::tr!("全部還原", "Reset all"), C::AdjustReset),
                        pick("全部還原（Q）", "Reset all (Q)"),
                    ),
                    (
                        map.adjust_keys(),
                        pick(
                            "W/E 亮度・R/T 對比・Y/U 飽和度・I/O 色相",
                            "W/E brightness · R/T contrast · Y/U saturation · I/O hue",
                        ),
                    ),
                    (
                        map.adjust_osd(crate::tr!("亮度 +3", "brightness +3")),
                        pick(
                            "影像調整中：亮度 +3（Q 還原）",
                            "Image adjusted: brightness +3 (Q to reset)",
                        ),
                    ),
                    (
                        map.resume_osd("1:23"),
                        pick(
                            "從 1:23 繼續播放（Home 從頭播放）",
                            "Resuming from 1:23 (Home plays from the start)",
                        ),
                    ),
                    (
                        map.delay_note(true),
                        pick(
                            "快捷鍵 [ / ]。正數 = 字幕晚一點出現",
                            "Keys [ / ]. Positive = subtitles appear later",
                        ),
                    ),
                    (
                        map.delay_note(false),
                        pick(
                            "快捷鍵 - / =。正數 = 聲音晚一點",
                            "Keys - / =. Positive = sound plays later",
                        ),
                    ),
                    // 「設定 → 播放」的跳轉秒數；大幅跳轉以前寫「Ctrl + ← / →」（macOS「⌘ + ← / →」），
                    // 改成跟快捷鍵頁、選單一樣的寫法
                    (map.seek_label(false), pick("← / → 跳轉", "← / → seek")),
                    (
                        map.seek_label(true),
                        pick(&format!("{cmd}+← / → 跳轉"), &format!("{cmd}+← / → seek")),
                    ),
                ];
                for (got, want) in texts {
                    assert_eq!(got, want, "{p:?} {lang:?}");
                }
            }
        }
        set_lang(Lang::ZhTw);
    }

    #[test]
    fn hints_follow_overrides_and_drop_unbound_parts() {
        set_lang(Lang::ZhTw);
        let s = custom(&[
            ("next-file", &["N"]),
            ("restart", &[]),
            ("adjust-reset", &[]),
            ("open-file", &[]),
            ("speed-down", &[]),
            ("brightness-down", &[]),
            ("contrast-down", &[]),
            ("contrast-up", &[]),
            ("next-chapter", &["Shift+PageDown"]),
            ("volume-up", &[]),
            ("volume-down", &[]),
        ]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.labeled("下一個檔案", Command::NextFile), "下一個檔案（N）");
        assert_eq!(map.resume_osd("1:23"), "從 1:23 繼續播放");
        assert_eq!(map.adjust_osd("亮度 +3"), "影像調整中：亮度 +3");
        assert_eq!(map.drop_hint(), "把影片拖放到這裡");
        assert_eq!(map.speed_keys(), "C 加快、Z 恢復正常");
        assert_eq!(map.adjust_keys(), "E 亮度・Y/U 飽和度・I/O 色相");
        assert_eq!(
            map.pair(Command::PrevChapter, Command::NextChapter),
            "Ctrl+PgUp / Shift+PgDn"
        );
        assert_eq!(paren("音量 80%", &map.volume_keys()), "音量 80%");
        assert_eq!(map.labeled("從頭播放", Command::Restart), "從頭播放");
        let s = custom(&[("prev-chapter", &[]), ("next-chapter", &[])]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.pair(Command::PrevChapter, Command::NextChapter), "");
        // 延遲的說明：只剩一個鍵、都沒有
        let s = custom(&[
            ("sub-delay-down", &[]),
            ("audio-delay-down", &[]),
            ("audio-delay-up", &[]),
        ]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.delay_note(true), "快捷鍵 ]。正數 = 字幕晚一點出現");
        assert_eq!(map.delay_note(false), "正數 = 聲音晚一點");
        // 歸零有按鍵時也寫出來
        let s = custom(&[
            ("audio-delay-down", &[]),
            ("audio-delay-up", &[]),
            ("audio-delay-reset", &["F9"]),
        ]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.delay_note(false), "歸零 F9。正數 = 聲音晚一點");
        let pot = Keymap::build(
            &KeySettings {
                preset: KeyPreset::Potplayer,
                ..Default::default()
            },
            Platform::Windows,
        );
        assert_eq!(
            pot.delay_note(true),
            "快捷鍵 Shift+, / Shift+.，歸零 /。正數 = 字幕晚一點出現"
        );
        set_lang(Lang::En);
        assert_eq!(
            pot.delay_note(true),
            "Keys Shift+, / Shift+., reset /. Positive = subtitles appear later"
        );
        assert_eq!(map.delay_note(false), "Reset F9. Positive = sound plays later");
        set_lang(Lang::ZhTw);
        // 跳轉沒有按鍵：只寫「跳轉」「大幅跳轉」
        let s = custom(&[("seek-back", &[]), ("seek-forward", &[]), ("seek-back-long", &[])]);
        let map = Keymap::build(&s, Platform::Windows);
        assert_eq!(map.seek_label(false), "跳轉");
        assert_eq!(map.seek_label(true), "Ctrl+→ 跳轉");
        let s = custom(&[("seek-back-long", &[]), ("seek-forward-long", &[])]);
        assert_eq!(Keymap::build(&s, Platform::Windows).seek_label(true), "大幅跳轉");
    }

    #[test]
    fn recorded_chords_drop_shift_from_shifted_symbols() {
        let w = Platform::Windows;
        let m = Platform::Mac;
        let shift = Modifiers::SHIFT;
        // 美式鍵盤的 Shift+= 收到的是 Plus：記成「+」，不記 Shift
        assert_eq!(
            recorded_chord(Key::Plus, shift, w),
            Some(Chord::new(Mods::NONE, Key::Plus))
        );
        for key in [
            Key::Questionmark,
            Key::Exclamationmark,
            Key::Colon,
            Key::Pipe,
            Key::OpenCurlyBracket,
            Key::CloseCurlyBracket,
        ] {
            assert_eq!(
                recorded_chord(key, shift, w),
                Some(Chord::new(Mods::NONE, key)),
                "{key:?}"
            );
        }
        // 一般的鍵照樣記 Shift；`<` 不是 egui 的按鍵，收到的是 Shift + 逗號
        assert_eq!(recorded_chord(Key::K, shift, w), Some(Chord::new(Mods::SHIFT, Key::K)));
        assert_eq!(
            recorded_chord(Key::Comma, shift, w),
            Some(Chord::new(Mods::SHIFT, Key::Comma))
        );
        // Ctrl 在 Windows 是 Cmd、在 macOS 是 Control
        assert_eq!(
            recorded_chord(Key::K, Modifiers::CTRL, w),
            Some(Chord::new(Mods::CMD, Key::K))
        );
        assert_eq!(
            recorded_chord(Key::K, Modifiers::CTRL, m),
            Some(Chord::new(Mods::CTRL, Key::K))
        );
        assert_eq!(
            recorded_chord(Key::K, Modifiers::MAC_CMD | Modifiers::COMMAND, m),
            Some(Chord::new(Mods::CMD, Key::K))
        );
        // 只按了修飾鍵：還沒錄到
        for key in [Key::ShiftLeft, Key::ControlRight, Key::AltLeft, Key::SuperLeft] {
            assert_eq!(recorded_chord(key, shift, w), None);
        }
    }

    #[test]
    fn assign_checks_reserved_conflicts_and_caps() {
        let p = Platform::Windows;
        let keys = KeySettings::default();
        let map = Keymap::build(&keys, p);
        let k = |s: &str| Chord::parse(s, p).unwrap();
        // 系統保留
        assert_eq!(
            map.try_assign(&keys, Command::TogglePause, None, k("Ctrl+V")),
            Assign::Reserved
        );
        assert_eq!(
            map.try_assign(&keys, Command::TogglePause, None, k("Esc")),
            Assign::Reserved
        );
        // 已經是自己的
        assert_eq!(
            map.try_assign(&keys, Command::TogglePause, None, k("Space")),
            Assign::Unchanged
        );
        assert_eq!(
            map.try_assign(&keys, Command::TogglePause, Some(0), k("Space")),
            Assign::Unchanged
        );
        // 別的指令的
        assert_eq!(
            map.try_assign(&keys, Command::TogglePause, None, k("M")),
            Assign::Conflict(Command::ToggleMute)
        );
        // 新增一組、換掉一組
        let Assign::Changed(added) = map.try_assign(&keys, Command::TogglePause, None, k("Shift+K")) else {
            panic!()
        };
        assert_eq!(added.custom["toggle-pause"], ["Space", "Shift+K"]);
        let Assign::Changed(replaced) = map.try_assign(&keys, Command::TogglePause, Some(0), k("K")) else {
            panic!()
        };
        assert_eq!(replaced.custom["toggle-pause"], ["K"]);
        assert!(replaced.overridden(Command::TogglePause));
        assert_eq!(replaced.override_count(), 1);
        // 改到這裡：兩個指令都記下來，設定裡沒有重複的按鍵
        let stolen = map.assign_stealing(&keys, Command::TogglePause, None, k("M"));
        assert_eq!(stolen.custom["toggle-pause"], ["Space", "M"]);
        assert_eq!(stolen.custom["toggle-mute"], Vec::<String>::new());
        let after = Keymap::build(&stolen, p);
        assert_eq!(after.lookup(Key::M, Modifiers::NONE), Some(Command::TogglePause));
        assert!(after.chords(Command::ToggleMute).is_empty());
        assert!(after.shadowed().is_empty());
        // 最多 MAX_CHORDS 組
        let full = custom(&[("stop", &["F9", "F10", "F11", "F12"])]);
        let map = Keymap::build(&full, p);
        assert_eq!(
            map.try_assign(&full, Command::Stop, None, k("S")),
            Assign::Unchanged,
            "已經 4 組（設定頁不會出現「+」）"
        );
        let Assign::Changed(swapped) = map.try_assign(&full, Command::Stop, Some(3), k("S")) else {
            panic!()
        };
        assert_eq!(swapped.custom["stop"], ["F9", "F10", "F11", "S"]);
        // 移除一組、全部移除 = 不指定（空陣列）
        let one = map.unassign(&full, Command::Stop, 1);
        assert_eq!(one.custom["stop"], ["F9", "F11", "F12"]);
        let map = Keymap::build(&custom(&[("stop", &["F9"])]), p);
        let none = map.unassign(&custom(&[("stop", &["F9"])]), Command::Stop, 0);
        assert_eq!(none.custom["stop"], Vec::<String>::new());
        // 還原一項、全部還原（預設組不變；認不得的也拿掉）
        let mut keys = custom(&[("stop", &["F9"]), ("restart", &[]), ("future-cmd", &["F8"])]);
        keys.preset = KeyPreset::Potplayer;
        assert_eq!(keys.override_count(), 2, "認不得的不算");
        let one = keys.reset_command(Command::Stop);
        assert!(!one.overridden(Command::Stop));
        assert!(one.overridden(Command::Restart));
        let all = keys.reset_all();
        assert_eq!(all.override_count(), 0);
        // 新版的指令（這版認不得）照樣留在設定檔裡
        assert_eq!(all.custom.len(), 1, "{:?}", all.custom);
        assert_eq!(all.custom["future-cmd"], ["F8"]);
        assert_eq!(all.preset, KeyPreset::Potplayer);
    }

    #[test]
    fn mouse_defaults_and_choices() {
        let m = MouseSettings::default();
        // 跟 v0.3.0 一樣：單擊播放／暫停、雙擊全螢幕、滾輪音量
        assert_eq!(m.command(MouseInput::Click), Some(Command::TogglePause));
        assert_eq!(m.command(MouseInput::DoubleClick), Some(Command::Fullscreen));
        for input in [MouseInput::Middle, MouseInput::Back, MouseInput::Forward] {
            assert_eq!(m.command(input), None);
            assert!(!m.changed(input));
        }
        assert_eq!(m.wheel, WheelMode::Volume);
        assert_eq!(KeySettings::default().mouse, m);
        // 單擊只能選開關（雙擊時才切得回來）；預設值都在可以選的清單裡
        for input in MouseInput::ALL {
            if let (Some(list), Some(cmd)) = (input.choices(), m.command(input)) {
                assert!(list.contains(&cmd), "{input:?}");
            }
        }
        assert_eq!(
            MouseInput::Click.choices(),
            Some(&[Command::TogglePause, Command::ToggleMute][..])
        );
        assert!(MouseInput::Middle.choices().is_none(), "中鍵、側鍵可以是任何指令");
        // 名稱：兩種語言都有、不重複
        for lang in [Lang::ZhTw, Lang::En] {
            set_lang(lang);
            let labels: std::collections::HashSet<_> = MouseInput::ALL.iter().map(|m| m.label()).collect();
            assert_eq!(labels.len(), MouseInput::ALL.len());
        }
        set_lang(Lang::ZhTw);
    }

    #[test]
    fn mouse_set_label_and_unknown_ids() {
        let mut m = MouseSettings::default();
        m.set(MouseInput::Middle, Some(Command::ToggleMute));
        assert_eq!(m.middle, "toggle-mute");
        assert_eq!(m.command(MouseInput::Middle), Some(Command::ToggleMute));
        assert!(m.changed(MouseInput::Middle));
        assert_eq!(m.label(MouseInput::Middle), "靜音");
        m.set(MouseInput::Click, None);
        assert_eq!(m.click, "", "不動作存成空字串");
        assert_eq!(m.command(MouseInput::Click), None);
        assert_eq!(m.label(MouseInput::Click), "不動作");
        assert!(m.changed(MouseInput::Click));
        // 新版的指令（這版認不得）：當成不動作，名稱照原樣寫，不改掉
        m.back = "mini-player-2030".into();
        assert_eq!(m.command(MouseInput::Back), None);
        assert_eq!(m.label(MouseInput::Back), "mini-player-2030");
        set_lang(Lang::En);
        assert_eq!(m.label(MouseInput::Click), "Do nothing");
        assert_eq!(WheelMode::Seek.label(5.0), "Seek (5 s per notch)");
        set_lang(Lang::ZhTw);
        assert_eq!(WheelMode::Seek.label(2.5), "跳轉（每格 2.5 秒）");
        assert_eq!(WheelMode::Volume.label(5.0), "音量");
        // 設定檔裡的寫法
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["wheel"], "volume");
        assert_eq!(serde_json::to_value(WheelMode::Seek).unwrap(), "seek");
        assert_eq!(serde_json::to_value(WheelMode::None).unwrap(), "none");
        let back: MouseSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
        // 只寫了一項：其他是預設值
        let partial: MouseSettings = serde_json::from_str(r#"{"wheel": "seek"}"#).unwrap();
        assert_eq!(partial.wheel, WheelMode::Seek);
        assert_eq!(partial.click, "toggle-pause");
        assert_eq!(partial.double_click, "fullscreen");
    }

    #[test]
    fn reset_all_also_resets_the_mouse() {
        let mut keys = custom(&[("stop", &["F9"])]);
        keys.mouse.set(MouseInput::Middle, Some(Command::ToggleMute));
        keys.mouse.wheel = WheelMode::Seek;
        // 滑鼠不算在「你改過的 n 個快捷鍵」裡（換預設組時滑鼠本來就不變）
        assert_eq!(keys.override_count(), 1);
        let all = keys.reset_all();
        assert_eq!(all.mouse, MouseSettings::default());
        assert!(all.custom.is_empty());
        // 一項一項還原只動按鍵
        assert_eq!(keys.reset_command(Command::Stop).mouse, keys.mouse);
    }

    #[test]
    fn bookmark_keys_in_both_presets() {
        set_lang(Lang::ZhTw);
        for preset in KeyPreset::ALL {
            for p in Platform::ALL {
                let map = Keymap::build(
                    &KeySettings {
                        preset,
                        ..Default::default()
                    },
                    p,
                );
                let cmd = if p == Platform::Mac {
                    Modifiers::MAC_CMD | Modifiers::COMMAND
                } else {
                    Modifiers::COMMAND
                };
                // 檔案、章節、書籤：同一組鍵，修飾鍵多的優先
                assert_eq!(map.lookup(Key::PageUp, Modifiers::NONE), Some(Command::PrevFile));
                assert_eq!(map.lookup(Key::PageUp, cmd), Some(Command::PrevChapter));
                assert_eq!(map.lookup(Key::PageUp, Modifiers::SHIFT), Some(Command::BookmarkPrev));
                assert_eq!(map.lookup(Key::PageDown, Modifiers::SHIFT), Some(Command::BookmarkNext));
                assert_eq!(
                    map.lookup(Key::PageDown, cmd | Modifiers::SHIFT),
                    Some(Command::NextChapter),
                    "{preset:?} {p:?}"
                );
                assert_eq!(map.lookup(Key::P, Modifiers::NONE), Some(Command::BookmarkAdd));
                assert_eq!(map.lookup(Key::P, cmd), Some(Command::FlipV), "Ctrl+P 照舊是上下翻轉");
                assert_eq!(map.lookup(Key::H, Modifiers::NONE), Some(Command::BookmarkList));
                assert_eq!(map.lookup(Key::H, Modifiers::SHIFT), Some(Command::BookmarkList));
                assert_eq!(
                    map.lookup(Key::H, cmd),
                    None,
                    "Ctrl+H 沒有作用（macOS 的 ⌘H 是隱藏程式）"
                );
                assert_eq!(map.labeled("書籤清單", Command::BookmarkList), "書籤清單（H）");
                assert_eq!(
                    map.pair(Command::BookmarkPrev, Command::BookmarkNext),
                    "Shift+PgUp / PgDn"
                );
                assert_eq!(map.no_bookmarks_osd(), "這個檔案還沒有書籤（按 P 新增）");
            }
        }
        // 改了按鍵：提示跟著改；沒有按鍵時括號整個不寫
        let map = Keymap::build(&custom(&[("bookmark-add", &["B"])]), Platform::Windows);
        assert_eq!(map.no_bookmarks_osd(), "這個檔案還沒有書籤（按 B 新增）");
        assert_eq!(map.bookmarks_empty_hint(), "按 B 在目前的位置新增書籤");
        let map = Keymap::build(&custom(&[("bookmark-add", &[])]), Platform::Windows);
        assert_eq!(map.no_bookmarks_osd(), "這個檔案還沒有書籤");
        assert_eq!(map.bookmarks_empty_hint(), "這個檔案還沒有書籤");
        assert_eq!(
            keymap(Platform::Windows).bookmarks_empty_hint(),
            "按 P 在目前的位置新增書籤"
        );
        set_lang(Lang::En);
        assert_eq!(
            keymap(Platform::Windows).no_bookmarks_osd(),
            "No bookmarks in this file yet (press P to add one)"
        );
        assert_eq!(Group::Bookmarks.label(), "Bookmarks");
        assert_eq!(Command::BookmarkList.label(), "Bookmark list");
        assert_eq!(
            keymap(Platform::Windows).bookmarks_empty_hint(),
            "Press P to bookmark the current position"
        );
        set_lang(Lang::ZhTw);
    }

    /// C3：Ctrl（⌘）+ U 開啟網址（兩組預設都是）；起始畫面的第二行跟著按鍵改，沒有按鍵時只寫貼上
    #[test]
    fn open_url_key_and_hint() {
        set_lang(Lang::ZhTw);
        for preset in KeyPreset::ALL {
            for p in Platform::ALL {
                let map = Keymap::build(
                    &KeySettings {
                        preset,
                        ..Default::default()
                    },
                    p,
                );
                let cmd = if p == Platform::Mac {
                    Modifiers::MAC_CMD | Modifiers::COMMAND
                } else {
                    Modifiers::COMMAND
                };
                assert_eq!(map.lookup(Key::U, cmd), Some(Command::OpenUrl), "{preset:?} {p:?}");
                assert_eq!(map.lookup(Key::U, cmd | Modifiers::SHIFT), Some(Command::OpenUrl));
                assert_eq!(map.lookup(Key::U, Modifiers::NONE), Some(Command::SaturationUp));
                let c = if p == Platform::Mac { "Cmd" } else { "Ctrl" };
                assert_eq!(map.hint(Command::OpenUrl), format!("{c}+U"));
                assert_eq!(map.url_hint(), format!("按 {c}+U 開啟網址，或按 {c}+V 貼上網址"));
            }
        }
        let map = Keymap::build(&custom(&[("open-url", &[])]), Platform::Windows);
        assert_eq!(map.url_hint(), "按 Ctrl+V 貼上網址");
        let map = Keymap::build(&custom(&[("open-url", &["F3"])]), Platform::Windows);
        assert_eq!(map.url_hint(), "按 F3 開啟網址，或按 Ctrl+V 貼上網址");
        set_lang(Lang::En);
        assert_eq!(
            keymap(Platform::Windows).url_hint(),
            "Press Ctrl+U to open a URL, or Ctrl+V to paste one"
        );
        assert_eq!(Command::OpenUrl.label(), "Open URL…");
        set_lang(Lang::ZhTw);
    }
}
