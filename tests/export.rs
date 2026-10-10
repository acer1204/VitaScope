//! 匯出（片段、GIF、縮圖總覽圖）：背景工作的取消與暫存檔、兩個工作同名不衝突、寫好之後重新打開檢查、
//! mpv 的記錄對應到原因（headless：不出畫面、不出聲音，三個平台的 CI 都跑）。
//!
//! 只看 mpv 自己的訊息：FFmpeg 的記錄只送到第一個建立的 mpv，這裡不檢查 FFmpeg 的文字

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use vitascope::export::{
    self, Done, Expect, Failure, Job, JobEvent, Kind, LogTail, Phase, Progress, map_mpv_error, verify_media,
};
use vitascope::instance::Wake;
use vitascope::mpv::{Event, Mpv};
use vitascope::player::{Track, TrackKind};
use vitascope::save;

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
