//! 匯出（片段、GIF、縮圖總覽圖）：背景工作的取消與暫存檔、兩個工作同名不衝突、寫好之後重新打開檢查、
//! mpv 的記錄對應到原因、片段（不重新編碼：關鍵影格對齊、各種格式、只有聲音、旋轉、磁碟快取、
//! 網路影片）（headless：不出畫面、不出聲音，三個平台的 CI 都跑；網路只連本機的測試伺服器 127.0.0.1）。
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
use vitascope::export::{
    self, ClipFormat, Done, Expect, Failure, Job, JobEvent, Kind, LogLine, LogTail, Note, Phase, Progress,
    map_mpv_error, verify_media,
};
use vitascope::instance::Wake;
use vitascope::mpv::{Event, Mpv};
use vitascope::net::{self, NetSettings};
use vitascope::player::{EngineCaps, Options, Player, PlayerEvent, Track, TrackKind};
use vitascope::save;
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
    // 等 mpv 選好軌道（慢的機器上軌道清單比選好的軌道早到）：預設的片段軌道是主播放器選的
    p.wait_state(TIMEOUT, |s| {
        s.duration.is_some() && (s.selected(TrackKind::Video).is_some() || s.selected(TrackKind::Audio).is_some())
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
    // 等 mpv 選好軌道（慢的機器上軌道清單比選好的軌道早到）：預設的片段軌道是主播放器選的
    p.wait_state(TIMEOUT, |s| {
        s.duration.is_some() && (s.selected(TrackKind::Video).is_some() || s.selected(TrackKind::Audio).is_some())
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
    q.wait_state(TIMEOUT, |s| !s.tracks.is_empty()).unwrap();
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
