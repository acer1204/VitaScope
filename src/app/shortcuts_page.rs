//! 「設定 → 快捷鍵」：選預設組（影戲 / PotPlayer 風格）、改每個指令的按鍵。
//!
//! - 按一下按鍵（或「+」）開始錄：下一個按下的鍵（含修飾鍵）就是新的按鍵，Esc 取消。
//!   錄的時候其他快捷鍵都停用（`handle_keys` 最前面先交給 [`VitascopeApp::capture_key`]）。
//! - 系統保留的按鍵（Ctrl+V…）不能指定；已經用在別的指令時問要不要改到這裡。
//! - 改了馬上生效、存檔；自己改過的指令標「•」，可以一項一項還原，或「還原成預設組…」全部還原。
//! - 底下是滑鼠：單擊、雙擊、中鍵、側鍵、滾輪各做什麼（兩個預設組一樣；「還原成預設組…」也會還原）。

use super::VitascopeApp;
use crate::keymap::{
    self, Assign, Chord, Command, Group, KeyPreset, KeySettings, Keymap, MAX_CHORDS, Mods, MouseInput, WheelMode,
};
use crate::theme::Palette;
use crate::{tf, tr};
use eframe::egui::{self, Event, Id, Key};

/// 指令名稱那一欄的寬度
const LABEL_WIDTH: f32 = 170.0;

/// 正在錄的按鍵：哪個指令、換掉第幾組（None = 新增一組）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeyCapture {
    cmd: Command,
    slot: Option<usize>,
}

/// 錄到的按鍵已經用在別的指令：等使用者選「改到這裡」或「取消」
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyConflict {
    cmd: Command,
    slot: Option<usize>,
    chord: Chord,
    other: Command,
}

/// 快捷鍵設定頁的狀態（不存檔）
#[derive(Debug, Default)]
pub(super) struct ShortcutsUi {
    capture: Option<KeyCapture>,
    conflict: Option<KeyConflict>,
    /// 錄到系統保留的按鍵（那一列下面顯示說明，繼續錄）
    reserved: Option<(Command, Chord)>,
    search: String,
    /// 「還原成預設組…」的確認對話框
    reset_open: bool,
}

impl ShortcutsUi {
    /// 正在錄按鍵（`handle_keys` 先把按鍵交給設定頁）
    pub(super) fn recording(&self) -> bool {
        self.capture.is_some()
    }

    /// 設定視窗關掉、換到別的分頁：錄到一半的、還沒回答的衝突都取消
    pub(super) fn cancel(&mut self) {
        self.capture = None;
        self.conflict = None;
        self.reserved = None;
    }

    /// 設定視窗關掉、換到別的分頁：連確認對話框一起關，搜尋也清掉（下次打開看到全部）
    pub(super) fn close(&mut self) {
        self.cancel();
        self.reset_open = false;
        self.search.clear();
    }
}

/// 錄按鍵時這一幀收到的
enum Recorded {
    Cancel,
    Chord(Chord),
}

/// 按鍵上的寫法：錄的時候換成提示
fn recording_text() -> &'static str {
    tr!("請按下新的按鍵…（Esc 取消）", "Press the new key… (Esc to cancel)")
}

impl VitascopeApp {
    /// 換快捷鍵的設定：重建對照表（選單、提示跟著變）、存檔
    fn set_keys(&mut self, keys: KeySettings) {
        self.settings.keys = keys;
        self.keymap = Keymap::build(&self.settings.keys, self.keymap.platform());
        self.save_settings();
    }

    /// 正在錄按鍵：這一幀的按鍵都給設定頁（其他快捷鍵、Esc 關視窗都不做），回傳是否在錄
    pub(super) fn capture_key(&mut self, ctx: &egui::Context) -> bool {
        if !self.keys_ui.recording() {
            return false;
        }
        let platform = self.keymap.platform();
        let got = ctx.input_mut(|i| {
            let mut got = None;
            for e in &i.events {
                if got.is_some() {
                    break;
                }
                got = match e {
                    Event::Key {
                        key: Key::Escape,
                        pressed: true,
                        ..
                    } => Some(Recorded::Cancel),
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => keymap::recorded_chord(*key, *modifiers, platform).map(Recorded::Chord),
                    // Cmd+C、Cmd+X、Cmd+V 不是按鍵事件：egui 變成複製、剪下、貼上（貼上要剪貼簿裡有文字才有）。
                    // Cmd+C 可以指定（從「複製」觸發），剪下、貼上是保留的：讓使用者看到說明，而不是沒反應
                    Event::Copy => Some(Recorded::Chord(Chord::new(Mods::CMD, Key::C))),
                    Event::Cut => Some(Recorded::Chord(Chord::new(Mods::CMD, Key::X))),
                    Event::Paste(_) => Some(Recorded::Chord(Chord::new(Mods::CMD, Key::V))),
                    _ => None,
                };
            }
            // 錄的時候按鍵不給任何人（包括有焦點的按鈕：空白鍵、Enter 不會又按到它；打出來的字也不進搜尋框）
            i.events.retain(|e| {
                !matches!(
                    e,
                    Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_)
                )
            });
            got
        });
        match got {
            Some(Recorded::Cancel) => self.keys_ui.cancel(),
            Some(Recorded::Chord(chord)) => self.recorded(chord),
            None => {}
        }
        true
    }

    /// 錄到一組按鍵
    fn recorded(&mut self, chord: Chord) {
        let Some(KeyCapture { cmd, slot }) = self.keys_ui.capture else {
            return;
        };
        match self.keymap.try_assign(&self.settings.keys, cmd, slot, chord) {
            // 保留的：說明一下，繼續錄（可以直接按別的鍵）
            Assign::Reserved => self.keys_ui.reserved = Some((cmd, chord)),
            Assign::Unchanged => self.keys_ui.cancel(),
            Assign::Conflict(other) => {
                self.keys_ui.cancel();
                self.keys_ui.conflict = Some(KeyConflict {
                    cmd,
                    slot,
                    chord,
                    other,
                });
            }
            Assign::Changed(keys) => {
                self.keys_ui.cancel();
                self.set_keys(keys);
            }
        }
    }

    pub(super) fn shortcuts_page(&mut self, ui: &mut egui::Ui) {
        let platform = self.keymap.platform();
        // 預設組
        ui.horizontal(|ui| {
            ui.label(tr!("預設組：", "Preset:"));
            let current = self.settings.keys.preset;
            for preset in KeyPreset::ALL {
                if ui.radio(current == preset, preset.label()).clicked() && preset != current {
                    self.keys_ui.cancel();
                    let keys = KeySettings {
                        preset,
                        ..self.settings.keys.clone()
                    };
                    self.set_keys(keys);
                }
            }
            ui.add_space(12.0);
            if ui.button(tr!("還原成預設組…", "Reset to the preset…")).clicked() {
                self.keys_ui.cancel();
                self.keys_ui.reset_open = true;
            }
        });
        let changed = self.settings.keys.override_count();
        if changed == 1 {
            ui.weak(tr!(
                "你改過的 1 個快捷鍵（標 •）換預設組時照樣保留",
                "Your 1 changed shortcut (marked •) is kept when you switch presets"
            ));
        } else if changed > 1 {
            ui.weak(tf!(
                "你改過的 {changed} 個快捷鍵（標 •）換預設組時照樣保留",
                "Your {changed} changed shortcuts (marked •) are kept when you switch presets"
            ));
        }
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.keys_ui.search)
                .hint_text(tr!("搜尋功能或按鍵", "Search commands or keys"))
                .desired_width(240.0),
        );
        // 錄到一半點了搜尋框：不錄了（不然打的第一個字也會被錄成按鍵）
        if search.gained_focus() || search.changed() {
            self.keys_ui.cancel();
        }
        ui.add_space(4.0);
        let search = self.keys_ui.search.trim().to_lowercase();
        let matches = |keymap: &Keymap, cmd: Command| {
            search.is_empty()
                || cmd.label().to_lowercase().contains(&search)
                || keymap
                    .chords(cmd)
                    .iter()
                    .any(|c| c.display(platform).to_lowercase().contains(&search))
        };
        for group in Group::ALL {
            let cmds: Vec<Command> = Command::ALL
                .iter()
                .copied()
                .filter(|&c| c.group() == group && matches(&self.keymap, c))
                .collect();
            if cmds.is_empty() {
                continue;
            }
            ui.add_space(6.0);
            ui.strong(group.label());
            egui::Grid::new(("shortcuts_group", group.label()))
                .num_columns(3)
                .striped(true)
                .spacing([10.0, 4.0])
                .min_col_width(0.0)
                .show(ui, |ui| {
                    for cmd in cmds {
                        self.shortcut_row(ui, cmd);
                    }
                });
        }
        self.mouse_section(ui, &search);
        ui.add_space(10.0);
        // macOS 的 Backspace：預設組或自己指定的用到它時就不是「從清單移除」（例如 PotPlayer 風格的從頭播放）
        let backspace_free = self.keymap.owner(Chord::new(Mods::NONE, Key::Backspace)).is_none();
        fixed_keys(ui, platform, backspace_free);
    }

    /// 一個指令一列：名稱、按鍵（按一下重錄、× 移除、+ 新增）、↺ 還原；底下是衝突、保留按鍵的說明
    fn shortcut_row(&mut self, ui: &mut egui::Ui, cmd: Command) {
        let platform = self.keymap.platform();
        let overridden = self.settings.keys.overridden(cmd);
        ui.horizontal(|ui| {
            // 每一組是各自的表格：名稱欄固定寬度，按鍵才會上下對齊
            ui.set_min_width(LABEL_WIDTH);
            ui.label(cmd.label());
            if overridden {
                ui.weak("•")
                    .on_hover_text(tr!("跟預設組不一樣", "Changed from the preset"));
            }
            // 預設組的按鍵被別的指令拿走了（例如自己把 M 指定給播放／暫停）
            for &(c, chord, other) in self.keymap.shadowed() {
                if c == cmd {
                    ui.weak("⚠").on_hover_text(tf!(
                        "{} 被「{}」使用",
                        "{} is used by “{}”",
                        chord.display(platform),
                        other.label()
                    ));
                }
            }
        });
        let mut start: Option<KeyCapture> = None;
        let mut remove: Option<usize> = None;
        ui.horizontal(|ui| {
            let chords = self.keymap.chords(cmd).to_vec();
            for (i, chord) in chords.iter().enumerate() {
                let slot = KeyCapture { cmd, slot: Some(i) };
                let recording = self.keys_ui.capture == Some(slot);
                let text = if recording {
                    recording_text().to_owned()
                } else {
                    chord.display(platform)
                };
                // 按鍵和它的 × 靠在一起，跟下一組分開
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let button = ui
                        .add(egui::Button::new(text).selected(recording))
                        .on_hover_text(tr!("按一下換成別的按鍵", "Click to change this key"));
                    if button.clicked() {
                        start = Some(slot);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text(tf!("移除 {}", "Remove {}", chord.display(platform)))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                ui.add_space(6.0);
            }
            if chords.len() < MAX_CHORDS {
                let slot = KeyCapture { cmd, slot: None };
                let recording = self.keys_ui.capture == Some(slot);
                if chords.is_empty() && !recording {
                    ui.weak(tr!("（未指定）", "(none)"));
                }
                let text = if recording { recording_text() } else { "+" };
                if ui
                    .add(egui::Button::new(text).selected(recording))
                    .on_hover_text(tr!("新增一組按鍵", "Add a key"))
                    .clicked()
                {
                    start = Some(slot);
                }
            }
        });
        let reset = overridden
            && ui
                .small_button("↺")
                .on_hover_text(tr!("還原成預設組的按鍵", "Back to the preset's keys"))
                .clicked();
        ui.end_row();
        if let Some(slot) = start {
            // 再按一下正在錄的同一個：取消
            let again = self.keys_ui.capture == Some(slot);
            self.keys_ui.cancel();
            if !again {
                self.keys_ui.capture = Some(slot);
            }
        }
        if let Some(i) = remove {
            self.keys_ui.cancel();
            let keys = self.keymap.unassign(&self.settings.keys, cmd, i);
            self.set_keys(keys);
        }
        if reset {
            self.keys_ui.cancel();
            let keys = self.settings.keys.reset_command(cmd);
            self.set_keys(keys);
        }
        self.row_notes(ui, cmd);
    }

    /// 那一列底下：錄按鍵的提示、系統保留的按鍵、衝突的詢問
    fn row_notes(&mut self, ui: &mut egui::Ui, cmd: Command) {
        let platform = self.keymap.platform();
        let problem = Palette::of(ui.visuals()).problem;
        if self.keys_ui.capture.is_some_and(|c| c.cmd == cmd) {
            ui.label("");
            ui.vertical(|ui| {
                if let Some((c, chord)) = self.keys_ui.reserved
                    && c == cmd
                {
                    ui.colored_label(
                        problem,
                        tf!(
                            "{} 是系統保留的按鍵，不能指定",
                            "{} is reserved by the system and can't be assigned",
                            chord.display(platform)
                        ),
                    );
                }
                // Cmd+V 在剪貼簿沒有文字時什麼都收不到：先寫出來
                let cut = Chord::new(Mods::CMD, Key::X).display(platform);
                let paste = Chord::new(Mods::CMD, Key::V).display(platform);
                ui.weak(tf!("{paste}、{cut} 不能指定", "{paste} and {cut} can't be assigned"));
                // Windows 的這幾個鍵 egui 也變成複製、貼上、剪下（分不出是哪個鍵按的）
                if platform == keymap::Platform::Windows {
                    ui.weak(tr!(
                        "Ctrl+Insert、Shift+Insert、Shift+Delete 會當成 Ctrl+C、Ctrl+V、Ctrl+X",
                        "Ctrl+Insert, Shift+Insert and Shift+Delete count as Ctrl+C, Ctrl+V and Ctrl+X"
                    ));
                }
            });
            ui.label("");
            ui.end_row();
        }
        let Some(conflict) = self.keys_ui.conflict.filter(|c| c.cmd == cmd) else {
            return;
        };
        let mut answer = None;
        ui.label("");
        ui.horizontal_wrapped(|ui| {
            ui.label(tf!(
                "{} 已經用在「{}」。",
                "{} is already used for “{}”.",
                conflict.chord.display(platform),
                conflict.other.label()
            ));
            if ui.button(tr!("改到這裡", "Use it here")).clicked() {
                answer = Some(true);
            }
            if ui.button(tr!("取消", "Cancel")).clicked() {
                answer = Some(false);
            }
        });
        ui.label("");
        ui.end_row();
        match answer {
            Some(true) => {
                self.keys_ui.conflict = None;
                let keys =
                    self.keymap
                        .assign_stealing(&self.settings.keys, conflict.cmd, conflict.slot, conflict.chord);
                self.set_keys(keys);
            }
            Some(false) => self.keys_ui.conflict = None,
            None => {}
        }
    }

    /// 滑鼠：每個按鍵一個下拉選單（`search` 是小寫的搜尋字；比對「滑鼠」和按鍵的名稱，不比對選的指令，
    /// 不然搜尋指令名稱時兩邊都出現）
    fn mouse_section(&mut self, ui: &mut egui::Ui, search: &str) {
        let heading = tr!("滑鼠", "Mouse");
        let matches = |label: &str| {
            search.is_empty() || heading.to_lowercase().contains(search) || label.to_lowercase().contains(search)
        };
        let inputs: Vec<MouseInput> = MouseInput::ALL.into_iter().filter(|m| matches(m.label())).collect();
        let wheel = matches(wheel_label());
        if inputs.is_empty() && !wheel {
            return;
        }
        ui.add_space(6.0);
        ui.strong(heading);
        let mut chosen: Option<(MouseInput, Option<Command>)> = None;
        let mut chosen_wheel = None;
        let mouse = &self.settings.keys.mouse;
        egui::Grid::new("shortcuts_mouse")
            .num_columns(2)
            .striped(true)
            .spacing([10.0, 4.0])
            .min_col_width(0.0)
            .show(ui, |ui| {
                for input in inputs {
                    let name = mouse_row_label(ui, input.label(), mouse.changed(input));
                    egui::ComboBox::from_id_salt(("shortcuts_mouse", input))
                        .selected_text(mouse.label(input))
                        // 「任何指令」的清單很長：開高一點
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            let current = mouse.command(input);
                            let mut item = |ui: &mut egui::Ui, cmd: Option<Command>| {
                                let text = cmd.map_or(keymap::no_action_label(), Command::label);
                                let on = current == cmd && (cmd.is_some() || mouse.id(input).is_empty());
                                if ui.selectable_label(on, text).clicked() && !on {
                                    chosen = Some((input, cmd));
                                }
                            };
                            item(ui, None);
                            match input.choices() {
                                Some(list) => {
                                    for &cmd in list {
                                        item(ui, Some(cmd));
                                    }
                                }
                                None => {
                                    for group in Group::ALL {
                                        ui.separator();
                                        ui.weak(group.label());
                                        for &cmd in Command::ALL.iter().filter(|c| c.group() == group) {
                                            item(ui, Some(cmd));
                                        }
                                    }
                                }
                            }
                        })
                        .response
                        .labelled_by(name);
                    ui.end_row();
                }
                if wheel {
                    let seek = self.settings.seek_short;
                    let name = mouse_row_label(ui, wheel_label(), mouse.wheel != WheelMode::default());
                    egui::ComboBox::from_id_salt("shortcuts_mouse_wheel")
                        .selected_text(mouse.wheel.label(seek))
                        .show_ui(ui, |ui| {
                            for mode in WheelMode::ALL {
                                let on = mouse.wheel == mode;
                                let r = ui.selectable_label(on, mode.label(seek));
                                let r = if mode == WheelMode::Seek {
                                    r.on_hover_text(tr!(
                                        "往上捲前進、往下捲後退；秒數跟「設定 → 播放」的跳轉一樣",
                                        "Scroll up to go forward, down to go back; the step is the seek time in Settings → Playback"
                                    ))
                                } else {
                                    r
                                };
                                if r.clicked() && !on {
                                    chosen_wheel = Some(mode);
                                }
                            }
                        })
                        .response
                        .labelled_by(name);
                    ui.end_row();
                }
            });
        if chosen.is_some() || chosen_wheel.is_some() {
            self.keys_ui.cancel();
            let mut keys = self.settings.keys.clone();
            if let Some((input, cmd)) = chosen {
                keys.mouse.set(input, cmd);
            }
            if let Some(mode) = chosen_wheel {
                keys.mouse.wheel = mode;
            }
            self.set_keys(keys);
        }
    }

    /// 「還原成預設組…」的確認對話框（設定視窗外面畫：蓋住整個畫面）
    pub(super) fn shortcuts_reset_modal(&mut self, ctx: &egui::Context) {
        if !self.keys_ui.reset_open {
            return;
        }
        let preset = self.settings.keys.preset;
        let mut answer = None;
        let modal = egui::Modal::new(Id::new("shortcuts_reset")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(tf!(
                "把所有快捷鍵還原成「{}」的預設值？",
                "Reset all shortcuts to the “{}” defaults?",
                preset.label()
            ));
            ui.weak(tr!("滑鼠的設定也一起還原。", "Mouse settings are reset too."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr!("還原", "Reset")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if answer == Some(true) {
            let keys = self.settings.keys.reset_all();
            self.set_keys(keys);
        }
        // Esc、點對話框外面：取消
        if answer.is_some() || modal.should_close() {
            self.keys_ui.reset_open = false;
        }
        // 對話框開著：下一幀的按鍵都交給它（空白鍵不會暫停、Esc 只關對話框）
        self.modal_open |= self.keys_ui.reset_open;
    }
}

/// 滑鼠一列的名稱（固定寬度，下拉選單才會對齊；跟預設不一樣時標「•」），回傳名稱的 id（下拉選單的無障礙名稱）
fn mouse_row_label(ui: &mut egui::Ui, label: &str, changed: bool) -> Id {
    ui.horizontal(|ui| {
        ui.set_min_width(LABEL_WIDTH);
        let id = ui.label(label).id;
        if changed {
            ui.weak("•")
                .on_hover_text(tr!("跟預設不一樣", "Changed from the default"));
        }
        id
    })
    .inner
}

/// 滾輪那一列的名稱
fn wheel_label() -> &'static str {
    tr!("在畫面上捲動滾輪", "Wheel over the video")
}

/// 固定的按鍵（不在對照表裡，不能改）；`backspace_free`：對照表沒有用到 Backspace（macOS 才用它從清單移除）
fn fixed_keys(ui: &mut egui::Ui, platform: keymap::Platform, backspace_free: bool) {
    let mac = platform == keymap::Platform::Mac;
    let cmd = if mac { "Cmd" } else { "Ctrl" };
    ui.strong(tr!("固定的按鍵（不能更改）", "Fixed keys (can't be changed)"));
    let rows: Vec<(String, &str)> = vec![
        (
            "Esc".to_owned(),
            tr!("關閉視窗、離開全螢幕", "Close a window, leave fullscreen"),
        ),
        (
            if mac && backspace_free {
                "Delete / Backspace"
            } else {
                "Delete"
            }
            .to_owned(),
            tr!(
                "從播放清單移除（清單開著時）",
                "Remove from the playlist (when it is open)"
            ),
        ),
        (
            format!("{cmd} + V / X"),
            tr!("保留給貼上、剪下", "Reserved for paste and cut"),
        ),
        (
            format!("{cmd} + {}", tr!("滾輪", "wheel")),
            tr!(
                "縮放畫面（觸控板也可以捏合）",
                "Zoom the picture (or pinch on a touchpad)"
            ),
        ),
        (tr!("右鍵", "Right-click").to_owned(), tr!("選單", "Menu")),
    ];
    egui::Grid::new("shortcuts_fixed")
        .num_columns(2)
        .striped(true)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for (key, what) in rows {
                ui.label(egui::RichText::new(key).monospace());
                ui.label(what);
                ui.end_row();
            }
        });
}
