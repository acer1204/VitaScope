//! 跳轉不準的格式（TS、M2TS、MTS、MPEG-PS／VOB）在主播放器裡的跳轉。
//!
//! 這些格式沒有索引：FFmpeg 用時間戳在檔案裡找位置，找到的封包不一定是關鍵影格，
//! 解碼要等到下一個關鍵影格才開始。mpv 的精確跳轉預設不往前多跳（`hr-seek-demuxer-offset` 是 0），
//! 所以精確跳轉常落在目標之後的關鍵影格（藍光、電視錄影晚 0.1–1 秒，跟關鍵影格跳轉一樣）；
//! 關鍵影格隔很久的檔案甚至讀到檔尾都沒有新的畫面（畫面停在跳轉前那一格，聲音照樣從目標開始播）。
//!
//! 主播放器（`Player`）的做法：
//! - 開檔時看 `file-format`（[`approximate_seeks`]，跟轉成 GIF、縮圖總覽圖用的一樣）：這種格式把
//!   `hr-seek-demuxer-offset` 設成 [`FIRST_OFFSET`]（file-local，換檔時 mpv 自己還原）：
//!   精確跳轉時分離器先往前多跳這麼多，再解碼到目標。
//! - 每次跳轉之後核對（[`Check`]、[`judge`]）：mpv 的記錄說了停在哪一格（[`Restart`]）、有沒有新的畫面。
//!   落在目標之後、或沒有新的畫面時，往前多跳的秒數加倍（[`retry_offset`]）再精確跳一次。
//!   真的因為加大才跳對的話（[`Landing::lead`]），這個檔案之後都用加大的秒數（關鍵影格的間隔通常整個檔案都差不多）；
//!   加大了也停在同一格的（影像本身在那裡缺了幾格），只算那一次，不會越加越大。
//! - mpv 自己做的精確跳轉（A-B 重播繞回 A、逐格往回、章節、播完從頭播）也核對：沒有在核對的跳轉時，
//!   從 mpv 的「hr-seek, skipping to …」記錄開始核對（[`parse_hr_seek`]）。
//! - 拖曳進度條的關鍵影格跳轉、方向鍵往回跳也往前多跳同樣的秒數：落在目標之前的關鍵影格（跟 MP4 一樣），
//!   不會跑到目標後面（mpv 原本的做法常落在目標之後，往回跳甚至原地不動）。還是落在目標之後就改成精確跳轉。
//! - 方向鍵往後跳照 mpv 原本的做法（落在目標之後的關鍵影格），只在沒有新的畫面時改成精確跳轉。
//!   跳到片尾（超過片尾的方向鍵）是精確跳轉：往前多跳的秒數不夠時一樣沒有新的畫面。
//! - 哪幾筆記錄是哪一次跳轉的（記錄照產生的順序送達，但跟事件之間沒有順序）：
//!   - 跳轉指令和一個標記（`print-text`，[`marker`]）用同一個指令清單送出（[`seek_command`]），中間不會插進別的；
//!   - 標記之後，mpv 開始做這次跳轉（精確跳轉看「hr-seek, skipping to」，其他看分離器的「queuing seek to」，
//!     都是 mpv 執行跳轉時同步記下的）之後的「第一個畫面出來了」「跳轉完成」才是這次的。
//!     上一次跳轉的這兩筆可能在標記之後才到：mpv 會等上一次的第一個畫面出來才做下一次（playloop.c `execute_queued_seek`）。
//!     精確跳轉不認「queuing seek to」：外掛字幕預先讀取時分離器也會跳到 0（sub.c），那不是這次跳轉。
//!   - 核對的永遠是最新的那一次（送出新的跳轉時舊的作廢）。

/// 這種封裝格式（mpv 的 `file-format`）跳轉不準：沒有索引，FFmpeg 用時間戳搜尋位置，
/// 常落在目標之後的關鍵影格、甚至檔尾（TS、M2TS、MTS 是 `mpegts`，MPEG-PS／VOB 是 `mpeg`）
pub fn approximate_seeks(file_format: &str) -> bool {
    matches!(file_format, "mpegts" | "mpeg")
}

/// 開檔時分離器往前多跳的秒數：藍光、AVCHD、電視錄影的關鍵影格大多隔 0.5–1 秒。
/// 再多的話每次精確跳轉都要多解碼這麼久（1080p 的藍光 1 秒多 45 毫秒、3 秒多 200 毫秒）；
/// 不夠的檔案第一次跳轉時就會加大
pub const FIRST_OFFSET: f64 = 1.0;
/// 最多往前多跳幾秒：再多的話每次跳轉都要解碼太久，寧可落在目標之後
pub const MAX_OFFSET: f64 = 32.0;
/// 同一次跳轉最多再試幾次（1 → 2 → 4 → 8 → 16 → 32 秒）
pub const MAX_TRIES: u32 = 6;
/// 落點可以比目標晚一格多這麼一點（mpv 精確跳轉停在第一個不早於目標 − 0.005 秒的畫面）
pub const OVERSHOOT_SLACK: f64 = 0.01;
/// 往前多跳加倍了還停在同一格、而且只晚這麼多格以內：影像在目標那裡本來就缺格
/// （每格長短不一的 MPEG-2、電視錄影掉格），不是往前多跳不夠，不再加大
pub const GAP_FRAMES: f64 = 2.0;
/// 兩次落點的時間差這麼少以內算同一格
const SAME_FRAME: f64 = 0.0005;
/// 分離器往前多跳的秒數已經超過目標這麼多：已經從檔案開頭讀了，再加大也一樣
const FROM_START: f64 = 0.5;
/// 不知道格率時當成一格這麼久（落點的誤差）
const UNKNOWN_FRAME: f64 = 0.1;

/// 一格多久（秒）：容器標示的格率；不知道或不合理時用 [`UNKNOWN_FRAME`]
pub fn frame_duration(fps: Option<f64>) -> f64 {
    match fps {
        Some(f) if f.is_finite() && f >= 1.0 => (1.0 / f).max(1.0 / 1000.0),
        _ => UNKNOWN_FRAME,
    }
}

/// 送出跳轉之前放進 mpv 記錄的標記（`print-text`；一個字，不用加引號）
pub fn marker(id: u64) -> String {
    format!("vitascope-seek-check:{id}")
}

/// 記錄裡的標記 → 編號
pub fn parse_marker(text: &str) -> Option<u64> {
    text.trim().strip_prefix("vitascope-seek-check:")?.parse().ok()
}

/// 標記和跳轉放在同一個指令清單（`mpv_command_string`，「;」分開）：mpv 一次做完，中間不會插進別的
/// （例如上一次跳轉剛好完成）。`target` 是秒數，`mode` 是 mpv 的跳轉旗標（`absolute+exact` 之類）
pub fn seek_command(id: u64, target: f64, mode: &str) -> String {
    format!("print-text {}; seek {target} {mode}", marker(id))
}

/// mpv 開始做一次跳轉：分離器（demux）排進跳轉時同步記下（`queuing seek to 12.345 …`）。
/// 外掛的軌道各自也有一筆；外掛字幕預先讀取時也有一筆（跳到 0，不是使用者的跳轉）
fn is_seek_queued(text: &str) -> bool {
    text.starts_with("queuing seek to ")
}

/// mpv 開始做一次精確跳轉（cplayer，v 等級；playloop.c `mp_seek`，分離器開始跳之後同步記下）：
/// `hr-seek, skipping to 12.345000`，逐格往回時後面多 ` (backstep)`。回傳（目標, 是不是逐格往回）。
/// 只有播放器真的在跳轉時才有（外掛字幕預先讀取之類的分離器跳轉沒有）
pub fn parse_hr_seek(text: &str) -> Option<(f64, bool)> {
    let rest = text.trim().strip_prefix("hr-seek, skipping to ")?;
    let target = rest.split(' ').next()?.parse::<f64>().ok().filter(|t| t.is_finite())?;
    Some((target, rest.contains("(backstep)")))
}

/// 跳轉之後第一個畫面出來了（cplayer，v 等級）。跳過檔尾時 mpv 用最後一格，這時「跳轉完成」寫的是 video=eof，
/// 但這一筆照樣有（video.c：hr-seek past EOF -> returns last frame）
fn is_first_frame(text: &str) -> bool {
    text.starts_with("first video frame after restart shown")
}

/// 跳轉完成時影像的狀態（mpv 的 `video=`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Video {
    /// 有畫面在播（playing、ready、draining…）
    Shown,
    /// 影像讀到檔尾了（eof）：可能完全沒有新的畫面，也可能是跳過檔尾、用了最後一格（見 [`is_first_frame`]）
    Eof,
    /// 沒有影像（none）
    None,
}

/// mpv 的「跳轉完成」記錄（cplayer，v 等級）：
/// `playback restart complete @ 12.345678, audio=playing, video=playing (paused)`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Restart {
    /// 停在哪裡（播放時間，就是當時的 time-pos）；mpv 不知道時 None
    pub pts: Option<f64>,
    pub video: Video,
}

/// 讀「跳轉完成」記錄；不是這一筆時 None
pub fn parse_restart(text: &str) -> Option<Restart> {
    let rest = text.trim().strip_prefix("playback restart complete @ ")?;
    let (pts, rest) = rest.split_once(',')?;
    // 不知道的時間 mpv 印成很大的負數（MP_NOPTS_VALUE）
    let pts = pts.trim().parse::<f64>().ok().filter(|t| t.is_finite() && *t > -1e9);
    let status = rest.split("video=").nth(1)?.split([' ', ',']).next()?;
    let video = match status {
        "eof" => Video::Eof,
        "none" => Video::None,
        _ => Video::Shown,
    };
    Some(Restart { pts, video })
}

/// 哪一種跳轉
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 精確跳轉：要停在目標那一格
    Exact,
    /// 關鍵影格跳轉（拖曳進度條、方向鍵往回跳）：停在目標或之前的關鍵影格
    Keyframe,
    /// 方向鍵往後跳（mpv 的相對跳轉，落在目標之後的關鍵影格）：有新的畫面就好
    Forward,
    /// 逐格往回（mpv 的 frame-back-step）：目標是往回之前那一格，要停在它之前
    Backstep,
}

/// 核對到哪一步了（見模組說明）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// 送出了，標記還沒到：之前的記錄都不是這次的
    Sent,
    /// 標記到了，mpv 還沒開始做這次跳轉：「第一個畫面」「跳轉完成」可能是上一次的
    Marked,
    /// mpv 開始做這次跳轉了
    Running,
}

/// 一次正在核對的跳轉
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// 標記的編號（[`marker`]）
    pub id: u64,
    /// 目標（播放時間）
    pub target: f64,
    pub kind: Kind,
    /// 這次分離器往前多跳的秒數（方向鍵的相對跳轉是 0）
    pub lead: f64,
    /// 已經重試了幾次
    pub tries: u32,
    /// 之前的精確跳轉停在哪一格（時間），和最早停在那一格時往前多跳的秒數
    pub prev: Option<(f64, f64)>,
    stage: Stage,
    /// 這次跳轉之後有新的畫面出來了
    shown: bool,
}

impl Check {
    pub fn new(id: u64, target: f64, kind: Kind, lead: f64) -> Check {
        Check {
            id,
            target,
            kind,
            lead,
            tries: 0,
            prev: None,
            stage: Stage::Sent,
            shown: false,
        }
    }

    /// mpv 自己做的跳轉（沒有標記）：從它開始做的那一筆記錄（[`parse_hr_seek`]）起核對
    pub fn started(id: u64, target: f64, kind: Kind, lead: f64) -> Check {
        Check {
            stage: Stage::Running,
            ..Check::new(id, target, kind, lead)
        }
    }

    /// 這次跳轉沒停對（[`judge`] 說 [`Verdict::Retry`]）：往前多跳 `offset` 秒再精確跳一次的核對。
    /// 逐格往回改成精確跳到前一格（往回之前那一格 − 一格）
    pub fn retry(&self, id: u64, restart: &Restart, offset: f64, frame: f64) -> Check {
        let target = match self.kind {
            Kind::Backstep => (self.target - frame).max(0.0),
            _ => self.target,
        };
        // 記下精確跳轉停在哪一格：加大了還停在同一格時，最早停在那裡的秒數就夠了
        let shown = self.shown || restart.video == Video::Shown;
        let prev = match (self.kind, shown, restart.pts) {
            (Kind::Exact, true, Some(pts)) => match self.prev {
                Some((p, lead)) if (p - pts).abs() < SAME_FRAME => Some((p, lead)),
                _ => Some((pts, self.lead)),
            },
            _ => None,
        };
        Check {
            tries: self.tries + 1,
            prev,
            ..Check::new(id, target, Kind::Exact, offset)
        }
    }

    /// 讀一筆 mpv 的記錄（任何來源；照送達的順序）。這次跳轉完成時回傳「跳轉完成」的內容
    pub fn feed(&mut self, text: &str) -> Option<Restart> {
        if let Some(id) = parse_marker(text) {
            if id == self.id && self.stage == Stage::Sent {
                self.stage = Stage::Marked;
            }
            return None;
        }
        if self.stage == Stage::Sent {
            return None;
        }
        // 這次跳轉開始了：精確跳轉（hr-seek）看 mpv 自己的那一筆，其他看分離器的
        let started = match self.kind {
            Kind::Exact | Kind::Backstep => parse_hr_seek(text).is_some(),
            Kind::Keyframe | Kind::Forward => is_seek_queued(text),
        };
        if started {
            // 之前看到的畫面是上一次跳轉的
            self.stage = Stage::Running;
            self.shown = false;
            return None;
        }
        match self.stage {
            Stage::Running if is_first_frame(text) => {
                self.shown = true;
                None
            }
            Stage::Running => parse_restart(text),
            _ => None,
        }
    }
}

/// 核對完的結果（自動測試讀它）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    pub target: f64,
    pub kind: Kind,
    /// 停在哪裡（mpv 記下的播放時間）
    pub pts: Option<f64>,
    /// 最後有新的畫面
    pub shown: bool,
    /// 重試了幾次
    pub tries: u32,
    /// 停對了的話，最少要往前多跳幾秒才停在這一格（這個檔案之後的跳轉用它）；沒停對時 None
    pub lead: Option<f64>,
}

/// 核對之後怎麼做
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// 停在對的地方，或已經沒辦法更好：結束
    Done(Landing),
    /// 分離器往前多跳 `offset` 秒，再精確跳一次（[`Check::retry`]）
    Retry { offset: f64 },
}

/// 這次跳轉停住了（`restart`，[`Check::feed`] 回傳的）：對不對、要不要再試。
/// `offset` = 這個檔案目前往前多跳的秒數，`frame` = 一格多久（[`frame_duration`]）
pub fn judge(check: &Check, restart: &Restart, offset: f64, frame: f64) -> Verdict {
    let shown = check.shown || restart.video == Video::Shown;
    let done = |shown: bool, lead: Option<f64>| {
        Verdict::Done(Landing {
            target: check.target,
            kind: check.kind,
            pts: restart.pts,
            shown,
            tries: check.tries,
            lead,
        })
    };
    // 沒有影像（純音訊）：聲音的跳轉本來就準
    if restart.video == Video::None {
        return done(false, None);
    }
    // 加大之前也停在這一格：最早停在這裡的秒數就夠了
    let same = check
        .prev
        .filter(|(p, _)| restart.pts.is_some_and(|pts| (pts - p).abs() < SAME_FRAME));
    let lead = same.map_or(check.lead, |(_, lead)| lead);
    // 落在目標一格之後：跳過頭了（往後跳的方向鍵本來就落在目標之後）。已經從檔案開頭讀了的話，
    // 那就是目標之後第一個有的畫面（例如影像比聲音晚開始），`retry_offset` 不再試。
    // 逐格往回要停在往回之前那一格之前
    let overshot = restart.pts.is_some_and(|pts| match check.kind {
        Kind::Forward => false,
        Kind::Backstep => pts > check.target - SAME_FRAME,
        Kind::Exact | Kind::Keyframe => pts > check.target + frame + OVERSHOOT_SLACK,
    });
    if shown && !overshot {
        return done(true, Some(lead));
    }
    // 往前多跳加倍了還停在同一格，而且只晚一兩格：影像在那裡缺格，不是往前多跳不夠。
    // 晚很多的不能這樣斷定：關鍵影格隔很久時，往前多跳 1、2、4 秒可能都落在目標之後同一個關鍵影格
    if shown
        && same.is_some()
        && restart
            .pts
            .is_some_and(|pts| pts <= check.target + GAP_FRAMES * frame + OVERSHOOT_SLACK)
    {
        return done(true, Some(lead));
    }
    match retry_offset(check, offset) {
        Some(offset) if check.tries < MAX_TRIES => Verdict::Retry { offset },
        _ => done(shown, None),
    }
}

/// 再試一次（精確跳轉）時分離器往前多跳的秒數：這個檔案目前的秒數比這次用的多就用它
/// （方向鍵往後跳沒有往前多跳；或之前的跳轉已經加大了），不然加倍，最多 [`MAX_OFFSET`]。
/// 已經從檔案開頭讀了、或已經到上限時 None（再試也一樣）
pub fn retry_offset(check: &Check, offset: f64) -> Option<f64> {
    if offset > check.lead {
        return Some(offset);
    }
    if check.lead > check.target + FROM_START {
        return None;
    }
    let next = (check.lead * 2.0).clamp(FIRST_OFFSET, MAX_OFFSET);
    (next > check.lead).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_restart_log() {
        assert_eq!(
            parse_restart("playback restart complete @ 12.345678, audio=playing, video=playing (paused)\n"),
            Some(Restart {
                pts: Some(12.345678),
                video: Video::Shown
            })
        );
        assert_eq!(
            parse_restart("playback restart complete @ 0.303287, audio=playing, video=eof (paused)\n"),
            Some(Restart {
                pts: Some(0.303287),
                video: Video::Eof
            })
        );
        assert_eq!(
            parse_restart("playback restart complete @ 1.000000, audio=playing, video=eof\n").map(|r| r.video),
            Some(Video::Eof)
        );
        assert_eq!(
            parse_restart("playback restart complete @ 3.5, audio=playing, video=none\n").map(|r| r.video),
            Some(Video::None)
        );
        // mpv 不知道時間時印出 MP_NOPTS_VALUE
        assert_eq!(
            parse_restart("playback restart complete @ -9223372036854775808.000000, audio=eof, video=eof\n"),
            Some(Restart {
                pts: None,
                video: Video::Eof
            })
        );
        for other in [
            "first video frame after restart shown\n",
            "hr-seek, skipping to 1.500000\n",
            "playback restart complete @ 1.0\n",
            "",
        ] {
            assert_eq!(parse_restart(other), None, "{other:?}");
        }
    }

    #[test]
    fn markers_round_trip() {
        assert_eq!(parse_marker(&format!("{}\n", marker(42))), Some(42));
        assert_eq!(parse_marker("vitascope-seek-check:x"), None);
        assert_eq!(parse_marker("something else 42"), None);
        assert_eq!(
            seek_command(7, 12.5, "absolute+exact"),
            "print-text vitascope-seek-check:7; seek 12.5 absolute+exact"
        );
        // 負數、很小的數也照一般的小數寫（不用科學記號）
        assert_eq!(
            seek_command(1, -0.5, "absolute"),
            "print-text vitascope-seek-check:1; seek -0.5 absolute"
        );
        assert_eq!(
            seek_command(1, 1e-7, "relative"),
            "print-text vitascope-seek-check:1; seek 0.0000001 relative"
        );
    }

    #[test]
    fn frame_duration_from_the_container() {
        assert!((frame_duration(Some(24.0)) - 1.0 / 24.0).abs() < 1e-12);
        assert!((frame_duration(Some(30000.0 / 1001.0)) - 1001.0 / 30000.0).abs() < 1e-12);
        for unknown in [None, Some(0.0), Some(f64::NAN), Some(-1.0)] {
            assert_eq!(frame_duration(unknown), UNKNOWN_FRAME);
        }
    }

    const FIRST_FRAME: &str = "first video frame after restart shown\n";
    const QUEUED: &str = "queuing seek to 3.500000\n";
    const HR_SEEK: &str = "hr-seek, skipping to 5.000000\n";
    const RESTART_OK: &str = "playback restart complete @ 5.000000, audio=playing, video=playing (paused)\n";
    const RESTART_EOF: &str = "playback restart complete @ 5.000000, audio=playing, video=eof (paused)\n";

    #[test]
    fn reads_the_hr_seek_log() {
        assert_eq!(parse_hr_seek(HR_SEEK), Some((5.0, false)));
        assert_eq!(
            parse_hr_seek("hr-seek, skipping to 12.345000 (no framedrop) (backstep)\n"),
            Some((12.345, true))
        );
        assert_eq!(
            parse_hr_seek("hr-seek, skipping to 1.5 (no framedrop)\n"),
            Some((1.5, false))
        );
        for other in [QUEUED, FIRST_FRAME, "hr-seek, skipping to nan\n", ""] {
            assert_eq!(parse_hr_seek(other), None, "{other:?}");
        }
    }

    #[test]
    fn only_lines_after_this_seek_started_count() {
        let mut c = Check::new(2, 5.0, Kind::Exact, 1.0);
        // 標記之前：上一次跳轉的
        for line in [QUEUED, HR_SEEK, FIRST_FRAME, RESTART_OK] {
            assert_eq!(c.feed(line), None);
        }
        // 別的編號的標記不算
        assert_eq!(c.feed(&marker(1)), None);
        assert_eq!(c.feed(RESTART_OK), None);
        assert_eq!(c.feed(&marker(2)), None);
        // 標記之後、這次跳轉開始之前：上一次的第一個畫面和跳轉完成
        assert_eq!(c.feed(FIRST_FRAME), None);
        assert_eq!(c.feed(RESTART_OK), None);
        // 分離器跳轉了，但不是播放器的精確跳轉（外掛字幕預先讀取，跳到 0）：還沒開始
        assert_eq!(c.feed("queuing seek to 0.000000\n"), None);
        assert_eq!(c.feed(FIRST_FRAME), None);
        assert_eq!(
            c.feed("playback restart complete @ 0.000000, audio=playing, video=playing\n"),
            None
        );
        // 這次開始了（外掛字幕的分離器也各有一筆）
        assert_eq!(c.feed(QUEUED), None);
        assert_eq!(c.feed("queuing seek to 3.500000 (cascade)\n"), None);
        assert_eq!(c.feed(HR_SEEK), None);
        assert_eq!(c.feed("some other line\n"), None);
        let r = c.feed(RESTART_EOF).unwrap();
        // 上一次的畫面不算：沒有新的畫面
        assert_eq!(r.video, Video::Eof);
        assert!(matches!(judge(&c, &r, 1.0, FRAME), Verdict::Retry { .. }));
        // 關鍵影格跳轉、方向鍵往後跳沒有 hr-seek：看分離器的
        let mut k = Check::new(3, 5.0, Kind::Keyframe, 1.0);
        for line in [marker(3).as_str(), RESTART_OK, QUEUED, FIRST_FRAME] {
            assert_eq!(k.feed(line), None);
        }
        assert!(k.feed(RESTART_OK).is_some());
    }

    #[test]
    fn seeks_mpv_makes_itself_are_checked_from_the_hr_seek_log() {
        // A-B 重播繞回 A、章節：沒有標記，從 hr-seek 那一筆起核對
        let mut c = Check::started(4, 5.0, Kind::Exact, 1.0);
        assert_eq!(c.feed(FIRST_FRAME), None);
        let r = c.feed(RESTART_OK).unwrap();
        assert!(matches!(judge(&c, &r, 1.0, FRAME), Verdict::Done(l) if l.shown && l.lead == Some(1.0)));
        // 逐格往回：要停在往回之前那一格（目標）之前
        let b = Check::started(5, 5.0, Kind::Backstep, 1.0);
        assert!(matches!(judge(&b, &shown(5.0 - FRAME), 1.0, FRAME), Verdict::Done(l) if l.shown));
        // 沒往回（Backstep failed）、跑到後面的關鍵影格：精確跳到前一格
        for pts in [5.0, 6.0] {
            let v = judge(&b, &shown(pts), 1.0, FRAME);
            assert_eq!(v, Verdict::Retry { offset: 2.0 }, "{pts}");
            let next = b.retry(6, &shown(pts), 2.0, FRAME);
            assert_eq!((next.kind, next.tries, next.prev), (Kind::Exact, 1, None));
            assert!((next.target - (5.0 - FRAME)).abs() < 1e-12);
        }
    }

    #[test]
    fn a_frame_before_the_latest_seek_started_does_not_count() {
        // 這次跳轉的第一個畫面出來之後，mpv 又跳了一次（例如 A-B 重播剛好繞回）：只算最後那一次的畫面
        let mut c = Check::new(1, 5.0, Kind::Exact, 1.0);
        for line in [marker(1).as_str(), HR_SEEK, FIRST_FRAME, HR_SEEK] {
            assert_eq!(c.feed(line), None);
        }
        let r = c.feed(RESTART_EOF).unwrap();
        assert_eq!(judge(&c, &r, 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        // 關鍵影格跳轉一樣（分離器的那一筆）
        let mut k = Check::new(2, 5.0, Kind::Keyframe, 1.0);
        for line in [marker(2).as_str(), QUEUED, FIRST_FRAME, QUEUED] {
            assert_eq!(k.feed(line), None);
        }
        let r = k.feed(RESTART_EOF).unwrap();
        assert_eq!(judge(&k, &r, 1.0, FRAME), Verdict::Retry { offset: 2.0 });
    }

    #[test]
    fn last_frame_at_the_end_counts_as_shown() {
        // 跳過檔尾：mpv 用最後一格（第一個畫面出來了），跳轉完成寫 video=eof
        let mut c = Check::new(1, 2.93, Kind::Exact, 4.0);
        for line in [marker(1).as_str(), QUEUED, HR_SEEK, FIRST_FRAME] {
            assert_eq!(c.feed(line), None);
        }
        let r = c
            .feed("playback restart complete @ 2.736223, audio=playing, video=eof (paused)\n")
            .unwrap();
        assert!(matches!(judge(&c, &r, 4.0, FRAME), Verdict::Done(l) if l.shown && l.tries == 0));
        // 沒有第一個畫面：真的沒有新的畫面
        let mut c = Check::new(1, 20.0, Kind::Exact, 1.0);
        for line in [marker(1).as_str(), QUEUED, HR_SEEK] {
            c.feed(line);
        }
        let r = c.feed(RESTART_EOF).unwrap();
        assert_eq!(judge(&c, &r, 1.0, FRAME), Verdict::Retry { offset: 2.0 });
    }

    fn shown(pts: f64) -> Restart {
        Restart {
            pts: Some(pts),
            video: Video::Shown,
        }
    }

    const EOF: Restart = Restart {
        pts: Some(5.0),
        video: Video::Eof,
    };
    const FRAME: f64 = 1.0 / 24.0;

    #[test]
    fn exact_seek_on_target_is_done() {
        let c = Check::new(1, 5.0, Kind::Exact, 1.0);
        // 第一個不早於目標的畫面（最多晚一格）
        for pts in [4.996, 5.0, 5.0 + FRAME] {
            let v = judge(&c, &shown(pts), 1.0, FRAME);
            assert_eq!(
                v,
                Verdict::Done(Landing {
                    target: 5.0,
                    kind: Kind::Exact,
                    pts: Some(pts),
                    shown: true,
                    tries: 0,
                    lead: Some(1.0)
                }),
                "{pts}"
            );
        }
    }

    #[test]
    fn overshoot_and_missing_frames_retry_with_a_longer_lead() {
        let c = Check::new(1, 5.0, Kind::Exact, 1.0);
        assert_eq!(judge(&c, &shown(5.5), 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        // 只晚兩、三格也是跳過頭（關鍵影格就在目標後面一點）
        for late in [2.0, 3.0] {
            assert_eq!(
                judge(&c, &shown(5.0 + late * FRAME), 1.0, FRAME),
                Verdict::Retry { offset: 2.0 },
                "{late}"
            );
        }
        assert_eq!(judge(&c, &EOF, 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        // 關鍵影格跳轉：落在目標之前沒關係，落在目標之後或沒有新的畫面就改成精確跳轉
        let k = Check::new(1, 5.0, Kind::Keyframe, 1.0);
        for pts in [3.2, 4.0, 5.0, 5.0 + FRAME] {
            assert!(
                matches!(judge(&k, &shown(pts), 1.0, FRAME), Verdict::Done(l) if l.shown),
                "{pts}"
            );
        }
        assert_eq!(judge(&k, &shown(9.0), 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        assert_eq!(judge(&k, &EOF, 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        // 方向鍵往後跳：落在目標之後沒關係，沒有新的畫面才改成精確跳轉
        let fwd = Check::new(1, 5.0, Kind::Forward, 0.0);
        assert!(matches!(judge(&fwd, &shown(9.0), 1.0, FRAME), Verdict::Done(l) if l.shown));
        // 沒有往前多跳：先用這個檔案目前的秒數
        assert_eq!(judge(&fwd, &EOF, 4.0, FRAME), Verdict::Retry { offset: 4.0 });
        assert_eq!(judge(&fwd, &EOF, 0.0, FRAME), Verdict::Retry { offset: 1.0 });
    }

    #[test]
    fn landing_after_the_target_from_the_start_is_accepted() {
        // 影像比聲音晚開始：目標在第一格之前，已經從檔案開頭讀了還是停在第一格
        let c = Check::new(1, 0.25, Kind::Exact, 1.0);
        assert!(matches!(judge(&c, &shown(1.605), 1.0, FRAME), Verdict::Done(l) if l.shown));
        // 還沒從開頭讀：同樣停在目標之後就再往前（關鍵影格可能在更前面）
        let c = Check::new(1, 1.25, Kind::Exact, 1.0);
        assert_eq!(judge(&c, &shown(1.605), 1.0, FRAME), Verdict::Retry { offset: 2.0 });
        let c = Check::new(1, 1.25, Kind::Exact, 2.0);
        assert!(matches!(judge(&c, &shown(1.605), 2.0, FRAME), Verdict::Done(l) if l.shown));
    }

    #[test]
    fn retries_stop_at_the_start_and_the_cap() {
        // 已經從檔案開頭讀了
        let c = Check::new(1, 0.3, Kind::Exact, 1.0);
        assert!(matches!(judge(&c, &EOF, 1.0, FRAME), Verdict::Done(l) if !l.shown));
        // 已經到上限
        let c = Check::new(1, 100.0, Kind::Exact, MAX_OFFSET);
        assert!(matches!(judge(&c, &EOF, MAX_OFFSET, FRAME), Verdict::Done(l) if !l.shown));
        assert!(matches!(judge(&c, &shown(102.0), MAX_OFFSET, FRAME), Verdict::Done(l) if l.shown));
        // 試太多次
        let mut c = Check::new(1, 100.0, Kind::Exact, 4.0);
        c.tries = MAX_TRIES;
        assert!(matches!(judge(&c, &EOF, 4.0, FRAME), Verdict::Done(_)));
        // 一路加倍到上限
        let mut lead = FIRST_OFFSET;
        let mut steps = vec![lead];
        while let Some(next) = retry_offset(&Check::new(1, 1000.0, Kind::Exact, lead), lead) {
            lead = next;
            steps.push(lead);
        }
        assert_eq!(steps, [1.0, 2.0, 4.0, 8.0, 16.0, 32.0]);
        assert!(steps.len() as u32 <= MAX_TRIES);
    }

    #[test]
    fn a_landing_the_longer_lead_did_not_change_stops_the_escalation() {
        // 影像在目標那裡缺一格（每格長短不一）：往前多跳 1 秒、2 秒都停在目標之後一格多的同一格
        let frame = 1001.0 / 30000.0;
        let t = 5.006;
        let late = shown(t + 0.044);
        let first = Check::new(1, t, Kind::Exact, 1.0);
        assert_eq!(judge(&first, &late, 1.0, frame), Verdict::Retry { offset: 2.0 });
        let second = first.retry(2, &late, 2.0, frame);
        assert_eq!(second.prev, Some((t + 0.044, 1.0)));
        // 加大了還是同一格：結束，這個檔案的秒數不用加大（最早停在這一格的是 1 秒）
        assert!(matches!(
            judge(&second, &late, 1.0, frame),
            Verdict::Done(l) if l.shown && l.tries == 1 && l.lead == Some(1.0)
        ));
        // 關鍵影格隔很久：1、2 秒都落在目標之後同一個關鍵影格（晚很多），要繼續加大
        let far = shown(t + 4.0);
        let second = first.retry(2, &far, 2.0, frame);
        assert_eq!(judge(&second, &far, 1.0, frame), Verdict::Retry { offset: 4.0 });
        let third = second.retry(3, &far, 4.0, frame);
        assert_eq!(third.prev, Some((t + 4.0, 1.0)));
        // 4 秒才停對：這個檔案之後用 4 秒
        assert!(matches!(
            judge(&third, &shown(t + 0.01), 1.0, frame),
            Verdict::Done(l) if l.tries == 2 && l.lead == Some(4.0)
        ));
        // 往前多跳加大後停對：要的就是這次的秒數
        let second = first.retry(2, &shown(t + 0.5), 2.0, frame);
        assert!(matches!(
            judge(&second, &shown(t), 1.0, frame),
            Verdict::Done(l) if l.lead == Some(2.0)
        ));
        // 一直沒停對（缺了很多格，到上限）：不算這個檔案要的秒數
        let mut c = Check::new(1, t, Kind::Exact, MAX_OFFSET);
        c.tries = 5;
        assert!(matches!(judge(&c, &far, 1.0, frame), Verdict::Done(l) if l.shown && l.lead.is_none()));
        // 關鍵影格跳轉的落點不是精確跳轉的落點：不記
        let k = Check::new(1, t, Kind::Keyframe, 1.0);
        assert_eq!(k.retry(2, &far, 2.0, frame).prev, None);
    }

    #[test]
    fn audio_only_needs_no_check() {
        let c = Check::new(1, 5.0, Kind::Exact, 1.0);
        let none = Restart {
            pts: Some(5.0),
            video: Video::None,
        };
        assert!(matches!(judge(&c, &none, 1.0, FRAME), Verdict::Done(l) if !l.shown && l.tries == 0));
    }
}
