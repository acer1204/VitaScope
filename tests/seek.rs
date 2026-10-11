//! 跳轉不準的格式（TS、M2TS、MTS、MPEG-PS）在主播放器裡的跳轉（headless，見 `vitascope::approx_seek`）。
//!
//! 沒有索引的格式用時間戳搜尋，mpv 原本的精確跳轉常落在目標之後的關鍵影格，甚至一格都沒有（畫面停在跳轉前）。
//! 這裡確認：
//! - 精確跳轉停在目標那一格（最多晚一格），而且畫面就是那一格：跟分離器從檔案開頭讀起的參考播放器的截圖一樣；
//! - 關鍵影格跳轉（拖曳進度條）停在目標或之前、不會跳到目標後面，畫面跟停的時間一致；
//! - 方向鍵往後、往回都有新的畫面，往回跳不會原地不動，按住往回跳每一下退 5 秒；跳到片尾停在最後一格；
//! - mpv 自己做的精確跳轉（A-B 重播繞回 A、逐格往回）也核對、停對；
//! - 其他格式不受影響，使用者自己指定了 hr-seek-demuxer-offset 時照使用者的；換檔時 mpv 自己還原 file-local 的選項
//!   （包括加大過的）。
//!
//! 樣本來自 `python scripts/gen_samples.py`。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vitascope::approx_seek::{Kind, Landing};
use vitascope::player::{Options, Player, PlayerEvent};
use vitascope::screenshot::Image;

/// 慢的 CI 也要等得到（跳轉可能要重試幾次，每次都從更前面解碼）
const TIMEOUT: Duration = Duration::from_secs(30);
/// mpv 精確跳轉停在第一個不早於目標 − 0.005 秒的畫面；時間的誤差
const EPS: f64 = 0.006;

/// 每個測試自己的暫存資料夾（截圖）；測試失敗時也會刪掉
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vitascope-seek-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

/// 暫停、播完停在最後一格的 headless 播放器，開好 `rel`、第一格解好
fn open_with(rel: &str, extra: &[(&str, &str)]) -> Player {
    let mut opts = Options {
        keep_open: true,
        ..Options::headless()
    };
    opts.extra.push(("pause".into(), "yes".into()));
    opts.extra
        .extend(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    let mut p = Player::new(opts).expect("建立 mpv 失敗");
    let path = sample(rel);
    p.open(&path).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {rel}：{e}"));
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
        .unwrap_or_else(|e| panic!("{rel} 第一格：{e}"));
    p.wait_state(TIMEOUT, |s| s.duration.is_some()).unwrap();
    p
}

fn open(rel: &str) -> Player {
    open_with(rel, &[])
}

/// 參考用：分離器一律從檔案開頭之前讀起（樣本都很短），精確跳轉一定準。
/// 自己指定了 hr-seek-demuxer-offset，播放器不核對、不改它
fn reference(rel: &str) -> Player {
    let p = open_with(rel, &[("hr-seek-demuxer-offset", "1000")]);
    assert_eq!(p.seek_lead(), None, "使用者自己指定了 hr-seek-demuxer-offset：不核對");
    p
}

/// 一直處理事件，直到條件成立
fn until(p: &mut Player, what: &str, mut cond: impl FnMut(&mut Player) -> bool) {
    let start = Instant::now();
    while !cond(p) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}（核對中 {}，上一次的結果 {:?}，time-pos {:?}）",
            p.seek_checking(),
            p.seek_landing(),
            p.get_f64("time-pos")
        );
        p.wait(Duration::from_millis(20));
    }
}

/// 等到核對完、結果符合 `want` 的那一次跳轉（可能重試了幾次）
fn landing_where(p: &mut Player, what: &str, want: impl Fn(&Landing) -> bool) -> Landing {
    let mut landing = None;
    until(p, what, |p| {
        landing = p.seek_landing().filter(|l| !p.seek_checking() && want(l));
        landing.is_some()
    });
    landing.unwrap()
}

/// 等這次跳轉核對完（可能重試了幾次），回傳結果與 mpv 停住的位置
fn landed(p: &mut Player, target: f64, what: &str) -> (Landing, f64) {
    let mut landing = None;
    until(p, what, |p| {
        landing = p.seek_landing().filter(|l| !p.seek_checking() && l.target == target);
        landing.is_some()
    });
    // 核對完了，mpv 也不在跳轉中：time-pos 就是畫面上那一格
    until(p, what, |p| p.get_string("seeking").is_ok_and(|v| v == "no"));
    (landing.unwrap(), p.get_f64("time-pos").unwrap())
}

/// 參考播放器精確跳到 `t`，回傳停住的位置
fn reference_at(r: &mut Player, t: f64) -> f64 {
    r.seek_to(t, true).unwrap();
    // 之前排著的 PlaybackRestart（開檔、上一次跳轉）不算：每次都問 mpv 還在不在跳轉、停在哪裡
    let start = Instant::now();
    loop {
        let left = TIMEOUT.saturating_sub(start.elapsed());
        r.wait_for(left, |e| *e == PlayerEvent::PlaybackRestart)
            .unwrap_or_else(|e| panic!("參考播放器跳不到 {t}：{e}"));
        let seeking = r.get_string("seeking").map_or(true, |v| v != "no");
        let pos = r.get_f64("time-pos").unwrap_or(f64::NAN);
        if !seeking && pos >= t - EPS - 0.01 && pos < t + 0.2 {
            return pos;
        }
    }
}

/// 畫面上那一格的截圖（原始大小）
fn frame(p: &mut Player, dir: &Path, id: u64) -> Image {
    let png = dir.join(format!("{id}.png"));
    p.screenshot_to_file(id, &png.to_string_lossy(), false).unwrap();
    match p.wait_for(
        TIMEOUT,
        |e| matches!(e, PlayerEvent::CommandReply { id: i, .. } if *i == id),
    ) {
        Ok(PlayerEvent::CommandReply { error: None, .. }) => {}
        other => panic!("截圖失敗：{other:?}"),
    }
    let img = vitascope::screenshot::decode_png(&png).unwrap();
    std::fs::remove_file(&png).unwrap();
    img
}

/// 兩張截圖是同一格（同一個解碼器解同一個檔案：只容許轉換的捨入誤差）
fn same_frame(a: &Image, b: &Image) -> bool {
    (a.w, a.h) == (b.w, b.h) && a.rgba.iter().zip(&b.rgba).all(|(x, y)| x.abs_diff(*y) <= 2)
}

/// 這個檔案一格多久
fn frame_len(p: &Player) -> f64 {
    1.0 / p.get_f64("container-fps").unwrap()
}

/// 畫面跟參考播放器在 `pos` 的那一格一樣（新的畫面出來了，而且就是停住的那一格）
fn assert_shows(p: &mut Player, r: &mut Player, pos: f64, dir: &Path, id: u64, what: &str) {
    let want = reference_at(r, pos);
    assert!((want - pos).abs() < 0.002, "{what}：參考停在 {want}，主播放器 {pos}");
    let got = frame(p, dir, id);
    let reference = frame(r, dir, id + 1);
    assert!(
        same_frame(&got, &reference),
        "{what}：畫面不是 {pos} 那一格（還是跳轉前的畫面？）"
    );
}

#[test]
fn exact_seeks_land_on_the_target_frame() {
    let dir = TempDir::new("exact");
    let mut id = 100;
    for rel in [
        // 整個檔案一個關鍵影格（在開頭）：mpv 原本一格都沒有
        "general/ts_h264_aac.ts",
        "general/m2ts_h264_ac3.m2ts",
        // 每 10 秒一個關鍵影格：mpv 原本落在 10 秒那一格或一格都沒有
        "general/ts_h264_gop10.ts",
        // 像電視錄影（0.5 秒）、MPEG-PS：mpv 原本晚 0.2–0.5 秒
        "general/ts_h264_gop05.ts",
        "general/mpg_mpeg1_mp2.mpg",
        // MPEG-2（電視錄影的 TS、DVD 的 VOB）：開放式 GOP，跳轉後解碼器丟掉開頭不完整的 B 格
        "general/ts_mpeg2_interlaced.ts",
        "general/vob_mpeg2_ac3_anamorphic.vob",
    ] {
        let mut p = open(rel);
        let mut r = reference(rel);
        assert_eq!(p.seek_lead(), Some(vitascope::approx_seek::FIRST_OFFSET), "{rel}");
        let dur = p.state.duration.unwrap();
        let fl = frame_len(&p);
        // 往後、往回、靠近片尾
        for f in [0.37, 0.81, 0.12, 0.55, 0.93] {
            let t = dur * f;
            let what = format!("{rel} 精確跳到 {t:.3}");
            p.seek_to(t, true).unwrap();
            let (landing, pos) = landed(&mut p, t, &what);
            assert!(landing.shown, "{what}：沒有新的畫面 {landing:?}");
            assert_eq!(landing.kind, Kind::Exact);
            assert!(
                pos >= t - EPS && pos <= t + fl + EPS,
                "{what}：停在 {pos}（{landing:?}，往前多跳 {:?} 秒）",
                p.seek_lead()
            );
            id += 10;
            assert_shows(&mut p, &mut r, pos, dir.path(), id, &what);
        }
    }
}

#[test]
fn exact_seeks_a_few_frames_before_a_keyframe_are_not_late() {
    // 每 2 秒一個關鍵影格（40.02、42.02、…）：目標在 42.02 之前三、四格時，往前多跳 1 秒落在 42.02，
    // 只晚 0.1 多秒（藍光、電視錄影最常見的「晚一點」，不是一格都沒有）：也要看出來、從更前面重讀
    let rel = "general/ts_h264_long.ts";
    let dir = TempDir::new("near-keyframe");
    let mut p = open(rel);
    let mut r = reference(rel);
    assert_eq!(p.seek_lead(), Some(vitascope::approx_seek::FIRST_OFFSET));
    let fl = frame_len(&p);
    let t = 41.88;
    let what = format!("{rel} 精確跳到 {t}（關鍵影格前幾格）");
    p.seek_to(t, true).unwrap();
    let (landing, pos) = landed(&mut p, t, &what);
    assert!(
        landing.shown && pos >= t - EPS && pos <= t + fl + EPS,
        "{what}：停在 {pos}（{landing:?}）"
    );
    // 往前多跳 1 秒那一次落在 42.02（晚了），重讀了
    assert!(landing.tries > 0, "{what}：{landing:?}");
    assert_shows(&mut p, &mut r, pos, dir.path(), 5000, &what);
}

#[test]
fn keyframe_seeks_stay_at_or_before_the_target() {
    let dir = TempDir::new("keyframe");
    let mut id = 1000;
    // （樣本, 關鍵影格的間隔）
    for (rel, gop) in [
        ("general/ts_h264_gop05.ts", 0.5),
        ("general/mpg_mpeg1_mp2.mpg", 0.48),
        ("general/ts_h264_long.ts", 2.0),
        ("general/ts_h264_gop10.ts", 10.0),
        ("general/m2ts_h264_ac3.m2ts", 3.1),
    ] {
        let mut p = open(rel);
        let mut r = reference(rel);
        let dur = p.state.duration.unwrap();
        let fl = frame_len(&p);
        for f in [0.45, 0.78, 0.21, 0.66] {
            let t = dur * f;
            let what = format!("{rel} 關鍵影格跳到 {t:.3}");
            p.seek_to(t, false).unwrap();
            let (landing, pos) = landed(&mut p, t, &what);
            assert!(landing.shown, "{what}：沒有新的畫面 {landing:?}");
            let lead = p.seek_lead().unwrap();
            // 不會跳到目標後面；最多早一個關鍵影格的間隔（或往前多跳的秒數，比較長的那個）
            assert!(
                pos <= t + fl + EPS && pos >= t - lead.max(gop) - fl - EPS,
                "{what}：停在 {pos}（{landing:?}，往前多跳 {lead} 秒）"
            );
            if gop < vitascope::approx_seek::FIRST_OFFSET && t > lead {
                // 關鍵影格比往前多跳的秒數密：往前多跳的關鍵影格跳轉一次就停在目標之前（快，不用改成精確跳轉）。
                // 離開頭比往前多跳的秒數近的目標本來就改成精確跳轉（見 `Player::seek_to`）
                assert!(
                    landing.kind == Kind::Keyframe && landing.tries == 0 && pos < t - EPS,
                    "{what}：停在 {pos}（{landing:?}）"
                );
            }
            id += 10;
            assert_shows(&mut p, &mut r, pos, dir.path(), id, &what);
        }
    }
}

/// 暫停中的位置：介面的狀態也要跟上（方向鍵用它判斷會不會超過片尾）
fn settle_state(p: &mut Player) {
    until(p, "狀態跟上 mpv 的位置", |p| {
        p.get_string("seeking").is_ok_and(|v| v == "no")
            && p.get_f64("time-pos").is_ok_and(|t| (t - p.state.time_pos).abs() < 1e-6)
    });
}

#[test]
fn arrow_keys_move_and_show_a_frame() {
    let dir = TempDir::new("arrows");
    let mut id = 2000;
    for (rel, gop) in [("general/ts_h264_gop10.ts", 10.0), ("general/ts_h264_long.ts", 2.0)] {
        let mut p = open(rel);
        let mut r = reference(rel);
        let fl = frame_len(&p);
        p.seek_to(8.3, true).unwrap();
        landed(&mut p, 8.3, "起點");
        for step in [5.0, 5.0, -5.0, -2.5, 2.5, -5.0] {
            settle_state(&mut p);
            let before = p.get_f64("time-pos").unwrap();
            let t = before + step;
            let what = format!("{rel} 從 {before:.3} 跳 {step:+}");
            p.seek_relative(step).unwrap();
            // 往回跳跟拖曳進度條一樣是關鍵影格跳轉（目標是同一個時間）；往後跳照 mpv 原本的做法
            let mut landing = None;
            until(&mut p, &what, |p| {
                // 目標是播放器自己算的（剛送出精確跳轉時從那次的目標算，跟 mpv 的 time-pos 差一點）
                landing = p
                    .seek_landing()
                    .filter(|l| !p.seek_checking() && (l.target - t).abs() < 0.05);
                landing.is_some()
            });
            settle_state(&mut p);
            let landing = landing.unwrap();
            let pos = p.get_f64("time-pos").unwrap();
            assert!(landing.shown, "{what}：沒有新的畫面 {landing:?}");
            if step > 0.0 {
                // 落在目標或之後，最多一個關鍵影格的間隔
                assert!(
                    pos >= t - fl - EPS && pos <= t + gop + fl,
                    "{what}：停在 {pos}（{landing:?}）"
                );
            } else {
                // 落在目標或之前，而且真的往回了
                let lead = p.seek_lead().unwrap();
                assert!(
                    pos <= t + fl + EPS && pos >= t - lead - gop - fl && pos < before - 1.0,
                    "{what}：停在 {pos}（{landing:?}）"
                );
            }
            id += 10;
            assert_shows(&mut p, &mut r, pos, dir.path(), id, &what);
        }
    }
}

#[test]
fn seeking_past_the_end_shows_the_last_frame() {
    let dir = TempDir::new("end");
    let mut id = 3000;
    // 最後一個關鍵影格離片尾 10 秒、整個檔案一個關鍵影格：mpv 原本一格都沒有（畫面、時間都停在奇怪的地方）
    for rel in ["general/ts_h264_gop10.ts", "general/ts_h264_aac.ts"] {
        let mut p = open(rel);
        let mut r = reference(rel);
        let dur = p.state.duration.unwrap();
        let fl = frame_len(&p);
        p.seek_to(dur * 0.4, true).unwrap();
        landed(&mut p, dur * 0.4, "起點");
        settle_state(&mut p);
        let what = format!("{rel} 方向鍵跳過片尾");
        // 片長可能在開檔之後才修正（TS 讀到後面才知道）：目標是播放器當時知道的片長
        let dur = p.state.duration.unwrap();
        p.seek_relative(dur).unwrap();
        let (landing, pos) = landed(&mut p, dur, &what);
        assert!(landing.shown, "{what}：沒有新的畫面 {landing:?}");
        // 停在最後一格（最後一格的時間 = 片長 − 一格左右；聲音可能比影像長一點）
        assert!(
            pos > dur - 4.0 * fl && pos <= dur + EPS,
            "{what}：停在 {pos}（片長 {dur}）"
        );
        id += 10;
        assert_shows(&mut p, &mut r, pos, dir.path(), id, &what);
    }
}

#[test]
fn only_approximate_formats_are_checked_and_options_reset_per_file() {
    use vitascope::approx_seek::FIRST_OFFSET;
    // 關鍵影格隔 10 秒：往前多跳 1 秒不夠，加大（這個檔案之後一直用）
    let mut p = open("general/ts_h264_gop10.ts");
    assert_eq!(p.seek_lead(), Some(FIRST_OFFSET));
    p.seek_to(16.2, true).unwrap();
    let (landing, _) = landed(&mut p, 16.2, "gop10 精確跳到 16.2");
    let lead = p.seek_lead().unwrap();
    assert!(landing.tries > 0 && lead > FIRST_OFFSET, "{landing:?} {lead}");
    assert_eq!(landing.lead, Some(lead));
    assert_eq!(p.get_f64("hr-seek-demuxer-offset").unwrap(), lead);
    // 換檔：加大過的也還原，下一個檔案從頭開始
    p.open(&sample("general/ts_h264_gop05.ts")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.seek_lead(), Some(FIRST_OFFSET));
    assert_eq!(p.get_f64("hr-seek-demuxer-offset").unwrap(), FIRST_OFFSET);
    // 換成有索引的格式：mpv 自己把 file-local 的選項還原，不核對
    p.seek_to(12.0, true).unwrap();
    landed(&mut p, 12.0, "gop05 精確跳到 12");
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.seek_lead(), None);
    assert_eq!(p.get_f64("hr-seek-demuxer-offset").unwrap(), 0.0);
    p.seek_to(1.5, true).unwrap();
    until(&mut p, "MP4 跳轉", |p| {
        p.get_f64("time-pos").is_ok_and(|t| (t - 1.5).abs() < 0.05)
    });
    assert!(!p.seek_checking() && p.seek_landing().is_none(), "MP4 不核對");
    // 純音訊（沒有影像）也不用
    p.open(&sample("general/audio_wav.wav")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.seek_lead(), None);
    // 使用者自己指定了 hr-seek-demuxer-offset：照使用者的
    let r = reference("general/ts_h264_gop05.ts");
    assert_eq!(r.get_f64("hr-seek-demuxer-offset").unwrap(), 1000.0);
}

#[test]
fn holding_the_back_arrow_steps_five_seconds_each_press() {
    // 每 2 秒一個關鍵影格（40.02、42.02、…）：41.3 往前多跳 1 秒不夠，加大到 2 秒
    let rel = "general/ts_h264_long.ts";
    let mut p = open(rel);
    let fl = frame_len(&p);
    p.seek_to(61.3, true).unwrap();
    landed(&mut p, 61.3, "起點");
    let lead = p.seek_lead().unwrap();
    assert!(lead > vitascope::approx_seek::FIRST_OFFSET, "{lead}");
    settle_state(&mut p);
    // 按住往回：每一下都在上一下停住之前送出（mpv 還在跳，或還沒開始做）
    for _ in 0..3 {
        p.seek_relative(-5.0).unwrap();
    }
    let t = 61.3 - 15.0;
    let landing = landing_where(&mut p, "按住往回跳", |_| true);
    let pos = landing.pts.unwrap();
    assert!(
        (landing.target - t).abs() < 1e-6 && landing.shown && pos <= t + fl + EPS && pos >= t - lead - fl - EPS,
        "一下退 5 秒，三下停在 {t} 或之前的關鍵影格：{landing:?}（往前多跳 {lead} 秒）"
    );
}

#[test]
fn ab_loop_wraps_back_onto_a() {
    // A 在關鍵影格（40.02）之後 1.28 秒：mpv 繞回 A 時往前多跳 1 秒不夠，落在下一個關鍵影格（42.02）
    let rel = "general/ts_h264_long.ts";
    let mut p = open(rel);
    let fl = frame_len(&p);
    let (a, b) = (41.3, 42.4);
    p.seek_to(42.1, true).unwrap();
    landed(&mut p, 42.1, "起點");
    assert_eq!(p.seek_lead(), Some(vitascope::approx_seek::FIRST_OFFSET));
    p.set_ab_loop(Some(a), Some(b)).unwrap();
    p.set_pause(false).unwrap();
    // 繞回 A 是 mpv 自己做的：從它的記錄開始核對，停在 A 之後就從更前面重讀
    let near_a = |l: &Landing| (l.target - a).abs() < 1e-6;
    let first = landing_where(&mut p, "第一次繞回 A", near_a);
    let ok = |l: &Landing| l.shown && l.pts.is_some_and(|pts| pts >= a - EPS && pts <= a + fl + EPS);
    assert!(ok(&first) && first.tries > 0, "第一次繞回 A：{first:?}");
    let lead = p.seek_lead().unwrap();
    assert!(lead > vitascope::approx_seek::FIRST_OFFSET, "{lead}");
    // 之後都從夠前面讀：一次就停在 A
    let again = landing_where(&mut p, "再繞回 A", |l| near_a(l) && l.tries == 0);
    assert!(ok(&again), "再繞回 A：{again:?}");
    p.set_pause(true).unwrap();
    p.clear_ab_loop().unwrap();
}

#[test]
fn frame_back_step_lands_on_the_previous_frame() {
    let rel = "general/ts_h264_long.ts";
    let dir = TempDir::new("backstep");
    let mut p = open(rel);
    let mut r = reference(rel);
    let fl = frame_len(&p);
    // 停在關鍵影格（40.02）之後 1.3 秒多：往回一格時往前多跳 1 秒讀不到 40.02，mpv 原本會跑到 42.02
    p.seek_to(40.5, false).unwrap();
    landed(&mut p, 40.5, "起點");
    assert_eq!(p.seek_lead(), Some(vitascope::approx_seek::FIRST_OFFSET));
    p.set_pause(false).unwrap();
    until(&mut p, "播到 41.35", |p| {
        p.get_f64("time-pos").is_ok_and(|t| t >= 41.35)
    });
    p.set_pause(true).unwrap();
    until(&mut p, "暫停", |p| p.get_string("pause").is_ok_and(|v| v == "yes"));
    settle_state(&mut p);
    let x = p.get_f64("time-pos").unwrap();
    p.frame_step(false).unwrap();
    // 逐格往回是 mpv 自己的精確跳轉：從它的記錄開始核對（沒往回就精確跳到前一格）
    let landing = landing_where(&mut p, "往回一格", |l| {
        l.target <= x + 1e-6 && l.target >= x - fl - EPS
    });
    settle_state(&mut p);
    let pos = p.get_f64("time-pos").unwrap();
    assert!(
        landing.shown && pos < x - 0.001 && pos >= x - fl - EPS && landing.pts.is_some_and(|l| (l - pos).abs() < 1e-6),
        "從 {x} 往回一格：停在 {pos}（{landing:?}）"
    );
    assert_shows(&mut p, &mut r, pos, dir.path(), 4000, "往回一格");
}
