//! 播放引擎的建置內容：影戲用到的解碼器、分離器、協定、濾鏡、截圖編碼器都要在。
//! （mpv 的 decoder-list 只有影音解碼器；字幕格式由格式矩陣 tests/formats.rs 測）
//!
//! L3 功能（DASH、片段輸出、轉 GIF、等化器與音量正規化、HDR 轉 SDR）需要重新建置的播放引擎
//! （components.json 列有 libxml2 的那一版）。舊的播放引擎、系統的 libmpv 沒有這些元件，相關測試會略過。
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vitascope::mpv::{Event, Mpv};
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};

const TIMEOUT: Duration = Duration::from_secs(30);

fn engine() -> Player {
    Player::new(Options::headless()).expect("建立 mpv 失敗")
}
fn list(p: &Player, prop: &str, key: &str) -> Vec<String> {
    // mpv 把節點清單印成 JSON
    let v: serde_json::Value = serde_json::from_str(&p.get_string(prop).unwrap()).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e[key].as_str().map(str::to_owned))
        .collect()
}
fn has_protocol(p: &Player, name: &str) -> bool {
    p.get_string("protocol-list").unwrap().split(',').any(|x| x == name)
}
/// 本專案建置的 libmpv 的 components.json（系統的 libmpv 沒有）
fn manifest() -> Option<serde_json::Value> {
    let path = option_env!("VITASCOPE_LIBMPV_MANIFEST")?;
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("讀不到 {path}：{e}"));
    Some(serde_json::from_str(&text).unwrap())
}
/// 含 L3 元件的播放引擎：components.json 列有 libxml2（DASH 分離器用）。
/// 沒有的話印出原因、回傳 false，呼叫的測試直接結束（略過）
fn l3_engine(test: &str) -> bool {
    let has = manifest().is_some_and(|m| {
        m["components"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["name"] == "libxml2"))
    });
    if !has {
        eprintln!("略過 {test}：這個播放引擎還不是含 L3 元件（libxml2）的版本");
    }
    has
}
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
    let d = std::env::temp_dir().join(format!("vitascope-engine-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
/// 用 headless 播放器打開，等到軌道（`duration`：還有長度）都讀到
fn reopen(path: &Path, duration: bool) -> Player {
    let mut p = engine();
    p.open(path.to_str().unwrap()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {}：{e}", path.display()));
    p.wait_state(TIMEOUT, |s| (!duration || s.duration.is_some()) && !s.tracks.is_empty())
        .unwrap_or_else(|e| panic!("{} 讀不到長度或軌道：{e}", path.display()));
    p
}
fn codec(p: &Player, kind: TrackKind) -> Option<String> {
    p.state.tracks_of(kind).next().and_then(|t| t.codec.clone())
}

#[test]
fn decoders_demuxers_protocols_filters() {
    let p = engine();
    let codecs = list(&p, "decoder-list", "codec");
    for c in [
        "h264",
        "hevc",
        "vp8",
        "vp9",
        "av1",
        "mpeg1video",
        "mpeg2video",
        "mpeg4",
        "vc1",
        "wmv3",
        "prores",
        "mjpeg",
        "png",
        "wrapped_avframe",
        "aac",
        "ac3",
        "eac3",
        "dts",
        "truehd",
        "mp3",
        "mp2",
        "flac",
        "opus",
        "vorbis",
        "alac",
        "wmav2",
        "pcm_s16le",
    ] {
        assert!(codecs.iter().any(|x| x == c), "缺少 {c} 解碼器");
    }
    assert!(
        list(&p, "decoder-list", "driver").iter().any(|d| d == "libdav1d"),
        "缺少 libdav1d"
    );
    let demux = p.get_string("demuxer-lavf-list").unwrap();
    for d in ["hls", "mpegts", "rtsp", "flv", "avi", "asf", "ogg"] {
        assert!(demux.contains(d), "缺少 {d} 分離器");
    }
    for x in ["file", "http", "https", "tls", "tcp", "udp", "rtmp"] {
        assert!(has_protocol(&p, x), "缺少 {x} 協定");
    }
    for f in ["rotate", "hflip", "vflip", "bwdif", "crop", "scale"] {
        p.mpv()
            .command(&["vf", "add", &format!("@check:{f}")])
            .unwrap_or_else(|e| panic!("缺少 {f} 濾鏡：{e}"));
        p.mpv().command(&["vf", "remove", "@check"]).unwrap();
    }
    assert!(
        list(&p, "encoder-list", "driver").iter().any(|e| e == "png"),
        "缺少 png 編碼器（截圖）"
    );
}
/// 本專案建置的 libmpv（Windows、macOS、AppImage 的那一份）：版本與建置選項要跟它的 components.json 一致
#[test]
fn vendored_libmpv_matches_its_manifest() {
    let Some(m) = manifest() else {
        eprintln!("略過：不是本專案建置的 libmpv");
        return;
    };
    let ver = |n: &str| {
        m["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .unwrap()["version"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let p = engine();
    assert_eq!(p.get_string("mpv-version").unwrap(), format!("mpv v{}", ver("mpv")));
    assert_eq!(p.get_string("ffmpeg-version").unwrap(), ver("ffmpeg"));
    assert!(p.get_string("mpv-configuration").unwrap().contains("gpl=false"));
    // 加密的 HLS（AES-128）；Ubuntu 的 FFmpeg 沒有，所以只要求本專案建置的
    assert!(has_protocol(&p, "crypto"), "缺少 crypto 協定");
    // 含 L3 元件的版本另外檢查（l3_components）；之前的版本只有截圖用的 png
    if !m["components"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "libxml2")
    {
        assert_eq!(list(&p, "encoder-list", "driver"), ["png"]);
    }
    assert!(!list(&p, "decoder-list", "driver").iter().any(|d| d.contains("zvbi")));
}

// ───────────── L3：重新建置的播放引擎才有的元件 ─────────────

/// 編碼器、DASH 分離器、協定、音訊與影像濾鏡都要在（只在含 L3 元件的播放引擎上檢查）
#[test]
fn l3_components() {
    if !l3_engine("l3_components") {
        return;
    }
    let p = engine();
    // 截圖的 png、轉 GIF 的 gif、即時編碼輸出的 ac3（mpv 列出的順序不固定，排序再比）
    let mut enc = list(&p, "encoder-list", "driver");
    enc.sort();
    assert_eq!(enc, ["ac3", "gif", "png"]);
    // 要逐項比對：子字串比對時 webm_dash_manifest 也會讓「dash」通過
    let demux = p.get_string("demuxer-lavf-list").unwrap();
    let demux: Vec<&str> = demux.split(',').collect();
    assert!(demux.contains(&"dash"), "缺少 dash 分離器");
    assert!(!demux.contains(&"imf"), "不該有 imf 分離器");
    for x in ["ftp", "mmsh", "mmst", "rtmpts", "srtp"] {
        assert!(has_protocol(&p, x), "缺少 {x} 協定");
    }
    // mpv 解析選項時就會檢查濾鏡名稱（不用開檔）
    for f in [
        "equalizer",
        "bass",
        "treble",
        "acompressor",
        "alimiter",
        "dynaudnorm",
        "speechnorm",
        "loudnorm",
        "pan",
    ] {
        p.mpv()
            .command(&["af", "add", &format!("@check:{f}")])
            .unwrap_or_else(|e| panic!("缺少 {f} 音訊濾鏡：{e}"));
        p.mpv().command(&["af", "remove", "@check"]).unwrap();
    }
    // paletteuse 有兩個輸入，不能單獨當成 mpv 的濾鏡：由 gif_export_encode_mode 測
    for f in ["fps", "split", "palettegen", "transpose", "zscale", "tonemap"] {
        p.mpv()
            .command(&["vf", "add", &format!("@check:{f}")])
            .unwrap_or_else(|e| panic!("缺少 {f} 濾鏡：{e}"));
        p.mpv().command(&["vf", "remove", "@check"]).unwrap();
    }
}

/// GIF 的影格數（逐一走過區塊，數影像描述區塊）
fn gif_frames(data: &[u8]) -> usize {
    assert!(data.len() > 13, "GIF 太短");
    let mut i = 13;
    if data[10] & 0x80 != 0 {
        i += 3 << ((data[10] & 7) + 1); // 全域色盤
    }
    let skip_sub_blocks = |mut i: usize| {
        while i < data.len() && data[i] != 0 {
            i += data[i] as usize + 1;
        }
        i + 1
    };
    let mut frames = 0;
    while i < data.len() {
        match data[i] {
            0x21 => i = skip_sub_blocks(i + 2), // 擴充區塊
            0x2c => {
                frames += 1;
                let flags = data[i + 9];
                i += 10;
                if flags & 0x80 != 0 {
                    i += 3 << ((flags & 7) + 1); // 區域色盤
                }
                i = skip_sub_blocks(i + 1); // LZW 最小碼長之後是影像資料
            }
            0x3b => break,
            b => panic!("GIF 格式不對：位置 {i} 是 0x{b:02x}"),
        }
    }
    frames
}

/// 轉 GIF：編碼模式（o=），濾鏡 fps、scale、split、palettegen、paletteuse，gif 編碼器與封裝格式
#[test]
fn gif_export_encode_mode() {
    if !l3_engine("gif_export_encode_mode") {
        return;
    }
    let dir = scratch("gif");
    let out = dir.join("out.gif");
    let g = "fps=10,scale=160:-2,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse";
    let vf = format!("lavfi=graph=%{}%{g}", g.len());
    // idle=once：播完唯一的檔案就結束，GIF 在 mpv 結束時才寫完（idle=no 會在 loadfile 之前就結束）
    let mpv = Mpv::new(&[
        ("o", out.to_str().unwrap()),
        ("aid", "no"),
        ("sid", "no"),
        ("idle", "once"),
        ("video-rotate", "no"),
        ("vf", &vf),
    ])
    .unwrap();
    mpv.command(&["loadfile", "av://lavfi:testsrc2=size=320x180:rate=30:duration=2"])
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "轉 GIF 逾時");
        if let Some(Event::Shutdown) = mpv.wait_event(left.as_secs_f64()) {
            break;
        }
    }
    drop(mpv);
    let data = std::fs::read(&out).unwrap_or_else(|e| panic!("沒有寫出 GIF：{e}"));
    assert!(data.starts_with(b"GIF89a"), "不是 GIF89a");
    let frames = gif_frames(&data);
    assert!(frames > 1, "GIF 只有 {frames} 格");
    // GIF 不一定讀得到長度（影格數已經數過了）
    let p = reopen(&out, false);
    assert_eq!(codec(&p, TrackKind::Video).as_deref(), Some("gif"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 片段輸出（不重新編碼）：另開一個只讀檔、不播放的 mpv，整個檔案讀進快取後 dump-cache
#[test]
fn clip_export_dump_cache() {
    if !l3_engine("clip_export_dump_cache") {
        return;
    }
    /// 來源樣本、輸出的副檔名、預期的影像（None = 只輸出聲音）與聲音編碼
    struct Clip {
        rel: &'static str,
        exts: &'static [&'static str],
        vcodec: Option<&'static str>,
        acodec: &'static str,
    }
    let dir = scratch("clip");
    let cases = [
        Clip {
            rel: "common/mp4_h264_aac.mp4",
            exts: &["mkv", "mp4", "ts"],
            vcodec: Some("h264"),
            acodec: "aac",
        },
        Clip {
            rel: "common/webm_vp9_opus.webm",
            exts: &["webm"],
            vcodec: Some("vp9"),
            acodec: "opus",
        },
        Clip {
            rel: "general/audio_flac.flac",
            exts: &["mka", "flac"],
            vcodec: None,
            acodec: "flac",
        },
    ];
    for Clip {
        rel,
        exts,
        vcodec,
        acodec,
    } in cases
    {
        let video = vcodec.is_some();
        let src = sample(rel);
        // mpv 自己的 MKV 分離器不給 DTS，輸出用 FFmpeg 的分離器
        let mpv = Mpv::new(&[
            ("vo", "null"),
            ("ao", "null"),
            ("cache", "yes"),
            ("demuxer", "lavf"),
            ("demuxer-lavf-probe-info", "yes"),
            ("pause", "yes"),
            ("sid", "no"),
            ("vid", if video { "auto" } else { "no" }),
            ("idle", "yes"),
        ])
        .unwrap();
        mpv.command(&["loadfile", src.to_str().unwrap()]).unwrap();
        // 等整個檔案進了快取
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "{rel}：快取讀不到檔尾");
            while mpv.wait_event(0.0).is_some() {}
            let eof = mpv
                .get_string("demuxer-cache-state")
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .is_some_and(|v| v["eof"] == true);
            if eof {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for ext in exts {
            let out = dir.join(format!(
                "{}.{ext}",
                Path::new(rel).file_stem().unwrap().to_str().unwrap()
            ));
            // dump-cache 寫完才回覆；用非同步指令，才有逾時
            mpv.command_async(1, &["dump-cache", "0.5", "2.5", out.to_str().unwrap()])
                .unwrap();
            let deadline = Instant::now() + TIMEOUT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "{rel} → {ext}：dump-cache 逾時");
                if let Some(Event::CommandReply { id: 1, result }) = mpv.wait_event(left.as_secs_f64()) {
                    result.unwrap_or_else(|e| panic!("{rel} → {ext}：dump-cache 失敗：{e}"));
                    break;
                }
            }
            // 沒寫進任何封包時 dump-cache 也回報成功：重新打開確認內容
            let p = reopen(&out, true);
            let d = p.state.duration.unwrap();
            assert!(d > 1.0, "{rel} → {ext}：長度只有 {d} 秒");
            assert_eq!(codec(&p, TrackKind::Video).as_deref(), vcodec, "{rel} → {ext}：影像");
            assert_eq!(
                codec(&p, TrackKind::Audio).as_deref(),
                Some(acodec),
                "{rel} → {ext}：聲音"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 本機的 DASH（MPD + .m4s 片段）：FFmpeg 的 dash 分離器（libxml2）；
/// 再從兩個執行緒反覆打開，確認同時解析 MPD 不會出事（dashdec 的 xmlCleanupParser 修正）
#[test]
fn dash_local_manifest() {
    if !l3_engine("dash_local_manifest") {
        return;
    }
    let mpd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/general/dash_h264_aac/manifest.mpd");
    if !mpd.exists() {
        // gen_samples.py 在沒有 dash 封裝格式的 ffmpeg 上會略過這個樣本；CI 的 ffmpeg 都有
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "找不到 {}，請先執行：python scripts/gen_samples.py",
            mpd.display()
        );
        eprintln!("略過 dash_local_manifest：沒有 DASH 樣本（本機的 ffmpeg 沒有 dash 封裝格式？）");
        return;
    }
    let p = reopen(&mpd, true);
    assert_eq!(p.get_string("file-format").unwrap(), "dash");
    let d = p.state.duration.unwrap();
    assert!((d - 3.0).abs() < 0.5, "長度 {d}");
    assert_eq!(codec(&p, TrackKind::Video).as_deref(), Some("h264"));
    assert_eq!(codec(&p, TrackKind::Audio).as_deref(), Some("aac"));
    drop(p);
    let path = mpd.to_str().unwrap().to_owned();
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut p = engine();
                for _ in 0..10 {
                    p.open(&path).unwrap();
                    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
                        .unwrap_or_else(|e| panic!("DASH 打不開：{e}"));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("同時打開 DASH 的執行緒失敗");
    }
}

/// 等化器、夜間模式的濾鏡鏈：播放中用 af-command 即時調整（loudnorm、pan 不支援即時指令，要用 af set）
#[test]
fn audio_filters_live() {
    if !l3_engine("audio_filters_live") {
        return;
    }
    let mut p = engine();
    p.mpv()
        .set_property(
            "af",
            "@eq:lavfi=[equalizer@b1=f=1000:t=o:w=1:g=6],@night:lavfi=[acompressor=threshold=0.125:ratio=4,alimiter=level=0]",
        )
        .unwrap();
    p.open(sample("common/mp4_h264_aac.mp4").to_str().unwrap()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    p.mpv()
        .command(&["af-command", "eq", "g", "3", "equalizer@b1"])
        .unwrap_or_else(|e| panic!("af-command 失敗：{e}"));
    p.mpv()
        .command(&["af-command", "night", "ratio", "2", "acompressor"])
        .unwrap_or_else(|e| panic!("af-command 失敗：{e}"));
    // 濾鏡失敗時 mpv 會停用它並記一筆錯誤（Disabling filter …），播放仍會繼續
    let _ = p.wait_state(Duration::from_millis(500), |_| false);
    assert!(
        !p.recent_errors().iter().any(|e| e.contains("Disabling filter")),
        "有濾鏡被停用：{:?}",
        p.recent_errors()
    );
    assert!(p.state.loaded, "播放中斷：{:?}", p.recent_errors());
}
