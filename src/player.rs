//! 播放器核心：把 mpv 的屬性與事件整理成 Rust 狀態。
//! 介面（app）和自動測試（tests/）都透過這一層操作 mpv。

use crate::mpv::{self, EndReason, Event, Format, Mpv, Value};
use crate::subs::{self, ExternalSub, SubLang};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 建立播放器的設定。
pub struct Options {
    /// true = 不開視窗、不出聲音（自動測試用）
    pub headless: bool,
    /// 硬體解碼模式：`auto-safe`（預設，失敗自動退回軟解）、`auto-copy`、`no`…
    pub hwdec: String,
    /// 播完停在最後一格（播放器介面用）；false = 播完就卸載檔案（測試用）
    pub keep_open: bool,
    /// 開檔後自動選字幕：繁中優先；沒有字幕被選上時也選一條（比照 PotPlayer）
    pub auto_select_subs: bool,
    /// 外掛字幕自己找、自己判斷編碼和語言（見 `subs` 模組）；false = 交給 mpv 的 sub-auto
    pub external_subs: bool,
    /// 有新事件時呼叫（在 mpv 的執行緒上，只能用來喚醒 UI）
    pub wakeup: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            headless: false,
            hwdec: "auto-safe".into(),
            keep_open: true,
            auto_select_subs: true,
            external_subs: true,
            wakeup: None,
        }
    }
}

impl Options {
    pub fn headless() -> Self {
        Self {
            headless: true,
            hwdec: "no".into(),
            keep_open: false,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Sub,
    #[serde(other)]
    Other,
}

impl TrackKind {
    /// 選擇這類軌道的 mpv 屬性名稱
    fn property(self) -> Option<&'static str> {
        match self {
            TrackKind::Video => Some("vid"),
            TrackKind::Audio => Some("aid"),
            TrackKind::Sub => Some("sid"),
            TrackKind::Other => None,
        }
    }
}

/// `track-list` 裡的一條軌道。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: TrackKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub external: bool,
    #[serde(default)]
    pub external_filename: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub albumart: bool,
    #[serde(default, rename = "demux-w")]
    pub width: Option<i64>,
    #[serde(default, rename = "demux-h")]
    pub height: Option<i64>,
    #[serde(default, rename = "demux-channel-count")]
    pub channels: Option<i64>,
    #[serde(default, rename = "demux-samplerate")]
    pub samplerate: Option<i64>,
}

impl Track {
    /// 選單上顯示的名稱，例如「#2 日本語 (jpn) · aac 2ch」
    pub fn label(&self) -> String {
        let mut s = format!("#{}", self.id);
        if let Some(t) = self.title.as_deref().filter(|t| !t.is_empty()) {
            s += &format!(" {t}");
        }
        if let Some(l) = self.lang.as_deref().filter(|l| !l.is_empty()) {
            s += &format!(" ({l})");
        }
        let mut detail = Vec::new();
        if let Some(c) = &self.codec {
            detail.push(c.clone());
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            detail.push(format!("{w}×{h}"));
        }
        if let Some(ch) = self.channels {
            detail.push(format!("{ch}ch"));
        }
        if self.external {
            detail.push("外掛".into());
        }
        if !detail.is_empty() {
            s += &format!(" · {}", detail.join(" "));
        }
        s
    }
}

/// 介面需要的播放狀態，由 mpv 的屬性變化事件即時更新。
#[derive(Debug, Clone, Default)]
pub struct State {
    pub path: Option<String>,
    pub title: Option<String>,
    /// 已送出開檔、還沒載入完成
    pub loading: bool,
    /// 有檔案載入中（FileLoaded 到 EndFile 之間）
    pub loaded: bool,
    pub time_pos: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub eof: bool,
    pub seekable: bool,
    pub tracks: Vec<Track>,
    /// 實際顯示尺寸（已套用旋轉與像素比例）
    pub video_size: Option<[i64; 2]>,
    /// 目前使用的硬體解碼器；None 或 "no" = 軟解
    pub hwdec: Option<String>,
    /// 最近一次開檔失敗的原因（中文）
    pub last_error: Option<String>,
}

impl State {
    pub fn tracks_of(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |t| t.kind == kind)
    }

    pub fn selected(&self, kind: TrackKind) -> Option<&Track> {
        self.tracks_of(kind).find(|t| t.selected)
    }

    /// 有實際影像（不是只有專輯封面）
    pub fn has_video(&self) -> bool {
        self.tracks_of(TrackKind::Video).any(|t| !t.albumart)
    }
}

/// 介面關心的播放事件（屬性變化已經反映在 `State`，不另外回報）。
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    StartFile,
    FileLoaded,
    PlaybackRestart,
    VideoReconfig,
    Seek,
    EndFile { reason: EndReason, error: Option<String> },
    Shutdown,
}

const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("volume", Format::Double),
    ("mute", Format::Flag),
    ("track-list", Format::String),
    ("media-title", Format::String),
    ("path", Format::String),
    ("eof-reached", Format::Flag),
    ("seekable", Format::Flag),
    ("hwdec-current", Format::String),
    // 不用 dwidth/dheight：GPU 負責旋轉時（GUI 的情況），它們是旋轉前的尺寸
    ("video-out-params", Format::String),
];

/// 字幕語言偏好：繁中優先，其次中文，再來英文
const SUB_LANGS: &str = "zh-TW,zh-Hant,cht,tc,zht,zh-HK,zh,chi,zho,en,eng";

pub struct Player {
    mpv: Arc<Mpv>,
    pub state: State,
    /// 這次開檔期間 mpv 回報的錯誤訊息（開檔失敗時拿來說明原因）
    recent_errors: Vec<String>,
    auto_select_subs: bool,
    external_subs: bool,
    /// 最近一次開檔失敗的基本原因。mpv 的記錄訊息要等一般事件都取完才會送出，
    /// 常常比 EndFile 晚到，所以失敗後收到的錯誤記錄還要補進說明裡
    failure: Option<String>,
}

impl Player {
    pub fn new(opts: Options) -> mpv::Result<Self> {
        let keep_open = if opts.keep_open { "yes" } else { "no" };
        let mut options: Vec<(&str, &str)> = vec![
            ("vo", if opts.headless { "null" } else { "libmpv" }),
            ("hwdec", &opts.hwdec),
            ("keep-open", keep_open),
            ("idle", "yes"),
            // 介面和按鍵都由我們處理
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            // 外掛字幕預設由 subs 模組處理（mpv 對 GBK / UTF-16 字幕常猜錯編碼）
            ("sub-auto", if opts.external_subs { "no" } else { "fuzzy" }),
            ("slang", SUB_LANGS),
            // 網站影片（yt-dlp）屬於 L3，先關掉避免開檔時意外呼叫外部程式
            ("ytdl", "no"),
            ("audio-client-name", "VitaScope"),
        ];
        if opts.headless {
            options.push(("ao", "null"));
        }
        // 排查用：VITASCOPE_MPV_OPTS="名稱=值 名稱=值" 額外指定 mpv 選項（以空白分隔）
        let extra = std::env::var("VITASCOPE_MPV_OPTS").unwrap_or_default();
        options.extend(extra.split_whitespace().filter_map(|kv| kv.split_once('=')));
        let mut mpv = Mpv::new(&options)?;
        if let Some(wakeup) = opts.wakeup {
            mpv.set_wakeup_callback(wakeup);
        }
        // VITASCOPE_DEBUG=1：把 mpv 的警告與錯誤印到 stderr（排查顯示卡、驅動之類的問題）；
        // 也可以指定 mpv 的記錄等級，例如 VITASCOPE_DEBUG=v
        let level = match std::env::var("VITASCOPE_DEBUG") {
            Ok(v) if v == "1" || v.is_empty() => "warn".to_owned(),
            Ok(v) => v,
            Err(_) => "error".to_owned(),
        };
        mpv.request_log_messages(&level)?;
        for (i, (name, format)) in OBSERVED.iter().enumerate() {
            mpv.observe(i as u64 + 1, name, *format)?;
        }
        Ok(Self {
            mpv: Arc::new(mpv),
            state: State {
                volume: 100.0,
                ..Default::default()
            },
            recent_errors: Vec::new(),
            auto_select_subs: opts.auto_select_subs,
            external_subs: opts.external_subs,
            failure: None,
        })
        .inspect(|_| subs::clean_cache())
    }

    pub fn mpv(&self) -> &Arc<Mpv> {
        &self.mpv
    }

    /// 這次開檔以來 mpv 回報的錯誤訊息（最多 8 筆）
    pub fn recent_errors(&self) -> &[String] {
        &self.recent_errors
    }

    // ───────────── 操作 ─────────────

    pub fn open(&mut self, path: &str) -> mpv::Result<()> {
        self.state.last_error = None;
        self.mpv.command(&["loadfile", path, "replace"])
    }

    pub fn stop(&self) -> mpv::Result<()> {
        self.mpv.command(&["stop"])
    }

    pub fn set_pause(&self, paused: bool) -> mpv::Result<()> {
        self.mpv.set_property("pause", paused)
    }

    pub fn toggle_pause(&self) -> mpv::Result<()> {
        // 播完停在最後一格時按播放 = 從頭開始
        if self.state.eof && self.state.paused {
            self.mpv.command(&["seek", "0", "absolute"])?;
        }
        self.mpv.command(&["cycle", "pause"])
    }

    /// 相對跳轉（秒），可為負數
    pub fn seek_relative(&self, seconds: f64) -> mpv::Result<()> {
        self.mpv.command(&["seek", &format!("{seconds}"), "relative"])
    }

    /// 跳到絕對時間（秒）。`exact` = 精準到影格（較慢）；拖曳進度條時用關鍵影格比較順
    pub fn seek_to(&self, seconds: f64, exact: bool) -> mpv::Result<()> {
        let mode = if exact { "absolute+exact" } else { "absolute+keyframes" };
        self.mpv.command(&["seek", &format!("{seconds}"), mode])
    }

    pub fn set_volume(&self, volume: f64) -> mpv::Result<()> {
        self.mpv.set_property("volume", volume.clamp(0.0, 100.0))
    }

    pub fn set_mute(&self, muted: bool) -> mpv::Result<()> {
        self.mpv.set_property("mute", muted)
    }

    /// 選擇軌道；`None` = 關閉這類軌道（例如關字幕）
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) -> mpv::Result<()> {
        let Some(prop) = kind.property() else { return Ok(()) };
        match id {
            Some(id) => self.mpv.set_property(prop, id),
            None => self.mpv.set_property(prop, "no"),
        }
    }

    /// 載入外掛字幕並立刻顯示（拖放字幕檔、選單「載入字幕」）。編碼和語言一樣自動判斷
    pub fn add_subtitle(&self, path: &str) -> mpv::Result<()> {
        let sub_path = Path::new(path);
        let video = self
            .state
            .path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| sub_path.to_path_buf());
        match subs::load(sub_path, &video) {
            Ok(sub) => self.add_external(&sub, true),
            // 讀不了（例如不是本機檔案）就直接交給 mpv
            Err(_) => self.mpv.command(&["sub-add", path, "select"]),
        }
    }

    fn add_external(&self, sub: &ExternalSub, select: bool) -> mpv::Result<()> {
        let path = sub.load_path().map_err(|e| mpv::Error {
            code: libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED,
            context: format!("無法轉換字幕 {}：{e}", sub.path.display()),
        })?;
        let path = path.to_string_lossy();
        let flag = if select { "select" } else { "auto" };
        let mut args = vec!["sub-add", &path, flag, &sub.title];
        if let Some(lang) = sub.lang.code() {
            args.push(lang);
        }
        self.mpv.command(&args)
    }

    /// 找出並載入目前影片的外掛字幕
    fn load_external_subs(&mut self) {
        let Ok(path) = self.mpv.get_string("path") else { return };
        if path.contains("://") {
            return; // 網路串流沒有「同資料夾」可找
        }
        let video = PathBuf::from(path);
        for p in subs::find_external(&video) {
            let result = subs::load(&p, &video)
                .map_err(|e| e.to_string())
                .and_then(|sub| self.add_external(&sub, false).map_err(|e| e.to_string()));
            if let Err(e) = result {
                self.recent_errors.push(format!("[subs] 無法載入 {}：{e}", p.display()));
            }
        }
    }

    pub fn get_string(&self, name: &str) -> mpv::Result<String> {
        self.mpv.get_string(name)
    }

    pub fn get_f64(&self, name: &str) -> mpv::Result<f64> {
        self.mpv.get_property::<f64>(name)
    }

    // ───────────── 事件 ─────────────

    /// 處理所有待處理的事件（介面每一幀呼叫），回傳介面需要反應的事件。
    pub fn poll(&mut self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        while let Some(ev) = self.mpv.wait_event(0.0) {
            if let Some(pe) = self.handle(ev) {
                out.push(pe);
            }
        }
        out
    }

    /// 等待下一個播放事件，最多 `timeout`（自動測試用）。
    pub fn wait(&mut self, timeout: Duration) -> Option<PlayerEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            if let Some(ev) = self.mpv.wait_event(remaining.as_secs_f64())
                && let Some(pe) = self.handle(ev)
            {
                return Some(pe);
            }
        }
    }

    /// 等到符合條件的事件。檔案播放失敗或逾時都回傳 Err（自動測試用）。
    pub fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&PlayerEvent) -> bool,
    ) -> Result<PlayerEvent, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(ev) = self.wait(remaining) else {
                return Err(format!("等待逾時（{:.1} 秒）", timeout.as_secs_f64()));
            };
            if pred(&ev) {
                return Ok(ev);
            }
            if let PlayerEvent::EndFile { error: Some(e), .. } = &ev {
                return Err(e.clone());
            }
        }
    }

    /// 一直處理事件，直到狀態符合條件（屬性變化是非同步送達的，自動測試用）。
    pub fn wait_state(&mut self, timeout: Duration, mut pred: impl FnMut(&State) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while !pred(&self.state) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("等待狀態逾時（{:.1} 秒）", timeout.as_secs_f64()));
            }
            if let Some(ev) = self.mpv.wait_event(remaining.as_secs_f64().min(0.1))
                && let Some(PlayerEvent::EndFile { error: Some(e), .. }) = self.handle(ev)
            {
                return Err(e);
            }
        }
        Ok(())
    }

    fn handle(&mut self, ev: Event) -> Option<PlayerEvent> {
        match ev {
            Event::PropertyChange { name, value, .. } => {
                self.apply_property(&name, value);
                None
            }
            Event::Log { prefix, level, text } => {
                if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                    eprint!("[mpv/{level}] [{prefix}] {text}");
                }
                if matches!(level.as_str(), "error" | "fatal") {
                    if self.recent_errors.len() >= 8 {
                        self.recent_errors.remove(0);
                    }
                    self.recent_errors.push(format!("[{prefix}] {}", text.trim_end()));
                    if let Some(base) = &self.failure {
                        self.state.last_error = Some(self.compose_failure(base));
                    }
                }
                None
            }
            Event::StartFile => {
                self.state.loading = true;
                self.state.loaded = false;
                self.state.last_error = None;
                self.failure = None;
                self.recent_errors.clear();
                Some(PlayerEvent::StartFile)
            }
            Event::FileLoaded => {
                self.state.loading = false;
                self.state.loaded = true;
                if self.external_subs {
                    self.load_external_subs();
                }
                if self.auto_select_subs {
                    self.choose_subtitle();
                }
                self.refresh_tracks();
                Some(PlayerEvent::FileLoaded)
            }
            Event::EndFile { reason, error } => {
                self.state.loading = false;
                self.state.loaded = false;
                let error = error.map(|e| {
                    let base = failure_reason(e.code);
                    let full = self.compose_failure(base);
                    self.failure = Some(base.to_owned());
                    full
                });
                if error.is_some() {
                    self.state.last_error = error.clone();
                }
                Some(PlayerEvent::EndFile { reason, error })
            }
            Event::PlaybackRestart => {
                self.refresh_tracks();
                Some(PlayerEvent::PlaybackRestart)
            }
            Event::VideoReconfig => {
                // 直接讀當下的值：屬性通知是非同步的，換檔時可能還是上一部影片的尺寸
                self.state.video_size = self
                    .mpv
                    .get_string("video-out-params")
                    .ok()
                    .and_then(|j| display_size(&j));
                Some(PlayerEvent::VideoReconfig)
            }
            Event::Seek => Some(PlayerEvent::Seek),
            Event::Shutdown => Some(PlayerEvent::Shutdown),
            _ => None,
        }
    }

    fn apply_property(&mut self, name: &str, value: Value) {
        let s = &mut self.state;
        match name {
            "time-pos" => s.time_pos = value.as_f64().unwrap_or(0.0),
            "duration" => s.duration = value.as_f64(),
            "pause" => s.paused = value.as_bool().unwrap_or(false),
            "volume" => s.volume = value.as_f64().unwrap_or(s.volume),
            "mute" => s.muted = value.as_bool().unwrap_or(false),
            "track-list" => {
                s.tracks = value
                    .as_str()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
            }
            "media-title" => s.title = value.as_str().map(str::to_owned),
            "path" => s.path = value.as_str().map(str::to_owned),
            "eof-reached" => s.eof = value.as_bool().unwrap_or(false),
            "seekable" => s.seekable = value.as_bool().unwrap_or(false),
            "hwdec-current" => s.hwdec = value.as_str().map(str::to_owned),
            "video-out-params" => s.video_size = value.as_str().and_then(display_size),
            _ => {}
        }
    }

    /// mpv 只會自動選「語言符合 slang」或「標記為預設」的字幕軌，
    /// 很多 MKV 的字幕軌兩者都沒有，結果有字幕卻不顯示。
    /// 這裡比照 PotPlayer：有字幕軌但沒選上時，選第一條非強制字幕。
    /// 直接讀當下的軌道清單。開檔時 mpv 會先通知一份「還沒選軌」的清單，
    /// 選好軌的版本可能晚於 FileLoaded / PlaybackRestart 才送達，介面和測試都可能讀到舊的
    fn refresh_tracks(&mut self) {
        if let Ok(json) = self.mpv.get_string("track-list")
            && let Ok(tracks) = serde_json::from_str(&json)
        {
            self.state.tracks = tracks;
        }
    }

    /// 開檔後選字幕：
    /// - 有繁中就選繁中（其次中文、簡中），依語言碼、標題和外掛字幕的內容判斷
    /// - 沒有中文字幕時尊重 mpv 的選擇；mpv 什麼都沒選時（很多 MKV 的字幕軌沒有語言標籤）選第一條，比照 PotPlayer
    fn choose_subtitle(&self) {
        let Ok(json) = self.mpv.get_string("track-list") else {
            return;
        };
        let tracks: Vec<Track> = serde_json::from_str(&json).unwrap_or_default();
        let subs: Vec<&Track> = tracks.iter().filter(|t| t.kind == TrackKind::Sub).collect();
        if subs.is_empty() {
            return;
        }
        let lang_of = |t: &Track| {
            let label = format!(
                "{} {}",
                t.lang.as_deref().unwrap_or(""),
                t.title.as_deref().unwrap_or("")
            );
            subs::classify_label(&label).unwrap_or(SubLang::Unknown)
        };
        // 語言最優先；同語言時外掛優先（字幕組另外附的通常比內嵌的好）；再來是軌道順序
        let best = subs
            .iter()
            .filter(|t| !t.forced)
            .max_by_key(|t| (lang_of(t), t.external, std::cmp::Reverse(t.id)))
            .or(subs.first());
        let current = subs.iter().find(|t| t.selected);
        let pick = match (current, best) {
            (None, Some(b)) => Some(b),
            (Some(c), Some(b)) if lang_of(b).is_chinese() && lang_of(b) > lang_of(c) => Some(b),
            _ => None,
        };
        if let Some(t) = pick {
            let _ = self.select_track(TrackKind::Sub, Some(t.id));
        }
    }

    /// 基本原因加上 mpv 記錄裡的細節，整理成給使用者看的說明。
    fn compose_failure(&self, base: &str) -> String {
        // 常見情況直接翻成中文；其他的附上 mpv 的原始訊息，方便回報問題
        let all = self.recent_errors.join("\n");
        let known = [
            ("No such file", "找不到檔案"),
            ("Failed to open", "檔案不存在，或沒有讀取權限"),
            ("Permission denied", "沒有讀取權限"),
            ("Failed to recognize file format", "無法辨識的檔案格式"),
            ("No video or audio streams", "檔案裡沒有可播放的影像或聲音"),
        ];
        if let Some((_, zh)) = known.iter().find(|(en, _)| all.contains(en)) {
            return format!("{base}：{zh}");
        }
        match self.recent_errors.last() {
            Some(detail) => format!("{base}（{detail}）"),
            None => base.to_owned(),
        }
    }
}

/// 從 `video-out-params`（JSON）算出實際顯示尺寸：
/// dw/dh 已套用像素比例；如果旋轉交給 GPU 處理（rotate 是 90 或 270），還要交換寬高。
/// 用濾鏡旋轉時（例如 headless），濾鏡之後的 rotate 已經是 0、寬高也已交換。
fn display_size(json: &str) -> Option<[i64; 2]> {
    #[derive(Deserialize)]
    struct OutParams {
        dw: i64,
        dh: i64,
        #[serde(default)]
        rotate: i64,
    }
    let p: OutParams = serde_json::from_str(json).ok()?;
    if p.dw <= 0 || p.dh <= 0 {
        return None;
    }
    Some(if p.rotate.rem_euclid(180) == 90 {
        [p.dh, p.dw]
    } else {
        [p.dw, p.dh]
    })
}

/// mpv 錯誤碼 → 中文
fn failure_reason(code: i32) -> &'static str {
    use libmpv2_sys as sys;
    match code {
        sys::mpv_error_MPV_ERROR_LOADING_FAILED => "無法載入檔案",
        sys::mpv_error_MPV_ERROR_UNKNOWN_FORMAT => "無法辨識的檔案格式",
        sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY => "檔案裡沒有可播放的影像或聲音",
        sys::mpv_error_MPV_ERROR_AO_INIT_FAILED => "無法開啟音訊裝置",
        sys::mpv_error_MPV_ERROR_VO_INIT_FAILED => "無法初始化影像輸出",
        sys::mpv_error_MPV_ERROR_UNSUPPORTED => "不支援的格式",
        _ => "播放失敗",
    }
}

#[cfg(test)]
mod tests {
    use super::display_size;

    #[test]
    fn display_size_accounts_for_gpu_rotation() {
        // 濾鏡已經轉好（headless）：照原樣
        assert_eq!(
            display_size(r#"{"w":240,"h":320,"dw":240,"dh":320,"rotate":0}"#),
            Some([240, 320])
        );
        // GPU 負責旋轉（GUI）：寬高交換
        assert_eq!(
            display_size(r#"{"w":320,"h":240,"dw":320,"dh":240,"rotate":90}"#),
            Some([240, 320])
        );
        assert_eq!(display_size(r#"{"dw":320,"dh":240,"rotate":270}"#), Some([240, 320]));
        assert_eq!(display_size(r#"{"dw":320,"dh":240,"rotate":180}"#), Some([320, 240]));
        // 沒有影像
        assert_eq!(display_size("null"), None);
        assert_eq!(display_size(r#"{"dw":0,"dh":0}"#), None);
    }
}
