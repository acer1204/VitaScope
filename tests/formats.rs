//! 格式測試矩陣（ROADMAP 4.1）
//!
//! 讀 samples/generated/manifest.json（`python scripts/gen_samples.py` 產生），
//! 每個樣本用 headless mpv 檢查：
//! 1. 能開啟，偵測到的影像 / 音訊 / 字幕編碼與預期一致
//! 2. 第一格能解出來
//! 3. 字幕會自動選上，跳到指定時間後畫面上的字幕文字正確（含 Big5 / UTF-16 編碼偵測）
//! 4. 中繼資料：旋轉、HDR 傳輸特性、畫面比例
//! 5. 圖形字幕（PGS、DVD、DVB）：播放中字幕會畫出來（`sub_bitmap`；比較含字幕與不含字幕的截圖）
//! 6. 播放引擎解不了的編碼（`missing`，例如 AVS2）：播放器記下是哪個，其他的照常播到結尾
//! 7. 跳到中間、播到結尾，過程中沒有錯誤。檔頭沒有正確長度的（`duration: null`，例如 AMV）不跳轉、直接播到結尾
//!
//! 「常見」「通用」有任何失敗 → 測試失敗；「罕見」失敗只列在報告裡。
//!
//! 環境變數：
//!   VITASCOPE_TIERS=common,general   只測這些等級（預設全部）
//!   VITASCOPE_ONLY=h264              只測 id 包含這段文字的樣本
//!   VITASCOPE_MPV_SUBAUTO=1          外掛字幕改交給 mpv 內建的 sub-auto（對照用）

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vitascope::mpv::EndReason;
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};

const REQUIRED_TIERS: &[&str] = &["common", "general"];
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
struct Sample {
    id: String,
    tier: String,
    file: String,
    video: Option<String>,
    audio: Option<String>,
    #[serde(default)]
    subs: Vec<String>,
    /// null：檔頭沒有正確的長度（AMV、FFmpeg 寫的 .ra），不檢查長度、不跳轉
    duration: Option<f64>,
    #[serde(default)]
    note: String,
    sub_probe: Option<SubProbe>,
    rotate: Option<i64>,
    gamma: Option<String>,
    primaries: Option<String>,
    aspect: Option<f64>,
    /// 圖形字幕：沒有文字可以比對，播放中每換一句（`sub-start` 有新的值）就比較含字幕與不含字幕的截圖
    #[serde(default)]
    sub_bitmap: bool,
    /// 預期播放引擎解不了的編碼（mpv 的編碼名稱）：播放器要記下（`missing_decoders`），其他的照常播
    #[serde(default)]
    missing: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SubProbe {
    time: f64,
    text: String,
}

struct Outcome {
    sample: Sample,
    problems: Vec<String>,
    info: String,
}

fn samples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated")
}

fn load_manifest() -> Vec<Sample> {
    let path = samples_dir().join("manifest.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("找不到 {}，請先執行：python scripts/gen_samples.py", path.display()));
    let all: Vec<Sample> = serde_json::from_str(&text).expect("manifest.json 格式錯誤");

    let tiers: Option<Vec<String>> = std::env::var("VITASCOPE_TIERS")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect());
    let only = std::env::var("VITASCOPE_ONLY").ok();
    all.into_iter()
        .filter(|s| tiers.as_ref().is_none_or(|t| t.contains(&s.tier)))
        .filter(|s| only.as_ref().is_none_or(|o| s.id.contains(o.as_str())))
        .collect()
}

fn check(sample: &Sample) -> Outcome {
    let mut problems = Vec::new();
    let info = match run_checks(sample, &mut problems) {
        Ok(info) => info,
        Err(e) => {
            problems.push(e);
            String::new()
        }
    };
    Outcome {
        sample: sample.clone(),
        problems,
        info,
    }
}

/// 回傳 Err = 無法繼續的錯誤；可以繼續檢查的問題放進 `problems`。
fn run_checks(s: &Sample, problems: &mut Vec<String>) -> Result<String, String> {
    let path = samples_dir().join(&s.file);
    // VITASCOPE_MPV_SUBAUTO=1：外掛字幕改用 mpv 內建的 sub-auto，對照自己的處理差在哪
    let opts = Options {
        external_subs: std::env::var_os("VITASCOPE_MPV_SUBAUTO").is_none(),
        ..Options::headless()
    };
    let mut p = Player::new(opts).map_err(|e| e.to_string())?;
    p.set_pause(true).map_err(|e| e.to_string())?;
    p.open(path.to_str().unwrap()).map_err(|e| e.to_string())?;

    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .map_err(|e| format!("開檔：{e}"))?;
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
        .map_err(|e| format!("解出第一格：{e}"))?;
    p.wait_state(TIMEOUT, |st| {
        !st.tracks.is_empty() && (s.duration.is_none() || st.duration.is_some())
    })
    .map_err(|e| format!("讀取軌道：{e}"))?;
    // 解不了的編碼：mpv 的記錄比開檔的事件晚到
    if !s.missing.is_empty() {
        let recorded = |st: &vitascope::player::State| {
            s.missing
                .iter()
                .all(|c| st.missing_decoders.iter().any(|m| &m.codec == c))
        };
        if p.wait_state(TIMEOUT, recorded).is_err() {
            let got: Vec<&str> = p.state.missing_decoders.iter().map(|m| m.codec.as_str()).collect();
            problems.push(format!("解不了的編碼 {got:?}，預期 {:?}", s.missing));
        }
    }
    if s.video.is_some() {
        p.wait_state(TIMEOUT, |st| st.video_size.is_some())
            .map_err(|e| format!("影像尺寸：{e}"))?;
    }

    // 1. 編碼
    let st = &p.state;
    let codec_of = |kind| st.selected(kind).and_then(|t| t.codec.clone());
    let video = codec_of(TrackKind::Video).filter(|_| st.has_video());
    if video != s.video {
        problems.push(format!("影像編碼 {video:?}，預期 {:?}", s.video));
    }
    let audio = codec_of(TrackKind::Audio);
    if audio != s.audio {
        problems.push(format!("音訊編碼 {audio:?}，預期 {:?}", s.audio));
    }
    let sub_codecs: Vec<String> = st.tracks_of(TrackKind::Sub).filter_map(|t| t.codec.clone()).collect();
    for want in &s.subs {
        if !sub_codecs.contains(want) {
            problems.push(format!("缺少字幕 {want}（找到 {sub_codecs:?}）"));
        }
    }
    let duration = st.duration.unwrap_or(0.0);
    if let Some(want) = s.duration
        && (duration - want).abs() > 0.5
    {
        problems.push(format!("長度 {duration:.2} 秒，預期約 {want} 秒"));
    }

    let mut info = [video.as_deref(), audio.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("+");
    if !sub_codecs.is_empty() {
        info += &format!(" 字幕:{}", sub_codecs.join(","));
    }
    if let Some([w, h]) = st.video_size {
        info += &format!(" {w}×{h}");
    }
    if !st.missing_decoders.is_empty() {
        let codecs: Vec<&str> = st.missing_decoders.iter().map(|m| m.codec.as_str()).collect();
        info += &format!(" 解不了:{}", codecs.join(","));
    }
    if s.duration.is_none() {
        info += " 長度未知";
    }

    // 2. 字幕：要自動選上，而且文字正確
    if let Some(probe) = &s.sub_probe {
        if st.selected(TrackKind::Sub).is_none() {
            problems.push("字幕沒有自動選上".into());
            let first = st.tracks_of(TrackKind::Sub).next().map(|t| t.id);
            p.select_track(TrackKind::Sub, first).map_err(|e| e.to_string())?;
        }
        p.seek_to(probe.time, true).map_err(|e| e.to_string())?;
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
            .map_err(|e| format!("字幕跳轉：{e}"))?;
        let text = p.get_string("sub-text").unwrap_or_default();
        if text.trim() != probe.text {
            problems.push(format!("{} 秒的字幕是 {text:?}，預期 {:?}", probe.time, probe.text));
        }
    }

    // 3. 中繼資料
    if let Some(want) = s.rotate {
        let got = p
            .get_string("video-params/rotate")
            .ok()
            .and_then(|v| v.parse::<i64>().ok());
        if got != Some(want) {
            problems.push(format!("旋轉 {got:?}，預期 {want}"));
        }
    }
    for (prop, want) in [
        ("video-params/gamma", &s.gamma),
        ("video-params/primaries", &s.primaries),
    ] {
        if let Some(want) = want {
            let got = p.get_string(prop).unwrap_or_default();
            if &got != want {
                problems.push(format!("{prop} = {got:?}，預期 {want:?}"));
            }
        }
    }
    if let Some(want) = s.aspect {
        let got = p.get_f64("video-params/aspect").unwrap_or(0.0);
        if (got - want).abs() > 0.02 {
            problems.push(format!("畫面比例 {got:.3}，預期 {want:.3}"));
        }
    }

    // 4. 圖形字幕：暫停中（vo=null）跳轉後 sub-start 不會更新，要播放中檢查。
    // 從頭用 0.5 倍速播，mpv 每換一句（sub-start 有新的值）就暫停，比較含字幕與不含字幕的截圖：
    // 有畫出東西才算（只看 sub-start 不夠：沒有圖的字幕事件也有開始時間）
    if s.sub_bitmap {
        if p.state.selected(TrackKind::Sub).is_none() {
            problems.push("字幕沒有自動選上".into());
            let first = p.state.tracks_of(TrackKind::Sub).next().map(|t| t.id);
            p.select_track(TrackKind::Sub, first).map_err(|e| e.to_string())?;
        }
        p.seek_to(0.0, true).map_err(|e| e.to_string())?;
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
            .map_err(|e| format!("圖形字幕跳到開頭：{e}"))?;
        p.set_speed(0.5).map_err(|e| e.to_string())?;
        p.set_pause(false).map_err(|e| e.to_string())?;
        let start = Instant::now();
        let mut last_cue = None;
        let shown = loop {
            if let Ok(cue) = p.get_f64("sub-start")
                && last_cue != Some(cue)
            {
                last_cue = Some(cue);
                p.set_pause(true).map_err(|e| e.to_string())?;
                if bitmap_sub_drawn(&mut p, &s.id)? {
                    break true;
                }
                p.set_pause(false).map_err(|e| e.to_string())?;
            }
            // 0.5 倍速播完 3 秒要 6 秒，每句還要截兩張圖：慢的機器上給兩倍的時間
            if start.elapsed() > 2 * TIMEOUT {
                break false;
            }
            if let Some(PlayerEvent::EndFile { .. }) = p.wait(Duration::from_millis(20)) {
                problems.push(format!("圖形字幕沒有畫出來（已經播到結尾；最後一句 {last_cue:?}）"));
                return Ok(info);
            }
        };
        p.set_pause(true).map_err(|e| e.to_string())?;
        p.set_speed(1.0).map_err(|e| e.to_string())?;
        if !shown {
            problems.push(format!("圖形字幕沒有畫出來（最後一句 {last_cue:?}）"));
        }
    }

    // 5. 跳到中間，再播到結尾（長度不明的直接從現在的位置播到結尾）
    if s.duration.is_some() {
        p.seek_to(duration * 0.5, true).map_err(|e| e.to_string())?;
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
            .map_err(|e| format!("跳到中間：{e}"))?;
        p.seek_to((duration - 0.4).max(0.0), true).map_err(|e| e.to_string())?;
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
            .map_err(|e| format!("跳到結尾前：{e}"))?;
    }
    p.set_pause(false).map_err(|e| e.to_string())?;
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Eof, ..
        }) => {}
        Ok(other) => problems.push(format!("沒有正常播到結尾：{other:?}")),
        Err(e) => problems.push(format!("播到結尾：{e}")),
    }

    Ok(info)
}

/// 目前這一格的截圖（`subtitles` = 含字幕），解成像素
fn snapshot(p: &mut Player, id: &str, subtitles: bool) -> Result<Vec<u8>, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("vitascope-formats-{}-{seq}-{id}.png", std::process::id()));
    p.screenshot_to_file(seq, &path.to_string_lossy(), subtitles)
        .map_err(|e| e.to_string())?;
    let mut reply = None;
    p.wait_for(TIMEOUT, |e| match e {
        PlayerEvent::CommandReply { id, error } if *id == seq => {
            reply = Some(error.clone());
            true
        }
        _ => false,
    })
    .map_err(|e| format!("截圖：{e}"))?;
    if let Some(Some(e)) = reply {
        return Err(format!("截圖失敗：{e}"));
    }
    let read = || -> Result<Vec<u8>, String> {
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG 太大")?];
        let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(info.buffer_size());
        Ok(buf)
    };
    let pixels = read();
    let _ = std::fs::remove_file(&path);
    pixels
}

/// 圖形字幕真的畫出來了：同一格含字幕的截圖跟不含字幕的有明顯不同（至少幾百個位元組差超過 64）
fn bitmap_sub_drawn(p: &mut Player, id: &str) -> Result<bool, String> {
    let video = snapshot(p, id, false)?;
    let subs = snapshot(p, id, true)?;
    if video.len() != subs.len() {
        return Err(format!("兩張截圖大小不同：{} / {}", video.len(), subs.len()));
    }
    let changed = video.iter().zip(&subs).filter(|(a, b)| a.abs_diff(**b) > 64).count();
    Ok(changed >= 300)
}

#[test]
fn format_matrix() {
    let samples = load_manifest();
    assert!(!samples.is_empty(), "沒有符合條件的樣本");

    // 每個樣本各自一個 mpv，平行跑
    let queue = Mutex::new(samples.clone());
    let results = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(sample) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let outcome = check(&sample);
                    results.lock().unwrap().push(outcome);
                }
            });
        }
    });

    let mut results = results.into_inner().unwrap();
    let tier_order = |t: &str| ["common", "general", "rare"].iter().position(|x| *x == t).unwrap_or(9);
    results.sort_by(|a, b| (tier_order(&a.sample.tier), &a.sample.id).cmp(&(tier_order(&b.sample.tier), &b.sample.id)));

    println!("\n格式測試矩陣（{} 個樣本）", results.len());
    let mut current_tier = "";
    for r in &results {
        if r.sample.tier != current_tier {
            current_tier = &r.sample.tier;
            let name = match current_tier {
                "common" => "常見",
                "general" => "通用",
                "rare" => "罕見",
                other => other,
            };
            println!("\n── {name} ──");
        }
        let mark = if r.problems.is_empty() { "✔" } else { "✘" };
        let note = if r.sample.note.is_empty() {
            String::new()
        } else {
            format!("  ({})", r.sample.note)
        };
        println!("{mark} {:<28} {}{note}", r.sample.id, r.info);
        for p in &r.problems {
            println!("      ↳ {p}");
        }
    }

    println!();
    for tier in ["common", "general", "rare"] {
        let in_tier: Vec<_> = results.iter().filter(|r| r.sample.tier == tier).collect();
        if in_tier.is_empty() {
            continue;
        }
        let ok = in_tier.iter().filter(|r| r.problems.is_empty()).count();
        println!("{tier:<8} {ok}/{} 通過", in_tier.len());
    }

    let blocking: Vec<_> = results
        .iter()
        .filter(|r| !r.problems.is_empty() && REQUIRED_TIERS.contains(&r.sample.tier.as_str()))
        .map(|r| r.sample.id.as_str())
        .collect();
    assert!(blocking.is_empty(), "常見 / 通用樣本失敗：{blocking:?}");
}
