//! 匯出（片段、GIF、縮圖總覽圖）：背景工作的取消與暫存檔、兩個工作同名不衝突、寫好之後重新打開檢查、
//! mpv 的記錄對應到原因、片段（不重新編碼：關鍵影格對齊、各種格式、只有聲音、旋轉、磁碟快取、
//! 網路影片）、GIF（EDL 的一段、TS 從 A 那一格開始、大小、轉正、HDR、燒進字幕、濾鏡失敗、取消）、
//! 縮圖總覽圖（格線與大小、標頭、時間標記、TS 的每一格就是標記的那一格、只取 A-B、轉正、HDR、濾鏡失敗、取消、逾時、網路影片）
//! （headless：不出畫面、不出聲音，三個平台的 CI 都跑；網路只連本機的測試伺服器 127.0.0.1）。
//!
//! 只看 mpv 自己的訊息：FFmpeg 的記錄只送到第一個建立的 mpv，這裡不檢查 FFmpeg 的文字

mod support;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};
use support::fake_ytdl::{FakeResolver, site_video_json};
use support::http::Server;
use vitascope::export::clip::{self, ClipSpec, Container, Source, StreamPick};
use vitascope::export::gif::{self, GifSpec};
use vitascope::export::sheet::{self, SheetSpec};
use vitascope::export::{
    self, ClipFormat, Done, Expect, Failure, GifPrefs, ImageFormat, Job, JobEvent, Kind, LogLine, LogTail, Note, Phase,
    Progress, SheetPrefs, map_mpv_error, verify_media,
};
use vitascope::geometry::Geometry;
use vitascope::instance::Wake;
use vitascope::mpv::{Event, Mpv};
use vitascope::net::{self, NetSettings};
use vitascope::picture::{Deinterlace, ToneSettings};
use vitascope::player::{EngineCaps, Options, Player, PlayerEvent, Track, TrackKind};
use vitascope::save;
use vitascope::settings::SubStyle;
use vitascope::ytdl::Resolve;

/// CI 的機器比開發的電腦慢很多：等條件成立，給足時間
const TIMEOUT: Duration = Duration::from_secs(30);

fn sample(rel: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p
}

/// 這個測試專用的暫存資料夾（每次重建）
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vitascope-export-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 資料夾裡影戲的暫存檔
fn parts(dir: &Path) -> Vec<String> {
    names(dir).into_iter().filter(|n| save::is_part(n)).collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn no_wake() -> Wake {
    Arc::new(|| {})
}

/// 等到工作結束，回傳結果（途中的進度另外收集）
fn finish(job: &Job) -> (Vec<Progress>, Result<Done, Failure>) {
    let deadline = Instant::now() + TIMEOUT;
    let mut progress = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "等不到工作結束");
        match job.try_recv() {
            Some(JobEvent::Progress(p)) => progress.push(p),
            Some(JobEvent::Finished(r)) => return (progress, r),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn done(kind: Kind, path: PathBuf) -> Done {
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Done {
        kind,
        path,
        bytes,
        actual: None,
        notes: Vec::new(),
    }
}

// ───────────── 背景工作 ─────────────

#[test]
fn dropping_a_job_mid_run_leaves_no_partial_file() {
    let dir = scratch("drop");
    let (started_tx, started_rx) = mpsc::channel();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let temp = ctl.temp_in(&d, "片段", "mkv");
        std::fs::write(&temp, b"partial").unwrap();
        started_tx.send(()).unwrap();
        // 讀到一半（等著被取消）
        while !ctl.cancelled() {
            std::thread::sleep(Duration::from_millis(5));
        }
        stopped_tx.send(()).unwrap();
        Err(Failure::Cancelled)
    });
    started_rx.recv_timeout(TIMEOUT).expect("工作沒有開始");
    assert_eq!(parts(&dir).len(), 1, "寫到一半有暫存檔");
    // 關閉影戲、介面測試結束：丟掉 Job 就要取消（工作停下來，不是只刪檔案）、刪掉暫存檔
    drop(job);
    stopped_rx
        .recv_timeout(TIMEOUT)
        .expect("丟掉 Job 沒有取消工作（匯出用的 mpv 會一直讀寫下去）");
    assert!(names(&dir).is_empty(), "留下了 {:?}", names(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_job_stuck_writing_still_loses_its_partial_file() {
    // 寫檔中不能中斷（片段的 dump-cache）：等不到工作結束，也要刪掉暫存檔
    let dir = scratch("stuck");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (ended_tx, ended_rx) = mpsc::channel();
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let temp = ctl.temp_in(&d, "片段", "mkv");
        std::fs::write(&temp, b"partial").unwrap();
        ctl.set_writing(true);
        started_tx.send(()).unwrap();
        // 不理會取消，直到測試放行
        let _ = release_rx.recv_timeout(TIMEOUT);
        ctl.set_writing(false);
        let result = ctl.finish(&d.join("片段.mkv")).map(|p| done(Kind::Clip, p));
        ended_tx.send(result.clone()).unwrap();
        result
    });
    started_rx.recv_timeout(TIMEOUT).expect("工作沒有開始");
    assert!(!job.cancellable(), "寫檔中不能取消");
    job.abandon(Duration::from_millis(50));
    assert!(
        parts(&dir).is_empty(),
        "等不到工作結束也要刪掉暫存檔：{:?}",
        names(&dir)
    );
    // 工作之後才結束：暫存檔已經不在，不會變成正式的檔案
    release_tx.send(()).unwrap();
    let result = ended_rx.recv_timeout(TIMEOUT).expect("工作沒有結束");
    assert_eq!(result, Err(Failure::Cancelled));
    assert!(names(&dir).is_empty(), "留下了 {:?}", names(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 像 FFmpeg 那樣開暫存檔來寫：Windows 上不允許別人刪除（`_wsopen(..., SH_DENYNO)`，沒有 FILE_SHARE_DELETE）
fn open_like_ffmpeg(path: &Path) -> std::fs::File {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE
        o.share_mode(0x1 | 0x2);
    }
    o.open(path).unwrap()
}

/// 等到資料夾裡沒有任何檔案（工作的執行緒結束時才刪；CI 很慢，給足時間）
fn wait_empty(dir: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while !names(dir).is_empty() {
        assert!(Instant::now() < deadline, "留下了 {:?}", names(dir));
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_partial_file_the_writer_still_holds_is_removed_when_the_job_ends() {
    use std::io::Write;
    // 放棄工作時 mpv 還開著暫存檔（Windows 上刪不掉）：登記要留著，工作結束時再刪，也不能變成正式的檔案
    let dir = scratch("held");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (ended_tx, ended_rx) = mpsc::channel();
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let temp = ctl.temp_in(&d, "片段", "mkv");
        let mut file = open_like_ffmpeg(&temp);
        file.write_all(b"partial").unwrap();
        ctl.set_writing(true);
        started_tx.send(()).unwrap();
        // 不理會取消，直到測試放行
        let _ = release_rx.recv_timeout(TIMEOUT);
        file.write_all(b", then the rest").unwrap();
        drop(file);
        ctl.set_writing(false);
        let result = ctl.finish(&d.join("片段.mkv")).map(|p| done(Kind::Clip, p));
        ended_tx.send(result.clone()).unwrap();
        result
    });
    started_rx.recv_timeout(TIMEOUT).expect("工作沒有開始");
    job.abandon(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    let result = ended_rx.recv_timeout(TIMEOUT).expect("工作沒有結束");
    assert_eq!(result, Err(Failure::Cancelled), "放棄之後寫完的也不能變成正式的檔案");
    wait_empty(&dir);

    // 暫存檔在放棄工作之後才建立（mpv 剛好在等待結束之後才開檔）：工作結束時也要刪掉
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (ended_tx, ended_rx) = mpsc::channel();
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let temp = ctl.temp_in(&d, "晚到", "mkv");
        started_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(TIMEOUT);
        std::fs::write(&temp, b"late").unwrap();
        ended_tx.send(()).unwrap();
        ctl.check()?;
        Ok(done(Kind::Clip, temp))
    });
    started_rx.recv_timeout(TIMEOUT).expect("工作沒有開始");
    job.abandon(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    ended_rx.recv_timeout(TIMEOUT).expect("工作沒有結束");
    wait_empty(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_and_crashed_jobs_remove_their_partial_file() {
    let dir = scratch("fail");
    let d = dir.clone();
    let job = Job::spawn(Kind::Gif, no_wake(), move |ctl| {
        std::fs::write(ctl.temp_in(&d, "a", "gif"), b"x").unwrap();
        Err(Failure::FilterFailed)
    });
    assert_eq!(finish(&job).1, Err(Failure::FilterFailed));
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));
    let d = dir.clone();
    let job = Job::spawn(Kind::Sheet, no_wake(), move |ctl| {
        std::fs::write(ctl.temp_in(&d, "b", "jpg"), b"x").unwrap();
        panic!("測試：匯出的程式出錯");
    });
    assert_eq!(finish(&job).1, Err(Failure::Crashed));
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn finishing_without_a_written_file_is_an_error() {
    // 沒有登記暫存檔：程式寫錯了，不給使用者看英文的內部說明
    let dir = scratch("unregistered");
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let path = ctl.finish(&d.join("a.mkv"))?;
        Ok(done(Kind::Clip, path))
    });
    assert_eq!(finish(&job).1, Err(Failure::Crashed));
    std::fs::remove_dir_all(&dir).unwrap();
    // 寫檔的程式什麼都沒寫出來：不能變成「暫存檔留在…」，也不能出現正式的檔案
    let dir = scratch("nothing");
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        ctl.temp_in(&d, "a", "mkv");
        let path = ctl.finish(&d.join("a.mkv"))?;
        Ok(done(Kind::Clip, path))
    });
    let result = finish(&job).1;
    assert!(matches!(result, Err(Failure::Io(_))), "{result:?}");
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_complete_file_that_cannot_be_renamed_is_kept() {
    // 寫好了但換不成正式的名稱（這裡：目的地的資料夾不見了）：完整的暫存檔留著，原因裡寫出它在哪裡，
    // 工作結束時不能被當成失敗的暫存檔刪掉
    let dir = scratch("keep");
    let d = dir.clone();
    let (temp_tx, temp_rx) = mpsc::channel();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let temp = ctl.temp_in(&d, "片段", "mkv");
        std::fs::write(&temp, b"complete clip").unwrap();
        temp_tx.send(temp).unwrap();
        let path = ctl.finish(&d.join("不見的資料夾").join("片段.mkv"))?;
        Ok(done(Kind::Clip, path))
    });
    let temp = temp_rx.recv_timeout(TIMEOUT).expect("工作沒有開始");
    // 執行緒在送出結果之前就刪掉還登記著的暫存檔：收到結果時已經定案
    let result = finish(&job).1;
    match &result {
        Err(Failure::Finish { temp: kept, .. }) => assert_eq!(kept, &temp),
        other => panic!("應該是換不成名稱：{other:?}"),
    }
    assert_eq!(
        std::fs::read(&temp).ok().as_deref(),
        Some(&b"complete clip"[..]),
        "完整的檔案要留著：{:?}",
        names(&dir)
    );
    assert!(!dir.join("不見的資料夾").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_progress_and_result_reach_the_ui() {
    let dir = scratch("progress");
    let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = woken.clone();
    let wake: Wake = Arc::new(move || {
        w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let d = dir.clone();
    let job = Job::spawn(Kind::Clip, wake, move |ctl| {
        for phase in [Phase::Reading, Phase::Writing, Phase::Checking] {
            for i in 0..5 {
                ctl.progress(Progress {
                    phase,
                    fraction: Some(i as f32 / 5.0),
                });
            }
        }
        std::fs::write(ctl.temp_in(&d, "a", "mkv"), b"clip").unwrap();
        let path = ctl.finish(&d.join("a.mkv"))?;
        Ok(done(Kind::Clip, path))
    });
    let (progress, result) = finish(&job);
    // 「結束」的消息送出之後才叫醒介面、執行緒才結束：等到成立（CI 很慢）
    let deadline = Instant::now() + TIMEOUT;
    while !(job.is_finished() && woken.load(std::sync::atomic::Ordering::SeqCst) >= 4) {
        assert!(Instant::now() < deadline, "執行緒沒有結束，或沒有叫醒介面");
        std::thread::sleep(Duration::from_millis(10));
    }
    let done = result.unwrap();
    assert_eq!(done.path, dir.join("a.mkv"));
    assert_eq!(done.bytes, 4);
    assert_eq!(std::fs::read(&done.path).unwrap(), b"clip");
    // 換階段的那一次一定送到（同一個階段太密的會略過），順序不變
    let mut phases: Vec<Phase> = progress.iter().map(|p| p.phase).collect();
    phases.dedup();
    assert_eq!(phases, [Phase::Reading, Phase::Writing, Phase::Checking]);
    assert!(
        woken.load(std::sync::atomic::Ordering::SeqCst) >= 4,
        "進度、結束都要叫醒介面"
    );
    assert!(job.is_finished() && job.cancellable());
    assert_eq!(names(&dir), ["a.mkv"]);

    // 取消：工作在下一個檢查點停下
    let (started_tx, started_rx) = mpsc::channel();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        started_tx.send(()).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            ctl.check()?;
            assert!(Instant::now() < deadline, "等不到取消");
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    started_rx.recv_timeout(TIMEOUT).unwrap();
    job.cancel();
    assert_eq!(finish(&job).1, Err(Failure::Cancelled));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn two_jobs_on_the_same_name_do_not_collide() {
    // 兩個工作同時匯出同一部影片的同一段（例如兩個影戲）：暫存檔不同，正式的檔案一個加上「(2)」
    let dir = scratch("same");
    let barrier = Arc::new(Barrier::new(2));
    let spawn = |content: &'static [u8]| {
        let (d, b) = (dir.clone(), barrier.clone());
        Job::spawn(Kind::Clip, no_wake(), move |ctl| {
            let temp = ctl.temp_in(&d, "第1集 00.00.01-00.00.02", "mkv");
            std::fs::write(&temp, content).unwrap();
            // 兩個暫存檔同時存在之後才一起換名稱
            b.wait();
            let path = ctl.finish(&d.join("第1集 00.00.01-00.00.02.mkv"))?;
            Ok(done(Kind::Clip, path))
        })
    };
    let (a, b) = (spawn(b"first"), spawn(b"second"));
    let a = finish(&a).1.unwrap();
    let b = finish(&b).1.unwrap();
    assert_ne!(a.path, b.path);
    assert_eq!(std::fs::read(&a.path).unwrap(), b"first");
    assert_eq!(std::fs::read(&b.path).unwrap(), b"second");
    assert_eq!(
        names(&dir),
        ["第1集 00.00.01-00.00.02 (2).mkv", "第1集 00.00.01-00.00.02.mkv"]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────── 留下的暫存檔 ─────────────

#[test]
fn leftovers_are_swept_from_the_cache_and_the_export_folders() {
    let root = scratch("sweep");
    let cache = export::cache_dir(&root);
    let clips = root.join("clips");
    let images = root.join("images");
    for d in [&cache, &clips, &images] {
        std::fs::create_dir_all(d).unwrap();
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    let make = |p: &Path, aged: bool| {
        std::fs::write(p, b"x").unwrap();
        if aged {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    };
    let gone = [
        save::temp_in(&cache, "a", "mkv"),
        cache.join("mpv-cache-AbC123.dat"),
        save::temp_in(&clips, "b", "mp4"),
        save::temp_in(&images, "c", "gif"),
    ];
    let kept = [
        (save::temp_in(&clips, "還在寫", "mkv"), false),
        (clips.join("使用者的影片.mkv"), true),
        (cache.join("notes.txt"), true),
        (cache.join("mpv-cache-new.dat"), false),
    ];
    for p in &gone {
        make(p, true);
    }
    for (p, aged) in &kept {
        make(p, *aged);
    }
    // 同一個資料夾給兩次只清一次
    let removed = export::sweep_leftovers(&root, &[clips.clone(), images.clone(), clips.clone()]);
    assert_eq!(removed, gone.len());
    for p in &gone {
        assert!(!p.exists(), "{} 要刪掉", p.display());
    }
    for (p, _) in &kept {
        assert!(p.exists(), "{} 不能刪", p.display());
    }
    std::fs::remove_dir_all(&root).unwrap();
}

// ───────────── 寫好之後檢查 ─────────────

#[test]
fn verify_accepts_a_good_file_and_checks_tracks_and_length() {
    let mp4 = sample("common/mp4_h264_aac.mp4");
    let v = verify_media(
        &mp4,
        &Expect {
            video: Some("h264".into()),
            audio: Some("aac".into()),
            sub: None,
            min_len: 2.0,
        },
    )
    .unwrap();
    assert!((v.duration - 3.0).abs() < 0.2, "{}", v.duration);
    assert!(v.tracks.iter().any(|t| t.kind == TrackKind::Video));
    assert_eq!(
        verify_media(
            &mp4,
            &Expect {
                min_len: 10.0,
                ..Default::default()
            }
        )
        .map(|v| v.duration),
        Err(Failure::TooShort {
            got: v.duration,
            want: 10.0
        })
    );
    assert_eq!(
        verify_media(
            &mp4,
            &Expect {
                audio: Some("opus".into()),
                ..Default::default()
            }
        )
        .map(|v| v.duration),
        Err(Failure::MissingTrack(TrackKind::Audio))
    );
    // Matroska 裡的 WebVTT：mpv 自己的分離器叫 webvtt-webm，跟 FFmpeg 的 webvtt 算同一種
    let vtt = sample("general/mkv_h264_aac_webvtt.mkv");
    verify_media(
        &vtt,
        &Expect {
            sub: Some("webvtt".into()),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn verify_does_not_pick_up_files_next_to_the_clip() {
    // 片段存在使用者的資料夾：旁邊同名的字幕、音軌不能被當成片段的軌道
    let with_ext_sub = sample("common/extsub_srt_utf8.mp4");
    assert_eq!(
        verify_media(
            &with_ext_sub,
            &Expect {
                sub: Some("subrip".into()),
                ..Default::default()
            }
        )
        .map(|v| v.duration),
        Err(Failure::MissingTrack(TrackKind::Sub))
    );
    // 旁邊同名的音軌：libmpv 預設就不找，匯出用的 mpv 另外明確關掉（不靠預設值）。
    // 先確認樣本旁邊真的有會被找到的音軌，檢查才有意義
    let with_ext_audio = sample("common/mkv_extaudio.mkv");
    let audio = |tracks: &[Track]| tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
    let found = tracks_with(&with_ext_audio, &[("audio-file-auto", "exact")]);
    assert_eq!(audio(&found), 2, "樣本旁邊應該有同名的音軌：{found:?}");
    let v = verify_media(&with_ext_audio, &Expect::default()).unwrap();
    assert_eq!(audio(&v.tracks), 1, "{:?}", v.tracks);
}

/// 用一個不出畫面、不出聲音的 mpv（加上 `extra` 選項）打開 `path`，回傳軌道
fn tracks_with(path: &Path, extra: &[(&str, &str)]) -> Vec<Track> {
    let mut opts = vec![("vo", "null"), ("ao", "null"), ("idle", "yes"), ("pause", "yes")];
    opts.extend_from_slice(extra);
    let mpv = Mpv::new(&opts).unwrap();
    mpv.command(&["loadfile", path.to_str().unwrap()]).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "打不開 {}", path.display());
        match mpv.wait_event(0.5) {
            Some(Event::FileLoaded) => break,
            Some(Event::EndFile { .. }) => panic!("打不開 {}", path.display()),
            _ => {}
        }
    }
    serde_json::from_str(&mpv.get_string("track-list").unwrap()).unwrap()
}

#[test]
fn verify_rejects_broken_and_missing_files() {
    let dir = scratch("verify");
    let junk = dir.join("壞掉的.mkv");
    std::fs::write(&junk, vec![0x5au8; 64 * 1024]).unwrap();
    let r = verify_media(&junk, &Expect::default());
    assert!(matches!(r, Err(Failure::Unplayable(_))), "{r:?}");
    let r = verify_media(&dir.join("沒有這個檔案.mkv"), &Expect::default());
    assert!(matches!(r, Err(Failure::Unplayable(_))), "{r:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────── mpv 的記錄 ─────────────

#[test]
fn a_dump_to_a_missing_folder_maps_to_a_write_permission_failure() {
    // 對照的是 mpv 原始碼裡的字串：這裡用真的引擎（含 Linux 的系統 libmpv 0.37）確認寫法沒變
    let src = sample("common/mp4_h264_aac.mp4");
    let mut opts = vec![
        ("vo", "null"),
        ("ao", "null"),
        ("idle", "yes"),
        ("pause", "yes"),
        ("cache", "yes"),
        ("demuxer", "lavf"),
        ("sid", "no"),
    ];
    opts.extend_from_slice(export::INSTANCE_OPTIONS);
    let mpv = Mpv::new(&opts).unwrap();
    mpv.request_log_messages("warn").unwrap();
    mpv.command(&["loadfile", src.to_str().unwrap()]).unwrap();
    let mut log = LogTail::default();
    // 等整個檔案進了快取
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "快取讀不到檔尾：{:?}", log.lines());
        while let Some(ev) = mpv.wait_event(0.0) {
            log.push_event(&ev);
        }
        let eof = mpv
            .get_string("demuxer-cache-state")
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["eof"] == true);
        if eof {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let dir = scratch("nodir");
    let out = dir.join("沒有這個資料夾").join("片段.mkv");
    mpv.command_async(1, &["dump-cache", "0.5", "2.0", out.to_str().unwrap()])
        .unwrap();
    // 記錄是非同步送來的：等到認得的原因出現（回覆之後可能還沒到）
    let deadline = Instant::now() + TIMEOUT;
    let mut replied = false;
    loop {
        if replied && log.failure() == Some(Failure::NoPermission(None)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "等不到「Failed opening output file」：{:?}",
            log.lines()
        );
        if let Some(ev) = mpv.wait_event(0.2) {
            replied |= matches!(ev, Event::CommandReply { id: 1, .. });
            log.push_event(&ev);
        }
    }
    assert!(!out.exists());
    assert_eq!(
        map_mpv_error(&log.lines()),
        Some(Failure::NoPermission(None)),
        "{:?}",
        log.lines()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────── 留下的完整檔案 ─────────────

#[test]
fn kept_files_are_never_swept() {
    // 換不成正式名稱留下的完整檔案（名稱還是暫存檔的樣子）：登記過，啟動時清暫存檔就算超過一小時也不刪
    let root = scratch("kept");
    let cache = export::cache_dir(&root);
    let clips = root.join("clips");
    std::fs::create_dir_all(&clips).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    let age = |p: &Path| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    };
    let d = clips.clone();
    let c = cache.clone();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        ctl.set_keep_dir(Some(c));
        let temp = ctl.temp_in(&d, "片段", "mkv");
        std::fs::write(&temp, b"complete clip").unwrap();
        // 換名稱失敗（目的地的資料夾不見了）
        let path = ctl.finish(&d.join("不見的資料夾").join("片段.mkv"))?;
        Ok(done(Kind::Clip, path))
    });
    let kept = match finish(&job).1 {
        Err(Failure::Finish { temp, .. }) => temp,
        other => panic!("應該是換不成名稱：{other:?}"),
    };
    assert_eq!(
        export::kept_files(&cache),
        std::slice::from_ref(&kept),
        "要登記在快取資料夾的清單裡"
    );
    // 一般的舊暫存檔照樣刪
    let leftover = save::temp_in(&clips, "當掉留下的", "mkv");
    std::fs::write(&leftover, b"x").unwrap();
    age(&leftover);
    age(&kept);
    assert_eq!(export::sweep_leftovers(&root, std::slice::from_ref(&clips)), 1);
    assert!(!leftover.exists());
    assert_eq!(
        std::fs::read(&kept).ok().as_deref(),
        Some(&b"complete clip"[..]),
        "登記過的完整檔案不能刪"
    );
    // 資料夾的寫法不同（結尾多一個斜線）也認得
    let mut spelled = clips.clone().into_os_string();
    spelled.push(std::path::MAIN_SEPARATOR_STR);
    assert_eq!(export::sweep_leftovers(&root, &[PathBuf::from(spelled)]), 0);
    assert!(kept.exists());
    // 使用者改名之後：從清單拿掉
    let renamed = clips.join("改好的名稱.mkv");
    std::fs::rename(&kept, &renamed).unwrap();
    assert!(export::kept_files(&cache).is_empty());
    assert_eq!(export::sweep_leftovers(&root, std::slice::from_ref(&clips)), 0);
    assert!(renamed.exists());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn kept_files_on_an_offline_drive_stay_listed() {
    // 留下的完整檔案在隨身碟、網路磁碟上：啟動時那個磁碟還沒接上（資料夾不在，系統一樣回報「找不到」），
    // 清單不能拿掉它，不然接回來之後會被當成舊暫存檔刪掉
    let root = scratch("kept-offline");
    let cache = export::cache_dir(&root);
    let drive = root.join("隨身碟");
    let clips = drive.join("片段");
    std::fs::create_dir_all(&clips).unwrap();
    let kept = save::temp_in(&clips, "片段", "mkv");
    std::fs::write(&kept, b"complete clip").unwrap();
    export::keep_file(&cache, &kept).unwrap();
    // 拔掉
    let away = root.join("拔掉的隨身碟");
    std::fs::rename(&drive, &away).unwrap();
    assert_eq!(
        export::kept_files(&cache),
        std::slice::from_ref(&kept),
        "資料夾不在：當成還在"
    );
    assert_eq!(export::sweep_leftovers(&root, std::slice::from_ref(&clips)), 0);
    // 接回來：就算超過一小時也不刪
    std::fs::rename(&away, &drive).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&kept)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 3600))
        .unwrap();
    assert_eq!(export::sweep_leftovers(&root, std::slice::from_ref(&clips)), 0);
    assert_eq!(std::fs::read(&kept).unwrap(), b"complete clip");
    assert_eq!(export::kept_files(&cache), std::slice::from_ref(&kept));
    std::fs::remove_dir_all(&root).unwrap();
}

// ───────────── 片段 ─────────────

/// 片段的工作最多等多久（CI 的機器很慢；網路的要讀兩次）
const CLIP_TIMEOUT: Duration = Duration::from_secs(180);

/// 主播放器（不出畫面、不出聲音、停在開頭）：先問播放引擎的功能（要在開檔之前），再開 `src`
fn main_player(src: &str) -> (Player, EngineCaps) {
    main_player_with(
        Options {
            extra: vec![("pause".into(), "yes".into())],
            ..Options::headless()
        },
        src,
    )
}

fn main_player_with(opts: Options, src: &str) -> (Player, EngineCaps) {
    let mut p = Player::new(opts).expect("建立 mpv 失敗");
    let caps = p.probe_caps();
    assert!(caps.dump_cache, "播放引擎沒有 dump-cache");
    p.open(src).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {src}：{e}"));
    // 等 mpv 選好軌道（慢的機器上軌道清單比選好的軌道早到）：預設的片段軌道是主播放器選的。
    // 路徑也是另外送來的屬性，可能比 FileLoaded 晚到：匯出要從它找來源（沒有時是 NoData）
    p.wait_state(TIMEOUT, |s| {
        s.loaded
            && s.path.is_some()
            && s.duration.is_some()
            && (s.selected(TrackKind::Video).is_some() || s.selected(TrackKind::Audio).is_some())
    })
    .unwrap_or_else(|e| panic!("{src}：{e}"));
    (p, caps)
}

/// 同 `main_player`，開檔前先套用「設定 → 網路」的設定
fn main_player_net(s: &NetSettings, src: &str) -> (Player, EngineCaps) {
    let mut p = Player::new(Options {
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    let caps = p.probe_caps();
    let opts = net::mpv_options(s, &p.net_defaults());
    for (name, _, r) in p.apply_net(&opts, true) {
        r.unwrap_or_else(|e| panic!("無法設定 {name}：{e}"));
    }
    p.open(src).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {src}：{e}"));
    // 等 mpv 選好軌道（慢的機器上軌道清單比選好的軌道早到）：預設的片段軌道是主播放器選的。
    // 路徑也是另外送來的屬性，可能比 FileLoaded 晚到：匯出要從它找來源（沒有時是 NoData）
    p.wait_state(TIMEOUT, |s| {
        s.loaded
            && s.path.is_some()
            && s.duration.is_some()
            && (s.selected(TrackKind::Video).is_some() || s.selected(TrackKind::Audio).is_some())
    })
    .unwrap_or_else(|e| panic!("{src}：{e}"));
    (p, caps)
}

fn sample_str(rel: &str) -> String {
    sample(rel).to_string_lossy().into_owned()
}

/// 片段的資料夾與磁碟快取的資料夾（分開：確認片段的資料夾裡只有片段）
fn clip_dirs(name: &str) -> (PathBuf, PathBuf) {
    (scratch(name), scratch(&format!("{name}-cache")))
}

fn clip_spec(p: &Player, caps: &EngineCaps, a: f64, b: f64, format: ClipFormat, dirs: &(PathBuf, PathBuf)) -> ClipSpec {
    ClipSpec::from_player(p, caps, a, b, format, dirs.0.clone(), dirs.1.clone())
        .unwrap_or_else(|f| panic!("不能存片段：{f:?}"))
}

/// 匯出這個片段，等到結束
fn export_clip(spec: ClipSpec) -> Result<Done, Failure> {
    let job = clip::spawn(spec, no_wake());
    let deadline = Instant::now() + CLIP_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "等不到片段匯出完");
        match job.try_recv() {
            Some(JobEvent::Finished(r)) => return r,
            Some(JobEvent::Progress(_)) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// 用另一個 mpv 打開寫好的片段：軌道、長度
fn reopen(path: &Path) -> (Vec<Track>, f64) {
    let mpv = Mpv::new(&[
        ("vo", "null"),
        ("ao", "null"),
        ("idle", "yes"),
        ("pause", "yes"),
        ("sub-auto", "no"),
        ("audio-file-auto", "no"),
    ])
    .unwrap();
    mpv.command(&["loadfile", path.to_str().unwrap()]).unwrap();
    wait_loaded(&mpv, path);
    let tracks = serde_json::from_str(&mpv.get_string("track-list").unwrap()).unwrap();
    let duration = mpv.get_property::<f64>("duration").unwrap();
    (tracks, duration)
}

fn wait_loaded(mpv: &Mpv, path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "打不開 {}", path.display());
        match mpv.wait_event(0.5) {
            Some(Event::FileLoaded) => return,
            Some(Event::EndFile { .. }) => panic!("打不開 {}", path.display()),
            _ => {}
        }
    }
}

/// 片段裡某一種軌道最後一個封包的時間（只選那一種軌道、整個讀進快取）：
/// 聲音比影像早很多結束，就是結尾有一段沒有聲音的影像
fn stream_end(path: &Path, kind: TrackKind) -> f64 {
    let (vid, aid) = match kind {
        TrackKind::Video => ("auto", "no"),
        _ => ("no", "auto"),
    };
    let mpv = Mpv::new(&[
        ("vo", "null"),
        ("ao", "null"),
        ("idle", "yes"),
        ("pause", "yes"),
        ("cache", "yes"),
        ("vid", vid),
        ("aid", aid),
        ("sid", "no"),
    ])
    .unwrap();
    mpv.command(&["loadfile", path.to_str().unwrap()]).unwrap();
    wait_loaded(&mpv, path);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "{} 讀不到檔尾", path.display());
        while mpv.wait_event(0.0).is_some() {}
        let state: serde_json::Value = serde_json::from_str(&mpv.get_string("demuxer-cache-state").unwrap()).unwrap();
        if state["eof"] == true
            && let Some(end) = state["cache-end"].as_f64()
        {
            return end;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn codecs(tracks: &[Track], kind: TrackKind) -> Vec<String> {
    tracks
        .iter()
        .filter(|t| t.kind == kind && !t.albumart)
        .map(|t| t.codec.clone().unwrap_or_default())
        .collect()
}

#[test]
fn clip_keyframe_aligned() {
    // 每 2 秒一個關鍵影格：選 3.0–6.5，實際是 2.0（之前的關鍵影格）到 8.0（下一個關鍵影格之前的最後一格 + 一格）
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("aligned");
    let spec = clip_spec(&p, &caps, 3.0, 6.5, ClipFormat::Auto, &dirs);
    assert_eq!(spec.container, Container::Mkv);
    assert!(spec.align, "播放引擎有 ab-loop-align-cache");
    let done = export_clip(spec).unwrap();
    assert_eq!(done.kind, Kind::Clip);
    assert_eq!(done.path, dirs.0.join("mkv_h264_gop2 00.00.03-00.00.06.mkv"));
    let (a, b) = done.actual.unwrap();
    assert!((a - 2.0).abs() < 0.05, "起點對齊到之前的關鍵影格：{a}");
    assert!((b - 8.0).abs() < 0.15, "終點是下一個關鍵影格：{b}");
    assert!(done.notes.contains(&Note::KeyframeAligned));
    let (tracks, duration) = reopen(&done.path);
    assert!((duration - 6.0).abs() < 0.15, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"]);
    // 聲音跟影像一起結束（沒對齊的話聲音停在 6.5，最後 1.5 秒沒有聲音）
    let audio = stream_end(&done.path, TrackKind::Audio);
    let video = stream_end(&done.path, TrackKind::Video);
    assert!(audio >= 5.8, "聲音太早結束：{audio}（影像 {video}）");
    assert_eq!(done.bytes, std::fs::metadata(&done.path).unwrap().len());
    assert_eq!(names(&dirs.0), ["mkv_h264_gop2 00.00.03-00.00.06.mkv"], "不留暫存檔");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_reads_without_decoding_up_to_the_start() {
    // 從 A 前面 5 秒開始讀（3.0）：匯出用的 mpv 不精確跳轉（hr-seek=no），停在之前的關鍵影格（2.0），
    // 不會從關鍵影格一路解碼到 3.0；解碼只用一個執行緒（不搶正在播放的那一個的 CPU）
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("nodecode");
    let mut spec = clip_spec(&p, &caps, 8.0, 9.0, ClipFormat::Auto, &dirs);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        s.lock().unwrap().push((
            mpv.get_property::<f64>("time-pos").unwrap_or(f64::NAN),
            mpv.get_string("vd-lavc-threads").unwrap_or_default(),
        ));
    }));
    export_clip(spec).unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let (pos, threads) = &seen[0];
    assert!(*pos < 2.5, "開頭解碼到了 {pos}（應該停在 2.0 的關鍵影格）");
    assert_eq!(threads, "1");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_rereads_from_earlier_when_the_seek_lands_after_the_keyframe() {
    // 每 10 秒一個關鍵影格的 TS：從 3.0 開始讀（A = 8 的前 5 秒）時，用時間跳轉落在 0 秒的關鍵影格之後，
    // 讀到的範圍從 10 秒的關鍵影格才開始（A 不在裡面）：從更前面重讀一次，片段從 0 秒的關鍵影格開始
    let (p, caps) = main_player(&sample_str("general/ts_h264_gop10.ts"));
    let dirs = clip_dirs("earlier");
    let mut spec = clip_spec(&p, &caps, 8.0, 9.0, ClipFormat::Auto, &dirs);
    assert_eq!(spec.container, Container::Ts);
    let starts = Arc::new(Mutex::new(Vec::new()));
    let s = starts.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        s.lock().unwrap().push((
            mpv.get_string("start").unwrap_or_default(),
            mpv.get_property::<f64>("time-pos").unwrap_or(f64::NAN),
        ));
    }));
    let done = export_clip(spec).unwrap();
    let (a, b) = done.actual.unwrap();
    let starts = starts.lock().unwrap().clone();
    assert!(
        a < 0.1,
        "片段要從 A 之前的關鍵影格（0 秒）開始：{a}–{b}（讀了 {starts:?}）"
    );
    // 確定真的重讀了：第一次停在 10 秒的關鍵影格、第二次從頭讀。
    // 別的播放引擎的 TS 跳轉剛好落在 0 秒的關鍵影格時，這裡測不到重讀：寫出來，不要默默通過
    if starts.first().is_some_and(|(_, pos)| *pos < 5.0) {
        eprintln!("略過重讀的檢查：這個播放引擎的跳轉落在 0 秒的關鍵影格（{starts:?}）");
    } else {
        assert_eq!(starts.len(), 2, "要從更前面重讀一次：{starts:?}");
        assert_ne!(starts[0].0, starts[1].0, "{starts:?}");
    }
    assert!((b - 10.0).abs() < 0.2, "到下一個關鍵影格：{b}");
    let (tracks, duration) = reopen(&done.path);
    assert!((duration - 10.0).abs() < 0.2, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_starts_at_the_first_cached_keyframe_when_a_is_not_cached() {
    // 最後的退路（不重讀時，或重讀之後還是讀不到 A 之前）：從讀到的第一個關鍵影格（10 秒）開始。
    // 長度的檢查、完成時的實際範圍都要用這個起點（不是使用者選的 A）
    let (p, caps) = main_player(&sample_str("general/ts_h264_gop10.ts"));
    let dirs = clip_dirs("firstcached");
    let mut spec = clip_spec(&p, &caps, 8.0, 14.0, ClipFormat::Auto, &dirs);
    spec.test.no_earlier = true;
    let landed = Arc::new(Mutex::new(Vec::new()));
    let l = landed.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        l.lock()
            .unwrap()
            .push(mpv.get_property::<f64>("time-pos").unwrap_or(f64::NAN));
    }));
    let done = export_clip(spec).unwrap_or_else(|f| panic!("{f:?}（停在 {:?}）", landed.lock().unwrap()));
    let landed = landed.lock().unwrap().clone();
    let (a, b) = done.actual.unwrap();
    if landed.first().is_some_and(|pos| *pos < 5.0) {
        eprintln!("略過：這個播放引擎的跳轉落在 0 秒的關鍵影格（{landed:?}），A 在快取裡");
        assert!(a < 0.1, "{a}–{b}");
    } else {
        assert!((a - 10.0).abs() < 0.2, "起點是讀到的第一個關鍵影格：{a}–{b}");
        let (_, duration) = reopen(&done.path);
        assert!((b - a - duration).abs() < 0.2, "實際範圍 {a}–{b}、長度 {duration}");
        assert!(duration > 9.0, "長度 {duration}");
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_reads_past_b_when_the_keyframe_is_far_before_a() {
    // 每 30 秒一個關鍵影格：從 51 秒（A = 56 的前 5 秒）開始讀，不精確跳轉停在 30 秒的關鍵影格。
    // mpv 往前讀的秒數從解碼器拿到的封包（30 秒）算起：只給「段落 + 25 秒」的話讀到 57 秒就停，讀不到 B（58）
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop30.mkv"));
    let dirs = clip_dirs("longgop");
    let spec = clip_spec(&p, &caps, 56.0, 58.0, ClipFormat::Auto, &dirs);
    let done = export_clip(spec).unwrap();
    let (a, b) = done.actual.unwrap();
    assert!((a - 30.0).abs() < 0.05, "起點是 A 之前的關鍵影格：{a}");
    assert!((b - 60.0).abs() < 0.15, "終點是 B 之後的關鍵影格：{b}");
    let (tracks, duration) = reopen(&done.path);
    assert!((duration - 30.0).abs() < 0.15, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"]);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_does_not_wait_once_reading_stops_after_b() {
    // 分離器讀到 B 之後自己停了（往前讀的秒數到了，沒到檔尾、記憶體也沒滿）：馬上用讀到的，
    // 不等「很久沒有新資料」（20 秒）。測試把往前讀的秒數設成 8 秒：從頭讀到約 8 秒就停（B = 6.5）
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("settle");
    let mut spec = clip_spec(&p, &caps, 3.0, 6.5, ClipFormat::Auto, &dirs);
    spec.test.readahead = Some(8.0);
    let marks = Arc::new(Mutex::new(Vec::new()));
    let (r, d) = (marks.clone(), marks.clone());
    spec.test.on_ready = Some(Arc::new(move |_: &Mpv| r.lock().unwrap().push(Instant::now())));
    spec.test.on_dump = Some(Arc::new(move || d.lock().unwrap().push(Instant::now())));
    let done = export_clip(spec).unwrap();
    let marks = marks.lock().unwrap().clone();
    assert_eq!(marks.len(), 2, "{marks:?}");
    let waited = marks[1] - marks[0];
    // 有看到停下來的話大約 1 秒；沒看的話要等滿 20 秒。界線取在 20 秒之前（慢的機器也夠）
    assert!(waited < Duration::from_secs(15), "讀到 B 之後等了 {waited:?}");
    let (_, duration) = reopen(&done.path);
    assert!((duration - 6.0).abs() < 0.15, "長度 {duration}");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_writing_cannot_be_cancelled_and_reports_progress() {
    // 寫檔中（dump-cache）不能中斷：介面的「取消」要停用；進度看暫存檔的大小，寫完才檢查
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("writing");
    let mut spec = clip_spec(&p, &caps, 3.0, 6.5, ClipFormat::Auto, &dirs);
    let (dumped_tx, dumped_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (dumped_tx, release_rx) = (Mutex::new(dumped_tx), Mutex::new(release_rx));
    spec.test.on_dump = Some(Arc::new(move || {
        let _ = dumped_tx.lock().unwrap().send(());
        let _ = release_rx.lock().unwrap().recv_timeout(TIMEOUT);
    }));
    let job = clip::spawn(spec, no_wake());
    let deadline = Instant::now() + CLIP_TIMEOUT;
    let mut progress = Vec::new();
    let mut checked = false;
    let result = loop {
        assert!(Instant::now() < deadline, "等不到片段匯出完");
        if !checked && dumped_rx.try_recv().is_ok() {
            assert!(!job.cancellable(), "寫檔中不能取消");
            checked = true;
            release_tx.send(()).unwrap();
        }
        match job.try_recv() {
            Some(JobEvent::Progress(p)) => progress.push(p),
            Some(JobEvent::Finished(r)) => break r,
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    assert!(checked, "沒有寫檔");
    result.unwrap();
    assert!(job.cancellable(), "寫完之後又能取消");
    let writing = progress.iter().position(|p| p.phase == Phase::Writing);
    let checking = progress.iter().position(|p| p.phase == Phase::Checking);
    assert!(
        writing.is_some_and(|w| checking.is_some_and(|c| w < c)),
        "先寫檔、再檢查：{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|p| p.phase == Phase::Writing && p.fraction.is_some()),
        "寫檔的進度看暫存檔的大小：{progress:?}"
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_shorter_than_the_aligned_range_is_rejected() {
    // 寫到一半磁碟滿了、隨身碟拔掉的檔案也打得開，軌道也對：長度至少要有對齊後的範圍減 1 秒，
    // 不夠就是失敗，不留檔案。先存一個 2 秒的片段（2.0–4.0），再把 6 秒的片段（2.0–8.0）寫好的暫存檔換成它
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("tooshort");
    let short_dirs = clip_dirs("tooshort-src");
    let short = export_clip(clip_spec(&p, &caps, 3.0, 3.5, ClipFormat::Auto, &short_dirs)).unwrap();
    let (_, short_len) = reopen(&short.path);
    assert!((short_len - 2.0).abs() < 0.15, "長度 {short_len}");
    let mut spec = clip_spec(&p, &caps, 3.0, 6.5, ClipFormat::Auto, &dirs);
    let replaced = Arc::new(Mutex::new(false));
    let (r, from) = (replaced.clone(), short.path.clone());
    spec.test.after_dump = Some(Arc::new(move |temp: &Path| {
        std::fs::copy(&from, temp).unwrap();
        *r.lock().unwrap() = true;
    }));
    match export_clip(spec) {
        Err(Failure::TooShort { got, want }) => {
            assert!((got - 2.0).abs() < 0.15, "讀到的長度 {got}");
            assert!((want - 5.0).abs() < 0.2, "要的長度（6 秒 − 1 秒）{want}");
        }
        other => panic!("長度不夠要失敗：{:?}", other.map(|d| d.path)),
    }
    assert!(*replaced.lock().unwrap(), "沒有寫檔");
    wait_empty(&dirs.0);
    for d in [&dirs.0, &dirs.1, &short_dirs.0, &short_dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_jobs_register_the_kept_files_folder() {
    // 換不成正式名稱留下的完整檔案要登記（啟動時清暫存檔才不會刪掉它）：片段的工作一開始就設定登記的資料夾
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("keepdir");
    let spec = clip_spec(&p, &caps, 2.0, 2.0, ClipFormat::Auto, &dirs);
    let (tx, rx) = mpsc::channel();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let r = clip::run(ctl, spec);
        let _ = tx.send(ctl.keep_dir());
        r
    });
    assert_eq!(finish(&job).1.map(|d| d.path), Err(Failure::NoData));
    assert_eq!(rx.recv_timeout(TIMEOUT).unwrap(), Some(dirs.1.clone()));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_takes_embedded_audio_when_an_external_track_plays() {
    // 正在播放外掛的音軌（另外載入的配音）：片段放影片檔裡的第一條音軌，不是沒有聲音
    let (mut p, caps) = main_player(&sample_str("common/mkv_extaudio.mkv"));
    let ext = p
        .add_audio(&sample_str("common/mkv_extaudio.mka"))
        .unwrap()
        .expect("外掛音軌沒有加進去");
    p.select_track(TrackKind::Audio, Some(ext)).unwrap();
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Audio).is_some_and(|t| t.external))
        .unwrap();
    let dirs = clip_dirs("extaudio");
    let spec = clip_spec(&p, &caps, 0.5, 2.0, ClipFormat::Auto, &dirs);
    assert_eq!(spec.audio.as_ref().map(|a| a.ordinal), Some(1), "{:?}", spec.audio);
    assert!(!spec.drops_audio);
    let done = export_clip(spec).unwrap();
    assert!(!done.notes.contains(&Note::NoAudioTrack));
    let (tracks, _) = reopen(&done.path);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"], "{tracks:#?}");

    // 影片檔裡沒有音軌、只有外掛的：片段只有影像，完成時說明沒有聲音
    let (mut q, caps) = main_player(&sample_str("net/video_only.mp4"));
    let ext = q
        .add_audio(&sample_str("common/mkv_extaudio.mka"))
        .unwrap()
        .expect("外掛音軌沒有加進去");
    q.select_track(TrackKind::Audio, Some(ext)).unwrap();
    q.wait_state(TIMEOUT, |s| s.selected(TrackKind::Audio).is_some_and(|t| t.external))
        .unwrap();
    let spec = clip_spec(&q, &caps, 0.5, 2.0, ClipFormat::Auto, &dirs);
    assert!(spec.audio.is_none() && spec.drops_audio, "{spec:?}");
    let done = export_clip(spec).unwrap();
    assert!(done.notes.contains(&Note::NoAudioTrack), "{:?}", done.notes);
    let (tracks, _) = reopen(&done.path);
    assert!(codecs(&tracks, TrackKind::Audio).is_empty(), "{tracks:#?}");
    // 介面選的軌道：格式依這些軌道判斷
    let picks = clip::Picks {
        video: q
            .state
            .selected(TrackKind::Video)
            .and_then(|t| StreamPick::of(&q.state.tracks, t)),
        audio: None,
    };
    let spec = ClipSpec::from_player_with(
        &q,
        &caps,
        0.5,
        2.0,
        ClipFormat::Webm,
        picks,
        dirs.0.clone(),
        dirs.1.clone(),
    );
    assert_eq!(
        spec.map(|s| s.container),
        Err(Failure::Incompatible {
            container: "WebM".into(),
            codec: "h264".into()
        })
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_timeline_sources_are_refused() {
    // 使用者開的 EDL（把好幾段接成一條時間線）：主播放器的時間對不到檔案，不能存片段
    let dirs = clip_dirs("timeline");
    let src = sample_str("general/mkv_h264_gop2.mkv");
    let entry = format!("%{}%{src}", src.len());
    let edl = dirs.1.join("list.edl");
    std::fs::write(&edl, format!("# mpv EDL v0\n{entry},0,3\n{entry},6,3\n")).unwrap();
    let (p, caps) = main_player(&edl.to_string_lossy());
    assert_eq!(clip::unavailable(&p, &caps), Some(Failure::Timeline));
    assert_eq!(
        ClipSpec::from_player(&p, &caps, 1.0, 2.0, ClipFormat::Auto, dirs.0.clone(), dirs.1.clone()).map(|s| s.a),
        Err(Failure::Timeline)
    );
    // 偵測不到的（章節連結之類）：匯出用的 mpv 開起來的長度跟主播放器的不同，也不存
    let (p, caps) = main_player(&src);
    let mut spec = clip_spec(&p, &caps, 1.0, 2.0, ClipFormat::Auto, &dirs);
    spec.main_duration = spec.main_duration.map(|d| d + 20.0);
    assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::Timeline));
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_containers() {
    // 只比對編碼（目前的播放引擎不寫軌道的語言、標題）
    struct Case {
        rel: &'static str,
        format: ClipFormat,
        container: Container,
        video: &'static str,
        audio: &'static str,
    }
    let cases = [
        Case {
            rel: "common/mp4_h264_aac.mp4",
            format: ClipFormat::Auto,
            container: Container::Mp4,
            video: "h264",
            audio: "aac",
        },
        Case {
            rel: "common/webm_vp9_opus.webm",
            format: ClipFormat::Auto,
            container: Container::Webm,
            video: "vp9",
            audio: "opus",
        },
        Case {
            rel: "general/ts_h264_aac.ts",
            format: ClipFormat::Auto,
            container: Container::Ts,
            video: "h264",
            audio: "aac",
        },
        // MKV → 指定 MP4
        Case {
            rel: "general/mkv_h264_gop2.mkv",
            format: ClipFormat::Mp4,
            container: Container::Mp4,
            video: "h264",
            audio: "aac",
        },
        // AVI 之類：MKV
        Case {
            rel: "common/avi_xvid_mp3.avi",
            format: ClipFormat::Auto,
            container: Container::Mkv,
            video: "mpeg4",
            audio: "mp3",
        },
    ];
    let dirs = clip_dirs("containers");
    for c in cases {
        let (p, caps) = main_player(&sample_str(c.rel));
        let spec = clip_spec(&p, &caps, 0.5, 2.0, c.format, &dirs);
        assert_eq!(spec.container, c.container, "{}", c.rel);
        let done = export_clip(spec).unwrap_or_else(|f| panic!("{}：{f:?}", c.rel));
        assert_eq!(
            done.path.extension().unwrap().to_str(),
            Some(c.container.ext()),
            "{}",
            c.rel
        );
        let (tracks, duration) = reopen(&done.path);
        assert!(duration > 1.0, "{}：長度 {duration}", c.rel);
        assert_eq!(codecs(&tracks, TrackKind::Video), [c.video], "{}", c.rel);
        assert_eq!(codecs(&tracks, TrackKind::Audio), [c.audio], "{}", c.rel);
    }

    // 雙音軌、雙字幕：選第二條音軌，片段只有影像與那一條聲音，沒有字幕
    let (mut p, caps) = main_player(&sample_str("common/mkv_multitrack.mkv"));
    p.select_track(TrackKind::Audio, Some(2)).unwrap();
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Audio).is_some_and(|t| t.id == 2))
        .unwrap();
    let mut spec = clip_spec(&p, &caps, 1.0, 3.0, ClipFormat::Auto, &dirs);
    assert_eq!(spec.audio.as_ref().map(|a| a.ordinal), Some(2));
    assert!(spec.drops_subtitles, "顯示著字幕、音軌有語言：要說明片段不會有");
    // 兩條音軌都是 AAC，片段又不保留語言：看匯出用的 mpv 讀的是哪一條（它還看得到來源的語言、標題）。
    // 第一個匯出用的 mpv 故意用錯的音軌編號：要用對的編號重開，不是照錯的讀
    spec.test.initial_ids = Some((Some(1), Some(1)));
    let read = Arc::new(Mutex::new(Vec::new()));
    let r = read.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        r.lock().unwrap().push((
            mpv.get_string("aid").unwrap_or_default(),
            mpv.get_string("current-tracks/audio/lang").unwrap_or_default(),
            mpv.get_string("current-tracks/audio/title").unwrap_or_default(),
        ));
    }));
    let done = export_clip(spec).unwrap();
    assert_eq!(
        *read.lock().unwrap(),
        [("2".to_owned(), "chi".to_owned(), "國語".to_owned())],
        "讀的要是第二條音軌（國語）"
    );
    assert_eq!(done.path.extension().unwrap(), "mkv");
    assert!(done.notes.contains(&Note::NoSubtitleTrack));
    let (tracks, _) = reopen(&done.path);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"], "只有一條聲音：{tracks:#?}");
    assert!(codecs(&tracks, TrackKind::Sub).is_empty(), "不放字幕：{tracks:#?}");

    // 指定的格式放不下：開始前就知道
    let (p, caps) = main_player(&sample_str("common/webm_vp9_opus.webm"));
    let r = ClipSpec::from_player(&p, &caps, 0.5, 2.0, ClipFormat::Ts, dirs.0.clone(), dirs.1.clone());
    assert_eq!(
        r.map(|s| s.container),
        Err(Failure::Incompatible {
            container: "TS".into(),
            codec: "vp9".into()
        })
    );
    assert!(parts(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_audio_only() {
    let dirs = clip_dirs("audio");
    for (rel, container, codec) in [
        ("general/audio_flac.flac", Container::Flac, "flac"),
        // 有封面的 MP3：封面不放進去
        ("general/audio_mp3_cover.mp3", Container::Mp3, "mp3"),
        ("general/audio_aac.m4a", Container::M4a, "aac"),
        ("general/audio_wav.wav", Container::Wav, "pcm_s16le"),
    ] {
        let (p, caps) = main_player(&sample_str(rel));
        let spec = clip_spec(&p, &caps, 0.5, 2.0, ClipFormat::Auto, &dirs);
        assert_eq!(spec.container, container, "{rel}");
        assert!(spec.video.is_none(), "{rel}：封面不是影像");
        let done = export_clip(spec).unwrap_or_else(|f| panic!("{rel}：{f:?}"));
        assert!(
            !done.notes.contains(&Note::KeyframeAligned),
            "{rel}：沒有影像就沒有關鍵影格的說明"
        );
        let (tracks, duration) = reopen(&done.path);
        assert!(duration > 1.0, "{rel}：長度 {duration}");
        assert_eq!(codecs(&tracks, TrackKind::Audio), [codec], "{rel}");
        assert!(
            tracks.iter().all(|t| t.kind != TrackKind::Video),
            "{rel}：只有聲音：{tracks:#?}"
        );
    }
    // 指定 MKV：只有聲音的存 MKA
    let (p, caps) = main_player(&sample_str("general/audio_flac.flac"));
    let spec = clip_spec(&p, &caps, 0.5, 2.0, ClipFormat::Mkv, &dirs);
    assert_eq!(spec.container, Container::Mka);
    let done = export_clip(spec).unwrap();
    assert_eq!(done.path.extension().unwrap(), "mka");
    assert_eq!(codecs(&reopen(&done.path).0, TrackKind::Audio), ["flac"]);
    // APE、DSD、Musepack 之類：開始前就拒絕（樣本做不出來，比對編碼名稱）
    for codec in ["ape", "dsd_lsbf", "musepack8"] {
        assert_eq!(clip::audio_container(codec), Err(Failure::AudioCodec), "{codec}");
    }
    assert!(parts(&dirs.0).is_empty());
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_keeps_rotation() {
    // 手機直拍（檔案標示旋轉 90°）：寫出來的片段也標示一樣的旋轉。MKV 的樣本舊版 ffmpeg 做不出來，沒有就略過
    let dirs = clip_dirs("rotation");
    for rel in ["common/mov_hevc_aac_rot90.mov", "rare/mkv_hevc_aac_rot90.mkv"] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("samples/generated")
            .join(rel);
        if !path.exists() {
            eprintln!("略過 {rel}：沒有這個樣本（舊版 ffmpeg 寫不出 MKV 的旋轉）");
            continue;
        }
        let (mut p, caps) = main_player(&path.to_string_lossy());
        p.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
        // 同一個播放引擎讀來源與片段（系統的 libmpv 0.37 讀 MKV 容器層的旋轉方向跟新版相反：比的是「一樣」）
        let source = p.natural_shape().map(|s| s.1);
        if rel.ends_with(".mov") {
            assert_eq!(source, Some(90), "{rel}");
        }
        assert!(source.is_some_and(|r| r != 0), "{rel}：來源要有旋轉：{source:?}");
        let spec = clip_spec(&p, &caps, 0.5, 2.0, ClipFormat::Auto, &dirs);
        let done = export_clip(spec).unwrap_or_else(|f| panic!("{rel}：{f:?}"));
        let (mut q, _) = main_player(&done.path.to_string_lossy());
        q.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
        let shape = q.natural_shape();
        assert_eq!(shape.map(|s| s.1), source, "{rel} → {}：{shape:?}", done.path.display());
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_disk_cache_retry() {
    // 記憶體快取的上限比要讀的少（測試用的上限，比樣本小）：改放磁碟快取重來一次，照樣成功
    let src = sample("general/mkv_h264_gop2.mkv");
    let (p, caps) = main_player(&src.to_string_lossy());
    let dirs = clip_dirs("diskcache");
    let mut spec = clip_spec(&p, &caps, 1.0, 9.0, ClipFormat::Auto, &dirs);
    let size = std::fs::metadata(&src).unwrap().len();
    spec.test.max_bytes = Some(size / 6);
    let modes = Arc::new(Mutex::new(Vec::new()));
    let m = modes.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        m.lock()
            .unwrap()
            .push(mpv.get_string("cache-on-disk").unwrap_or_default());
    }));
    let done = export_clip(spec).unwrap();
    assert_eq!(*modes.lock().unwrap(), ["no", "yes"], "先放記憶體，不夠時改放磁碟");
    let (a, b) = done.actual.unwrap();
    assert!(a <= 1.0 && b >= 9.0, "{a}–{b}");
    let (tracks, duration) = reopen(&done.path);
    assert!(duration >= 7.9, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_cancel_and_out_of_range() {
    let (p, caps) = main_player(&sample_str("common/mp4_long.mp4"));
    let dirs = clip_dirs("cancel");
    // 開始之前就取消
    let spec = clip_spec(&p, &caps, 10.0, 20.0, ClipFormat::Auto, &dirs);
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let _ = go_rx.recv_timeout(TIMEOUT);
        clip::run(ctl, spec)
    });
    job.cancel();
    go_tx.send(()).unwrap();
    assert_eq!(finish(&job).1, Err(Failure::Cancelled));
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    // 讀到一半丟掉工作（關閉影戲）：停下來，不留任何檔案。
    // 匯出用的 mpv 開好時先停住，確定丟掉的時候工作還在讀（不靠這台電腦夠不夠快）
    let mut spec = clip_spec(&p, &caps, 10.0, 80.0, ClipFormat::Auto, &dirs);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (ready_tx, release_rx) = (Mutex::new(ready_tx), Mutex::new(release_rx));
    spec.test.on_ready = Some(Arc::new(move |_: &Mpv| {
        let _ = ready_tx.lock().unwrap().send(());
        let _ = release_rx.lock().unwrap().recv_timeout(TIMEOUT);
    }));
    let (result_tx, result_rx) = mpsc::channel();
    let job = Job::spawn(Kind::Clip, no_wake(), move |ctl| {
        let r = clip::run(ctl, spec);
        let _ = result_tx.send(r.clone());
        r
    });
    ready_rx.recv_timeout(TIMEOUT).expect("匯出用的 mpv 沒有開好");
    // 丟掉 Job 會先要求取消、再等工作停下：等的時候放行
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let _ = release_tx.send(());
    });
    drop(job);
    release.join().unwrap();
    assert_eq!(
        result_rx.recv_timeout(TIMEOUT).map(|r| r.map(|d| d.path)),
        Ok(Err(Failure::Cancelled)),
        "丟掉 Job 要讓讀到一半的工作停下"
    );
    wait_empty(&dirs.0);
    // A 超過結尾：讀不到資料，不留檔案
    let spec = clip_spec(&p, &caps, 200.0, 210.0, ClipFormat::Auto, &dirs);
    assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::NoData));
    // 範圍反過來、空的
    let spec = clip_spec(&p, &caps, 5.0, 5.0, ClipFormat::Auto, &dirs);
    assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::NoData));
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    // 影片檔不見了
    let mut spec = clip_spec(&p, &caps, 1.0, 2.0, ClipFormat::Auto, &dirs);
    spec.source = Source::File(dirs.0.join("沒有這個檔案.mp4"));
    assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::SourceMissing));
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    // 影片檔還在，但讀不到（別的程式鎖著、沒有權限）：不是「被移動或刪除」
    let locked = dirs.1.join("鎖著的.mp4");
    std::fs::copy(sample("common/mp4_long.mp4"), &locked).unwrap();
    if let Some(_guard) = make_unreadable(&locked) {
        let mut spec = clip_spec(&p, &caps, 1.0, 2.0, ClipFormat::Auto, &dirs);
        spec.source = Source::File(locked.clone());
        assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::SourceNoAccess));
        assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 讓檔案讀不到：Windows 開著不讓別人讀（像別的程式鎖著），Unix 拿掉讀取權限（root 照樣讀得到：略過，回傳 None）。
/// 回傳的東西留著的期間都讀不到
fn make_unreadable(path: &Path) -> Option<Box<dyn std::any::Any>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new().read(true).share_mode(0).open(path).unwrap();
        Some(Box::new(f))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(path).is_ok() {
            eprintln!("略過讀不到的檔案的檢查：這個使用者（root）沒有權限也讀得到");
            return None;
        }
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o644));
            }
        }
        Some(Box::new(Restore(path.to_path_buf())))
    }
}

#[test]
fn clip_http_vod() {
    // 能跳轉的網路影片：匯出用的 mpv 用同樣的連線設定再讀一次這一段（不動正在播放的那一個）
    let server = Server::start();
    let url = server.file_url("general/mkv_h264_gop2.mkv");
    // 「設定 → 網路」的 User-Agent、標頭：匯出用的 mpv 也要送
    let net_settings = NetSettings {
        user_agent: "VitaScope-Clip/1.0".into(),
        headers: vec!["X-Clip: a, b".into()],
        ..NetSettings::default()
    };
    let (mut p, caps) = main_player_net(&net_settings, &url);
    p.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let dirs = clip_dirs("http");
    let spec = clip_spec(&p, &caps, 3.0, 6.5, ClipFormat::Auto, &dirs);
    match &spec.source {
        Source::Net(s) => {
            assert_eq!(s.open, url);
            assert!(!s.site);
        }
        other => panic!("應該是網路影片：{other:?}"),
    }
    let before = server.requests_to("/f/general/mkv_h264_gop2.mkv").len();
    let done = export_clip(spec).unwrap();
    let reqs = server.requests_to("/f/general/mkv_h264_gop2.mkv");
    assert!(reqs.len() > before, "匯出用的 mpv 要自己讀");
    for r in &reqs[before..] {
        assert_eq!(r.header("User-Agent"), Some("VitaScope-Clip/1.0"), "{r:#?}");
        assert_eq!(r.header("X-Clip"), Some("a, b"), "{r:#?}");
    }
    let (a, b) = done.actual.unwrap();
    assert!((a - 2.0).abs() < 0.05 && (b - 8.0).abs() < 0.15, "{a}–{b}");
    let (tracks, duration) = reopen(&done.path);
    assert!((duration - 6.0).abs() < 0.15, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"]);
    // 正在播放的那一個沒有被動到：讀 mpv 現在的值（不是之前存下來的狀態）
    p.poll();
    assert!(p.state.loaded);
    assert_eq!(p.get_string("pause").unwrap(), "yes");
    let pos = p.get_f64("time-pos").unwrap();
    assert!(pos < 0.5, "主播放器的位置被動到了：{pos}");
    // 匯出用的 mpv 打不開網址（連結過期、斷線）：說明是網址打不開，不是「找不到影片檔」
    let mut spec = clip_spec(&p, &caps, 1.0, 2.0, ClipFormat::Auto, &dirs);
    if let Source::Net(s) = &mut spec.source {
        s.open = server.url("/status/403");
    }
    assert_eq!(export_clip(spec).map(|d| d.path), Err(Failure::SourceUnreachable));

    // HLS（VOD）：一樣用匯出用的 mpv 再讀一次
    let (mut h, hcaps) = main_player(&server.url("/f/net/hls_vod/index.m3u8"));
    h.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let spec = clip_spec(&h, &hcaps, 0.5, 2.0, ClipFormat::Auto, &dirs);
    assert_eq!(spec.container, Container::Mkv, "HLS 存 MKV");
    let done = export_clip(spec).unwrap();
    let (tracks, duration) = reopen(&done.path);
    assert!(duration > 1.0, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"]);

    // 直播、不能跳轉的網路影片：不能存片段
    // （直播不一定有總長度：只等開好）
    let mut q = Player::new(Options {
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    })
    .unwrap();
    let caps = q.probe_caps();
    q.open(&server.url("/hlslive/net/hls_vod/index.m3u8")).unwrap();
    q.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 路徑可能比 FileLoaded 晚到（沒有路徑時是 NoData，不是直播）
    q.wait_state(TIMEOUT, |s| s.loaded && s.path.is_some() && !s.tracks.is_empty())
        .unwrap();
    assert_eq!(clip::unavailable(&q, &caps), Some(Failure::Live));
    let (mut q, caps) = main_player(&server.url("/norange/general/mkv_h264_gop2.mkv"));
    q.wait_state(TIMEOUT, |s| s.duration.is_some()).unwrap();
    assert_eq!(clip::unavailable(&q, &caps), Some(Failure::NotSeekable));
    assert_eq!(
        ClipSpec::from_player(&q, &caps, 1.0, 2.0, ClipFormat::Auto, dirs.0.clone(), dirs.1.clone()).map(|s| s.a),
        Err(Failure::NotSeekable)
    );
    // 播放引擎沒有 dump-cache
    let no_dump = EngineCaps {
        dump_cache: false,
        ..caps
    };
    assert_eq!(clip::unavailable(&p, &no_dump), Some(Failure::NoDump));
    assert_eq!(clip::unavailable(&p, &caps), None);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn clip_site_video() {
    // 網站影片（yt-dlp：影像、聲音分開，合成一個 EDL）：匯出用的 mpv 開同一個 EDL、帶網站要的標頭與 Cookie
    let server = Server::start();
    let page = server.url("/watch?v=clip1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "clip1")).arc();
    let resolver: Arc<dyn Resolve> = fake.clone();
    let opts = Options {
        net_hooks: true,
        net_resolver: Some(resolver),
        net_sites: vec!["127.0.0.1".into()],
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    };
    let (mut p, caps) = main_player_with(opts, &page);
    p.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let dirs = clip_dirs("site");
    let spec = clip_spec(&p, &caps, 0.5, 2.0, ClipFormat::Auto, &dirs);
    match &spec.source {
        Source::Net(s) => assert!(s.site && s.open.starts_with("edl://"), "{s:?}"),
        other => panic!("應該是網站影片：{other:?}"),
    }
    assert_eq!(spec.container, Container::Mkv);
    assert!(
        spec.stem.starts_with("假的網站影片"),
        "檔名用網站影片的標題：{}",
        spec.stem
    );
    let before = server.requests_to("/f/net/video_only.mp4").len();
    let done = export_clip(spec).unwrap();
    let reqs = server.requests_to("/f/net/video_only.mp4");
    assert!(reqs.len() > before, "匯出用的 mpv 要自己讀");
    for r in &reqs[before..] {
        assert_eq!(r.header("User-Agent"), Some(support::fake_ytdl::SITE_UA), "{r:#?}");
    }
    assert_eq!(fake.calls(), 1, "匯出不再問一次 yt-dlp");
    let (tracks, duration) = reopen(&done.path);
    assert!(duration > 1.0, "長度 {duration}");
    assert_eq!(codecs(&tracks, TrackKind::Video), ["h264"]);
    assert_eq!(codecs(&tracks, TrackKind::Audio), ["aac"]);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn write_error_maps_to_disk_full() {
    // dump-cache 寫到一半失敗（磁碟滿了、隨身碟拔掉）時 mpv 照樣回報成功，只有記錄裡的這兩句
    let line = |text: &str| LogLine::new("error", "recorder", text);
    for text in ["Failed writing packet.", "Writing trailer failed."] {
        let r = clip::dump_result(Ok(()), &[line(text)]);
        assert_eq!(r, Err(Failure::WriteFailed), "{text}");
        vitascope::i18n::set_lang(vitascope::i18n::Lang::ZhTw);
        let msg = r.unwrap_err().osd();
        assert!(msg.contains("磁碟空間") && msg.contains("無法寫入"), "{msg}");
    }
    assert_eq!(clip::dump_result(Ok(()), &[]), Ok(()));
}

// ───────────── GIF ─────────────

/// 播放引擎能不能轉 GIF（Linux 的系統 libmpv 不一定有 gif 編碼器、palettegen）：不能時略過這個測試
fn gif_engine(caps: &EngineCaps, test: &str) -> bool {
    if caps.gif && caps.palettegen {
        return true;
    }
    eprintln!(
        "略過 {test}：播放引擎不能轉 GIF（gif 編碼器 {}、palettegen {}）",
        caps.gif, caps.palettegen
    );
    false
}

fn gif_prefs(long_side: u32, fps: u32, subtitles: bool) -> GifPrefs {
    GifPrefs {
        long_side,
        fps,
        subtitles,
    }
}

/// 等主播放器解出第一格（之前不知道檔案本身的旋轉、像素比例，不能轉 GIF）。直接問 mpv，不用等屬性通知
fn wait_first_frame(p: &Player) {
    let start = Instant::now();
    while p.natural_shape().is_none() {
        assert!(start.elapsed() < TIMEOUT, "主播放器一直沒有解出第一格");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 從主播放器準備一個 GIF（設定用預設的：HDR、字幕外觀、去交錯）。先等第一格解出來
fn gif_spec_with(
    p: &Player,
    caps: &EngineCaps,
    a: f64,
    b: f64,
    prefs: GifPrefs,
    geometry: &Geometry,
    dirs: &(PathBuf, PathBuf),
) -> Result<GifSpec, Failure> {
    wait_first_frame(p);
    let tone = ToneSettings::default();
    let style = SubStyle::default();
    let choice = gif::Choice {
        prefs,
        geometry,
        tone: &tone,
        style: &style,
        deinterlace: Deinterlace::Auto,
    };
    GifSpec::from_player(p, caps, a, b, &choice, dirs.0.clone(), dirs.1.clone())
}

fn gif_spec(p: &Player, caps: &EngineCaps, a: f64, b: f64, prefs: GifPrefs, dirs: &(PathBuf, PathBuf)) -> GifSpec {
    gif_spec_with(p, caps, a, b, prefs, &Geometry::default(), dirs).unwrap_or_else(|f| panic!("不能轉 GIF：{f:?}"))
}

/// 轉這個 GIF，等到結束
fn export_gif(spec: GifSpec) -> Result<Done, Failure> {
    wait_job(&gif::spawn(spec, no_wake()))
}

/// 等到工作結束（片段、GIF 的工作很慢：CI 的機器上給足時間）
fn wait_job(job: &Job) -> Result<Done, Failure> {
    let deadline = Instant::now() + CLIP_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "等不到匯出結束");
        match job.try_recv() {
            Some(JobEvent::Finished(r)) => return r,
            Some(JobEvent::Progress(_)) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// GIF 的一格（RGBA，畫到整張畫布上之後的樣子）
struct GifFrames {
    w: usize,
    h: usize,
    frames: Vec<Vec<u8>>,
}

/// LZW 解壓縮（GIF 的影像資料：從低位元開始讀、碼長從最小碼長 + 1 開始、最長 12 位元）
fn lzw_decode(min_code: u8, data: &[u8]) -> Vec<u8> {
    let clear = 1usize << min_code;
    let eoi = clear + 1;
    let reset = || -> Vec<Vec<u8>> {
        let mut d: Vec<Vec<u8>> = (0..clear).map(|i| vec![i as u8]).collect();
        d.push(Vec::new());
        d.push(Vec::new());
        d
    };
    let mut dict = reset();
    let mut size = min_code as u32 + 1;
    let (mut bits, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut prev: Option<usize> = None;
    let mut out = Vec::new();
    loop {
        while nbits < size {
            let Some(&b) = data.get(pos) else { return out };
            bits |= u32::from(b) << nbits;
            nbits += 8;
            pos += 1;
        }
        let code = (bits & ((1 << size) - 1)) as usize;
        bits >>= size;
        nbits -= size;
        if code == clear {
            dict = reset();
            size = min_code as u32 + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return out;
        }
        let entry = match (code < dict.len(), prev) {
            (true, _) => dict[code].clone(),
            (false, Some(p)) if code == dict.len() => {
                let mut e = dict[p].clone();
                e.push(dict[p][0]);
                e
            }
            _ => return out,
        };
        out.extend_from_slice(&entry);
        if let Some(p) = prev
            && dict.len() < 4096
        {
            let mut e = dict[p].clone();
            e.push(entry[0]);
            dict.push(e);
        }
        if dict.len() == 1 << size && size < 12 {
            size += 1;
        }
        prev = Some(code);
    }
}

/// 解開整個 GIF（每一格都畫到畫布上，處理透明色與處置方式）
fn decode_gif(data: &[u8]) -> GifFrames {
    assert!(data.starts_with(b"GIF89a"), "不是 GIF89a");
    let w = u16::from_le_bytes([data[6], data[7]]) as usize;
    let h = u16::from_le_bytes([data[8], data[9]]) as usize;
    let mut i = 13;
    let palette = |at: usize, flags: u8| -> Vec<[u8; 3]> {
        let n = 1usize << ((flags & 7) + 1);
        (0..n)
            .map(|k| [data[at + k * 3], data[at + k * 3 + 1], data[at + k * 3 + 2]])
            .collect()
    };
    let global = if data[10] & 0x80 != 0 {
        let p = palette(13, data[10]);
        i += p.len() * 3;
        p
    } else {
        Vec::new()
    };
    let sub_blocks = |mut i: usize| -> (Vec<u8>, usize) {
        let mut v = Vec::new();
        while data[i] != 0 {
            let n = data[i] as usize;
            v.extend_from_slice(&data[i + 1..i + 1 + n]);
            i += n + 1;
        }
        (v, i + 1)
    };
    let mut canvas = vec![0u8; w * h * 4];
    let mut frames = Vec::new();
    let (mut transparent, mut disposal) = (None, 0u8);
    loop {
        match data[i] {
            0x21 => {
                if data[i + 1] == 0xF9 {
                    let flags = data[i + 3];
                    disposal = (flags >> 2) & 7;
                    transparent = (flags & 1 != 0).then_some(data[i + 6]);
                }
                i = sub_blocks(i + 2).1;
            }
            0x2c => {
                let x = u16::from_le_bytes([data[i + 1], data[i + 2]]) as usize;
                let y = u16::from_le_bytes([data[i + 3], data[i + 4]]) as usize;
                let fw = u16::from_le_bytes([data[i + 5], data[i + 6]]) as usize;
                let fh = u16::from_le_bytes([data[i + 7], data[i + 8]]) as usize;
                let flags = data[i + 9];
                assert_eq!(flags & 0x40, 0, "交錯的 GIF（FFmpeg 不會寫）");
                i += 10;
                let colors = if flags & 0x80 != 0 {
                    let p = palette(i, flags);
                    i += p.len() * 3;
                    p
                } else {
                    global.clone()
                };
                let min_code = data[i];
                let (lzw, next) = sub_blocks(i + 1);
                i = next;
                let before = canvas.clone();
                let indices = lzw_decode(min_code, &lzw);
                for (k, &idx) in indices.iter().enumerate().take(fw * fh) {
                    if Some(idx) == transparent {
                        continue;
                    }
                    let (px, py) = (x + k % fw, y + k / fw);
                    if px < w && py < h {
                        let c = colors[idx as usize];
                        canvas[(py * w + px) * 4..][..4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                frames.push(canvas.clone());
                match disposal {
                    2 => {
                        for py in y..(y + fh).min(h) {
                            for px in x..(x + fw).min(w) {
                                canvas[(py * w + px) * 4..][..4].fill(0);
                            }
                        }
                    }
                    3 => canvas = before,
                    _ => {}
                }
                transparent = None;
                disposal = 0;
            }
            0x3b => break,
            b => panic!("GIF 格式不對：位置 {i} 是 0x{b:02x}"),
        }
    }
    GifFrames { w, h, frames }
}

fn read_gif(path: &Path) -> GifFrames {
    decode_gif(&std::fs::read(path).unwrap())
}

/// 一格裡某些列的平均亮度（0–255）
fn band_luma(g: &GifFrames, frame: usize, rows: std::ops::Range<usize>) -> f64 {
    let f = &g.frames[frame];
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in rows {
        for x in 0..g.w {
            let p = &f[(y * g.w + x) * 4..][..3];
            sum += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            n += 1.0;
        }
    }
    sum / n
}

/// 兩個一樣大的 GIF 的同一格，某些列裡差很多（亮度差 64 以上）的像素有多少比例
fn band_changed(a: &GifFrames, b: &GifFrames, frame: usize, rows: std::ops::Range<usize>) -> f64 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let luma = |p: &[u8]| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
    let mut changed = 0.0;
    let mut n = 0.0;
    for y in rows {
        for x in 0..a.w {
            let k = (y * a.w + x) * 4;
            if (luma(&a.frames[frame][k..k + 3]) - luma(&b.frames[frame][k..k + 3])).abs() > 64.0 {
                changed += 1.0;
            }
            n += 1.0;
        }
    }
    changed / n
}

/// 兩張一樣大的圖每個像素的平均差（0–255）
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: f64 = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .map(|(p, q)| (0..3).map(|c| (f64::from(p[c]) - f64::from(q[c])).abs()).sum::<f64>() / 3.0)
        .sum();
    sum / (a.len() / 4) as f64
}

#[test]
fn gif_range_size_and_edl() {
    let (p, caps) = main_player(&sample_str("common/mkv_multitrack.mkv"));
    if !gif_engine(&caps, "gif_range_size_and_edl") {
        return;
    }
    let dirs = clip_dirs("gif-range");
    let mut spec = gif_spec(&p, &caps, 1.0, 2.5, gif_prefs(320, 10, false), &dirs);
    assert_eq!(spec.out, (320, 180));
    assert_eq!(spec.graph.palette, gif::PaletteMode::Global);
    assert!(spec.sub.is_none() && !spec.graph.subtitles);
    // 編碼用的 mpv 開的是只有這一段的 EDL：長度就是 B − A（讀到 B 就結束，不會一路解碼到檔尾）
    let seen: Arc<Mutex<Option<(f64, String)>>> = Arc::default();
    let s = seen.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        *s.lock().unwrap() = Some((
            mpv.get_property::<f64>("duration").unwrap_or(-1.0),
            mpv.get_string("path").unwrap_or_default(),
        ));
    }));
    let done = export_gif(spec).unwrap();
    let (duration, path) = seen.lock().unwrap().clone().expect("編碼用的 mpv 沒有開好");
    assert!((duration - 1.5).abs() < 0.05, "EDL 的長度 {duration}");
    assert!(path.starts_with("edl://"), "{path}");
    assert_eq!(done.kind, Kind::Gif);
    assert_eq!(done.path, dirs.0.join("mkv_multitrack 00.00.01-00.00.02.gif"));
    assert_eq!(done.actual, None);
    assert!(done.notes.is_empty(), "{:?}", done.notes);
    let data = std::fs::read(&done.path).unwrap();
    assert_eq!(done.bytes, data.len() as u64);
    let info = gif::gif_info(&data).expect("不是完整的 GIF");
    assert!(data.starts_with(b"GIF89a"));
    assert_eq!((info.width, info.height), (320, 180));
    assert!((14..=16).contains(&info.frames), "{} 格", info.frames);
    assert!((info.seconds() - 1.5).abs() <= 0.15, "總長 {} 秒", info.seconds());
    // 每一格都有畫面（testsrc2 不是全黑）
    let g = decode_gif(&data);
    assert_eq!(g.frames.len(), info.frames);
    for k in 0..g.frames.len() {
        assert!(band_luma(&g, k, 0..g.h) > 20.0, "第 {k} 格是黑的");
    }
    assert_eq!(names(&dirs.0), ["mkv_multitrack 00.00.01-00.00.02.gif"], "不留暫存檔");
    // 每格一個色盤（畫面很大、很長時）：一樣是完整的 GIF
    let mut spec = gif_spec(&p, &caps, 1.0, 2.0, gif_prefs(320, 10, false), &dirs);
    spec.graph.palette = gif::PaletteMode::PerFrame;
    spec.stem = "per-frame".into();
    let done = export_gif(spec).unwrap();
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert_eq!((info.width, info.height), (320, 180));
    assert!((9..=11).contains(&info.frames), "{} 格", info.frames);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_range_follows_the_start_time() {
    // TS 的時間戳不是從 0 開始（FFmpeg 寫 TS 時從 1.4 秒左右開始）：A、B 是主播放器的播放時間，
    // EDL 是影片的時間戳，要加上主播放器的 demuxer-start-time，GIF 才是畫面上看到的那一段。
    // TS 跳轉不準：A 離開頭不到 5 秒，EDL 從影片的時間戳 0 開始讀，再精確跳到 EDL 的 0.5 + 開始時間（A）。
    // 沒有換算時跳到 0.5 秒，影片在 1.4 秒才開始：GIF 從頭開始，多了將近 1 秒
    // Windows、macOS 的 CI 用的 FFmpeg 產生不了這個交錯式的樣本（其他用到它的測試也是略過）
    if !sample("general").join("ts_mpeg2_interlaced.ts").exists() {
        eprintln!("略過 gif_range_follows_the_start_time：沒有 general/ts_mpeg2_interlaced.ts（這個 FFmpeg 產生不了）");
        return;
    }
    let (p, caps) = main_player(&sample_str("general/ts_mpeg2_interlaced.ts"));
    if !gif_engine(&caps, "gif_range_follows_the_start_time") {
        return;
    }
    let offset = p.demuxer_start_time();
    assert!(offset > 0.5, "TS 的開始時間 {offset}：測不到換算");
    let dirs = clip_dirs("gif-start-time");
    let mut spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    assert!(spec.approx_seek, "TS 的跳轉不準");
    let seen = seen_plan(&mut spec);
    let done = export_gif(spec);
    let plan = seen.lock().unwrap().clone().expect("編碼用的 mpv 沒有開好");
    assert_eq!(plan.edl_start(), 0.0, "從頭讀：{}", plan.path);
    let start = plan.start.unwrap_or_else(|| panic!("沒有精確跳到 A：{plan:?}"));
    assert!(
        (start - (0.5 + offset)).abs() < 1e-3,
        "跳到 EDL 的 {start}，應該是 0.5 + {offset}"
    );
    let info = gif::gif_info(&std::fs::read(done.unwrap().path).unwrap()).unwrap();
    // 從 A 開始、到 B 結束：1 秒、10 fps
    assert!((9..=11).contains(&info.frames), "{} 格", info.frames);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 編碼用的 mpv 怎麼開的（開好時讀的）
#[derive(Debug, Clone)]
struct SeenPlan {
    path: String,
    /// `start` 選項（精確跳到 A；None = 不跳）
    start: Option<f64>,
    duration: f64,
    /// 濾鏡（`vf`）
    vf: String,
}

impl SeenPlan {
    /// EDL 從影片的哪個時間戳開始
    fn edl_start(&self) -> f64 {
        self.path
            .rsplit_once(",start=")
            .and_then(|(_, rest)| rest.split(',').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("EDL 沒有 start：{}", self.path))
    }
}

/// 記下編碼用的 mpv 怎麼開的
fn seen_plan(spec: &mut GifSpec) -> Arc<Mutex<Option<SeenPlan>>> {
    let seen: Arc<Mutex<Option<SeenPlan>>> = Arc::default();
    let s = seen.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        *s.lock().unwrap() = Some(SeenPlan {
            path: mpv.get_string("path").unwrap_or_default(),
            start: mpv.get_string("start").ok().and_then(|v| v.parse().ok()),
            duration: mpv.get_property::<f64>("duration").unwrap_or(-1.0),
            vf: mpv.get_string("vf").unwrap_or_default(),
        });
    }));
    seen
}

/// 主播放器精確跳到 `t` 秒（等停住的那一格解好），回傳畫面上那一格的時間與截圖（原始大小的 RGBA）
fn main_frame(p: &mut Player, t: f64, dir: &Path) -> (f64, vitascope::screenshot::Image) {
    p.seek_to(t, true).unwrap();
    // 等這次跳轉做完、停住的那一格解好：跳轉中的 time-pos 是目標，不是畫面上的那一格；
    // 之前排著的 PlaybackRestart（開檔、上一次跳轉）也不算，所以每次都問 mpv 還在不在跳轉
    let start = Instant::now();
    let pos = loop {
        let left = TIMEOUT.saturating_sub(start.elapsed());
        p.wait_for(left, |e| *e == PlayerEvent::PlaybackRestart)
            .unwrap_or_else(|e| panic!("主播放器跳不到 {t}：{e}"));
        let seeking = p.get_string("seeking").map_or(true, |v| v != "no");
        let pos = p.get_f64("time-pos").unwrap_or(f64::NAN);
        if !seeking && pos >= t - 0.01 && pos < t + 0.1 {
            break pos;
        }
    };
    std::fs::create_dir_all(dir).unwrap();
    let png = dir.join(format!("main-{t}.png"));
    let id = 7000 + (t * 1000.0) as u64;
    p.screenshot_to_file(id, &png.to_string_lossy(), false).unwrap();
    match p.wait_for(
        TIMEOUT,
        |e| matches!(e, PlayerEvent::CommandReply { id: i, .. } if *i == id),
    ) {
        Ok(PlayerEvent::CommandReply { error: None, .. }) => {}
        other => panic!("主播放器截圖失敗：{other:?}"),
    }
    let img = vitascope::screenshot::decode_png(&png).unwrap();
    std::fs::remove_file(&png).unwrap();
    (pos, img)
}

/// 開 TS 的主播放器（參考用）：主播放器自己精確跳轉時也一樣會落在 A 之後的關鍵影格、檔尾（沒有快取時），
/// 參考的畫面要準，分離器一律從開頭之前讀起（測試的檔案都很短）
fn ts_main_player(rel: &str) -> (Player, EngineCaps) {
    let opts = Options {
        extra: vec![
            ("pause".into(), "yes".into()),
            ("hr-seek-demuxer-offset".into(), "60".into()),
        ],
        ..Options::headless()
    };
    main_player_with(opts, &sample_str(rel))
}

/// 縮成 `w`×`h`（每個像素取對應範圍的平均）：主播放器的截圖是原始大小，GIF 縮小過
fn shrink(img: &vitascope::screenshot::Image, w: usize, h: usize) -> Vec<u8> {
    if (img.w, img.h) == (w, h) {
        return img.rgba.clone();
    }
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let (y0, y1) = (y * img.h / h, ((y + 1) * img.h / h).max(y * img.h / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * img.w / w, ((x + 1) * img.w / w).max(x * img.w / w + 1));
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u64::from(img.rgba[(sy * img.w + sx) * 4 + c]);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            for c in 0..4 {
                out[(y * w + x) * 4 + c] = (sum[c] / n) as u8;
            }
        }
    }
    out
}

/// 跳轉不準的格式（TS、M2TS、MPEG-PS）轉 GIF：第一格是主播放器在 A 的那一格（不是 A 之後的關鍵影格、
/// 也不是下一格），長度是 B − A。每個 `(t, lead)` 轉一個 1 秒的 GIF：`t` = 要的 A（主播放器精確跳過去，
/// A 是停住的那一格的時間），`lead` = EDL 從 A 前面幾秒開始（None = 從頭讀）。`out` = GIF 的大小（長邊 320）
fn check_ts_gif(rel: &str, out: (u32, u32), cases: &[(f64, Option<f64>)]) {
    let name = format!("gif-ts-{}", rel.replace(['/', '.'], "-"));
    let (mut p, caps) = ts_main_player(rel);
    if !gif_engine(&caps, &name) {
        return;
    }
    let dirs = clip_dirs(&name);
    let offset = p.demuxer_start_time();
    for (k, &(t, lead)) in cases.iter().enumerate() {
        // 主播放器在 A 的畫面、下一格（GIF 的格率對齊沒有從 A 算起時的樣子）、
        // 晚半秒的（關鍵影格隔 0.5 秒時，跳轉落在下一個關鍵影格的樣子）
        let (late_at, late) = main_frame(&mut p, t + 0.5, &dirs.1);
        let (a, want) = main_frame(&mut p, t, &dirs.1);
        let (next_at, next) = main_frame(&mut p, a + 0.02, &dirs.1);
        let (_, end) = main_frame(&mut p, a + 0.9, &dirs.1);
        let mut spec = gif_spec(&p, &caps, a, a + 1.0, gif_prefs(320, 10, false), &dirs);
        assert!(spec.approx_seek, "{rel}：跳轉不準的格式");
        assert_eq!(spec.out, out, "{rel}");
        spec.stem = format!("case-{k}");
        let seen = seen_plan(&mut spec);
        let (progress, done) = finish_gif(&gif::spawn(spec, no_wake()));
        let done = done.unwrap_or_else(|f| panic!("{rel} 在 {a}：{f:?}"));
        let plan = seen.lock().unwrap().clone().expect("編碼用的 mpv 沒有開好");
        let at = match lead {
            // 從 A 前面幾秒開始讀（試跳過，落在 A 之前），再精確跳到 A
            Some(secs) => {
                assert!((plan.edl_start() - (a - secs + offset)).abs() < 1e-3, "{rel}：{plan:?}");
                secs
            }
            // 從頭讀（A 離開頭很近，或往前試了還是落在 A 之後）
            None => {
                assert_eq!(plan.edl_start(), 0.0, "{rel}：{plan:?}");
                a + offset
            }
        };
        assert!(plan.start.is_some_and(|s| (s - at).abs() < 1e-3), "{rel}：{plan:?}");
        // 格率從 A 算起（fps 的 start_time）：A 之前多讀的畫面在濾鏡裡就丟掉，不進色盤
        assert!(
            plan.vf
                .contains(&format!(":start_time={:.6}:", at + gif::HR_SEEK_TOLERANCE)),
            "{rel}：{}",
            plan.vf
        );
        assert!(plan.duration > 1.0, "{rel}：EDL 的長度 {}", plan.duration);
        // 進度只往前走（A 之前多讀的部分算 0）
        let fractions: Vec<f32> = progress
            .iter()
            .filter(|p| p.phase == Phase::Converting)
            .filter_map(|p| p.fraction)
            .collect();
        assert!(
            fractions.windows(2).all(|w| w[0] <= w[1]),
            "{rel}：進度倒退 {fractions:?}"
        );
        let data = std::fs::read(&done.path).unwrap();
        let info = gif::gif_info(&data).unwrap();
        assert!((9..=11).contains(&info.frames), "{rel} 在 {a}：{} 格", info.frames);
        assert!(
            (info.seconds() - 1.0).abs() <= 0.15,
            "{rel} 在 {a}：總長 {} 秒",
            info.seconds()
        );
        let g = decode_gif(&data);
        assert_eq!((g.w, g.h), (out.0 as usize, out.1 as usize));
        // 色盤只有 256 色：跟同一格也有一點差；下一格差兩倍左右、晚半秒的差更多
        let first = mean_diff(&g.frames[0], &shrink(&want, g.w, g.h));
        let vs_next = mean_diff(&g.frames[0], &shrink(&next, g.w, g.h));
        let vs_late = mean_diff(&g.frames[0], &shrink(&late, g.w, g.h));
        assert!(
            first < 8.0 && vs_next > first * 1.3 && vs_late > first * 2.0,
            "{rel}：第一格跟 A（{a}）的畫面差 {first}、跟下一格（{next_at}）差 {vs_next}、跟 {late_at} 秒的差 {vs_late}"
        );
        // 最後一格是 B 前面的畫面（不是更早就結束）
        let last = mean_diff(&g.frames[g.frames.len() - 1], &shrink(&end, g.w, g.h));
        assert!(last < 12.0, "{rel}：最後一格跟 {} 秒的畫面差 {last}", a + 0.9);
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 等 GIF 的工作結束，收集進度（GIF 比片段慢：給足時間）
fn finish_gif(job: &Job) -> (Vec<Progress>, Result<Done, Failure>) {
    let deadline = Instant::now() + CLIP_TIMEOUT;
    let mut progress = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "等不到匯出結束");
        match job.try_recv() {
            Some(JobEvent::Progress(p)) => progress.push(p),
            Some(JobEvent::Finished(r)) => return (progress, r),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn gif_ts_starts_at_the_frame_at_a() {
    // 只有開頭一個關鍵影格的 TS、M2TS：在 A 跳轉會落到檔尾（以前整個失敗）。A 離開頭很近：從頭讀。
    // A 在影片時間戳的各種位置（每秒 24 格、GIF 每秒 10 格：1.5 到 1.7 秒是 0.1 秒格線上的五種相位），
    // 格率沒有從 A 算起時，有一半的位置第一格是 A 的下一格（以前只測到剛好對齊的 1.5 秒）
    let phases: Vec<(f64, Option<f64>)> = [1.5, 1.55, 1.6, 1.65, 1.7].iter().map(|&t| (t, None)).collect();
    check_ts_gif("general/ts_h264_aac.ts", (320, 240), &phases);
    check_ts_gif("general/m2ts_h264_ac3.m2ts", (320, 240), &[(1.5, None), (1.55, None)]);
}

#[test]
fn gif_ts_long_gop_reads_from_far_enough_back() {
    check_ts_gif(
        "general/ts_h264_gop10.ts",
        (320, 240),
        &[
            // 每 10 秒一個關鍵影格：A = 0 也要從頭讀（剛好跳到第一個時間戳會落到下一個關鍵影格，以前從 10 秒開始）；
            // 0.05 秒：不在 GIF 的格線上
            (0.0, None),
            (0.05, None),
            // A = 12：在 7 秒試跳，落在開頭的關鍵影格（A 之前），從 A 前面 5 秒讀
            (12.0, Some(5.0)),
            // A = 15：在 10 秒試跳，落到檔尾（11.5 秒的關鍵影格之後沒有了）；往前 30 秒已經在開頭之前：從頭讀
            (15.0, None),
        ],
    );
}

#[test]
fn gif_ts_short_gop_does_not_lose_a_gop() {
    // 每 0.5 秒一個關鍵影格（像電視錄影）：在 A 跳轉常落在 A 之後的關鍵影格，以前少了最多 0.5 秒、長度檢查看不出來。
    // A 在關鍵影格中間：從 A 前面 5 秒讀，再精確跳到 A
    check_ts_gif("general/ts_h264_gop05.ts", (320, 240), &[(10.3, Some(5.0))]);
}

#[test]
fn gif_mpeg_ps_starts_at_the_frame_at_a() {
    // MPEG-PS（.mpg）、DVD 的 VOB：FFmpeg 一樣用時間戳搜尋（file-format 是 mpeg），走 TS 的做法。
    // A 離開頭很近：從頭讀。VOB 是 16:9 的變形寬螢幕（720×480）：GIF 是 320×180
    check_ts_gif("general/mpg_mpeg1_mp2.mpg", (320, 240), &[(1.5, None), (1.62, None)]);
    check_ts_gif("general/vob_mpeg2_ac3_anamorphic.vob", (320, 180), &[(1.5, None)]);
}

#[test]
fn gif_ts_external_subtitles_follow_the_edl_start() {
    // TS 從頭讀時 EDL 的 0 秒是影片的時間戳 0（主播放器的 −1.46 秒），不是 A：外掛字幕的延遲跟著換算，
    // GIF 的第 2 格（影片的 1.6 秒）才有字幕檔 1.5 秒起的那一句
    let (mut p, caps) = ts_main_player("general/ts_h264_gop05.ts");
    if !gif_engine(&caps, "gif_ts_external_subtitles_follow_the_edl_start") {
        return;
    }
    p.add_subtitle(&sample_str("common/extsub_srt_utf8.srt")).unwrap();
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Sub).is_some_and(|t| t.external))
        .unwrap();
    let dirs = clip_dirs("gif-ts-extsub");
    let spec = gif_spec(&p, &caps, 1.5, 2.5, gif_prefs(320, 10, true), &dirs);
    assert!(matches!(spec.sub, Some(gif::GifSub::External { .. })), "{:?}", spec.sub);
    let mut none = spec.clone();
    none.sub = None;
    none.graph.subtitles = false;
    none.stem = "ts-ext-none".into();
    let mut with = spec;
    with.stem = "ts-ext".into();
    let seen = seen_plan(&mut with);
    let with = read_gif(&export_gif(with).unwrap().path);
    let without = read_gif(&export_gif(none).unwrap().path);
    assert_eq!(
        seen.lock().unwrap().as_ref().map(SeenPlan::edl_start),
        Some(0.0),
        "從頭讀"
    );
    let h = with.h;
    let bottom = band_changed(&with, &without, 1, h * 3 / 4..h);
    let top = band_changed(&with, &without, 1, 0..h / 4);
    assert!(
        bottom > 0.01 && bottom > top * 4.0,
        "外掛字幕：下方 {bottom}、上方 {top}"
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_ts_subtitles_follow_the_trimmed_grid() {
    // 從 A 前面讀時 fps 從 A 算起，送出的時間是 1/10 秒的整數倍，比畫面晚一點（最多一格）：字幕延遲跟著加上。
    // A = 1.443：EDL 的 A 是 1.443 + 1.462 + 0.005 = 2.91，第一格標 3.0 秒（晚 0.09 秒），畫面是主播放器 1.448 秒的。
    // 字幕檔第二句 1.5 秒開始：第一格沒有字幕、第二格（1.548 秒）有。沒有加上時第一格照 1.538 秒找字幕，早一格出現
    let (mut p, caps) = ts_main_player("general/ts_h264_gop05.ts");
    if !gif_engine(&caps, "gif_ts_subtitles_follow_the_trimmed_grid") {
        return;
    }
    let offset = p.demuxer_start_time();
    assert!((offset - 1.462).abs() < 0.01, "樣本的開始時間 {offset}");
    p.add_subtitle(&sample_str("common/extsub_srt_utf8.srt")).unwrap();
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Sub).is_some_and(|t| t.external))
        .unwrap();
    let dirs = clip_dirs("gif-ts-grid-subs");
    // 第一格的 EDL 時間剛好在 1/10 秒格線後面 0.01 秒（不管開始時間是多少）
    let a = 2.91 - offset - gif::HR_SEEK_TOLERANCE;
    let spec = gif_spec(&p, &caps, a, a + 1.0, gif_prefs(320, 10, true), &dirs);
    let mut none = spec.clone();
    none.sub = None;
    none.graph.subtitles = false;
    none.stem = "grid-none".into();
    let mut with = spec;
    with.stem = "grid-subs".into();
    let seen = seen_plan(&mut with);
    let with = read_gif(&export_gif(with).unwrap().path);
    let without = read_gif(&export_gif(none).unwrap().path);
    let plan = seen.lock().unwrap().clone().unwrap();
    assert_eq!(plan.edl_start(), 0.0, "從頭讀：{plan:?}");
    let h = with.h;
    let first = band_changed(&with, &without, 0, h * 3 / 4..h);
    let second = band_changed(&with, &without, 1, h * 3 / 4..h);
    assert!(
        first < 0.002 && second > 0.01,
        "第一格（1.448 秒）不該有字幕：下方變了 {first}；第二格（1.548 秒）該有：{second}"
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_ts_a_between_frames_keeps_the_frame_on_screen() {
    // A 在兩格之間（每秒 24 格的 TS，A 在第 2 格之後 0.02 秒）：第一格是 A 那一刻畫面上的那一格
    // （A 之前的最後一格，是 B 影格），不是開頭的關鍵影格
    let (mut p, caps) = ts_main_player("general/ts_h264_gop05.ts");
    if !gif_engine(&caps, "gif_ts_a_between_frames_keeps_the_frame_on_screen") {
        return;
    }
    let dirs = clip_dirs("gif-ts-between");
    let (shown_at, shown) = main_frame(&mut p, 0.06, &dirs.1);
    let (first_at, first) = main_frame(&mut p, 0.0, &dirs.1);
    assert!(shown_at - first_at > 0.03, "樣本的影格 {first_at}、{shown_at}");
    let a = shown_at + 0.02;
    let spec = gif_spec(&p, &caps, a, a + 1.0, gif_prefs(320, 10, false), &dirs);
    let g = read_gif(&export_gif(spec).unwrap().path);
    let vs_shown = mean_diff(&g.frames[0], &shown.rgba);
    let vs_first = mean_diff(&g.frames[0], &first.rgba);
    assert!(
        vs_shown < 8.0 && vs_first > vs_shown * 1.3,
        "第一格跟 A 那一刻的畫面（{shown_at}）差 {vs_shown}、跟開頭那一格（{first_at}）差 {vs_first}"
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_ts_video_starting_after_a_is_not_too_short() {
    // 電視錄影常從 GOP 中間開始：聲音先開始，影像到第一個關鍵影格（這個樣本晚 1.6 秒）才有。
    // 主播放器的時間從聲音算起，A 選在影像出來之前（離開頭超過 1 秒，長度檢查不多給）：
    // fps 從 A 算起，第一格補到 A（影像的第一格），GIF 是完整的 1 秒，不算失敗（以前只有 0.6 秒、TooShort）
    let (p, caps) = ts_main_player("general/ts_h264_late_video.ts");
    if !gif_engine(&caps, "gif_ts_video_starting_after_a_is_not_too_short") {
        return;
    }
    let dirs = clip_dirs("gif-ts-late-video");
    let spec = gif_spec(&p, &caps, 1.2, 2.2, gif_prefs(320, 10, false), &dirs);
    assert!(spec.approx_seek);
    assert!(spec.slack < 0.5, "不在檔案的頭尾：{}", spec.slack);
    let done = export_gif(spec).unwrap_or_else(|f| panic!("影像晚開始：{f:?}"));
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert!((9..=11).contains(&info.frames), "{} 格", info.frames);
    assert!((info.seconds() - 1.0).abs() <= 0.15, "總長 {} 秒", info.seconds());
    // 影像出來之前的幾格都是影像的第一格
    let g = read_gif(&done.path);
    assert!(mean_diff(&g.frames[0], &g.frames[3]) < 1.0, "補的格子不一樣");
    assert!(mean_diff(&g.frames[0], &g.frames[9]) > 4.0, "影像出來之後還是同一格");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_late_start_is_too_short_and_leaves_no_file() {
    // 開頭晚了（跳轉落在 A 之後的關鍵影格）的 GIF 算失敗、不留檔案：寫好之後的長度檢查照影片的格率，只容許幾格。
    // 故意走 EDL 從 A 開始的做法（以前 TS 的做法）：每 0.5 秒一個關鍵影格，A 在關鍵影格之後一點，
    // 跳轉最好也落在下一個關鍵影格，少了將近 0.5 秒（以前的寬鬆檢查容許少一半，看不出來）
    let (p, caps) = ts_main_player("general/ts_h264_gop05.ts");
    if !gif_engine(&caps, "gif_late_start_is_too_short_and_leaves_no_file") {
        return;
    }
    let dirs = clip_dirs("gif-late-start");
    let mut spec = gif_spec(&p, &caps, 10.05, 11.05, gif_prefs(320, 10, false), &dirs);
    assert!(spec.approx_seek);
    assert!(spec.slack < 0.5, "不在檔案的頭尾：{}", spec.slack);
    spec.approx_seek = false;
    match export_gif(spec) {
        Err(Failure::TooShort { got, want }) => {
            assert!(got < 0.75 && (want - 1.0).abs() < 1e-9, "{got} / {want}");
        }
        other => panic!("開頭晚了的 GIF 要算太短：{other:?}"),
    }
    assert!(names(&dirs.0).is_empty(), "不留檔案：{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn gif_rotated_is_portrait() {
    let dirs = clip_dirs("gif-rotate");
    // 手機直拍（檔案標示旋轉 90°）：GIF 是直的
    let (mut p, caps) = main_player(&sample_str("common/mov_hevc_aac_rot90.mov"));
    if !gif_engine(&caps, "gif_rotated_is_portrait") {
        return;
    }
    p.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
    let spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    assert_eq!(spec.graph.fixup.rotate, 90);
    assert_eq!(spec.out, (240, 320), "320×240 轉 90°");
    let done = export_gif(spec).unwrap();
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert_eq!((info.width, info.height), (240, 320));

    // 使用者轉了 90°（跟 Ctrl+E 截圖一樣，GIF 是畫面上看到的方向）：順時針轉
    let (mut q, caps) = main_player(&sample_str("common/mp4_h264_aac.mp4"));
    q.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
    let plain = gif_spec(&q, &caps, 0.5, 1.0, gif_prefs(320, 10, false), &dirs);
    let turned = Geometry {
        rotate: 90,
        ..Default::default()
    };
    let mut rotated = gif_spec_with(&q, &caps, 0.5, 1.0, gif_prefs(320, 10, false), &turned, &dirs).unwrap();
    assert_eq!((plain.out, rotated.out), ((320, 240), (240, 320)));
    rotated.stem = "rotated".into();
    let a = read_gif(&export_gif(plain).unwrap().path);
    let b = read_gif(&export_gif(rotated).unwrap().path);
    // 把沒轉的那一格順時針轉 90° 應該跟轉好的一樣，逆時針轉的不一樣
    let img = |g: &GifFrames| vitascope::screenshot::Image {
        w: g.w,
        h: g.h,
        rgba: g.frames[2].clone(),
    };
    let fix = |rotate: u32| vitascope::screenshot::Fixup {
        rotate,
        ..Default::default()
    };
    let cw = mean_diff(&img(&a).fixed(fix(90)).rgba, &b.frames[2]);
    let ccw = mean_diff(&img(&a).fixed(fix(270)).rgba, &b.frames[2]);
    assert!(cw < 12.0 && ccw > cw * 2.0, "順時針 {cw}、逆時針 {ccw}");
    // 左右翻轉：在旋轉之後（畫面上的方向）
    let flipped = Geometry {
        rotate: 90,
        hflip: true,
        ..Default::default()
    };
    let mut spec = gif_spec_with(&q, &caps, 0.5, 1.0, gif_prefs(320, 10, false), &flipped, &dirs).unwrap();
    spec.stem = "flipped".into();
    let c = read_gif(&export_gif(spec).unwrap().path);
    let want = img(&a).fixed(vitascope::screenshot::Fixup {
        rotate: 90,
        hflip: true,
        vflip: false,
    });
    let d = mean_diff(&want.rgba, &c.frames[2]);
    assert!(d < 12.0, "先轉再翻：{d}");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_hdr_tone_mapped() {
    let (mut p, caps) = main_player(&sample_str("general/mkv_hevc10_hdr10_mid.mkv"));
    if !gif_engine(&caps, "gif_hdr_tone_mapped") {
        return;
    }
    p.wait_state(TIMEOUT, |s| s.video_hdr).unwrap();
    let dirs = clip_dirs("gif-hdr");
    let spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    if !(caps.zscale && caps.tonemap) {
        // 沒有色調映射的濾鏡：不轉，完成時說明亮部會變白
        assert_eq!(spec.graph.tone, None);
        assert_eq!(spec.notes, [Note::HdrClipped]);
        eprintln!("略過 gif_hdr_tone_mapped 的色調映射：播放引擎沒有 zscale、tonemap");
        return;
    }
    // 自動的目標亮度 = 203 nits，曲線 Hable
    assert_eq!(
        spec.graph.tone,
        Some(gif::Tone {
            curve: "hable",
            npl: 203
        })
    );
    assert!(spec.notes.is_empty());
    // 比較：沒轉（PQ 的數值直接當成一般畫面）、目標亮度 100 nits（「畫質 → HDR」選 100：畫面比較亮）
    let mut plain = spec.clone();
    plain.graph.tone = None;
    plain.stem = "plain".into();
    let mut low = spec.clone();
    low.graph.tone = Some(gif::Tone {
        curve: "hable",
        npl: 100,
    });
    low.stem = "npl100".into();
    let mapped = read_gif(&export_gif(spec).unwrap().path);
    let raw = read_gif(&export_gif(plain).unwrap().path);
    let n100 = read_gif(&export_gif(low).unwrap().path);
    let mid = mapped.frames.len() / 2;
    let luma = band_luma(&mapped, mid, 0..mapped.h);
    let luma100 = band_luma(&n100, mid, 0..n100.h);
    let changed = mean_diff(&mapped.frames[mid], &raw.frames[mid]);
    eprintln!("HDR → 一般畫面：平均亮度 {luma:.1}（100 nits：{luma100:.1}；跟沒轉的差 {changed:.1}）");
    assert!((20.0..=200.0).contains(&luma), "平均亮度 {luma}");
    assert!(changed > 8.0, "有轉跟沒轉差不多：{changed}");
    // 目標亮度跟播放時一樣（數字越小越亮）
    assert!(luma100 > luma + 8.0, "100 nits {luma100}、203 nits {luma}");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_burns_subtitles() {
    // 內嵌的字幕（1.5–2.6 秒「影戲播放器測試」）：燒進畫面的下方
    let (mut p, caps) = main_player(&sample_str("common/mkv_multitrack.mkv"));
    if !gif_engine(&caps, "gif_burns_subtitles") {
        return;
    }
    p.wait_state(TIMEOUT, |s| s.sid.is_some()).unwrap();
    let dirs = clip_dirs("gif-subs");
    let spec = gif_spec(&p, &caps, 1.5, 2.5, gif_prefs(480, 10, true), &dirs);
    assert!(matches!(spec.sub, Some(gif::GifSub::Embedded(_))), "{:?}", spec.sub);
    assert_eq!(spec.sub_delay, 0.0, "內嵌的字幕延遲照舊");
    let mut none = spec.clone();
    none.sub = None;
    none.graph.subtitles = false;
    none.stem = "no-subs".into();
    let with = read_gif(&export_gif(spec).unwrap().path);
    let without = read_gif(&export_gif(none).unwrap().path);
    let mid = with.frames.len() / 2;
    let (h, w) = (with.h, with.w);
    let bottom = band_changed(&with, &without, mid, h * 3 / 4..h);
    let top = band_changed(&with, &without, mid, 0..h / 4);
    assert!(
        bottom > 0.01 && bottom > top * 4.0,
        "下方 {bottom}、上方 {top}（{w}×{h}）"
    );

    // 外掛的字幕（自動載入的 .srt）：外掛字幕照播放時間走，延遲要減掉 A，GIF 的開頭才有字
    let (mut q, caps) = main_player(&sample_str("common/extsub_unlabeled_content.mkv"));
    q.wait_state(TIMEOUT, |s| s.selected(TrackKind::Sub).is_some_and(|t| t.external))
        .unwrap();
    let spec = gif_spec(&q, &caps, 1.5, 2.5, gif_prefs(320, 10, true), &dirs);
    match &spec.sub {
        Some(gif::GifSub::External { embedded, .. }) => assert_eq!(*embedded, 0),
        other => panic!("應該是外掛字幕：{other:?}"),
    }
    assert!((spec.sub_delay + 1.5).abs() < 1e-9, "{}", spec.sub_delay);
    let mut none = spec.clone();
    none.sub = None;
    none.graph.subtitles = false;
    let mut with = spec;
    with.stem = "ext".into();
    none.stem = "ext-none".into();
    let with = read_gif(&export_gif(with).unwrap().path);
    let without = read_gif(&export_gif(none).unwrap().path);
    // 第 2 格（GIF 的 0.1 秒 = 影片的 1.6 秒）：字幕檔 1.5 秒起的那一句。沒有減掉 A 的話是字幕檔的 0.1 秒，那時沒有字幕
    let h = with.h;
    let bottom = band_changed(&with, &without, 1, h * 3 / 4..h);
    let top = band_changed(&with, &without, 1, 0..h / 4);
    assert!(
        bottom > 0.01 && bottom > top * 4.0,
        "外掛字幕：下方 {bottom}、上方 {top}"
    );
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_restarts_when_the_subtitle_id_does_not_match() {
    let (mut p, caps) = main_player(&sample_str("common/mkv_multitrack.mkv"));
    if !gif_engine(&caps, "gif_restarts_when_the_subtitle_id_does_not_match") {
        return;
    }
    p.wait_state(TIMEOUT, |s| s.sid.is_some()).unwrap();
    let dirs = clip_dirs("gif-sid");
    let mut spec = gif_spec(&p, &caps, 1.5, 2.0, gif_prefs(320, 10, true), &dirs);
    let want = spec.sub.as_ref().unwrap().predicted_id();
    // 第一次用錯的編號（沒有這條字幕）：發現選到的不對，用對的編號重開
    spec.test.initial_sid = Some(Some(9));
    let sid: Arc<Mutex<Option<String>>> = Arc::default();
    let s = sid.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        *s.lock().unwrap() = mpv.get_string("sid").ok();
    }));
    let done = export_gif(spec).unwrap();
    assert_eq!(sid.lock().unwrap().as_deref(), Some(want.to_string().as_str()));
    assert_eq!(names(&dirs.0), [file_name_of(&done.path)], "重開時不留暫存檔");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn gif_failing_filter_is_an_error_not_a_file() {
    // mpv 停用失敗的濾鏡、照樣把畫面送出去（我們的濾鏡還在，大小也對）：只有記錄裡的「Disabling filter」看得出來
    let (p, caps) = main_player(&sample_str("common/mp4_h264_aac.mp4"));
    if !gif_engine(&caps, "gif_failing_filter_is_an_error_not_a_file") {
        return;
    }
    let dirs = clip_dirs("gif-fail");
    let mut spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    let bad = "crop=w=9999:h=9999";
    spec.test.extra_vf = Some(format!("lavfi=graph=%{}%{bad}", bad.len()));
    let late = spec.clone();
    assert_eq!(export_gif(spec).map(|d| d.path), Err(Failure::FilterFailed));
    assert!(names(&dirs.0).is_empty(), "失敗時不留檔案：{:?}", names(&dirs.0));
    // 「Disabling filter」在 Shutdown 之後才收到（mpv 先送排著的事件、最後才送記錄）：一樣要算失敗。
    // 讓編碼用的 mpv 自己跑完、寫完 GIF 才開始收事件：Shutdown 排在所有記錄前面
    let mut spec = late;
    spec.test.on_ready = Some(Arc::new(|mpv: &Mpv| wait_until_encoded(mpv)));
    assert_eq!(export_gif(spec).map(|d| d.path), Err(Failure::FilterFailed));
    assert!(names(&dirs.0).is_empty(), "失敗時不留檔案：{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_cancel_leaves_nothing() {
    let (p, caps) = main_player(&sample_str("common/mp4_long.mp4"));
    if !gif_engine(&caps, "gif_cancel_leaves_nothing") {
        return;
    }
    let dirs = clip_dirs("gif-cancel");
    let mut spec = gif_spec(&p, &caps, 1.0, 20.0, gif_prefs(320, 10, false), &dirs);
    // 每格一個色盤：畫面一開始就寫進暫存檔（整段一個色盤要讀完才寫，取消時還沒有檔案）
    spec.graph.palette = gif::PaletteMode::PerFrame;
    // 編碼用的 mpv 開好之後停住（mpv 照樣在編碼），等測試取消
    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();
    spec.test.on_ready = Some(Arc::new(move |_: &Mpv| {
        b.wait();
        b.wait();
    }));
    let job = gif::spawn(spec, no_wake());
    barrier.wait();
    // 暫存檔已經寫了一部分（不然「不留檔案」什麼都沒測到）
    let start = Instant::now();
    while parts(&dirs.0).is_empty() {
        assert!(
            start.elapsed() < TIMEOUT,
            "編碼用的 mpv 一直沒有寫暫存檔：{:?}",
            names(&dirs.0)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(names(&dirs.0).len(), 1, "{:?}", names(&dirs.0));
    job.cancel();
    barrier.wait();
    assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled));
    drop(job);
    assert!(names(&dirs.0).is_empty(), "取消後不留檔案：{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 測試的觀察點：讓編碼用的 mpv 自己播完這一段才放行（工作在這之前不收事件、記錄）。
/// 播完之後 mpv 寫完 GIF、很快就送出 Shutdown：排著的事件都比記錄先送來，測「Shutdown 之後才收到的記錄」
fn wait_until_encoded(mpv: &Mpv) {
    let start = Instant::now();
    // 播完了：`path` 沒有了（idle=once：之後寫完 GIF 就結束）
    while mpv.get_string("path").is_ok() && start.elapsed() < CLIP_TIMEOUT {
        std::thread::sleep(Duration::from_millis(20));
    }
    // 讓 mpv 寫完 GIF、送出 Shutdown。不是證明：來不及時測試照樣通過，只是比較抓不到漏收的記錄
    std::thread::sleep(Duration::from_millis(500));
}

#[test]
fn gif_ends_promptly_when_the_encoder_stops_with_an_error() {
    // 編碼用的 mpv 讀到一半出錯就結束（EndFile 是 Error、接著 Shutdown）：收完排著的事件時收到的 Shutdown 也要算，
    // 不能一直等到逾時（2 分鐘以上）再回報「讀取逾時」
    let (p, caps) = main_player(&sample_str("common/mp4_h264_aac.mp4"));
    if !gif_engine(&caps, "gif_ends_promptly_when_the_encoder_stops_with_an_error") {
        return;
    }
    let dirs = clip_dirs("gif-error-end");
    let mut spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    // 一格都不送出去（fps 丟掉 start_time 之前的畫面；播放引擎沒有 select、trim）：mpv 播完時什麼都沒播到，
    // 結束的原因是錯誤，記錄裡沒有濾鏡失敗、寫不進去這些會讓工作馬上停下來的句子
    let none = "fps=fps=10:start_time=100000";
    spec.test.extra_vf = Some(format!("lavfi=graph=%{}%{none}", none.len()));
    spec.test.on_ready = Some(Arc::new(|mpv: &Mpv| wait_until_encoded(mpv)));
    let start = Instant::now();
    let r = export_gif(spec).map(|d| d.path);
    assert!(r.is_err(), "{r:?}");
    assert_ne!(r, Err(Failure::ReadTimeout), "等不到 Shutdown，一直等到逾時");
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "結束得太慢：{:?}",
        start.elapsed()
    );
    assert!(names(&dirs.0).is_empty(), "失敗時不留檔案：{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_waits_for_the_first_frame() {
    // 還沒解出第一格（例如網路很慢）：不知道檔案本身的旋轉、像素比例，不能開始（不能猜成橫的：手機直拍會轉成躺著的 GIF）。
    // 解碼器丟掉所有的畫面：檔案開好、選好影像，但一直沒有第一格（不靠時間）
    let opts = Options {
        extra: vec![
            ("pause".into(), "yes".into()),
            ("vd-lavc-skipframe".into(), "all".into()),
            ("keep-open".into(), "always".into()),
        ],
        ..Options::headless()
    };
    let (p, caps) = main_player_with(opts, &sample_str("common/mov_hevc_aac_rot90.mov"));
    if !gif_engine(&caps, "gif_waits_for_the_first_frame") {
        return;
    }
    assert!(p.natural_shape().is_none(), "測試的前提：還沒解出第一格");
    assert!(p.state.loaded && p.state.selected(TrackKind::Video).is_some());
    assert_eq!(gif::unavailable(&p, &caps), None);
    assert_eq!(gif::sizes_for(&p, &Geometry::default(), 320), None);
    let dirs = clip_dirs("gif-first-frame");
    let tone = ToneSettings::default();
    let style = SubStyle::default();
    let geometry = Geometry::default();
    let choice = gif::Choice {
        prefs: gif_prefs(320, 10, false),
        geometry: &geometry,
        tone: &tone,
        style: &style,
        deinterlace: Deinterlace::Auto,
    };
    let r = GifSpec::from_player(&p, &caps, 0.5, 1.5, &choice, dirs.0.clone(), dirs.1.clone());
    assert_eq!(r.map(|s| s.out), Err(Failure::NoData));
    drop(p);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_follows_the_container_pixel_aspect() {
    // 容器標示的像素比例（MP4 的 pasp）跟影像資料的不同：mpv 照容器的顯示，GIF 也要一樣
    let dirs = clip_dirs("gif-par");
    let mut data = std::fs::read(sample("common/mp4_h264_aac.mp4")).unwrap();
    let Some(at) = data.windows(4).position(|w| w == b"pasp") else {
        eprintln!("略過 gif_follows_the_container_pixel_aspect：這個平台的 ffmpeg 沒有寫 pasp");
        return;
    };
    // pasp：水平、垂直的間距（各 4 位元組）→ 像素寬 2 倍（320×240 的方形像素 → 畫面上 640×240）
    data[at + 4..at + 8].copy_from_slice(&2u32.to_be_bytes());
    data[at + 8..at + 12].copy_from_slice(&1u32.to_be_bytes());
    let file = dirs.1.join("wide-pixels.mp4");
    std::fs::write(&file, &data).unwrap();
    let (p, caps) = main_player(&file.to_string_lossy());
    if !gif_engine(&caps, "gif_follows_the_container_pixel_aspect") {
        return;
    }
    // 測試的前提：主播放器照容器的比例顯示（8:3），影像資料本身是 4:3
    let start = Instant::now();
    while p.get_f64("video-params/aspect").is_err() {
        assert!(start.elapsed() < TIMEOUT, "主播放器一直沒有畫面");
        std::thread::sleep(Duration::from_millis(20));
    }
    let shown = p.get_f64("video-params/aspect").unwrap();
    assert!((shown - 8.0 / 3.0).abs() < 0.01, "畫面上的比例 {shown}");
    let spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    assert_eq!(spec.out, (320, 120));
    let done = export_gif(spec).unwrap();
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert_eq!((info.width, info.height), (320, 120));
    drop(p);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_uses_the_selected_video_track() {
    // 有好幾條影像的檔案（多畫質）：GIF 用主播放器選的那一條，不是 mpv 預設的。
    // 本專案的引擎：本機的 DASH 每個畫質一條影像；系統的 libmpv 0.37：HLS 也是（新版的 HLS 是 edition）
    let candidates = ["net/dash_multi/manifest.mpd", "net/hls_multi/master.m3u8"];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated");
    let mut found = None;
    for rel in candidates {
        let path = root.join(rel);
        if !path.exists() {
            continue;
        }
        let (p, caps) = main_player(&path.to_string_lossy());
        if p.state.tracks_of(TrackKind::Video).count() >= 2 {
            found = Some((p, caps));
            break;
        }
    }
    let Some((mut p, caps)) = found else {
        eprintln!("略過 gif_uses_the_selected_video_track：沒有兩條影像的樣本（{candidates:?}）");
        return;
    };
    if !gif_engine(&caps, "gif_uses_the_selected_video_track") {
        return;
    }
    // 換成沒被選的那一條（另一個畫質，寬度不同）
    let current = p.state.selected(TrackKind::Video).unwrap().clone();
    let other = p
        .state
        .tracks_of(TrackKind::Video)
        .find(|t| t.id != current.id)
        .cloned()
        .unwrap();
    let width = other.width.expect("影像軌道沒有寬度");
    assert_ne!(current.width, Some(width), "兩條影像的大小要不同");
    p.select_track(TrackKind::Video, Some(other.id)).unwrap();
    p.wait_state(TIMEOUT, |s| {
        s.selected(TrackKind::Video).map(|t| t.id) == Some(other.id)
    })
    .unwrap();
    let dirs = clip_dirs("gif-vid");
    let mut spec = gif_spec(&p, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    assert!(spec.video.is_some());
    let seen: Arc<Mutex<Option<i64>>> = Arc::default();
    let s = seen.clone();
    spec.test.on_ready = Some(Arc::new(move |mpv: &Mpv| {
        *s.lock().unwrap() = mpv.get_property::<i64>("current-tracks/video/demux-w").ok();
    }));
    let done = export_gif(spec).unwrap();
    assert_eq!(*seen.lock().unwrap(), Some(width), "編碼用的 mpv 選了別的影像");
    assert!(gif::gif_info(&std::fs::read(&done.path).unwrap()).is_some());
    drop(p);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_is_unavailable_without_video_or_engine_support() {
    let dirs = clip_dirs("gif-unavailable");
    // 只有聲音（專輯封面不算影像）
    let (p, caps) = main_player(&sample_str("general/audio_mp3_cover.mp3"));
    assert_eq!(gif::unavailable(&p, &caps), Some(Failure::NoVideo));
    // 播放引擎沒有 gif 編碼器、palettegen
    let (q, caps) = main_player(&sample_str("common/mp4_long.mp4"));
    for no in [
        EngineCaps { gif: false, ..caps },
        EngineCaps {
            palettegen: false,
            ..caps
        },
    ] {
        assert_eq!(gif::unavailable(&q, &no), Some(Failure::EncoderMissing));
    }
    if !gif_engine(&caps, "gif_is_unavailable_without_video_or_engine_support") {
        return;
    }
    assert_eq!(gif::unavailable(&q, &caps), None);
    // 長度：最長 30 秒、最短 0.2 秒
    let len = |a: f64, b: f64| {
        gif_spec_with(&q, &caps, a, b, gif_prefs(320, 10, false), &Geometry::default(), &dirs).map(|s| s.b)
    };
    assert_eq!(len(1.0, 31.5), Err(Failure::GifTooLong));
    assert_eq!(len(1.0, 1.1), Err(Failure::RangeTooShort));
    assert_eq!(len(1.0, 31.0), Ok(31.0));
    // 使用者開的 EDL：時間跟檔案對不上
    let src = sample_str("general/mkv_h264_gop2.mkv");
    let entry = format!("%{}%{src}", src.len());
    let edl = dirs.1.join("list.edl");
    std::fs::write(&edl, format!("# mpv EDL v0\n{entry},0,3\n{entry},6,3\n")).unwrap();
    let (r, caps) = main_player(&edl.to_string_lossy());
    assert_eq!(gif::unavailable(&r, &caps), Some(Failure::Timeline));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn gif_http_vod_and_site_video() {
    let server = Server::start();
    // 能跳轉的網路影片：用同樣的連線設定再讀這一段
    let url = server.file_url("general/mkv_h264_gop2.mkv");
    let net_settings = NetSettings {
        user_agent: "VitaScope-Gif/1.0".into(),
        ..NetSettings::default()
    };
    let (mut p, caps) = main_player_net(&net_settings, &url);
    if !gif_engine(&caps, "gif_http_vod_and_site_video") {
        return;
    }
    p.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let dirs = clip_dirs("gif-http");
    let spec = gif_spec(&p, &caps, 3.0, 4.0, gif_prefs(320, 10, false), &dirs);
    assert!(matches!(&spec.source, Source::Net(s) if s.open == url));
    let before = server.requests_to("/f/general/mkv_h264_gop2.mkv").len();
    let done = export_gif(spec).unwrap();
    let reqs = server.requests_to("/f/general/mkv_h264_gop2.mkv");
    assert!(reqs.len() > before, "編碼用的 mpv 要自己讀");
    for r in &reqs[before..] {
        assert_eq!(r.header("User-Agent"), Some("VitaScope-Gif/1.0"), "{r:#?}");
    }
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert!((9..=11).contains(&info.frames), "{} 格", info.frames);
    // 直播：不能轉
    let mut q = Player::new(Options {
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    })
    .unwrap();
    let qcaps = q.probe_caps();
    q.open(&server.url("/hlslive/net/hls_vod/index.m3u8")).unwrap();
    q.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 路徑可能比 FileLoaded 晚到（沒有路徑時是 NoData，不是直播）
    q.wait_state(TIMEOUT, |s| s.loaded && s.path.is_some() && !s.tracks.is_empty())
        .unwrap();
    assert_eq!(gif::unavailable(&q, &qcaps), Some(Failure::Unbounded));

    // 網站影片（yt-dlp：影像、聲音分開的 EDL）：EDL 裡再包一層 EDL，帶網站要的標頭
    let page = server.url("/watch?v=gif1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "gif1")).arc();
    let resolver: Arc<dyn Resolve> = fake.clone();
    let opts = Options {
        net_hooks: true,
        net_resolver: Some(resolver),
        net_sites: vec!["127.0.0.1".into()],
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    };
    let (mut s, caps) = main_player_with(opts, &page);
    s.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let spec = gif_spec(&s, &caps, 0.5, 1.5, gif_prefs(320, 10, false), &dirs);
    assert!(matches!(&spec.source, Source::Net(n) if n.site && n.open.starts_with("edl://")));
    let before = server.requests_to("/f/net/video_only.mp4").len();
    let done = export_gif(spec).unwrap();
    let reqs = server.requests_to("/f/net/video_only.mp4");
    assert!(reqs.len() > before, "編碼用的 mpv 要自己讀");
    for r in &reqs[before..] {
        assert_eq!(r.header("User-Agent"), Some(support::fake_ytdl::SITE_UA), "{r:#?}");
    }
    assert_eq!(fake.calls(), 1, "轉 GIF 不再問一次 yt-dlp");
    let info = gif::gif_info(&std::fs::read(&done.path).unwrap()).unwrap();
    assert!((9..=11).contains(&info.frames), "{} 格", info.frames);
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

// ───────────── 縮圖總覽圖 ─────────────

fn sheet_prefs(columns: u32, rows: u32, width: u32, format: ImageFormat) -> SheetPrefs {
    SheetPrefs {
        columns,
        rows,
        width,
        timestamps: true,
        header: true,
        format,
        jpeg_quality: 90,
    }
}

/// 從主播放器準備一張總覽圖（設定用預設的：HDR、去交錯）。先等第一格解出來
fn sheet_spec_with(
    p: &Player,
    caps: &EngineCaps,
    prefs: SheetPrefs,
    geometry: &Geometry,
    range: Option<(f64, f64)>,
    dirs: &(PathBuf, PathBuf),
) -> Result<SheetSpec, Failure> {
    wait_first_frame(p);
    let tone = ToneSettings::default();
    let choice = sheet::Choice {
        prefs,
        geometry,
        tone: &tone,
        deinterlace: Deinterlace::Auto,
        range,
        title: None,
    };
    SheetSpec::from_player(p, caps, &choice, dirs.0.clone(), dirs.1.clone())
}

fn sheet_spec(p: &Player, caps: &EngineCaps, prefs: SheetPrefs, dirs: &(PathBuf, PathBuf)) -> SheetSpec {
    sheet_spec_with(p, caps, prefs, &Geometry::default(), None, dirs).unwrap_or_else(|f| panic!("不能做總覽圖：{f:?}"))
}

/// 做這張總覽圖，等到結束（收集進度）
fn export_sheet(spec: SheetSpec) -> (Vec<Progress>, Result<Done, Failure>) {
    finish_gif(&sheet::spawn(spec, no_wake()))
}

/// 取完的格子：第幾格、目標、實際的時間（主播放器的時間；取不到時 None）
type SeenCells = Arc<Mutex<Vec<(usize, f64, Option<f64>)>>>;

/// 記下每一格的目標與實際的時間
fn seen_cells(spec: &mut SheetSpec) -> SeenCells {
    let seen: SeenCells = Arc::default();
    let s = seen.clone();
    spec.test.on_cell = Some(Arc::new(move |i, t, at| s.lock().unwrap().push((i, t, at))));
    seen
}

/// 讀存好的總覽圖（RGBA）
fn read_sheet(path: &Path) -> vitascope::screenshot::Image {
    if path.extension().is_some_and(|e| e == "png") {
        return vitascope::screenshot::decode_png(path).unwrap();
    }
    let img = image::load_from_memory_with_format(&std::fs::read(path).unwrap(), image::ImageFormat::Jpeg)
        .unwrap()
        .to_rgba8();
    vitascope::screenshot::Image {
        w: img.width() as usize,
        h: img.height() as usize,
        rgba: img.into_raw(),
    }
}

/// 圖的一塊
fn crop(img: &vitascope::screenshot::Image, x: u32, y: u32, w: u32, h: u32) -> vitascope::screenshot::Image {
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        rgba.extend_from_slice(&img.rgba[(row * img.w + x) * 4..][..w * 4]);
    }
    vitascope::screenshot::Image { w, h, rgba }
}

/// 第 `i` 格
fn sheet_cell(img: &vitascope::screenshot::Image, l: &sheet::Layout, i: usize) -> vitascope::screenshot::Image {
    let (x, y) = l.cell_pos(i);
    crop(img, x, y, l.cell.0, l.cell.1)
}

/// 平均亮度（RGB 的平均）
fn mean_luma(img: &vitascope::screenshot::Image) -> f64 {
    let sum: u64 = img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| (u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])) / 3)
        .sum();
    sum as f64 / (img.w * img.h) as f64
}

/// 這張總覽圖的時間標記的方塊（每格右下角；跟 `sheet::compose` 一樣的算法，寬鬆一點）
fn stamp_region(l: &sheet::Layout, i: usize, text: &str) -> (u32, u32, u32, u32) {
    let mut painter = vitascope::export::text::TextPainter::new();
    let (w, h) = painter.measure(text, l.stamp_px);
    let pad = sheet::STAMP_PAD;
    let (bw, bh) = ((w + 2.0 * pad).ceil() as u32, (h + 2.0 * pad).ceil() as u32);
    let (x, y) = l.cell_pos(i);
    (x + l.cell.0 - bw - pad as u32, y + l.cell.1 - bh - pad as u32, bw, bh)
}

#[test]
fn sheet_grid_and_stamps() {
    // 20 秒、640×360 的影片：3 欄 × 2 列、1280 寬、JPEG，有標頭與時間標記
    let (p, caps) = main_player(&sample_str("common/mkv_multitrack.mkv"));
    let dirs = clip_dirs("sheet-grid");
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(3, 2, 1280, ImageFormat::Jpeg), &dirs);
    assert_eq!(spec.stem, "mkv_multitrack 縮圖");
    assert_eq!(spec.wanted(), dirs.0.join("mkv_multitrack 縮圖.jpg"));
    // 整部：0 到主播放器的總長度（20 秒多一點）
    let range = spec.range;
    assert_eq!(range, (0.0, p.state.duration.unwrap()));
    assert!((range.1 - 20.0).abs() < 0.1, "{range:?}");
    assert_eq!(spec.layout, sheet::layout(1280, 3, 2, 640.0 / 360.0, spec.header.len()));
    assert_eq!(spec.render, spec.layout.cell);
    // 標頭：檔名、大小／長度／格式、影像、聲音（沒有中文字型的電腦用英文：中文會變成方塊）
    let header = spec.header.join("\n");
    assert_eq!(spec.header[0], "mkv_multitrack.mkv");
    if vitascope::fonts::has_cjk() {
        for part in [
            "長度：00:20",
            "格式：Matroska",
            "影像：H.264",
            "640×360",
            "24 fps",
            "音訊：AAC",
            "48 kHz",
        ] {
            assert!(header.contains(part), "標頭沒有「{part}」：{header}");
        }
    } else {
        for part in [
            "Length: 00:20",
            "Format: Matroska",
            "Video: H.264",
            "640×360",
            "Audio: AAC",
        ] {
            assert!(header.contains(part), "標頭沒有「{part}」：{header}");
        }
    }
    let layout = spec.layout.clone();
    let seen = seen_cells(&mut spec);
    let (progress, done) = export_sheet(spec);
    let done = done.unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(done.kind, Kind::Sheet);
    assert_eq!(done.path, dirs.0.join("mkv_multitrack 縮圖.jpg"));
    assert_eq!(names(&dirs.0), ["mkv_multitrack 縮圖.jpg"], "不留暫存檔");
    assert_eq!(done.bytes, std::fs::metadata(&done.path).unwrap().len());
    // 進度：擷取 0/6 … 6/6（只往前走），合成，檢查
    let grabbed: Vec<u32> = progress
        .iter()
        .filter_map(|p| match p.phase {
            Phase::Grabbing { done, total: 6 } => Some(done),
            _ => None,
        })
        .collect();
    assert_eq!(grabbed.first(), Some(&0));
    assert_eq!(grabbed.last(), Some(&6));
    assert!(grabbed.windows(2).all(|w| w[0] <= w[1]), "{grabbed:?}");
    let phases: Vec<Phase> = progress.iter().map(|p| p.phase).collect();
    let composing = phases.iter().position(|p| *p == Phase::Composing).expect("沒有合成");
    let checking = phases.iter().position(|p| *p == Phase::Checking).expect("沒有檢查");
    assert!(composing < checking);
    // 每一格都取到了：實際的時間在目標的半個間隔以內、依序
    let cells = seen.lock().unwrap().clone();
    assert_eq!(cells.len(), 6);
    let gap = sheet::spacing(range, 6);
    let mut last = -1.0;
    for &(i, t, at) in &cells {
        let at = at.unwrap_or_else(|| panic!("第 {i} 格取不到"));
        assert!((at - t).abs() <= gap / 2.0 + 0.001, "第 {i} 格：目標 {t}，實際 {at}");
        assert!(at > last, "時間要依序：{cells:?}");
        last = at;
    }
    // 圖：大小照版面；每一格都有畫面（不是黑的、不是背景）；標頭有字
    let img = read_sheet(&done.path);
    assert_eq!((img.w as u32, img.h as u32), layout.size);
    for i in 0..6 {
        let luma = mean_luma(&sheet_cell(&img, &layout, i));
        assert!(luma > 40.0, "第 {i} 格太暗：{luma}");
    }
    let band = crop(&img, 0, layout.margin, layout.size.0, layout.header_h - layout.margin);
    let lit = band.rgba.as_chunks::<4>().0.iter().filter(|p| p[0] > 160).count();
    assert!(lit > 300, "標頭沒有字：{lit}");
    assert!(
        mean_luma(&crop(
            &img,
            0,
            layout.size.1 - layout.margin,
            layout.size.0,
            layout.margin
        )) < 30.0,
        "背景是暗的"
    );

    // 同樣的格子不要時間標記、標頭（PNG）：只有右下角的時間標記不一樣，其他地方是同一格畫面
    let mut prefs = sheet_prefs(3, 2, 1280, ImageFormat::Png);
    prefs.timestamps = false;
    prefs.header = false;
    let mut plain = sheet_spec(&p, &caps, prefs, &dirs);
    assert!(plain.header.is_empty());
    plain.stem = "plain".into();
    let plain_layout = plain.layout.clone();
    let (_, plain_done) = export_sheet(plain);
    let plain_img = read_sheet(&plain_done.unwrap().path);
    assert_eq!(plain_layout.cell, layout.cell);
    for &(i, t, at) in &cells {
        let text = sheet::stamp_text(at.unwrap(), gap < sheet::TENTHS_BELOW);
        let (sx, sy, sw, sh) = stamp_region(&layout, i, &text);
        let (px, py) = plain_layout.cell_pos(i);
        let (cx, cy) = layout.cell_pos(i);
        let with = crop(&img, sx, sy, sw, sh);
        let without = crop(&plain_img, sx - cx + px, sy - cy + py, sw, sh);
        let stamp = mean_diff(&with.rgba, &without.rgba);
        assert!(stamp > 20.0, "第 {i} 格（{t} 秒）的時間標記看不出來：{stamp}");
        // 左上角：同一格畫面（JPEG 有一點差）
        let a = crop(&img, cx, cy, layout.cell.0 / 2, layout.cell.1 / 2);
        let b = crop(&plain_img, px, py, layout.cell.0 / 2, layout.cell.1 / 2);
        let same = mean_diff(&a.rgba, &b.rgba);
        assert!(same < 6.0, "第 {i} 格的畫面不一樣：{same}");
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn sheet_png_rotated_hdr() {
    let dirs = clip_dirs("sheet-rotate");
    // 手機直拍（檔案標示旋轉 90°）：每格是直的
    let (mut p, caps) = main_player(&sample_str("common/mov_hevc_aac_rot90.mov"));
    p.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
    let spec = sheet_spec(&p, &caps, sheet_prefs(4, 2, 1280, ImageFormat::Png), &dirs);
    assert_eq!(spec.fixup.rotate, 90);
    let l = spec.layout.clone();
    assert!(l.cell.1 > l.cell.0, "直的格子：{l:?}");
    assert_eq!(spec.render, (l.cell.1, l.cell.0), "擷取時還沒轉正");
    assert_eq!(spec.wanted().extension().unwrap(), "png");
    let (_, done) = export_sheet(spec);
    let img = read_sheet(&done.unwrap().path);
    assert_eq!((img.w as u32, img.h as u32), l.size);
    for i in 0..l.count() {
        assert!(mean_luma(&sheet_cell(&img, &l, i)) > 30.0, "第 {i} 格太暗");
    }

    // 使用者轉了 90°（跟 Ctrl+E 截圖一樣，是畫面上看到的方向）：順時針轉，翻轉在旋轉之後
    let (mut q, caps) = main_player(&sample_str("common/mp4_h264_aac.mp4"));
    q.wait_state(TIMEOUT, |s| s.video_size.is_some()).unwrap();
    let make = |geometry: Geometry, stem: &str| {
        let mut prefs = sheet_prefs(2, 1, 1280, ImageFormat::Png);
        prefs.timestamps = false;
        prefs.header = false;
        let mut spec = sheet_spec_with(&q, &caps, prefs, &geometry, None, &dirs).unwrap();
        spec.stem = stem.into();
        let l = spec.layout.clone();
        let (_, done) = export_sheet(spec);
        (l, read_sheet(&done.unwrap().path))
    };
    let (pl, plain) = make(Geometry::default(), "plain");
    let (rl, turned) = make(
        Geometry {
            rotate: 90,
            ..Default::default()
        },
        "turned",
    );
    let (fl, flipped) = make(
        Geometry {
            rotate: 90,
            hflip: true,
            ..Default::default()
        },
        "flipped",
    );
    assert!(pl.cell.0 > pl.cell.1 && rl.cell.1 > rl.cell.0, "{pl:?} {rl:?}");
    let small = |img: &vitascope::screenshot::Image| shrink(img, 24, 32);
    let base = sheet_cell(&plain, &pl, 0);
    let fix = |rotate: u32, hflip: bool| vitascope::screenshot::Fixup {
        rotate,
        hflip,
        vflip: false,
    };
    let want_cw = small(&base.clone().fixed(fix(90, false)));
    let want_ccw = small(&base.clone().fixed(fix(270, false)));
    let got = small(&sheet_cell(&turned, &rl, 0));
    let (cw, ccw) = (mean_diff(&want_cw, &got), mean_diff(&want_ccw, &got));
    assert!(cw < 10.0 && ccw > cw * 2.0, "順時針 {cw}、逆時針 {ccw}");
    let want_flip = small(&base.fixed(fix(90, true)));
    let d = mean_diff(&want_flip, &small(&sheet_cell(&flipped, &fl, 0)));
    assert!(d < 10.0, "先轉再翻：{d}");

    // HDR：轉成一般畫面（目標亮度 203 nits、Hable，跟轉成 GIF 一樣）
    let (mut h, caps) = main_player(&sample_str("general/mkv_hevc10_hdr10_mid.mkv"));
    h.wait_state(TIMEOUT, |s| s.video_hdr).unwrap();
    let mut prefs = sheet_prefs(2, 1, 1280, ImageFormat::Png);
    prefs.timestamps = false;
    let spec = sheet_spec_with(&h, &caps, prefs, &Geometry::default(), None, &dirs).unwrap();
    if !(caps.zscale && caps.tonemap) {
        assert_eq!(spec.tone, None);
        assert_eq!(spec.notes, [Note::HdrClipped]);
        eprintln!("略過 sheet_png_rotated_hdr 的色調映射：播放引擎沒有 zscale、tonemap");
    } else {
        assert_eq!(
            spec.tone,
            Some(gif::Tone {
                curve: "hable",
                npl: 203
            })
        );
        assert!(spec.vf().contains("tonemap=tonemap=hable"), "{}", spec.vf());
        let mut raw = spec.clone();
        raw.tone = None;
        raw.stem = "raw".into();
        let l = spec.layout.clone();
        let (_, mapped) = export_sheet(spec);
        let mapped = read_sheet(&mapped.unwrap().path);
        let (_, raw) = export_sheet(raw);
        let raw = read_sheet(&raw.unwrap().path);
        let cell = sheet_cell(&mapped, &l, 0);
        let luma = mean_luma(&cell);
        let changed = mean_diff(&cell.rgba, &sheet_cell(&raw, &l, 0).rgba);
        eprintln!("總覽圖 HDR → 一般畫面：平均亮度 {luma:.1}（跟沒轉的差 {changed:.1}）");
        assert!((20.0..=200.0).contains(&luma), "平均亮度 {luma}");
        assert!(changed > 8.0, "有轉跟沒轉差不多：{changed}");
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 另外開一個擷取用的 mpv（同樣的濾鏡、大小），從檔案開頭精確跳到每個時間取一格：
/// 跟總覽圖同一條路（顏色一樣），但每一格都是確實的那一格
fn grabber_frames(
    rel: &str,
    render: (u32, u32),
    vf: String,
    deinterlace: &'static str,
    times: Vec<f64>,
) -> Vec<(f64, vitascope::screenshot::Image)> {
    use vitascope::export::grab::{Grab, Grabber, Seek, Setup};
    let path = sample(rel);
    let out: Arc<Mutex<Vec<(f64, vitascope::screenshot::Image)>>> = Arc::default();
    let o = out.clone();
    let job = Job::spawn(Kind::Sheet, no_wake(), move |ctl| {
        let setup = Setup {
            source: Source::File(path),
            size: render,
            vf,
            vid: None,
            deinterlace,
        };
        let mut g = Grabber::open(ctl, &setup)?;
        for t in times {
            let offset = t + g.start_time() + 1.0;
            match g.grab(ctl, t, Seek::Exact { demuxer_offset: offset }, TIMEOUT)? {
                Grab::Frame(f) => o.lock().unwrap().push((f.time, f.image)),
                other => panic!("精確跳到 {t}：{other:?}"),
            }
        }
        g.finish()?;
        Err(Failure::Cancelled)
    });
    assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled));
    out.lock().map(|frames| frames.clone()).unwrap()
}

/// 畫面跟誰比
#[derive(Clone, Copy, PartialEq)]
enum Reference {
    /// 主播放器精確跳轉後的截圖
    Main,
    /// 另外開的擷取用的 mpv（[`grabber_frames`]）。SMPTE-C 原色的 DVD：繪圖時轉成 sRGB 的原色，
    /// 主播放器的截圖不轉，顏色本來就差一截，比不出是不是同一格
    Grabber,
}

/// 跳轉不準的格式（TS、M2TS、MPEG-PS）：每一格是時間標記的那一格（實際的時間在目標的半個間隔以內，
/// 不是後面的關鍵影格、檔尾），畫面跟精確跳到那個時間的一樣，跟晚半秒的不一樣
fn check_ts_sheet(rel: &str, columns: u32, rows: u32, reference: Reference) -> sheet::Layout {
    let name = format!("sheet-ts-{}", rel.replace(['/', '.'], "-"));
    let (mut p, caps) = ts_main_player(rel);
    let dirs = clip_dirs(&name);
    let mut prefs = sheet_prefs(columns, rows, 1280, ImageFormat::Png);
    prefs.timestamps = false;
    prefs.header = false;
    let mut spec = sheet_spec(&p, &caps, prefs, &dirs);
    assert!(spec.approx_seek, "{rel}：跳轉不準的格式");
    let l = spec.layout.clone();
    let range = spec.range;
    let (render, vf, deinterlace) = (spec.render, spec.vf(), spec.deinterlace);
    let seen = seen_cells(&mut spec);
    let (_, done) = export_sheet(spec);
    let img = read_sheet(&done.unwrap_or_else(|f| panic!("{rel}：{f:?}")).path);
    let gap = sheet::spacing(range, l.count());
    let cells = seen.lock().unwrap().clone();
    assert_eq!(cells.len(), l.count());
    for (i, t, at) in cells {
        let at = at.unwrap_or_else(|| panic!("{rel}：第 {i} 格取不到"));
        assert!(
            (at - t).abs() <= gap / 2.0 + 0.001,
            "{rel}：第 {i} 格的目標 {t}，實際 {at}（間隔 {gap}）"
        );
        // 跟精確跳到標記的時間的畫面比（縮小一點比，縮放的演算法不同）
        let late_t = if at + 0.5 < range.1 { at + 0.5 } else { at - 0.5 };
        let ((_, want), (late_at, late)) = match reference {
            Reference::Main => (main_frame(&mut p, at, &dirs.1), main_frame(&mut p, late_t, &dirs.1)),
            Reference::Grabber => {
                let mut f = grabber_frames(rel, render, vf.clone(), deinterlace, vec![at, late_t]).into_iter();
                let (want, late) = (f.next().unwrap(), f.next().unwrap());
                assert!((want.0 - at).abs() < 0.002, "{rel}：精確跳到 {at}，拿到 {}", want.0);
                (want, late)
            }
        };
        let (w, h) = (l.cell.0 as usize / 4, l.cell.1 as usize / 4);
        let cell = shrink(&sheet_cell(&img, &l, i), w, h);
        let same = mean_diff(&cell, &shrink(&want, w, h));
        let other = mean_diff(&cell, &shrink(&late, w, h));
        assert!(
            same < 8.0 && other > same * 2.0,
            "{rel}：第 {i} 格（{at} 秒）跟主播放器同一格差 {same}、跟 {late_at} 秒的差 {other}"
        );
    }
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
    l
}

#[test]
fn sheet_ts_cells_are_the_frames_at_their_timestamps() {
    // 每 10 秒一個關鍵影格的 TS（20 秒）：跳到關鍵影格常落在 0 秒、10 秒或檔尾，精確跳轉落在後面的關鍵影格，
    // 要讓分離器從更前面開始
    check_ts_sheet("general/ts_h264_gop10.ts", 5, 2, Reference::Main);
    // 只有開頭一個關鍵影格的 TS（5 秒，點很密）、每 0.5 秒一個關鍵影格的 TS
    check_ts_sheet("general/ts_h264_aac.ts", 3, 2, Reference::Main);
    check_ts_sheet("general/ts_h264_gop05.ts", 4, 2, Reference::Main);
    // M2TS（藍光）、MPEG-PS（DVD 的 VOB，720×480 存成、16:9 顯示）：格子照顯示的比例（不是被壓扁的 3:2）
    check_ts_sheet("general/m2ts_h264_ac3.m2ts", 3, 2, Reference::Main);
    let vob = check_ts_sheet("general/vob_mpeg2_ac3_anamorphic.vob", 3, 2, Reference::Grabber);
    let aspect = f64::from(vob.cell.0) / f64::from(vob.cell.1);
    assert!(
        (aspect - 16.0 / 9.0).abs() < 0.03,
        "VOB 的格子 {:?}（{aspect}）不是 16:9",
        vob.cell
    );
}

/// 跳轉不準的格式：跳到關鍵影格常落在檔尾（沒有新的影格，mpv 照樣送出「跳轉完成」，畫面還是上一格、
/// 時間是檔尾）。那不是一格：不能拿上一格的畫面配檔尾的時間（之後精確跳轉逾時時，總覽圖會用它）
#[test]
fn sheet_grab_at_the_end_of_file_is_not_the_previous_picture() {
    use vitascope::export::grab::{Grab, Grabber, Seek, Setup};
    for rel in ["general/ts_h264_aac.ts", "general/ts_h264_gop10.ts"] {
        let path = sample(rel);
        let job = Job::spawn(Kind::Sheet, no_wake(), move |ctl| {
            let setup = Setup {
                source: Source::File(path),
                size: (160, 90),
                vf: String::new(),
                vid: None,
                deinterlace: "no",
            };
            let mut g = Grabber::open(ctl, &setup)?;
            let d = g.duration().expect("知道總長度");
            let timeout = Duration::from_secs(20);
            let mut empty = 0;
            // 上一次取到的那一格（開檔後的第一次還沒有：開檔時那一格可能在跳轉之後才畫到）
            let mut prev: Option<vitascope::export::grab::Frame> = None;
            for k in 1..=4 {
                let t = d * f64::from(k) / 5.0;
                match g.grab(ctl, t, Seek::Keyframe, timeout)? {
                    // 真的停在一格上（TS 常落在目標後面的關鍵影格）：不是檔尾的時間，畫面跟上一格一樣時時間也一樣
                    Grab::Frame(f) => {
                        assert!(f.time < d - 0.001, "{rel}：跳到 {t}，拿到檔尾的時間 {}", f.time);
                        if let Some(p) = prev.as_ref().filter(|p| p.image == f.image) {
                            assert!(
                                (p.time - f.time).abs() < 0.05,
                                "{rel}：跳到 {t}，拿到上一格（{} 秒）的畫面配 {} 秒",
                                p.time,
                                f.time
                            );
                        }
                    }
                    Grab::Empty => empty += 1,
                    Grab::TimedOut => panic!("{rel}：跳到 {t} 逾時"),
                }
                // 從檔案開頭精確跳轉：取到目標那一格（下一次跳轉之前畫面上是它）
                match g.grab(
                    ctl,
                    t,
                    Seek::Exact {
                        demuxer_offset: t + 10.0,
                    },
                    timeout,
                )? {
                    Grab::Frame(f) => {
                        assert!((f.time - t).abs() < 0.1, "{rel}：精確跳到 {t}，拿到 {} 秒", f.time);
                        prev = Some(f);
                    }
                    other => panic!("{rel}：精確跳到 {t}：{other:?}"),
                }
            }
            g.finish()?;
            assert!(empty > 0, "{rel}：跳到關鍵影格都沒有落在檔尾（這個樣本測不到）");
            Err(Failure::Cancelled)
        });
        assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled), "{rel}");
    }
}

#[test]
fn sheet_ab_range() {
    // 只取 A-B 段落：每一格都在 A-B 裡
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("sheet-ab");
    let mut prefs = sheet_prefs(3, 1, 1280, ImageFormat::Jpeg);
    prefs.header = false;
    let mut spec = sheet_spec_with(&p, &caps, prefs, &Geometry::default(), Some((4.0, 8.0)), &dirs).unwrap();
    assert_eq!(spec.range, (4.0, 8.0));
    let seen = seen_cells(&mut spec);
    let (_, done) = export_sheet(spec);
    done.unwrap();
    let cells = seen.lock().unwrap().clone();
    let targets: Vec<f64> = cells.iter().map(|c| c.1).collect();
    assert_eq!(targets, [5.0, 6.0, 7.0]);
    for (i, t, at) in cells {
        let at = at.unwrap();
        // 每 2 秒一個關鍵影格：5 秒、7 秒跳到關鍵影格落在 4、6 秒（離目標 1 秒，超過半個間隔）：精確跳轉
        assert!((at - t).abs() <= 0.5, "第 {i} 格：目標 {t}，實際 {at}");
        assert!((4.0..8.0).contains(&at));
    }
    // 範圍太短、在影片外面：不能做
    let short = sheet_spec_with(&p, &caps, prefs, &Geometry::default(), Some((4.0, 4.1)), &dirs);
    assert_eq!(short.map(|s| s.range), Err(Failure::RangeTooShort));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

/// 在沒有時間標記的總覽圖的第 `i` 格右下角畫上 `text` 的時間標記（跟 `sheet` 一樣的畫法），回傳那個方塊
fn stamp_on(
    plain: &vitascope::screenshot::Image,
    l: &sheet::Layout,
    i: usize,
    text: &str,
    region: (u32, u32, u32, u32),
) -> vitascope::screenshot::Image {
    use vitascope::export::text::{Canvas, TextPainter};
    let mut canvas = Canvas::new(plain.w, plain.h, [0, 0, 0]);
    canvas.blit(0, 0, plain);
    let mut painter = TextPainter::new();
    let pad = sheet::STAMP_PAD;
    let (w, h) = painter.measure(text, l.stamp_px);
    let (bw, bh) = ((w + 2.0 * pad).ceil(), (h + 2.0 * pad).ceil());
    let (x, y) = l.cell_pos(i);
    let bx = (x + l.cell.0) as f32 - bw - pad;
    let by = (y + l.cell.1) as f32 - bh - pad;
    canvas.blend_rounded(bx as i64, by as i64, bw as i64, bh as i64, sheet::STAMP_BOX, pad);
    painter.draw(&mut canvas, bx + pad, by + pad, text, l.stamp_px, sheet::STAMP_COLOR);
    let rgba = canvas
        .rgb
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    let img = vitascope::screenshot::Image {
        w: canvas.w,
        h: canvas.h,
        rgba,
    };
    let (rx, ry, rw, rh) = region;
    crop(&img, rx, ry, rw, rh)
}

#[test]
fn sheet_stamps_show_the_actual_time_of_each_picture() {
    // 每 2 秒一個關鍵影格、12 秒取 3 張（目標 3、6、9 秒）：跳到關鍵影格落在 2、8 秒，在半個間隔（1.5 秒）以內就用它。
    // 時間標記要寫實際取到的那一格的時間（00:02），不是目標（00:03）：圖跟時間要對得上
    let (p, caps) = main_player(&sample_str("general/mkv_h264_gop2.mkv"));
    let dirs = clip_dirs("sheet-stamp-actual");
    let mut prefs = sheet_prefs(3, 1, 1280, ImageFormat::Png);
    prefs.header = false;
    let mut spec = sheet_spec(&p, &caps, prefs, &dirs);
    let l = spec.layout.clone();
    let tenths = sheet::spacing(spec.range, l.count()) < sheet::TENTHS_BELOW;
    let seen = seen_cells(&mut spec);
    let (_, done) = export_sheet(spec);
    let stamped = read_sheet(&done.unwrap_or_else(|f| panic!("{f:?}")).path);
    // 同樣的格子、沒有時間標記（PNG：畫面一模一樣）
    let mut plain_prefs = prefs;
    plain_prefs.timestamps = false;
    let mut plain = sheet_spec(&p, &caps, plain_prefs, &dirs);
    plain.stem = "plain".into();
    assert_eq!(plain.layout, l);
    let (_, plain_done) = export_sheet(plain);
    let plain = read_sheet(&plain_done.unwrap_or_else(|f| panic!("{f:?}")).path);
    let cells = seen.lock().unwrap().clone();
    assert_eq!(cells.len(), 3);
    let mut differs = 0;
    for (i, t, at) in cells {
        let at = at.unwrap_or_else(|| panic!("第 {i} 格取不到"));
        let (real, target) = (sheet::stamp_text(at, tenths), sheet::stamp_text(t, tenths));
        // 兩個字串的方塊取大的那個
        let (ra, rb) = (stamp_region(&l, i, &real), stamp_region(&l, i, &target));
        let region = if ra.2 >= rb.2 { ra } else { rb };
        let got = crop(&stamped, region.0, region.1, region.2, region.3);
        let want = stamp_on(&plain, &l, i, &real, region);
        let d_real = mean_diff(&got.rgba, &want.rgba);
        assert!(
            d_real < 1.0,
            "第 {i} 格（實際 {at} 秒）的時間標記不是「{real}」：差 {d_real}"
        );
        if real != target {
            differs += 1;
            let d_target = mean_diff(&got.rgba, &stamp_on(&plain, &l, i, &target, region).rgba);
            assert!(
                d_target > d_real + 3.0,
                "第 {i} 格：「{real}」差 {d_real}、目標的「{target}」差 {d_target}"
            );
        }
    }
    assert!(differs > 0, "每一格都剛好落在目標上（這個樣本測不到）");
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn sheet_failing_filter_is_an_error_not_a_file() {
    // mpv 停用失敗的濾鏡、照樣把畫面送出去：只有記錄裡的「Disabling filter」看得出來
    let (p, caps) = main_player(&sample_str("common/mp4_h264_aac.mp4"));
    let dirs = clip_dirs("sheet-fail");
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(2, 1, 1280, ImageFormat::Jpeg), &dirs);
    let bad = "crop=w=9999:h=9999";
    spec.test.extra_vf = Some(format!("lavfi=graph=%{}%{bad}", bad.len()));
    let (_, done) = export_sheet(spec);
    assert_eq!(done.map(|d| d.path), Err(Failure::FilterFailed));
    assert!(names(&dirs.0).is_empty(), "失敗時不留檔案：{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn grabber_remembers_a_failed_filter_past_later_log_lines() {
    use vitascope::export::grab::{Grab, Grabber, Seek, Setup};
    // mpv 停用失敗的濾鏡之後，記錄裡又來了一大堆別的錯誤（FFmpeg 的記錄全部送到同一個 mpv：
    // 匯出用的 mpv 可能收到別的 mpv 的警告）：濾鏡失敗不能被擠出最近的那幾行
    let path = sample("common/mp4_h264_aac.mp4");
    let dir = scratch("grab-flood");
    let missing = dir.join("沒有這個字幕.srt").to_string_lossy().into_owned();
    // 失敗的話只印出結果的種類（不印整張圖）
    type Got = (Result<String, Failure>, Result<String, Failure>, Result<(), Failure>);
    let kind = |r: Result<Grab, Failure>| {
        r.map(|g| match g {
            Grab::Frame(f) => format!("畫面 {:.3} 秒", f.time),
            other => format!("{other:?}"),
        })
    };
    let got: Arc<Mutex<Option<Got>>> = Arc::default();
    let out = got.clone();
    let job = Job::spawn(Kind::Sheet, no_wake(), move |ctl| {
        let bad = "crop=w=9999:h=9999";
        let setup = Setup {
            source: Source::File(path),
            size: (320, 180),
            vf: format!("lavfi=graph=%{}%{bad}", bad.len()),
            vid: None,
            deinterlace: "no",
        };
        let mut g = Grabber::open(ctl, &setup)?;
        let first = g.grab(ctl, 1.0, Seek::Keyframe, TIMEOUT);
        // 每一次都打不開、記下錯誤：比最近幾行多很多
        for _ in 0..LogTail::CAP * 2 {
            let _ = g.mpv().command(&["sub-add", &missing]);
        }
        let second = g.grab(ctl, 2.0, Seek::Keyframe, TIMEOUT);
        let finish = g.finish();
        *out.lock().unwrap() = Some((kind(first), kind(second), finish));
        Err(Failure::Cancelled)
    });
    assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled));
    let (first, second, finish) = got.lock().unwrap().take().expect("擷取沒有跑完");
    assert_eq!(first, Err(Failure::FilterFailed), "第一次跳轉就看得到");
    assert_eq!(second, Err(Failure::FilterFailed), "之後的錯誤把濾鏡失敗擠掉了");
    assert_eq!(finish, Err(Failure::FilterFailed));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn grabber_reads_a_queued_filter_failure_before_returning_a_frame() {
    use vitascope::export::grab::{Grab, Grabber, Setup};
    // 跳轉完成時「Disabling filter」還排在事件後面（mpv 先送事件、最後才送記錄）：
    // 要收完排著的記錄再決定，不能拿沒經過濾鏡的畫面當成取到的那一格
    let path = sample("common/mp4_h264_aac.mp4");
    let got: Arc<Mutex<Option<Result<String, Failure>>>> = Arc::default();
    let out = got.clone();
    let job = Job::spawn(Kind::Sheet, no_wake(), move |ctl| {
        let bad = "crop=w=9999:h=9999";
        let setup = Setup {
            source: Source::File(path),
            size: (320, 180),
            vf: format!("lavfi=graph=%{}%{bad}", bad.len()),
            vid: None,
            deinterlace: "no",
        };
        let mut g = Grabber::open(ctl, &setup)?;
        // 開檔之後解出的第一格到了輸出端（濾鏡已經失敗、記錄已經送出），但還沒有人收記錄
        let deadline = Instant::now() + TIMEOUT;
        while g.mpv().get_property::<i64>("video-out-params/w").is_err() {
            assert!(Instant::now() < deadline, "第一格一直沒有送到輸出端");
            std::thread::sleep(Duration::from_millis(20));
        }
        let r = g.settle(true).map(|g| match g {
            Grab::Frame(f) => format!("畫面 {:.3} 秒", f.time),
            other => format!("{other:?}"),
        });
        *out.lock().unwrap() = Some(r);
        let _ = g.finish();
        Err(Failure::Cancelled)
    });
    assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled));
    let r = got.lock().unwrap().take().expect("擷取沒有跑完");
    assert_eq!(r, Err(Failure::FilterFailed));
}

#[test]
fn sheet_cancel_and_read_timeout_leave_nothing() {
    let (p, caps) = main_player(&sample_str("common/mp4_long.mp4"));
    let dirs = clip_dirs("sheet-cancel");
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(4, 5, 1280, ImageFormat::Jpeg), &dirs);
    // 取完第一格之後停住，等測試取消
    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();
    spec.test.on_cell = Some(Arc::new(move |i, _, _| {
        if i == 0 {
            b.wait();
            b.wait();
        }
    }));
    let job = sheet::spawn(spec, no_wake());
    barrier.wait();
    job.cancel();
    barrier.wait();
    assert_eq!(wait_job(&job).map(|d| d.path), Err(Failure::Cancelled));
    drop(job);
    assert!(names(&dirs.0).is_empty(), "取消後不留檔案：{:?}", names(&dirs.0));

    // 每一格都等不到（網路磁碟卡住）：連續兩格取不到就放棄，不留檔案
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(4, 5, 1280, ImageFormat::Jpeg), &dirs);
    spec.test.cell_timeout = Some(Duration::ZERO);
    let seen = seen_cells(&mut spec);
    let (_, done) = export_sheet(spec);
    assert_eq!(done.map(|d| d.path), Err(Failure::ReadTimeout));
    assert_eq!(seen.lock().unwrap().len(), 2, "連續兩格取不到就停");
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn sheet_is_unavailable_without_video() {
    let dirs = clip_dirs("sheet-unavailable");
    // 只有聲音（專輯封面不算影像）
    let (p, _) = main_player(&sample_str("general/audio_mp3_cover.mp3"));
    assert_eq!(sheet::unavailable(&p), Some(Failure::NoVideo));
    let (q, _) = main_player(&sample_str("common/mp4_long.mp4"));
    assert_eq!(sheet::unavailable(&q), None);
    // 使用者開的 EDL：時間跟檔案對不上
    let src = sample_str("general/mkv_h264_gop2.mkv");
    let entry = format!("%{}%{src}", src.len());
    let edl = dirs.1.join("list.edl");
    std::fs::write(&edl, format!("# mpv EDL v0\n{entry},0,3\n{entry},6,3\n")).unwrap();
    let (r, _) = main_player(&edl.to_string_lossy());
    assert_eq!(sheet::unavailable(&r), Some(Failure::Timeline));
    // 影片檔不見了：開始時說明
    let tmp = dirs.1.join("gone.mp4");
    std::fs::copy(sample("common/mp4_h264_aac.mp4"), &tmp).unwrap();
    let (s, caps) = main_player(&tmp.to_string_lossy());
    let spec = sheet_spec(&s, &caps, sheet_prefs(2, 1, 1280, ImageFormat::Jpeg), &dirs);
    drop(s);
    std::fs::remove_file(&tmp).unwrap();
    let (_, done) = export_sheet(spec);
    assert_eq!(done.map(|d| d.path), Err(Failure::SourceMissing));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn sheet_checks_the_timeline_after_loading() {
    // 擷取用的 mpv 開好後再比一次總長度：靜態的檢查沒抓到的章節連結之類（主播放器的時間線比檔案長），
    // 目標、時間標記會對到別的地方：說明，不做
    let (p, caps) = main_player(&sample_str("common/mp4_long.mp4"));
    let dirs = clip_dirs("sheet-timeline");
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(2, 1, 1280, ImageFormat::Jpeg), &dirs);
    let real = spec.main_duration.expect("主播放器知道總長度");
    assert!((real - p.state.duration.unwrap()).abs() < 1e-9);
    spec.main_duration = Some(real + 30.0);
    let seen = seen_cells(&mut spec);
    let (_, done) = export_sheet(spec);
    assert_eq!(done.map(|d| d.path), Err(Failure::Timeline));
    assert!(seen.lock().unwrap().is_empty(), "一格都不取");
    assert!(names(&dirs.0).is_empty(), "{:?}", names(&dirs.0));
    // 差不多（1 秒以內）：照常做
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(2, 1, 1280, ImageFormat::Jpeg), &dirs);
    spec.main_duration = Some(real + 0.5);
    let (_, done) = export_sheet(spec);
    done.unwrap();
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn sheet_http_vod() {
    let server = Server::start();
    // 能跳轉的網路影片：用同樣的連線設定開檔、跳轉
    let url = server.file_url("general/mkv_h264_gop2.mkv");
    let net_settings = NetSettings {
        user_agent: "VitaScope-Sheet/1.0".into(),
        ..NetSettings::default()
    };
    let (mut p, caps) = main_player_net(&net_settings, &url);
    p.wait_state(TIMEOUT, |s| s.seekable).unwrap();
    let dirs = clip_dirs("sheet-http");
    let mut spec = sheet_spec(&p, &caps, sheet_prefs(3, 1, 1280, ImageFormat::Jpeg), &dirs);
    assert!(matches!(&spec.source, Source::Net(s) if s.open == url));
    assert_eq!(spec.header[0], "mkv_h264_gop2.mkv");
    let seen = seen_cells(&mut spec);
    let before = server.requests_to("/f/general/mkv_h264_gop2.mkv").len();
    let (_, done) = export_sheet(spec);
    let done = done.unwrap_or_else(|f| panic!("{f:?}"));
    let reqs = server.requests_to("/f/general/mkv_h264_gop2.mkv");
    assert!(reqs.len() > before, "擷取用的 mpv 要自己讀");
    for r in &reqs[before..] {
        assert_eq!(r.header("User-Agent"), Some("VitaScope-Sheet/1.0"), "{r:#?}");
    }
    assert!(seen.lock().unwrap().iter().all(|c| c.2.is_some()));
    assert!(done.path.exists());
    // 直播：不能做
    let mut q = Player::new(Options {
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    })
    .unwrap();
    q.open(&server.url("/hlslive/net/hls_vod/index.m3u8")).unwrap();
    q.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 路徑可能比 FileLoaded 晚到（沒有路徑時是 NoData，不是直播）
    q.wait_state(TIMEOUT, |s| s.loaded && s.path.is_some() && !s.tracks.is_empty())
        .unwrap();
    assert_eq!(sheet::unavailable(&q), Some(Failure::Unbounded));
    for d in [&dirs.0, &dirs.1] {
        std::fs::remove_dir_all(d).unwrap();
    }
}
