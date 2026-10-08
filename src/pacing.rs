//! 流暢播放（依螢幕更新率同步影像）：要不要用、用哪個更新率的決定，以及播放中的防呆。
//! 這裡都是純函式（不碰 mpv、不碰視窗），介面那邊的接線在 `app/pacing.rs`。
//!
//! 流暢播放 = mpv 的 `video-sync=display-resample` + `display-fps-override=<螢幕的精確更新率>`：
//! 依螢幕更新率微調播放速度，每格固定顯示相同次數的更新（24p 在 120 Hz 上每格 5 次），不會忽快忽慢。

use crate::mpv::render::FrameInfo;
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

/// `VITASCOPE_PACING=off|block|no-drain`（可以用逗號隔開好幾個）：出問題時回到以前的做法
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    /// 不用流暢播放，也不動 mpv 的同步設定
    pub off: bool,
    /// 畫面輸出照以前一樣等 mpv（BLOCK_FOR_TARGET_TIME）
    pub block: bool,
    /// 視窗縮到最小時不排空影格
    pub no_drain: bool,
}

pub fn parse_overrides(env: Option<&str>) -> Overrides {
    let mut o = Overrides::default();
    for token in env.unwrap_or_default().split(',') {
        match token.trim().to_ascii_lowercase().as_str() {
            "off" => o.off = true,
            "block" => o.block = true,
            "no-drain" => o.no_drain = true,
            _ => {}
        }
    }
    o
}

// ───────────── 取影格的時機（畫面輸出不卡住介面） ─────────────

/// 比影格該顯示的時間早這麼多取影格（留給繪製與 swap）
pub const LEAD_NS: i64 = 2_000_000;

/// `target_raw` 看起來是微秒（離目前的微秒時間比較近）。libmpv 0.37 起都是 `mp_time_ns` 的奈秒
/// （render.h 的註解寫微秒，已經過時），呼叫 `mpv_get_time_ns` 就代表是 0.37 以上，照理不會發生
pub fn target_is_us(raw: i64, now_ns: i64, now_us: i64) -> bool {
    // 0 以下 = 沒有指定時間；abs_diff 不會溢位（mpv 給了奇怪的值也不會 panic）
    raw > 0 && raw.abs_diff(now_us) < raw.abs_diff(now_ns)
}

/// 影格該顯示的時間（奈秒）。只在除錯版檢查單位：能呼叫 `mpv_get_time_ns` 的 libmpv 一定是奈秒，
/// 執行時換算的分支永遠用不到
pub fn target_ns(raw: i64, now_ns: i64, now_us: i64) -> i64 {
    debug_assert!(
        !target_is_us(raw, now_ns, now_us),
        "target_time 看起來是微秒：{raw}（現在 {now_ns} ns）"
    );
    raw
}

/// 這一輪要不要先別取影格、等一下再來：回傳要等多久。
/// 顯示同步中（block_vsync）、重繪、沒有指定時間、已經遲了、或等待時間不合理（超過 100 ms）都馬上畫
pub fn defer(i: &FrameInfo, target_ns: i64, now_ns: i64) -> Option<Duration> {
    if !i.present || i.redraw || i.block_vsync || target_ns <= 0 {
        return None;
    }
    let wait = target_ns - LEAD_NS - now_ns;
    (wait > 0 && wait < 100_000_000).then(|| Duration::from_nanos(wait as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                no_drain: true
            }
        );
    }

    #[test]
    fn target_time_units() {
        let (now_ns, now_us) = (5_000_000_000_000i64, 5_000_000_000i64);
        // 奈秒（libmpv 0.37 起）：不變
        let ns = now_ns + 40_000_000;
        assert!(!target_is_us(ns, now_ns, now_us));
        assert_eq!(target_ns(ns, now_ns, now_us), ns);
        assert_eq!(target_ns(0, now_ns, now_us), 0);
        // 微秒（0.37 以前）：認得出來（除錯版的 target_ns 會報錯）
        let us = now_us + 40_000;
        assert!(target_is_us(us, now_ns, now_us));
        // 沒有時間、奇怪的值：不算微秒，也不會溢位
        assert!(!target_is_us(-1, now_ns, now_us));
        assert!(!target_is_us(i64::MIN, now_ns, now_us));
        assert_eq!(target_ns(i64::MIN, now_ns, now_us), i64::MIN);
        assert!(!target_is_us(i64::MAX, now_ns, now_us));
        assert!(!target_is_us(i64::MAX, i64::MAX, i64::MIN));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "微秒")]
    fn target_in_microseconds_is_a_bug() {
        let (now_ns, now_us) = (5_000_000_000_000i64, 5_000_000_000i64);
        target_ns(now_us + 40_000, now_ns, now_us);
    }

    #[test]
    fn defer_waits_until_just_before_the_target() {
        let now = 1_000_000_000_000i64;
        let frame = FrameInfo {
            present: true,
            target_raw: 0,
            ..Default::default()
        };
        let wait = defer(&frame, now + 40_000_000, now).unwrap();
        assert_eq!(wait, Duration::from_millis(38));
        // 顯示同步、重繪、沒有影格、沒有時間：馬上畫
        let sync = FrameInfo {
            block_vsync: true,
            ..frame
        };
        assert_eq!(defer(&sync, now + 40_000_000, now), None);
        let redraw = FrameInfo { redraw: true, ..frame };
        assert_eq!(defer(&redraw, now + 40_000_000, now), None);
        let nothing = FrameInfo {
            present: false,
            ..frame
        };
        assert_eq!(defer(&nothing, now + 40_000_000, now), None);
        assert_eq!(defer(&frame, 0, now), None);
        // 已經遲了，或剩不到 2 ms
        assert_eq!(defer(&frame, now - 1, now), None);
        assert_eq!(defer(&frame, now + 1_500_000, now), None);
        // 超過 100 ms：不合理，馬上畫
        assert_eq!(defer(&frame, now + 150_000_000, now), None);
    }
}
