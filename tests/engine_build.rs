//! 播放引擎的建置內容：影戲用到的解碼器、分離器、協定、濾鏡、截圖編碼器都要在。
//! （mpv 的 decoder-list 只有影音解碼器；字幕格式由格式矩陣 tests/formats.rs 測）
use vitascope::player::{Options, Player};
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
    let protos = p.get_string("protocol-list").unwrap();
    for x in ["file", "http", "https", "tls", "tcp", "crypto", "udp", "rtmp"] {
        assert!(protos.split(',').any(|y| y == x), "缺少 {x} 協定");
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
#[cfg(windows)]
#[test]
fn windows_libmpv_matches_its_manifest() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/libmpv/windows-x64/components.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("略過：不是本專案建置的 libmpv");
        return;
    };
    let m: serde_json::Value = serde_json::from_str(&text).unwrap();
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
    assert_eq!(list(&p, "encoder-list", "driver"), ["png"]);
    assert!(!list(&p, "decoder-list", "driver").iter().any(|d| d.contains("zvbi")));
}
