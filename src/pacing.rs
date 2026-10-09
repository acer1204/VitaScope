//! 流暢播放（依螢幕更新率同步影像）：要不要用、用哪個更新率的決定，以及播放中的防呆。
//! 這裡都是純函式（不碰 mpv、不碰視窗），介面那邊的接線在 `app/pacing.rs`。
//!
//! 流暢播放 = mpv 的 `video-sync=display-resample` + `display-fps-override=<螢幕的精確更新率>`：
//! 依螢幕更新率微調播放速度，每格固定顯示相同次數的更新（24p 在 120 Hz 上每格 5 次），不會忽快忽慢。

use crate::mpv::render::{FrameInfo, RenderOpts};
use crate::power::PowerSource;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 流暢播放的設定。介面上是兩個勾選：
/// 「流暢播放」沒勾 = Off；「使用電池時暫停」勾 = Auto、沒勾 = Always
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmoothMode {
    /// 開，使用電池時暫停
    Auto,
    /// 一直開
    Always,
    /// 關（一般播放）。先預設關，在實際的螢幕上量過之後再改成 Auto
    #[default]
    Off,
}

/// `VITASCOPE_DEBUG=pacing`（可以跟其他值用逗號隔開）：記錄更新率、電源、決定的變化
pub fn debug() -> bool {
    std::env::var("VITASCOPE_DEBUG").is_ok_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("pacing")))
}

/// 選單、提示上的更新率：119.88、120、59.94（最多三位小數，去掉多餘的 0）
pub fn fmt_hz(hz: f64) -> String {
    let s = format!("{hz:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

// ───────────── 決定 ─────────────

/// 不用流暢播放（改用一般的音訊同步）的原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// 設定關掉了
    Setting,
    /// 沒有影像（純音訊、專輯封面）
    NoVideo,
    /// 使用電池（設定是 Auto）
    Battery,
    /// 軟體繪圖（gpu-dumb-mode）：慢，跟不上每次更新都畫
    SoftwareRenderer,
    /// 遠端桌面：更新率是假的，swap 也不等垂直同步
    RemoteSession,
    /// mpv 不接受設定
    ApplyFailed,
    /// 量到 swap 沒有等垂直同步（G-SYNC/FreeSync 沒有上限、驅動關了垂直同步）
    NoVsync,
    /// 量到畫面更新跟不上螢幕（這個檔案）
    TooSlow,
    /// 視窗縮到最小 / 被遮住：eframe 不畫，沒有垂直同步可以對
    Hidden,
    /// 查不到螢幕的更新率
    NoRefresh,
    /// 介面卡住過（對話框、拖曳視窗、系統卡頓）：暫時改用一般播放，讓 mpv 丟掉晚了的影格追上聲音，
    /// 追上之後馬上回到依螢幕同步（見 [`StallWatch`]、[`resync_done`]）
    Resync,
}

/// 要怎麼設定 mpv
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Plan {
    /// 不管（使用者用 VITASCOPE_MPV_OPTS 自己指定了，或 VITASCOPE_PACING=off）
    Untouched,
    /// 一般播放（跟以前一樣：`video-sync=audio`）
    Audio(Reason),
    /// 依螢幕同步。`vdrop`：音訊直通時不能調整聲音的速度，改用略過或重複影格
    Display { hz: f64, vdrop: bool },
}

impl Plan {
    pub fn is_display(&self) -> bool {
        matches!(self, Plan::Display { .. })
    }

    /// 這個做法要設定的 mpv 選項，依設定的順序：
    /// 開啟時先設更新率再換同步方式（不會有一瞬間用錯的更新率同步）；關閉時先換回音訊同步
    pub fn props(&self) -> Vec<(&'static str, String)> {
        match self {
            Plan::Untouched => Vec::new(),
            Plan::Audio(_) => vec![
                ("video-sync", "audio".to_owned()),
                ("display-fps-override", "0".to_owned()),
            ],
            Plan::Display { hz, vdrop } => vec![
                ("display-fps-override", format!("{hz:.6}")),
                (
                    "video-sync",
                    if *vdrop { "display-vdrop" } else { "display-resample" }.to_owned(),
                ),
            ],
        }
    }
}

/// 從 `from`（目前套用的做法；None = 不知道）換到 `to` 要依序設定的選項（只列有變的）
pub fn transition(from: Option<&Plan>, to: &Plan) -> Vec<(&'static str, String)> {
    let before = match from {
        Some(p @ (Plan::Audio(_) | Plan::Display { .. })) => p.props(),
        _ => Vec::new(),
    };
    to.props()
        .into_iter()
        .filter(|(name, value)| !before.iter().any(|(n, v)| n == name && v == value))
        .collect()
}

/// 防呆量到的結果（見 [`Guard`]）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuardState {
    /// swap 不等垂直同步：這次執行（在這個螢幕、這個更新率上）都不用
    pub no_vsync: bool,
    /// 第幾個檔案畫面更新跟不上（只有那個檔案不用）
    pub too_slow: Option<u64>,
    /// swap 只跟得上一半的更新率（Windows 的動態更新率、部分 VRR）：改用一半的更新率
    pub half_rate: bool,
}

/// 決定要不要用流暢播放的所有條件
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inputs {
    pub mode: SmoothMode,
    pub power: PowerSource,
    /// 視窗所在螢幕的更新率
    pub refresh: Option<f64>,
    /// 軟體繪圖（gpu-dumb-mode）
    pub dumb: bool,
    /// 使用者自己指定了 video-sync / display-fps-override，或 VITASCOPE_PACING=off
    pub env_override: bool,
    /// 有沒有影像；None = 還在開檔或沒有檔案（沿用上一個決定的「有沒有影像」）
    pub has_video: Option<bool>,
    pub visible: bool,
    /// 音訊直通中
    pub passthrough: bool,
    /// 遠端桌面連線中
    pub remote: bool,
    pub guard: GuardState,
    /// 目前是第幾個檔案（跟 `guard.too_slow` 比）
    pub file_gen: u64,
    /// mpv 不接受設定（這次執行都不再試）
    pub apply_failed: bool,
}

/// 依條件決定做法；排在前面的條件優先。`previous` 是上一次的決定（開檔中沿用它「有沒有影像」）
pub fn decide(i: &Inputs, previous: Option<&Plan>) -> Plan {
    if i.env_override {
        return Plan::Untouched;
    }
    if i.mode == SmoothMode::Off {
        return Plan::Audio(Reason::Setting);
    }
    // 開檔中（或沒有檔案）還不知道有沒有影像：沿用上一個決定「有沒有影像」的部分，
    // 其他條件（電源、螢幕、更新率、視窗看不看得到）照目前的算，不然沒開檔時換螢幕、接上電源都不會反映。
    // 之前沒有決定、或之前的決定跟影像無關（設定關、不管）：當成有影像
    let has_video = i
        .has_video
        .unwrap_or(!matches!(previous, Some(Plan::Audio(Reason::NoVideo))));
    if !has_video {
        return Plan::Audio(Reason::NoVideo);
    }
    let reason = if i.mode == SmoothMode::Auto && i.power == PowerSource::Battery {
        Reason::Battery
    } else if i.dumb {
        Reason::SoftwareRenderer
    } else if i.remote {
        Reason::RemoteSession
    } else if i.apply_failed {
        Reason::ApplyFailed
    } else if i.guard.no_vsync {
        Reason::NoVsync
    } else if i.guard.too_slow == Some(i.file_gen) {
        Reason::TooSlow
    } else if !i.visible {
        Reason::Hidden
    } else if let Some(hz) = i.refresh {
        let hz = if i.guard.half_rate { hz / 2.0 } else { hz };
        return Plan::Display {
            hz,
            vdrop: i.passthrough,
        };
    } else {
        Reason::NoRefresh
    };
    Plan::Audio(reason)
}

// ───────────── 防呆：量 swap 的間隔 ─────────────

/// 用最近幾次的間隔判斷
pub const GUARD_SAMPLES: usize = 60;
/// 重新開始量之後（開始播放、跳轉、拖曳視窗…）先等這麼久，避開一開始的不穩定
const SETTLE: Duration = Duration::from_secs(1);
/// 超過這個間隔的不算（拖曳視窗、除錯器停住之類的卡頓，不是更新率的問題）
const MAX_DT: f64 = 0.1;
/// 間隔中位數小於更新週期的這個比例：swap 沒有等垂直同步
const NO_VSYNC_RATIO: f64 = 0.6;
const NO_VSYNC_FOR: Duration = Duration::from_secs(1);
/// 間隔中位數大於更新週期的這個比例：畫面更新跟不上。要持續 5 秒才算，
/// 實測過的短暫卡頓（3 秒內每格 14 ms）不會誤判
const TOO_SLOW_RATIO: f64 = 1.5;
const TOO_SLOW_FOR: Duration = Duration::from_secs(5);
/// 間隔中位數在兩倍更新週期 ±5% 內：swap 只跟得上一半的更新率
const HALF_TOLERANCE: f64 = 0.05;
const HALF_FOR: Duration = Duration::from_secs(2);

/// 防呆量到新的結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    NoVsync,
    TooSlow,
    HalfRate,
}

/// 依螢幕同步時量每一幀之間的間隔，發現 swap 沒有真的跟螢幕同步就改回一般播放。
/// 結果跟著「哪個螢幕 + 更新率」：換螢幕或改更新率就重新判斷
#[derive(Debug, Default)]
pub struct Guard {
    /// 目前的螢幕與更新率（mHz）
    key: Option<(Option<u64>, u64)>,
    state: GuardState,
    file_gen: u64,
    dts: VecDeque<f64>,
    /// 上一次取樣的時間與幀編號
    last: Option<(Instant, u64)>,
    /// 這個時間之前不取樣
    settle_until: Option<Instant>,
    low_since: Option<Instant>,
    high_since: Option<Instant>,
    half_since: Option<Instant>,
}

impl Guard {
    pub fn state(&self) -> GuardState {
        self.state
    }

    /// 目前量的是第幾個檔案（「跟不上」記在這個檔案上）
    pub fn file_gen(&self) -> u64 {
        self.file_gen
    }

    /// 視窗所在的螢幕與更新率；變了的話之前的結果都不算（回傳 true）
    pub fn set_display(&mut self, monitor: Option<u64>, hz: Option<f64>) -> bool {
        let key = (monitor, hz.map_or(0, |hz| (hz * 1000.0).round() as u64));
        if self.key == Some(key) {
            return false;
        }
        let first = self.key.is_none();
        self.key = Some(key);
        self.state = GuardState::default();
        self.reset();
        !first
    }

    /// 開了新的檔案
    pub fn start_file(&mut self, file_gen: u64) {
        self.file_gen = file_gen;
        self.reset();
    }

    /// 重新開始量（暫停、跳轉、視窗看不到…）；已經量到的結果保留
    pub fn reset(&mut self) {
        self.dts.clear();
        self.last = None;
        self.settle_until = None;
        self.low_since = None;
        self.high_since = None;
        self.half_since = None;
    }

    /// 視窗正在移動或改大小：Windows 拖曳視窗時整個介面會停住，那段時間不量，停下來 1 秒後再開始
    pub fn window_moved(&mut self, now: Instant) {
        self.reset();
        self.settle_until = Some(now + SETTLE);
    }

    /// 使用者改了設定：之前量到的結果都不算
    pub fn clear_verdicts(&mut self) {
        self.state = GuardState::default();
        self.reset();
    }

    /// 畫了一幀（`frame` 是 egui 的幀編號，同一幀呼叫好幾次只算一次）；`period` 是目前同步的更新週期（秒）。
    /// 有新的結果時回傳
    pub fn sample(&mut self, now: Instant, frame: u64, period: f64) -> Option<Verdict> {
        if self.last.is_some_and(|(_, f)| f == frame) {
            return None;
        }
        let previous = self.last.replace((now, frame));
        let settle = *self.settle_until.get_or_insert(now + SETTLE);
        let (t0, _) = previous?;
        if now < settle {
            return None;
        }
        let dt = now.saturating_duration_since(t0).as_secs_f64();
        if dt > MAX_DT || !(period.is_finite() && period > 0.0) {
            return None;
        }
        if self.dts.len() == GUARD_SAMPLES {
            self.dts.pop_front();
        }
        self.dts.push_back(dt);
        if self.dts.len() < GUARD_SAMPLES {
            return None;
        }
        let median = median(&self.dts);
        let half = !self.state.half_rate && (median - 2.0 * period).abs() <= HALF_TOLERANCE * 2.0 * period;
        let low = median < NO_VSYNC_RATIO * period;
        let high = !half && median > TOO_SLOW_RATIO * period;
        let since = |flag: bool, t: &mut Option<Instant>| {
            if flag {
                now.saturating_duration_since(*t.get_or_insert(now))
            } else {
                *t = None;
                Duration::ZERO
            }
        };
        let low_for = since(low, &mut self.low_since);
        let half_for = since(half, &mut self.half_since);
        let high_for = since(high, &mut self.high_since);
        if low && low_for >= NO_VSYNC_FOR && !self.state.no_vsync {
            self.state.no_vsync = true;
            return Some(Verdict::NoVsync);
        }
        if half && half_for >= HALF_FOR {
            self.state.half_rate = true;
            // 之後用一半的更新率同步，週期變了，重新量
            self.reset();
            return Some(Verdict::HalfRate);
        }
        if high && high_for >= TOO_SLOW_FOR && self.state.too_slow != Some(self.file_gen) {
            self.state.too_slow = Some(self.file_gen);
            return Some(Verdict::TooSlow);
        }
        None
    }
}

// ───────────── 介面卡住之後重新同步 ─────────────

/// 依螢幕同步、播放中，介面兩輪之間隔了這麼久就算卡住過。
/// 依螢幕同步時介面每次螢幕更新（120 Hz 是 8 ms，最慢 24 Hz 也只有 42 ms）就跑一輪。
/// 實測（120 Hz、24p）停 0.1、0.2 秒時 mpv 排好的影格還夠，影像跟聲音完全沒差；停 0.3 秒就差到約 190 ms。
/// 防呆不量的 0.1 秒（`MAX_DT`）以上的停頓還很常見（拖曳視窗、顯示卡驅動重設），不必每次都看
pub const STALL: Duration = Duration::from_millis(300);
/// 卡住之後先等這麼久再看 avsync：mpv 依螢幕同步時差 20 ms 以上就會略過影格追上，
/// 實測（這版 mpv、120 Hz）停 0.3～10 秒之後，1080p 約 0.15 秒、4K 軟體解碼 1 秒內就追上，剩下 15 ms 左右再慢慢修正
pub const STALL_GRACE: Duration = Duration::from_millis(500);
/// 等完之後 avsync（秒）還超過這個：mpv 沒有自己追上，改用一般播放追（字幕跟著影像，差 50 ms 以上看得出來）
pub const STALL_LIMIT: f64 = 0.050;
/// 重新同步最少維持這麼久：剛送出 `video-sync=audio` 時 mpv 還沒換過去，avsync 還是舊的
pub const RESYNC_MIN: Duration = Duration::from_millis(300);
/// 重新同步最多維持這麼久（追不上也回到依螢幕同步，剩下的交給 mpv 自己修正）
pub const RESYNC_MAX: Duration = Duration::from_millis(1500);
/// 重新同步時 avsync 小於這個（秒）就算追上了：mpv 依螢幕同步時差 20 ms 以上才會略過、重複影格
pub const RESYNC_OK: f64 = 0.020;

/// 看介面有沒有卡住：記下上一輪（依螢幕同步、播放中、看得到時）的時間
#[derive(Debug, Default)]
pub struct StallWatch {
    last: Option<Instant>,
}

impl StallWatch {
    /// 每一輪呼叫；`watching`：依螢幕同步已經套用、mpv 在同步、播放中、看得到（跟防呆量的條件一樣）。
    /// 這一輪跟上一輪都在看、中間隔了 [`STALL`] 以上：回傳隔了多久。
    /// 不在看的時候（暫停、縮到最小、一般播放）不算：那時介面本來就不會每次更新都跑
    pub fn tick(&mut self, now: Instant, watching: bool) -> Option<Duration> {
        if !watching {
            self.last = None;
            return None;
        }
        let gap = now.saturating_duration_since(self.last.replace(now)?);
        (gap >= STALL).then_some(gap)
    }
}

/// 卡住之後的下一步（見 [`after_stall`]）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterStall {
    /// 還在等 mpv 自己追
    Wait,
    /// mpv 自己追上了（或沒有聲音，沒有要對齊的）
    Recovered,
    /// 還沒追上：暫時改用一般播放（`Reason::Resync`）
    Resync,
}

/// 卡住 `elapsed` 之後怎麼辦：等 [`STALL_GRACE`]，那時 avsync（秒，讀不到是 None）還超過 [`STALL_LIMIT`]
/// 就重新同步。`always`（`VITASCOPE_PACING=resync`）：不等 mpv，一律重新同步
pub fn after_stall(elapsed: Duration, avsync: Option<f64>, always: bool) -> AfterStall {
    if always {
        AfterStall::Resync
    } else if elapsed < STALL_GRACE {
        AfterStall::Wait
    } else if avsync.is_none_or(|a| a.abs() < STALL_LIMIT) {
        AfterStall::Recovered
    } else {
        AfterStall::Resync
    }
}

/// 重新同步開始 `elapsed` 之後可以回到依螢幕同步了嗎：維持了 [`RESYNC_MIN`] 而且 avsync（秒，讀不到是 None）
/// 小於 [`RESYNC_OK`]，或已經 [`RESYNC_MAX`]
pub fn resync_done(elapsed: Duration, avsync: Option<f64>) -> bool {
    elapsed >= RESYNC_MAX || (elapsed >= RESYNC_MIN && avsync.is_some_and(|a| a.abs() < RESYNC_OK))
}

fn median(values: &VecDeque<f64>) -> f64 {
    let mut v: Vec<f64> = values.iter().copied().collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

// ───────────── 環境變數 VITASCOPE_PACING ─────────────

/// `VITASCOPE_PACING=off|block|no-drain|resync`（可以用逗號隔開好幾個）：出問題時回到以前的做法
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    /// 不用流暢播放，也不動 mpv 的同步設定
    pub off: bool,
    /// 畫面輸出照以前一樣等 mpv（BLOCK_FOR_TARGET_TIME）
    pub block: bool,
    /// 視窗縮到最小時不排空影格
    pub no_drain: bool,
    /// 依螢幕同步時介面卡住過，不等 mpv 自己追上，一律暫時改用一般播放（實機測試這條路用）
    pub resync: bool,
}

impl Overrides {
    /// 讀環境變數 VITASCOPE_PACING（啟動時讀一次；自動測試直接給 `Launch.pacing`，不看環境變數）
    pub fn from_env() -> Self {
        parse_overrides(std::env::var("VITASCOPE_PACING").ok().as_deref())
    }
}

pub fn parse_overrides(env: Option<&str>) -> Overrides {
    let mut o = Overrides::default();
    for token in env.unwrap_or_default().split(',') {
        match token.trim().to_ascii_lowercase().as_str() {
            "off" => o.off = true,
            "block" => o.block = true,
            "no-drain" => o.no_drain = true,
            "resync" => o.resync = true,
            _ => {}
        }
    }
    o
}

// ───────────── 取影格的時機（畫面輸出不卡住介面） ─────────────

/// 影格該顯示的時間（奈秒）。libmpv 0.37 起 `target_time` 都是 `mp_time_ns` 的奈秒（render.h 的註解寫微秒，已經過時）；
/// `Mpv::new` 已經要求 client API 2.2 以上（0.37），所以不換算。
/// 不在執行時猜單位：mpv 的時鐘從程式啟動算起，剛啟動時奈秒的數值還很小，跟微秒分不出來
/// （CI 實際碰過：剛開檔時 0.19 秒前的影格被誤判成微秒）
pub fn target_ns(raw: i64, _now_ns: i64, _now_us: i64) -> i64 {
    raw
}

/// 新影格離預定時間還有多久（奈秒）。顯示同步中（block_vsync）、重繪、沒有指定時間、
/// 已經到了或遲了、等待時間不合理（超過 100 ms）都是 None：馬上畫，不等
pub fn frame_wait(i: &FrameInfo, target_ns: i64, now_ns: i64) -> Option<i64> {
    if !i.present || i.redraw || i.block_vsync || target_ns <= 0 {
        return None;
    }
    let wait = target_ns - now_ns;
    (wait > 0 && wait < 100_000_000).then_some(wait)
}

/// 不知道螢幕更新率時當成 60 Hz
pub const DEFAULT_PERIOD_NS: i64 = 16_666_667;

/// 讓 mpv 等的時間上限（螢幕更新一次的時間，奈秒；0 以下 = 不知道，當成 60 Hz）。
/// 至少 4 ms：更新率很高（250 Hz 以上）時一次更新比計時器的誤差（約 1～2 ms）還短，
/// 叫醒的那一輪常常會落在範圍外；最多 50 ms（偵測到奇怪的數字時也不會等太久）
pub fn block_window(period_ns: i64) -> i64 {
    let p = if period_ns > 0 { period_ns } else { DEFAULT_PERIOD_NS };
    p.clamp(4_000_000, 50_000_000)
}

/// 範圍外再多容許這麼多：介面一直在重畫（滑鼠移動、動畫）時每輪隔一次更新，
/// 前一輪剛好在範圍外的話這一輪離預定時間至少還有這麼多，夠 mpv 畫完（render 本身約 1 ms），
/// 不會畫完已經過了預定時間；每輪開始到畫影片的時間差一點（約 1 ms）也還在範圍裡
pub const BLOCK_MARGIN_NS: i64 = 2_000_000;

/// 計時器叫醒的那一輪實際畫影片的時間比要求的晚多少：winit 的計時器晚 1～2 ms，
/// 加上一輪開始到畫影片要先跑介面（除錯版要好幾毫秒）。量出來，下次提早這麼多叫醒。
/// 介面一直在重畫（滑鼠移動、動畫）時每次垂直同步都有一輪，要求的時間之後的那一輪不是計時器叫醒的，
/// 晚多少只是跟垂直同步差多少：不算
#[derive(Debug, Clone, Copy, Default)]
pub struct WakeLate {
    /// 平均晚多少（奈秒）
    est: i64,
    /// 要求這個時間畫影片（mpv 時鐘的奈秒），還沒等到
    asked: Option<i64>,
    /// 上一輪畫影片的時間
    last: Option<i64>,
}

impl WakeLate {
    /// 晚超過這麼多的是意外（視窗拖動之類），不算進平均
    pub const OUTLIER: i64 = 10_000_000;

    /// 要求在 `at` 畫影片
    pub fn asked(&mut self, at: i64) {
        self.asked = Some(at);
    }

    /// 等的影格在要求的時間之前就取了：之後那一輪不是為了它叫醒的，不算
    pub fn cancel(&mut self) {
        self.asked = None;
    }

    /// 這一輪在 `now` 畫影片（`window`：一次螢幕更新）。比要求的時間早的是別的原因（滑鼠、mpv 的事件）叫醒的，繼續等；
    /// 上一輪在一次半更新之內的是介面一直在重畫
    pub fn painted(&mut self, now: i64, window: i64) {
        let prev = self.last.replace(now);
        let Some(at) = self.asked else { return };
        if now < at {
            return;
        }
        self.asked = None;
        if prev.is_some_and(|p| now - p < window + window / 2) {
            return;
        }
        let late = now - at;
        if late <= Self::OUTLIER {
            // 指數平均（1/8）：抓得到趨勢，偶爾一次特別晚不會影響太多
            self.est += (late - self.est) / 8;
        }
    }

    /// 平均晚多少（奈秒）
    pub fn estimate(&self) -> i64 {
        self.est.clamp(0, Self::OUTLIER)
    }
}

/// GPU 來不及在預定時間畫完影格時要提早多少取：mpv 在 render 裡先把影格交給 GPU（貼圖上傳、著色器）再等到預定時間，
/// 以前（發現新影格就取）GPU 有 40 ms 可以畫，現在取的時候離預定時間只剩幾毫秒（介面一直重畫時最少 2 ms）。
/// 實測介面一直重畫又播 4K 10-bit（軟體解碼）時，GPU 畫完的時間中位數、最晚的 5% 從預定時間後 0.7、1.8 ms
/// 變成 2.9、8.8 ms，常常晚一次垂直同步顯示；最多提早 4 ms 時是 1.5、5.0 ms，最多 10 ms 時是 1.6、2.8 ms。
/// 量法：render 回來之後的 GL timestamp（見 `video.rs` 的 `GpuTimer`），只看得到比預定時間晚多少（`over`；
/// render 回來時已經到了預定時間，GPU 早就畫完的話量到的也是那時候）：晚了就照晚的量慢慢提早，準時了再慢慢退回來。
/// GPU 一直滿載時提早也沒用（會一直提早到 `CAP`）：最多多等 `CAP`，還是比以前每格等 40 ms 短
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuLate {
    /// 要提早多少取影格（奈秒）
    lead: i64,
}

impl GpuLate {
    /// 預定時間後這麼久之內畫完算準時（以前的做法實測中位數約 0～0.8 ms、最晚的 5% 約 1～1.9 ms）
    pub const ON_TIME: i64 = 1_500_000;
    /// 最多提早這麼多（每格最多再多等這麼久）
    pub const CAP: i64 = 10_000_000;
    /// 超過這麼多的是意外（卡住、拖動視窗），不算
    const OUTLIER: i64 = 100_000_000;

    /// 一格影格的 GPU 比預定時間晚 `over`（奈秒；負的 = 早）畫完
    pub fn record(&mut self, over: i64) {
        if over > Self::ON_TIME && over < Self::OUTLIER {
            // 晚多少提早四分之一：幾格之內追上，偶爾一格特別晚不會一下子提早太多
            self.lead = (self.lead + (over - Self::ON_TIME) / 4).min(Self::CAP);
        } else if over <= Self::ON_TIME {
            // 準時：慢慢退回來（44 格少一半，24 fps 約 2 秒）
            self.lead -= self.lead / 64;
        }
    }

    /// 要提早多少取影格（奈秒）
    pub fn lead(&self) -> i64 {
        self.lead
    }
}

/// GPU 的時鐘（GL 的 timestamp）換成 mpv 時鐘（量 GPU 什麼時候畫完影格用，見 `video.rs` 的 `GpuTimer`）：
/// 每次在 `before`、`after`（mpv 時鐘）之間問到 GPU 的時間 `gpu`，
/// 時間差就在 `[before - gpu, after - gpu]` 之間。留最近幾次裡範圍最窄的一次（兩個時鐘的速度差很少，
/// 幾秒內不用管）
#[derive(Debug, Clone, Default)]
pub struct ClockSync {
    /// (範圍的寬度, 時間差)
    samples: Ring<(i64, i64)>,
}

impl ClockSync {
    pub fn sample(&mut self, before: i64, gpu: i64, after: i64) {
        if after >= before {
            self.samples.push((after - before, before + (after - before) / 2 - gpu));
        }
    }

    /// GPU 的時間加上這個就是 mpv 時鐘；還沒量過是 None
    pub fn offset(&self) -> Option<i64> {
        self.samples
            .values()
            .iter()
            .min_by_key(|(width, _)| *width)
            .map(|(_, off)| *off)
    }
}

/// 延後取影格時要跟 egui 要求多久之後重畫：egui 會從每個 `request_repaint_after` 扣掉 `predicted_dt`
/// （預計一輪要花的時間，eframe 沒設定，固定是 1/60 秒），不加回去的話會早 17 ms 醒來，
/// 接著一輪接一輪地空轉到時間（每輪還要等一次垂直同步）
pub fn repaint_after(wait: Duration, predicted_dt: f32) -> Duration {
    wait + Duration::try_from_secs_f32(predicted_dt).unwrap_or_default()
}

/// 看到新影格時這一輪怎麼做
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Take {
    /// 現在取；`block` = render 等到影格的預定時間才回來
    Now { block: bool },
    /// 這一輪先不取，`wake` 之後再來一輪（還沒加回 egui 扣掉的 predicted_dt，見 `repaint_after`）
    Later { wake: Duration },
}

/// 取影格的時機要用的量（奈秒）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Lead {
    /// 讓 render 等的範圍：一次螢幕更新（見 `block_window`）
    pub window: i64,
    /// 計時器叫醒的那一輪平均晚多少（見 `WakeLate`）
    pub late: i64,
    /// GPU 來不及在預定時間畫完時要提早多少（見 `GpuLate`）
    pub gpu: i64,
}

/// 看到新影格（預定時間 `due`，mpv 時鐘的奈秒）時要現在取還是等一下。
/// 做法：等到離預定時間不到一次螢幕更新（`lead.window`）才取，取的時候讓 render 等到預定時間。
/// 影格交出的時間跟以前（每格都讓 render 等，`VITASCOPE_PACING=block`）完全一樣，都是 mpv 到了預定時間才放行，
/// 跟這一輪是計時器叫醒的還是滑鼠之類叫醒的無關；介面的執行緒每格最多等一次更新多一點
/// （`BLOCK_MARGIN_NS`，GPU 來不及時再加上 `lead.gpu`）。
/// - `pace` false（`VITASCOPE_PACING=block`）或問不到 `info`：照以前讓 render 等（不然影像會比聲音早）。
/// - 視窗大小變了：馬上重畫（render 一定會取走新影格），不等。
/// - 顯示同步、重繪、沒有預定時間、已經遲了、時間不合理：馬上畫，不等（見 `frame_wait`）。
/// - 離預定時間超過範圍（加上 `BLOCK_MARGIN_NS`）：先不取，在大約 `due - window / 2` 再來一輪
///   （扣掉計時器那一輪平均晚多少 `lead.late`），落在範圍中間，早一點晚一點都還在範圍裡。
/// - GPU 來不及：整個範圍提早 `lead.gpu`，當成影格早這麼多到期
pub fn take(pace: bool, resized: bool, info: Option<&FrameInfo>, due: i64, now: i64, lead: Lead) -> Take {
    let Some(info) = info.filter(|_| pace) else {
        return Take::Now { block: true };
    };
    if resized {
        return Take::Now { block: false };
    }
    let Some(wait) = frame_wait(info, due, now) else {
        return Take::Now { block: false };
    };
    let wait = wait - lead.gpu.clamp(0, GpuLate::CAP);
    if wait <= lead.window + BLOCK_MARGIN_NS {
        return Take::Now { block: true };
    }
    // 估計得太大時叫醒得太早，那一輪會再延後一次（多一輪而已）；估計得太小會在預定時間之後才醒，
    // 影格晚一次垂直同步。更新率很高時半次更新比計時器晚的量還短，所以不用範圍限制
    let aim = wait - lead.window / 2 - lead.late.clamp(0, WakeLate::OUTLIER);
    Take::Later {
        wake: Duration::from_nanos(aim.max(0) as u64),
    }
}

// ───────────── 畫面輸出的統計 ─────────────

/// VITASCOPE_DEBUG=pacing：一段時間內每格新影格交出、顯示的時間（微秒，跟預定時間比；負的 = 早）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Presents {
    /// render 回來（影格交出去、接著 swap）的時間
    pub done: Vec<i32>,
    /// 估計的顯示時間：swap 之後的下一次垂直同步（只有 Windows 量得到）
    pub late: Vec<i32>,
    /// 相鄰兩格隔了幾次螢幕更新（同上）
    pub gaps: Vec<u32>,
    /// 決定取影格時離預定時間還有多久（正的 = 還沒到；`VITASCOPE_PACING=block` 是發現新影格就取）
    pub ahead: Vec<i32>,
    /// GPU 做完影格（mpv 畫的部分）的時間（GL 的 timestamp query；不支援的話沒有）
    pub gpu: Vec<i32>,
}

/// 累計每格新影格交出、顯示的時間（`video.rs` 的 VITASCOPE_DEBUG=pacing 紀錄用）
#[derive(Debug, Clone, Default)]
pub struct PresentLog {
    /// 上一格在第幾次垂直同步顯示
    last: Option<i64>,
    p: Presents,
}

impl PresentLog {
    /// 最多留這麼多筆（沒人拿的話不會一直長大）
    pub const MAX: usize = 4096;

    /// 剛畫好一格新影格：`now` 是 render 回來的時間，`due` 是預定時間（mpv 時鐘的奈秒；0 = 沒有指定），
    /// `vsync` 是現在之後的第一次垂直同步：(第幾次, 離現在幾奈秒)，None = 量不到
    pub fn record(&mut self, now: i64, due: i64, vsync: Option<(i64, i64)>) {
        if due > 0 && self.p.done.len() < Self::MAX {
            self.p.done.push(us(now - due));
        }
        let Some((n, until_ns)) = vsync else { return };
        if let Some(prev) = self.last.replace(n) {
            let gap = n - prev;
            // 隔太久是暫停、拖動，不算
            if (1..=30).contains(&gap) && self.p.gaps.len() < Self::MAX {
                self.p.gaps.push(gap as u32);
            }
        }
        if due > 0 && self.p.late.len() < Self::MAX {
            self.p.late.push(us(now + until_ns - due));
        }
    }

    /// 決定取一格新影格（有預定時間的）時，離預定時間還有 `ahead_ns`
    pub fn taken(&mut self, ahead_ns: i64) {
        if self.p.ahead.len() < Self::MAX {
            self.p.ahead.push(us(ahead_ns));
        }
    }

    /// 一格影格的 GPU 工作在預定時間之後 `over_ns` 做完（負的 = 之前）
    pub fn gpu_done(&mut self, over_ns: i64) {
        if self.p.gpu.len() < Self::MAX {
            self.p.gpu.push(us(over_ns));
        }
    }

    /// 拿走這一段的紀錄；下一段的第一格不跟這一段的最後一格比（垂直同步的時間表會重新要）
    pub fn take(&mut self) -> Presents {
        self.last = None;
        std::mem::take(&mut self.p)
    }
}

/// 奈秒換成微秒（存成 i32，超出範圍的截掉）
fn us(ns: i64) -> i32 {
    (ns / 1000).clamp(i32::MIN.into(), i32::MAX.into()) as i32
}

/// 第 `pct` 百分位（四捨五入到最近的一筆）；沒有資料就是 None
pub fn percentile(values: &[i32], pct: u32) -> Option<i32> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    let i = ((v.len() - 1) * pct.min(100) as usize + 50) / 100;
    Some(*v.select_nth_unstable(i).1)
}

/// 垂直同步的時間表（`anchor` 是某一次的時間，每 `period` 一次）上，`now` 之後的第一次：
/// (從 `anchor` 數第幾次, 時間)。剛好在垂直同步上算下一次（swap 趕不上這一次）
pub fn next_vsync(anchor: i64, period: i64, now: i64) -> (i64, i64) {
    let n = (now - anchor).div_euclid(period) + 1;
    (n, anchor + n * period)
}

/// 統計最近幾次 render、幾格新影格
pub const STATS_RING: usize = 128;

/// 畫面輸出的統計（媒體資訊面板「播放流暢度」、VITASCOPE_DEBUG=pacing）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RenderStats {
    /// mpv 畫了幾次（新影格、視窗大小改變的重畫都算）
    pub renders: u64,
    /// 新影格還沒到時間、這一輪先不取的次數
    pub deferred: u64,
    /// 只取走不畫的次數（SKIP_RENDERING；目前不用，一直是 0）
    pub skipped: u64,
    /// render 等到影格預定時間的次數（`RenderOpts.block`）：`VITASCOPE_PACING=block` 時每次都等（約 40 ms）；
    /// 自己挑時間時離預定時間不到一次螢幕更新才取、讓 render 等剩下的時間，一般播放每格一次
    pub blocking: u64,
    /// 這些 render 總共花了多少時間（微秒，含畫的時間）：除以 `blocking` 是平均等多久
    pub blocking_us: u64,
    /// 最近 128 次等待的 render 最久花了多久（微秒）
    pub blocking_max_us: u32,
    /// 畫面重繪（影片的 paint callback）次數
    pub passes: u64,
    /// 新影格的數量
    pub frames: u64,
    /// 最近 128 格新影格：發現之後到畫出來（含畫的那一輪）平均每格重繪了幾次（見 `RenderCounter::rendered`）。
    /// 只算延後的話是 1；mpv 的事件（播放位置）在發現之後才到的話多看一輪，一般播放 1.0～1.3。
    /// egui 太早叫醒時會一輪接一輪地空轉到時間（每次垂直同步一輪），介面一直重畫時也是每次更新一輪：約 4
    pub passes_per_frame: f64,
    /// 最近 128 次 render 花的時間（微秒）：中位數、最大
    pub p50_us: u32,
    pub max_us: u32,
    /// 目前讓 render 等的上限（微秒，螢幕更新一次的時間，見 `block_window`；`VideoView::stats` 填）
    pub window_us: u32,
    /// 計時器叫醒的那一輪平均晚多少畫影片（微秒，見 `WakeLate`；`VideoView::stats` 填）
    pub wake_late_us: u32,
    /// GPU 來不及時提早多少取影格（微秒，見 `GpuLate`；`VideoView::stats` 填）
    pub gpu_lead_us: u32,
}

/// 固定大小的環狀緩衝：只留最後 `STATS_RING` 筆
#[derive(Debug, Clone)]
struct Ring<T: Copy + Default> {
    items: [T; STATS_RING],
    len: usize,
    next: usize,
}

impl<T: Copy + Default> Default for Ring<T> {
    fn default() -> Self {
        Self {
            items: [T::default(); STATS_RING],
            len: 0,
            next: 0,
        }
    }
}

impl<T: Copy + Default> Ring<T> {
    fn push(&mut self, v: T) {
        self.items[self.next] = v;
        self.next = (self.next + 1) % STATS_RING;
        self.len = (self.len + 1).min(STATS_RING);
    }

    fn values(&self) -> &[T] {
        &self.items[..self.len]
    }
}

/// VITASCOPE_DEBUG=pacing 的一段時間裡最大的值（微秒）：環狀緩衝只留最近 128 次，一段（10 秒）不一定都在裡面
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SegmentMax {
    /// render 最久花了多久
    pub render_us: u32,
    /// 等待的 render 最久花了多久
    pub blocking_us: u32,
    /// GPU 來不及時最多提早多少取影格
    pub gpu_lead_us: u32,
}

/// 累計畫面輸出的統計（`VideoView` 在 paint callback 裡呼叫）
#[derive(Debug, Clone, Default)]
pub struct RenderCounter {
    totals: RenderStats,
    durations: Ring<u32>,
    /// 等待的 render 花的時間
    blocked: Ring<u32>,
    per_frame: Ring<u16>,
    /// 目前等著的新影格：已經延後幾次
    deferrals: u16,
    segment: SegmentMax,
}

impl RenderCounter {
    /// 一輪畫面重繪（影片的 paint callback）
    pub fn pass(&mut self) {
        self.totals.passes += 1;
    }

    /// 新影格還沒到時間，這一輪先不取
    pub fn defer(&mut self) {
        self.totals.deferred += 1;
        self.deferrals = self.deferrals.saturating_add(1);
    }

    /// 用 `o` 呼叫了一次 mpv 的 render，花了 `took`；`new_frame`：取走的是新影格（不是視窗大小改變的重畫）
    pub fn rendered(&mut self, took: Duration, new_frame: bool, o: RenderOpts) {
        self.totals.renders += 1;
        if o.skip {
            self.totals.skipped += 1;
        }
        let us = took.as_micros().min(u32::MAX as u128) as u32;
        if o.block {
            self.totals.blocking += 1;
            self.totals.blocking_us += u64::from(us);
            self.blocked.push(us);
            self.segment.blocking_us = self.segment.blocking_us.max(us);
        }
        self.durations.push(us);
        self.segment.render_us = self.segment.render_us.max(us);
        if new_frame {
            self.totals.frames += 1;
            // 第一次看到新影格的那一輪（一般播放時一定是先延後）是發現它，不算；
            // 之後每一輪（包括畫出來的那一輪）都是在等它。顯示同步時一看到就畫，算 1 次
            self.per_frame.push(self.deferrals.max(1));
            self.deferrals = 0;
        }
    }

    /// 取影格時 GPU 來不及要提早 `ns`（奈秒）
    pub fn gpu_lead(&mut self, ns: i64) {
        let us = (ns / 1000).clamp(0, u32::MAX.into()) as u32;
        self.segment.gpu_lead_us = self.segment.gpu_lead_us.max(us);
    }

    /// 上次拿了之後的最大值，拿了就重新算
    pub fn take_segment(&mut self) -> SegmentMax {
        std::mem::take(&mut self.segment)
    }

    pub fn stats(&self) -> RenderStats {
        let mut s = self.totals;
        let frames = self.per_frame.values();
        if !frames.is_empty() {
            s.passes_per_frame = frames.iter().map(|n| f64::from(*n)).sum::<f64>() / frames.len() as f64;
        }
        let mut d = self.durations.values().to_vec();
        if !d.is_empty() {
            let mid = d.len() / 2;
            s.p50_us = *d.select_nth_unstable(mid).1;
            s.max_us = d.iter().copied().max().unwrap_or(0);
        }
        s.blocking_max_us = self.blocked.values().iter().copied().max().unwrap_or(0);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_rate_labels() {
        assert_eq!(fmt_hz(119.88), "119.88");
        assert_eq!(fmt_hz(120.0), "120");
        assert_eq!(fmt_hz(120_000.0 / 1001.0), "119.88");
        assert_eq!(fmt_hz(60_000.0 / 1001.0), "59.94");
        assert_eq!(fmt_hz(143.856), "143.856");
        assert_eq!(fmt_hz(50.0), "50");
    }

    #[test]
    fn default_is_off_and_names_are_kebab_case() {
        assert_eq!(SmoothMode::default(), SmoothMode::Off);
        assert_eq!(serde_json::to_value(SmoothMode::Always).unwrap(), "always");
        assert_eq!(
            serde_json::from_str::<SmoothMode>(r#""auto""#).unwrap(),
            SmoothMode::Auto
        );
    }

    /// 什麼都符合（會用流暢播放）的條件
    fn ok() -> Inputs {
        Inputs {
            mode: SmoothMode::Auto,
            power: PowerSource::Ac,
            refresh: Some(119.88),
            dumb: false,
            env_override: false,
            has_video: Some(true),
            visible: true,
            passthrough: false,
            remote: false,
            guard: GuardState::default(),
            file_gen: 3,
            apply_failed: false,
        }
    }

    const DISPLAY: Plan = Plan::Display {
        hz: 119.88,
        vdrop: false,
    };

    #[test]
    fn decide_each_reason() {
        assert_eq!(decide(&ok(), None), DISPLAY);
        let cases: [(Inputs, Plan); 13] = [
            (
                Inputs {
                    env_override: true,
                    ..ok()
                },
                Plan::Untouched,
            ),
            (
                Inputs {
                    mode: SmoothMode::Off,
                    ..ok()
                },
                Plan::Audio(Reason::Setting),
            ),
            (
                Inputs {
                    has_video: Some(false),
                    ..ok()
                },
                Plan::Audio(Reason::NoVideo),
            ),
            (
                Inputs {
                    power: PowerSource::Battery,
                    ..ok()
                },
                Plan::Audio(Reason::Battery),
            ),
            (Inputs { dumb: true, ..ok() }, Plan::Audio(Reason::SoftwareRenderer)),
            (Inputs { remote: true, ..ok() }, Plan::Audio(Reason::RemoteSession)),
            (
                Inputs {
                    apply_failed: true,
                    ..ok()
                },
                Plan::Audio(Reason::ApplyFailed),
            ),
            (
                Inputs {
                    guard: GuardState {
                        no_vsync: true,
                        ..Default::default()
                    },
                    ..ok()
                },
                Plan::Audio(Reason::NoVsync),
            ),
            (
                Inputs {
                    guard: GuardState {
                        too_slow: Some(3),
                        ..Default::default()
                    },
                    ..ok()
                },
                Plan::Audio(Reason::TooSlow),
            ),
            (Inputs { visible: false, ..ok() }, Plan::Audio(Reason::Hidden)),
            (Inputs { refresh: None, ..ok() }, Plan::Audio(Reason::NoRefresh)),
            // 音訊直通：改用略過或重複影格
            (
                Inputs {
                    passthrough: true,
                    ..ok()
                },
                Plan::Display {
                    hz: 119.88,
                    vdrop: true,
                },
            ),
            // 只跟得上一半的更新率
            (
                Inputs {
                    guard: GuardState {
                        half_rate: true,
                        ..Default::default()
                    },
                    ..ok()
                },
                Plan::Display {
                    hz: 59.94,
                    vdrop: false,
                },
            ),
        ];
        for (inputs, plan) in cases {
            assert_eq!(decide(&inputs, None), plan, "{inputs:?}");
        }
    }

    #[test]
    fn decide_priority_order() {
        // 全部條件都不符合：排在前面的原因優先
        let all_bad = Inputs {
            mode: SmoothMode::Auto,
            power: PowerSource::Battery,
            refresh: None,
            dumb: true,
            env_override: true,
            has_video: Some(false),
            visible: false,
            passthrough: true,
            remote: true,
            guard: GuardState {
                no_vsync: true,
                too_slow: Some(3),
                half_rate: true,
            },
            file_gen: 3,
            apply_failed: true,
        };
        let order = [
            Plan::Untouched,
            Plan::Audio(Reason::Setting),
            Plan::Audio(Reason::NoVideo),
            Plan::Audio(Reason::Battery),
            Plan::Audio(Reason::SoftwareRenderer),
            Plan::Audio(Reason::RemoteSession),
            Plan::Audio(Reason::ApplyFailed),
            Plan::Audio(Reason::NoVsync),
            Plan::Audio(Reason::TooSlow),
            Plan::Audio(Reason::Hidden),
            Plan::Audio(Reason::NoRefresh),
        ];
        let mut i = Inputs {
            mode: SmoothMode::Off,
            ..all_bad
        };
        for (step, expected) in order.iter().enumerate() {
            assert_eq!(decide(&i, None), *expected, "第 {step} 步：{i:?}");
            // 拿掉這個原因，下一個原因就出現
            match step {
                0 => i.env_override = false,
                1 => i.mode = SmoothMode::Auto,
                2 => i.has_video = Some(true),
                3 => i.power = PowerSource::Ac,
                4 => i.dumb = false,
                5 => i.remote = false,
                6 => i.apply_failed = false,
                7 => i.guard.no_vsync = false,
                8 => i.guard.too_slow = None,
                9 => i.visible = true,
                _ => i.refresh = Some(60.0),
            }
        }
        assert_eq!(decide(&i, None), Plan::Display { hz: 30.0, vdrop: true });
    }

    #[test]
    fn decide_details() {
        // 一直開：用電池也開；不知道電源 = 當成接著電源
        let always = Inputs {
            mode: SmoothMode::Always,
            power: PowerSource::Battery,
            ..ok()
        };
        assert_eq!(decide(&always, None), DISPLAY);
        let unknown = Inputs {
            power: PowerSource::Unknown,
            ..ok()
        };
        assert_eq!(decide(&unknown, None), DISPLAY);
        // 更新跟不上只算那個檔案
        let other_file = Inputs {
            guard: GuardState {
                too_slow: Some(2),
                ..Default::default()
            },
            ..ok()
        };
        assert_eq!(decide(&other_file, None), DISPLAY);
    }

    #[test]
    fn loading_keeps_the_previous_plan() {
        let loading = Inputs {
            has_video: None,
            refresh: Some(60.0),
            ..ok()
        };
        // 之前在依螢幕同步：開下一個檔案的時候不要先切回去
        let same_screen = Inputs {
            refresh: Some(119.88),
            ..loading
        };
        assert_eq!(decide(&same_screen, Some(&DISPLAY)), DISPLAY);
        // 之前的檔案沒有影像：等開完檔才知道這個有沒有
        let audio_only = Plan::Audio(Reason::NoVideo);
        assert_eq!(decide(&loading, Some(&audio_only)), audio_only);
        // 只沿用「有沒有影像」，其他條件照目前的：換了螢幕（更新率）、接上電源、視窗縮小都要反映
        assert_eq!(
            decide(&loading, Some(&DISPLAY)),
            Plan::Display { hz: 60.0, vdrop: false }
        );
        assert_eq!(
            decide(&loading, Some(&Plan::Audio(Reason::Battery))),
            Plan::Display { hz: 60.0, vdrop: false }
        );
        assert_eq!(
            decide(
                &Inputs {
                    power: PowerSource::Battery,
                    ..loading
                },
                Some(&DISPLAY)
            ),
            Plan::Audio(Reason::Battery)
        );
        assert_eq!(
            decide(&loading, Some(&Plan::Audio(Reason::Hidden))),
            Plan::Display { hz: 60.0, vdrop: false }
        );
        assert_eq!(
            decide(
                &Inputs {
                    mode: SmoothMode::Always,
                    power: PowerSource::Battery,
                    ..loading
                },
                Some(&Plan::Audio(Reason::Battery))
            ),
            Plan::Display { hz: 60.0, vdrop: false }
        );
        // 第一次（沒有之前的決定）：當成有影像
        assert_eq!(decide(&loading, None), Plan::Display { hz: 60.0, vdrop: false });
        // 設定、環境變數不等開檔完
        assert_eq!(
            decide(
                &Inputs {
                    mode: SmoothMode::Off,
                    ..loading
                },
                Some(&DISPLAY)
            ),
            Plan::Audio(Reason::Setting)
        );
        assert_eq!(
            decide(
                &Inputs {
                    env_override: true,
                    ..loading
                },
                Some(&DISPLAY)
            ),
            Plan::Untouched
        );
        // 之前是因為設定關掉，現在打開了：不沿用
        assert_eq!(
            decide(&loading, Some(&Plan::Audio(Reason::Setting))),
            Plan::Display { hz: 60.0, vdrop: false }
        );
    }

    #[test]
    fn props_order() {
        assert_eq!(
            DISPLAY.props(),
            vec![
                ("display-fps-override", "119.880000".to_owned()),
                ("video-sync", "display-resample".to_owned()),
            ],
            "開啟：先設更新率"
        );
        assert_eq!(
            Plan::Display { hz: 120.0, vdrop: true }.props()[1],
            ("video-sync", "display-vdrop".to_owned())
        );
        assert_eq!(
            Plan::Audio(Reason::Battery).props(),
            vec![
                ("video-sync", "audio".to_owned()),
                ("display-fps-override", "0".to_owned()),
            ],
            "關閉：先換回音訊同步"
        );
        assert!(Plan::Untouched.props().is_empty());
    }

    #[test]
    fn transitions_send_only_changes() {
        let audio = Plan::Audio(Reason::Setting);
        // 開啟、關閉
        assert_eq!(transition(Some(&audio), &DISPLAY), DISPLAY.props());
        assert_eq!(transition(Some(&DISPLAY), &audio), audio.props());
        // 不知道目前的狀態：全部設
        assert_eq!(transition(None, &DISPLAY), DISPLAY.props());
        assert_eq!(transition(None, &audio), audio.props());
        // 原因不同，設定一樣：不用送
        assert!(transition(Some(&audio), &Plan::Audio(Reason::Battery)).is_empty());
        assert!(transition(Some(&DISPLAY), &DISPLAY).is_empty());
        // 換更新率：只改更新率
        assert_eq!(
            transition(Some(&DISPLAY), &Plan::Display { hz: 60.0, vdrop: false }),
            vec![("display-fps-override", "60.000000".to_owned())]
        );
        // 開始音訊直通：只改同步方式
        assert_eq!(
            transition(
                Some(&DISPLAY),
                &Plan::Display {
                    hz: 119.88,
                    vdrop: true
                }
            ),
            vec![("video-sync", "display-vdrop".to_owned())]
        );
        // 不管：什麼都不送
        assert!(transition(Some(&DISPLAY), &Plan::Untouched).is_empty());
        assert_eq!(transition(Some(&Plan::Untouched), &audio), audio.props());
    }

    // ── 防呆 ──

    const P120: f64 = 1.0 / 119.88;

    /// 模擬：從 `t` 開始每隔 `dt` 秒畫一幀，畫 `seconds` 秒；回傳量到的結果與它們出現的時間（離這段開始幾秒）
    struct Sim {
        guard: Guard,
        start: Instant,
        t: f64,
        frame: u64,
        period: f64,
    }

    impl Sim {
        fn new() -> Self {
            let mut guard = Guard::default();
            guard.set_display(Some(1), Some(119.88));
            guard.start_file(3);
            Self {
                guard,
                start: Instant::now(),
                t: 0.0,
                frame: 0,
                period: P120,
            }
        }

        fn now(&self) -> Instant {
            self.start + Duration::from_secs_f64(self.t)
        }

        fn run(&mut self, dt: f64, seconds: f64) -> Vec<(f64, Verdict)> {
            let begin = self.t;
            let mut out = Vec::new();
            while self.t - begin < seconds {
                self.t += dt;
                self.frame += 1;
                if let Some(v) = self.guard.sample(self.now(), self.frame, self.period) {
                    out.push((self.t - begin, v));
                }
            }
            out
        }
    }

    #[test]
    fn steady_display_rate_gives_nothing() {
        let mut s = Sim::new();
        assert_eq!(s.run(P120, 20.0), vec![]);
        assert_eq!(s.guard.state(), GuardState::default());
    }

    #[test]
    fn swaps_that_do_not_wait_mean_no_vsync() {
        let mut s = Sim::new();
        let v = s.run(0.001, 3.0);
        assert_eq!(v.len(), 1, "{v:?}");
        let (at, verdict) = v[0];
        assert_eq!(verdict, Verdict::NoVsync);
        // 先等 1 秒、湊滿 60 個間隔（0.06 秒），再持續 1 秒
        assert!((at - 2.06).abs() < 0.01, "{at}");
        assert!(s.guard.state().no_vsync);
        // 持續 1 秒才算：0.9 秒不算
        let mut s = Sim::new();
        assert_eq!(s.run(0.001, 1.95), vec![]);
    }

    #[test]
    fn short_slow_burst_is_not_too_slow() {
        // 實測（run F）：每格 14 ms 持續 3 秒，之後恢復
        let mut s = Sim::new();
        assert_eq!(s.run(P120, 3.0), vec![]);
        assert_eq!(s.run(0.014, 3.0), vec![]);
        assert_eq!(s.run(P120, 10.0), vec![]);
        assert_eq!(s.guard.state(), GuardState::default());
    }

    #[test]
    fn sustained_slow_frames_mean_too_slow_for_this_file() {
        let mut s = Sim::new();
        s.run(P120, 3.0);
        let v = s.run(0.020, 8.0);
        assert_eq!(v.len(), 1, "{v:?}");
        let (at, verdict) = v[0];
        assert_eq!(verdict, Verdict::TooSlow);
        // 中位數要過半的間隔變慢（31 × 20 ms）才算開始，再持續 5 秒
        assert!((5.5..6.0).contains(&at), "{at}");
        assert_eq!(s.guard.state().too_slow, Some(3));
        // 同一個檔案不會再報；下一個檔案重新判斷
        assert_eq!(s.run(0.020, 8.0), vec![]);
        s.guard.start_file(4);
        let v = s.run(0.020, 8.0);
        assert_eq!(v.iter().map(|(_, v)| *v).collect::<Vec<_>>(), [Verdict::TooSlow]);
        assert_eq!(s.guard.state().too_slow, Some(4));
    }

    #[test]
    fn half_rate_swaps_halve_the_plan() {
        let mut s = Sim::new();
        let v = s.run(2.0 * P120, 4.0);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].1, Verdict::HalfRate);
        // 1 秒 + 60 個間隔（1 秒）+ 2 秒
        assert!((v[0].0 - 4.0).abs() < 0.05, "{}", v[0].0);
        assert!(s.guard.state().half_rate);
        // 改用一半的更新率之後同樣的間隔就是正常的，不會再報「跟不上」
        s.period = 2.0 * P120;
        assert_eq!(s.run(2.0 * P120, 10.0), vec![]);
        // decide 用一半的更新率
        let i = Inputs {
            guard: s.guard.state(),
            ..ok()
        };
        assert_eq!(
            decide(&i, None),
            Plan::Display {
                hz: 59.94,
                vdrop: false
            }
        );
    }

    #[test]
    fn pause_resets_the_measurement() {
        let mut s = Sim::new();
        // 1.9 秒的「沒等垂直同步」（還差一點才算），暫停，再來 1.9 秒：都不算
        assert_eq!(s.run(0.001, 1.9), vec![]);
        s.guard.reset();
        s.run(0.5, 0.5); // 暫停中沒有畫面
        s.guard.reset();
        assert_eq!(s.run(0.001, 1.9), vec![]);
        // 繼續下去才算
        assert_eq!(s.run(0.001, 1.0).len(), 1);
    }

    #[test]
    fn same_frame_is_counted_once() {
        // 每 1 ms 呼叫一次，但幀編號每 8.3 ms 才變：間隔是 8.3 ms，不是 1 ms
        let mut s = Sim::new();
        let mut out = Vec::new();
        for ms in 1..=10_000u64 {
            let now = s.start + Duration::from_millis(ms);
            let frame = (ms as f64 / (P120 * 1000.0)) as u64;
            if let Some(v) = s.guard.sample(now, frame, P120) {
                out.push(v);
            }
        }
        assert_eq!(out, vec![]);
    }

    #[test]
    fn stall_watch_needs_two_watched_passes() {
        let t = Instant::now();
        let ms = |n: u64| t + Duration::from_millis(n);
        let mut w = StallWatch::default();
        // 第一輪只記時間；之後每次螢幕更新一輪，不算卡住
        assert_eq!(w.tick(ms(0), true), None);
        for n in (8..1_000).step_by(8) {
            assert_eq!(w.tick(ms(n), true), None, "{n}");
        }
        // 差一點點不到、剛好到
        assert_eq!(w.tick(ms(992 + 299), true), None);
        assert_eq!(w.tick(ms(1_291 + 300), true), Some(Duration::from_millis(300)));
        assert_eq!(w.tick(ms(1_600), true), None, "回報一次之後從這一輪重新算");
        assert_eq!(w.tick(ms(11_600), true), Some(Duration::from_secs(10)));
        // 不在看（暫停、縮到最小、一般播放）的時候不算，回來之後的第一輪也不算
        assert_eq!(w.tick(ms(11_608), false), None);
        assert_eq!(w.tick(ms(20_000), false), None);
        assert_eq!(w.tick(ms(30_000), true), None);
        assert_eq!(w.tick(ms(30_008), true), None);
        // 時間倒退（不應該發生）也不算
        assert_eq!(w.tick(ms(29_000), true), None);
    }

    #[test]
    fn after_a_stall_wait_for_mpv_then_decide() {
        let ms = Duration::from_millis;
        // 等的時間內不管 avsync 多少都先等（剛停完時讀到的 avsync 可能還是舊的）
        for avsync in [None, Some(0.0), Some(2.0), Some(-1.0)] {
            assert_eq!(after_stall(ms(0), avsync, false), AfterStall::Wait);
            assert_eq!(after_stall(ms(499), avsync, false), AfterStall::Wait);
        }
        // 等完：mpv 自己追上了（差不到 50 ms）
        assert_eq!(after_stall(ms(500), Some(0.0153), false), AfterStall::Recovered);
        assert_eq!(after_stall(ms(500), Some(-0.049), false), AfterStall::Recovered);
        // 讀不到（沒有聲音，沒有要對齊的）
        assert_eq!(after_stall(ms(500), None, false), AfterStall::Recovered);
        // 還沒追上
        assert_eq!(after_stall(ms(500), Some(0.050), false), AfterStall::Resync);
        assert_eq!(after_stall(ms(700), Some(-0.6), false), AfterStall::Resync);
        // VITASCOPE_PACING=resync：不等
        assert_eq!(after_stall(ms(0), Some(0.0), true), AfterStall::Resync);
    }

    #[test]
    fn resync_lasts_until_caught_up() {
        let ms = Duration::from_millis;
        // 剛換過去：avsync 還是舊的，再小也不算
        assert!(!resync_done(ms(0), Some(0.0)));
        assert!(!resync_done(ms(299), Some(0.001)));
        // 維持夠久、追上了
        assert!(resync_done(ms(300), Some(0.0)));
        assert!(resync_done(ms(300), Some(-0.019)));
        // 還沒追上、讀不到：繼續
        assert!(!resync_done(ms(300), Some(0.020)));
        assert!(!resync_done(ms(1_000), Some(-0.4)));
        assert!(!resync_done(ms(1_000), None));
        // 最多 1.5 秒
        assert!(resync_done(ms(1_500), Some(0.4)));
        assert!(resync_done(ms(1_500), None));
    }

    #[test]
    fn dragging_the_window_resets_and_long_stalls_are_ignored() {
        // Windows 拖曳視窗時介面停住：間隔很長、很亂。拖曳中一直有 window_moved，不量
        let mut s = Sim::new();
        for _ in 0..20 {
            assert_eq!(s.run(0.03, 0.5), vec![]);
            let now = s.now();
            s.guard.window_moved(now);
        }
        // 超過 100 ms 的間隔不算（卡住，不是更新率的問題）
        let mut s = Sim::new();
        assert_eq!(s.run(0.15, 20.0), vec![]);
    }

    #[test]
    fn monitor_or_refresh_change_clears_the_verdicts() {
        let mut s = Sim::new();
        s.run(0.001, 3.0);
        assert!(s.guard.state().no_vsync);
        // 同一個螢幕、同樣的更新率：保留
        assert!(!s.guard.set_display(Some(1), Some(119.88)));
        assert!(s.guard.state().no_vsync);
        // 換到另一個螢幕：重新判斷
        assert!(s.guard.set_display(Some(2), Some(119.88)));
        assert_eq!(s.guard.state(), GuardState::default());
        s.run(0.001, 3.0);
        // 改了更新率
        assert!(s.guard.set_display(Some(2), Some(60.0)));
        assert_eq!(s.guard.state(), GuardState::default());
        // 使用者改設定
        s.run(0.001, 3.0);
        s.guard.clear_verdicts();
        assert_eq!(s.guard.state(), GuardState::default());
    }

    #[test]
    fn pacing_env_overrides() {
        assert_eq!(parse_overrides(None), Overrides::default());
        assert_eq!(parse_overrides(Some("")), Overrides::default());
        assert_eq!(
            parse_overrides(Some("off")),
            Overrides {
                off: true,
                ..Default::default()
            }
        );
        assert_eq!(
            parse_overrides(Some(" Block , no-drain,whatever")),
            Overrides {
                off: false,
                block: true,
                no_drain: true,
                resync: false,
            }
        );
        assert_eq!(
            parse_overrides(Some("RESYNC")),
            Overrides {
                resync: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn target_time_units() {
        // 奈秒照原樣用，包括剛啟動時數值還很小的時候（以前除錯版在這裡誤判成微秒而中止）
        assert_eq!(target_ns(194_038_828, 393_790_596, 393_790), 194_038_828);
        let (now_ns, now_us) = (5_000_000_000_000i64, 5_000_000_000i64);
        assert_eq!(target_ns(now_ns + 40_000_000, now_ns, now_us), now_ns + 40_000_000);
        assert_eq!(target_ns(0, now_ns, now_us), 0);
        assert_eq!(target_ns(i64::MIN, now_ns, now_us), i64::MIN);
    }

    #[test]
    fn frame_wait_only_for_a_real_future_target() {
        let now = 1_000_000_000_000i64;
        let frame = FrameInfo {
            present: true,
            target_raw: 0,
            ..Default::default()
        };
        assert_eq!(frame_wait(&frame, now + 40_000_000, now), Some(40_000_000));
        assert_eq!(frame_wait(&frame, now + 1, now), Some(1));
        // 顯示同步、重繪、沒有影格、沒有時間：馬上畫
        let sync = FrameInfo {
            block_vsync: true,
            ..frame
        };
        assert_eq!(frame_wait(&sync, now + 40_000_000, now), None);
        let redraw = FrameInfo { redraw: true, ..frame };
        assert_eq!(frame_wait(&redraw, now + 40_000_000, now), None);
        let nothing = FrameInfo {
            present: false,
            ..frame
        };
        assert_eq!(frame_wait(&nothing, now + 40_000_000, now), None);
        assert_eq!(frame_wait(&frame, 0, now), None);
        // 已經到了或遲了
        assert_eq!(frame_wait(&frame, now, now), None);
        assert_eq!(frame_wait(&frame, now - 1, now), None);
        // 超過 100 ms：不合理，馬上畫
        assert_eq!(frame_wait(&frame, now + 100_000_000, now), None);
        assert_eq!(frame_wait(&frame, now + 99_999_999, now), Some(99_999_999));
    }

    #[test]
    fn block_window_is_one_refresh() {
        // 120 Hz、60 Hz、不知道（當 60 Hz）
        assert_eq!(block_window(8_333_333), 8_333_333);
        assert_eq!(block_window(16_683_350), 16_683_350);
        assert_eq!(block_window(0), DEFAULT_PERIOD_NS);
        assert_eq!(block_window(-5), DEFAULT_PERIOD_NS);
        // 360 Hz：至少 4 ms；奇怪的數字最多 50 ms
        assert_eq!(block_window(2_777_778), 4_000_000);
        assert_eq!(block_window(1_000_000_000), 50_000_000);
    }

    #[test]
    fn repaint_after_cancels_egui_predicted_dt() {
        // egui 會從要求的延遲扣掉 predicted_dt（context.rs 的 request_repaint_after）：扣完要剛好是要等的時間
        let egui = |d: Duration, pdt: f32| d.saturating_sub(Duration::from_secs_f32(pdt));
        let wait = Duration::from_millis(38);
        for pdt in [1.0 / 60.0, 1.0 / 120.0, 0.05] {
            let asked = repaint_after(wait, pdt);
            assert!(
                egui(asked, pdt).abs_diff(wait) < Duration::from_micros(1),
                "{pdt}: {asked:?}"
            );
        }
        // 沒加回去的話會早 16.7 ms 醒來
        assert!(egui(wait, 1.0 / 60.0) < Duration::from_millis(22));
        // 奇怪的 predicted_dt：照原本的時間
        assert_eq!(repaint_after(wait, 0.0), wait);
        assert_eq!(repaint_after(wait, -1.0), wait);
        assert_eq!(repaint_after(wait, f32::NAN), wait);
    }

    #[test]
    fn take_decides_when_to_render_a_new_frame() {
        let frame = FrameInfo {
            present: true,
            redraw: false,
            repeat: false,
            block_vsync: false,
            target_raw: 0,
        };
        let ms = 1_000_000i64;
        let us = 1_000i64;
        let now = 10_000 * ms;
        let p = block_window(8_333_333);
        let lead = |late: i64| Lead {
            window: p,
            late,
            gpu: 0,
        };
        let take_at = |due: i64, now: i64, late: i64| take(true, false, Some(&frame), due, now, lead(late));
        let later = |ns: i64| Take::Later {
            wake: Duration::from_nanos(ns as u64),
        };
        let block = Take::Now { block: true };
        let now_free = Take::Now { block: false };
        // 一般播放、發現新影格（約 40 ms 前）：先不取，在預定時間前半次更新再來
        let due = now + 40 * ms;
        assert_eq!(take_at(due, now, 0), later(40 * ms - p / 2));
        // 計時器那一輪平均晚 1.5 ms：提早這麼多叫醒
        assert_eq!(take_at(due, now, 1_500 * us), later(40 * ms - p / 2 - 1_500 * us));
        // 晚很多（除錯版的介面很重、更新率很高時半次更新很短）：照樣提早這麼多，最多 10 ms
        assert_eq!(take_at(due, now, 9 * ms), later(40 * ms - p / 2 - 9 * ms));
        assert_eq!(take_at(due, now, 15 * ms), later(40 * ms - p / 2 - WakeLate::OUTLIER));
        assert_eq!(take_at(due, now, -3 * ms), later(40 * ms - p / 2));
        // 估計得太大、叫醒時還在範圍外：再延後一次（最少 0，馬上再來一輪）
        assert_eq!(take_at(now + 12 * ms, now, 9 * ms), later(0));
        // 離預定時間不到一次更新（加上容許的量）：不管是哪一輪（計時器、滑鼠、mpv 的事件）都現在取，
        // 讓 render 等到預定時間（交出影格的時間跟 VITASCOPE_PACING=block 一樣）
        for wait in [p + BLOCK_MARGIN_NS, p, p / 2, ms, 1] {
            assert_eq!(take_at(now + wait, now, 0), block, "{wait}");
            assert_eq!(take_at(now + wait, now, 4 * ms), block, "{wait}");
        }
        // 剛好超過：再等一下，叫醒時落在範圍中間
        let wait = p + BLOCK_MARGIN_NS + 1;
        assert_eq!(take_at(now + wait, now, 0), later(wait - p / 2));
        // 已經到了、遲了：馬上畫，不等
        assert_eq!(take_at(now, now, 0), now_free);
        assert_eq!(take_at(now - 3 * ms, now, 0), now_free);
        // 時間不合理（超過 100 ms）：馬上畫，不等
        assert_eq!(take_at(now + 150 * ms, now, 0), now_free);
        // VITASCOPE_PACING=block：一律讓 render 等（以前的做法）
        assert_eq!(take(false, false, Some(&frame), due, now, lead(0)), block);
        assert_eq!(take(false, true, Some(&frame), due, now, lead(0)), block);
        // 問不到預定時間：照以前讓 mpv 等，不然影像會比聲音早
        assert_eq!(take(true, false, None, due, now, lead(0)), block);
        // 視窗大小變了：馬上畫，不等
        assert_eq!(take(true, true, Some(&frame), due, now, lead(0)), now_free);
        // 依螢幕同步：mpv 要我們馬上畫，swap 等垂直同步
        let sync = FrameInfo {
            block_vsync: true,
            ..frame
        };
        assert_eq!(take(true, false, Some(&sync), 0, now, lead(0)), now_free);
        // 重繪（暫停中改設定之類）：馬上畫
        let redraw = FrameInfo { redraw: true, ..frame };
        assert_eq!(take(true, false, Some(&redraw), due, now, lead(0)), now_free);
        // 60 Hz：等的上限跟著變長
        let p60 = block_window(16_666_667);
        let lead60 = Lead { window: p60, ..lead(0) };
        assert_eq!(take(true, false, Some(&frame), now + 15 * ms, now, lead60), block);
        assert_eq!(take_at(now + 15 * ms, now, 0), later(15 * ms - p / 2));
        // GPU 來不及：整個範圍提早，當成影格早 gpu 到期（離預定時間還有 12 ms 就取，等的時間也跟著變長）
        let gpu = |g: i64| Lead { gpu: g, ..lead(0) };
        assert_eq!(take(true, false, Some(&frame), now + 12 * ms, now, gpu(2 * ms)), block);
        assert_eq!(
            take(true, false, Some(&frame), due, now, gpu(2 * ms)),
            later(38 * ms - p / 2)
        );
        // 最多提早 GpuLate::CAP；已經過了預定時間的照樣不等
        assert_eq!(
            take(true, false, Some(&frame), due, now, gpu(GpuLate::CAP + 10 * ms)),
            later(40 * ms - GpuLate::CAP - p / 2)
        );
        assert_eq!(take(true, false, Some(&frame), now - ms, now, gpu(2 * ms)), now_free);
    }

    #[test]
    fn deferred_wake_lands_inside_the_window() {
        // 照 take 要求的時間叫醒（晚的量在估計附近）時，那一輪一定會取（不會再延後一次），
        // 等的時間不超過一次更新加上容許的量（GPU 來不及時再加上提早的量）。
        // 更新率很高時（4 ms）計時器晚的量可能超過半次更新，照樣要落在範圍裡
        let frame = FrameInfo {
            present: true,
            ..Default::default()
        };
        let ms = 1_000_000i64;
        for period in [4_000_000, 6_944_444, 8_333_333, 16_666_667, 33_366_700] {
            let p = block_window(period);
            for found in [3 * ms, 20 * ms, 41 * ms, 80 * ms] {
                let (now, due) = (1_000 * ms, 1_000 * ms + found);
                for (late, gpu) in [(0, 0), (ms, 0), (3 * ms, 0), (6 * ms, 0), (2 * ms, GpuLate::CAP)] {
                    let lead = Lead { window: p, late, gpu };
                    let Take::Later { wake } = take(true, false, Some(&frame), due, now, lead) else {
                        assert!(found <= p + BLOCK_MARGIN_NS + gpu, "{period} {found}");
                        continue;
                    };
                    let asked = now + wake.as_nanos() as i64;
                    // 實際晚的量（就是估計的量）比估計少半次更新到多四分之一次更新都還在範圍裡
                    for actual in [late - p / 2, late, late + p / 4] {
                        let woke = asked + actual.max(0);
                        assert_eq!(
                            take(true, false, Some(&frame), due, woke, lead),
                            Take::Now { block: true },
                            "{period} {found} {late} {actual}"
                        );
                        assert!(due - woke <= p + BLOCK_MARGIN_NS + gpu && due - woke > 0);
                    }
                }
            }
        }
    }

    #[test]
    fn present_log_counts_vsyncs_between_frames() {
        let ms = 1_000_000i64;
        let mut log = PresentLog::default();
        // 量不到垂直同步（不是 Windows）：只記交出的時間
        log.record(1_000 * ms, 1_001 * ms, None);
        let p = log.take();
        assert_eq!((p.done, p.late, p.gaps), (vec![-1_000], vec![], vec![]));
        // 每格隔 5 次更新；交出後 3 ms 的垂直同步顯示
        let mut t = 2_000 * ms;
        for n in [10, 15, 20, 26, 30] {
            log.record(t, t + ms, Some((n, 3 * ms)));
            t += 42 * ms;
        }
        // 暫停之後（隔 100 次）、往回的不算
        log.record(t, t + ms, Some((130, 3 * ms)));
        log.record(t, t + ms, Some((129, 3 * ms)));
        // 沒有預定時間的不記交出、顯示的時間，但更新次數照算
        log.record(t, 0, Some((134, 3 * ms)));
        let p = log.take();
        assert_eq!(p.gaps, vec![5, 5, 6, 4, 5]);
        assert_eq!(p.done, vec![-1_000; 7]);
        assert_eq!(p.late, vec![2_000; 7]);
        assert_eq!(p.ahead, Vec::<i32>::new());
        // 取影格時離預定時間多久（正的 = 還沒到）
        log.taken(4_200_000);
        log.taken(-300_000);
        assert_eq!(log.take().ahead, vec![4_200, -300]);
        // GPU 做完的時間
        log.gpu_done(1_200_000);
        log.gpu_done(-80_000);
        assert_eq!(log.take().gpu, vec![1_200, -80]);
        // 拿走之後重新開始：下一段的第一格不跟上一段的最後一格比
        log.record(t, t + ms, Some((139, 0)));
        assert_eq!(log.take().gaps, Vec::<u32>::new());
        // 最多留 MAX 筆
        for n in 0..PresentLog::MAX as i64 + 10 {
            log.record(t, t + ms, Some((n, 0)));
            log.taken(ms);
            log.gpu_done(ms);
        }
        let p = log.take();
        assert_eq!(
            (p.done.len(), p.late.len(), p.gaps.len(), p.ahead.len(), p.gpu.len()),
            (
                PresentLog::MAX,
                PresentLog::MAX,
                PresentLog::MAX,
                PresentLog::MAX,
                PresentLog::MAX
            )
        );
    }

    #[test]
    fn wake_late_learns_how_late_the_woken_pass_paints() {
        let p = 8_333_333;
        let mut o = WakeLate::default();
        assert_eq!(o.estimate(), 0);
        // 沒有要求過：不管
        o.painted(1_000, p);
        assert_eq!(o.estimate(), 0);
        let mut at = 1_000_000_000i64;
        for _ in 0..60 {
            // 發現新影格的那一輪要求叫醒
            o.painted(at - 36_000_000, p);
            o.asked(at);
            // 別的原因先畫的一輪（mpv 的事件）不算，繼續等
            o.painted(at - 20_000_000, p);
            o.painted(at + 2_600_000, p);
            at += 41_708_000;
        }
        let est = o.estimate();
        assert!((2_500_000..=2_600_000).contains(&est), "{est}");
        // 同一個要求只算一次
        o.painted(at + 9_000_000, p);
        assert_eq!(o.estimate(), est);
        // 新的要求取代舊的（延後之後又延後）
        o.asked(at);
        o.asked(at + 5_000_000);
        o.painted(at + 1_000_000, p);
        assert_eq!(o.estimate(), est, "比新的要求早");
        // 晚超過 10 ms 的是意外，不算進平均
        o.painted(at + 60_000_000, p);
        assert_eq!(o.estimate(), est);
        // 要求的時間之前就取了：之後的那一輪不算
        o.asked(at + 100_000_000);
        o.cancel();
        o.painted(at + 105_000_000, p);
        assert_eq!(o.estimate(), est);
        // 一直很晚：估計跟著變大，最多 10 ms
        at += 200_000_000;
        for _ in 0..200 {
            o.asked(at);
            o.painted(at + 9_500_000, p);
            at += 41_708_000;
        }
        assert!((9_000_000..=9_500_000).contains(&o.estimate()), "{}", o.estimate());
    }

    #[test]
    fn wake_late_ignores_passes_of_a_busy_interface() {
        // 介面一直在重畫：每次垂直同步一輪，要求的時間之後的那一輪晚多少只是跟垂直同步差多少（0～一次更新）
        let p = 8_333_333;
        let mut o = WakeLate::default();
        let mut t = 1_000_000_000i64;
        for n in 0..500i64 {
            if n % 5 == 0 {
                // 每 5 輪要求一次，要求的時間落在下一輪之前的不同位置
                o.asked(t + (n * 1_234_567) % p);
            }
            o.painted(t, p);
            t += p;
        }
        assert_eq!(o.estimate(), 0);
        // 60 Hz 也一樣
        let p60 = 16_666_667;
        for n in 0..500i64 {
            if n % 3 == 0 {
                o.asked(t + (n * 1_234_567) % p60);
            }
            o.painted(t, p60);
            t += p60;
        }
        assert_eq!(o.estimate(), 0);
        // 介面停下來之後（上一輪是很久以前）照常量
        t += 40_000_000;
        o.asked(t - 2_000_000);
        o.painted(t, p);
        assert_eq!(o.estimate(), 2_000_000 / 8);
    }

    #[test]
    fn gpu_late_takes_earlier_only_when_the_gpu_is_late() {
        let ms = 1_000_000i64;
        let mut g = GpuLate::default();
        // 準時畫完（render 回來時 GPU 也差不多畫完了）：不提早
        for _ in 0..100 {
            g.record(300_000);
            g.record(-200_000);
            g.record(GpuLate::ON_TIME);
        }
        assert_eq!(g.lead(), 0);
        // 晚 3.5 ms：提早晚的量（扣掉容許的 ON_TIME）的四分之一，晚幾格就追上
        g.record(GpuLate::ON_TIME + 2 * ms);
        assert_eq!(g.lead(), ms / 2);
        for _ in 0..3 {
            g.record(GpuLate::ON_TIME + 2 * ms);
        }
        assert_eq!(g.lead(), 2 * ms);
        // 準時了：慢慢退回來（44 格少一半）
        for _ in 0..44 {
            g.record(0);
        }
        assert!((800_000..=1_200_000).contains(&g.lead()), "{}", g.lead());
        // 偶爾一格晚一點（以前的做法也有）：只多一點，很快又退回來
        let before = g.lead();
        g.record(GpuLate::ON_TIME + 400_000);
        assert_eq!(g.lead(), before + 100_000);
        // 意外（卡住、拖動視窗）：不算，也不退
        let before = g.lead();
        g.record(150 * ms);
        assert_eq!(g.lead(), before);
        // 一直很晚（GPU 一直滿載，提早也沒用）：最多提早 CAP
        for _ in 0..100 {
            g.record(30 * ms);
        }
        assert_eq!(g.lead(), GpuLate::CAP);
    }

    #[test]
    fn clock_sync_uses_the_tightest_sample() {
        let mut c = ClockSync::default();
        assert_eq!(c.offset(), None);
        // GPU 時間 + 5000 = mpv 時鐘；問 GPU 的時間花了 400、40、1000
        c.sample(10_000, 5_200 - 5_000 + 5_000, 10_400);
        c.sample(20_000, 15_020, 20_040);
        c.sample(30_000, 25_100, 31_000);
        assert_eq!(c.offset(), Some(5_000));
        // 時間倒退（不會發生）不算
        c.sample(40_000, 1, 39_000);
        assert_eq!(c.offset(), Some(5_000));
        // 只看最近幾次（時鐘的速度差慢慢累積）
        for i in 0..STATS_RING as i64 {
            c.sample(
                100_000 + i * 10_000,
                100_000 + i * 10_000 - 7_000 + 50,
                100_000 + i * 10_000 + 100,
            );
        }
        assert_eq!(c.offset(), Some(7_000));
    }

    /// 不等、照常畫（自己挑時間取影格時的 render）
    const NOW: RenderOpts = RenderOpts {
        block: false,
        skip: false,
    };

    #[test]
    fn passes_per_frame_counts_the_passes_spent_waiting() {
        let ms = Duration::from_millis(1);
        // 依螢幕同步：一看到就畫
        let mut c = RenderCounter::default();
        for _ in 0..10 {
            c.pass();
            c.rendered(ms, true, NOW);
        }
        let s = c.stats();
        assert_eq!((s.renders, s.frames, s.passes, s.deferred), (10, 10, 10, 0));
        assert_eq!(s.passes_per_frame, 1.0);
        // 一般播放：發現時延後一次，到時間畫（發現的那一輪不算）
        let mut c = RenderCounter::default();
        for _ in 0..10 {
            c.pass();
            c.defer();
            c.pass();
            c.rendered(ms, true, NOW);
        }
        assert_eq!(c.stats().passes_per_frame, 1.0);
        // egui 太早叫醒：同一格延後 4 次
        for _ in 0..10 {
            for _ in 0..4 {
                c.pass();
                c.defer();
            }
            c.pass();
            c.rendered(ms, true, NOW);
        }
        let s = c.stats();
        assert_eq!(s.passes_per_frame, 2.5, "前 10 格 1 次、後 10 格 4 次");
        assert_eq!((s.frames, s.deferred, s.passes), (20, 50, 70));
        // 視窗大小改變的重畫不是新影格：不算一格，等著的那一格繼續累計
        c.defer();
        c.rendered(ms, false, NOW);
        c.defer();
        c.rendered(ms, true, NOW);
        let s = c.stats();
        assert_eq!((s.renders, s.frames), (22, 21));
        assert!(
            (s.passes_per_frame - 52.0 / 21.0).abs() < 1e-9,
            "{}",
            s.passes_per_frame
        );
        // 只看最近 128 格
        for _ in 0..STATS_RING {
            c.rendered(ms, true, NOW);
        }
        assert_eq!(c.stats().passes_per_frame, 1.0);
    }

    #[test]
    fn segment_max_since_the_last_take() {
        let mut c = RenderCounter::default();
        assert_eq!(c.take_segment(), SegmentMax::default());
        let block = RenderOpts {
            block: true,
            skip: false,
        };
        c.rendered(Duration::from_millis(40), true, block);
        c.gpu_lead(3_000_000);
        // 拿走之後重新算：之前的 40 ms 不算
        assert_eq!(
            c.take_segment(),
            SegmentMax {
                render_us: 40_000,
                blocking_us: 40_000,
                gpu_lead_us: 3_000
            }
        );
        c.rendered(Duration::from_millis(9), true, block);
        c.rendered(Duration::from_millis(12), false, NOW);
        // 環狀緩衝已經沒有的也算（一段比 128 次長）
        for _ in 0..STATS_RING {
            c.rendered(Duration::from_millis(1), true, block);
        }
        c.gpu_lead(-5);
        c.gpu_lead(1_500_000);
        c.gpu_lead(500_000);
        assert_eq!(c.stats().max_us, 1_000);
        assert_eq!(
            c.take_segment(),
            SegmentMax {
                render_us: 12_000,
                blocking_us: 9_000,
                gpu_lead_us: 1_500
            }
        );
    }

    #[test]
    fn render_time_percentiles() {
        let mut c = RenderCounter::default();
        assert_eq!(c.stats(), RenderStats::default());
        for us in 1..=100 {
            let o = RenderOpts {
                block: us % 4 == 0,
                skip: us % 10 == 0,
            };
            c.rendered(Duration::from_micros(us), true, o);
        }
        let s = c.stats();
        assert_eq!((s.p50_us, s.max_us, s.skipped, s.blocking), (51, 100, 10, 25));
        // 等待的 render：4、8、…、100 微秒，總共 1300、最久 100
        assert_eq!((s.blocking_us, s.blocking_max_us), (1_300, 100));
        // 只看最近 128 次：之前很慢的那次掉出去之後 max 跟著變
        c.rendered(Duration::from_millis(40), true, NOW);
        assert_eq!(c.stats().max_us, 40_000);
        for _ in 0..STATS_RING {
            c.rendered(Duration::from_micros(400), true, NOW);
        }
        let s = c.stats();
        assert_eq!((s.p50_us, s.max_us), (400, 400));
        assert_eq!(s.renders, 100 + 1 + STATS_RING as u64);
        // 不等的 render 不算進等待的時間
        assert_eq!((s.blocking, s.blocking_us, s.blocking_max_us), (25, 1_300, 100));
        // 等待的也只看最近 128 次的最大值；總共的時間一直累計
        let wait = RenderOpts {
            block: true,
            skip: false,
        };
        c.rendered(Duration::from_micros(8_400), true, wait);
        for _ in 0..STATS_RING {
            c.rendered(Duration::from_micros(4_000), true, wait);
        }
        let s = c.stats();
        assert_eq!(s.blocking, 25 + 1 + STATS_RING as u64);
        assert_eq!(s.blocking_us, 1_300 + 8_400 + 4_000 * STATS_RING as u64);
        assert_eq!(s.blocking_max_us, 4_000);
    }

    #[test]
    fn percentiles_pick_the_nearest_sample() {
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[5, 1, 3], 0), Some(1));
        assert_eq!(percentile(&[5, 1, 3], 50), Some(3));
        assert_eq!(percentile(&[5, 1, 3], 100), Some(5));
        assert_eq!(percentile(&[5, 1, 3], 1000), Some(5));
        let v: Vec<i32> = (1..=100).rev().collect();
        assert_eq!(percentile(&v, 5), Some(6));
        assert_eq!(percentile(&v, 95), Some(95));
        assert_eq!(percentile(&[-3, 7], 50), Some(7));
    }

    #[test]
    fn next_vsync_counts_from_the_anchor() {
        assert_eq!(next_vsync(1000, 100, 1000), (1, 1100), "剛好在垂直同步上：趕不上這一次");
        assert_eq!(next_vsync(1000, 100, 1001), (1, 1100));
        assert_eq!(next_vsync(1000, 100, 1099), (1, 1100));
        assert_eq!(next_vsync(1000, 100, 1100), (2, 1200));
        // 時間表的基準在現在之後也可以
        assert_eq!(next_vsync(1000, 100, 999), (0, 1000));
        assert_eq!(next_vsync(1000, 100, 850), (-1, 900));
    }
}
