//! 罕見的文字字幕格式與歌詞（headless）：
//! - MPL2、SubViewer、LRC、JACOsub、RealText、PJS、AQTitle、VPlayer、SCC（CEA-608）
//!   都走影戲自己的外掛字幕流程（轉成 UTF-8 的暫存檔再交給 mpv），跳到 2 秒時畫面上的字幕要對。
//!   AQTitle、PJS、VPlayer 的暫存檔不能有 BOM（FFmpeg 的格式偵測從第一個位元組開始比對）
//! - 純音訊檔旁邊的 LRC 歌詞：播放器把目前的那一句放在 `state.sub_text`（介面畫在歌名下面），
//!   mpv 畫在封面上的那份藏起來；影片不觀察 sub-text。封面上的 ASS 歌詞（樣式、卡拉 OK 效果）照舊由 mpv 畫
//!
//! 字幕檔都由測試自己寫到暫存資料夾（不需要產生的樣本）；歌詞用 `general/audio_flac.flac`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};

const TIMEOUT: Duration = Duration::from_secs(15);
/// 每個字幕檔用同樣的三句：2 秒時顯示第二句（純 ASCII，SCC 也寫得出來）
const CUES: [(f64, f64, &str); 3] = [
    (0.2, 1.4, "First line"),
    (1.5, 2.6, "VITASCOPE TEST"),
    (2.7, 2.95, "Third"),
];
const PROBE_TIME: f64 = 2.0;
const PROBE_TEXT: &str = "VITASCOPE TEST";

/// 每次執行、每個測試各自的暫存資料夾（路徑不同，轉碼的暫存檔名也不同：
/// 不會用到別次執行留下的暫存檔）
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "vitascope-subs-formats-{}-{}-{nanos}-{name}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

// ───────────── 字幕檔（跟 scripts/gen_samples.py 的寫法一樣） ─────────────

/// 時、分、秒、千分之一秒
fn hmsm(t: f64) -> (u64, u64, u64, u64) {
    let ms = (t * 1000.0).round() as u64;
    (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// `HH:MM:SS.cc`（百分之一秒）
fn centis(t: f64) -> String {
    let (h, m, s, ms) = hmsm(t);
    format!("{h:02}:{m:02}:{s:02}.{:02}", ms / 10)
}

fn mpl2() -> String {
    CUES.iter()
        .map(|(a, b, t)| format!("[{}][{}]{t}\n", (a * 10.0).round(), (b * 10.0).round()))
        .collect()
}

fn subviewer() -> String {
    let mut s = "[INFORMATION]\n[TITLE]test\n[AUTHOR]\n[SOURCE]\n[PRG]\n[FILEPATH]\n[DELAY]0\n[CD TRACK]0\n\
                 [COMMENT]\n[END INFORMATION]\n[SUBTITLE]\n[COLF]&HFFFFFF,[STYLE]bd,[SIZE]18,[FONT]Arial\n"
        .to_owned();
    for (a, b, t) in CUES {
        s += &format!("{},{}\n{t}\n\n", centis(a), centis(b));
    }
    s
}

fn lrc_time(t: f64) -> String {
    let cs = (t * 100.0).round() as u64;
    format!("[{:02}:{:02}.{:02}]", cs / 6000, cs / 100 % 60, cs % 100)
}

fn lrc() -> String {
    let mut s = "[ar:test]\n[ti:test]\n".to_owned();
    for (a, b, t) in CUES {
        s += &format!("{}{t}\n{}\n", lrc_time(a), lrc_time(b));
    }
    s
}

fn jacosub() -> String {
    let jts = |t: f64| {
        let (h, m, s, ms) = hmsm(t);
        format!("{h}:{m:02}:{s:02}.{:02}", ms / 10)
    };
    let mut s = "#TITLE test\n#TIMERES 100\n".to_owned();
    for (a, b, t) in CUES {
        s += &format!("{} {} D {t}\n", jts(a), jts(b));
    }
    s
}

fn vplayer() -> String {
    CUES.iter()
        .map(|(a, b, t)| format!("{}:{t}\n{}:\n", centis(*a), centis(*b)))
        .collect()
}

fn realtext() -> String {
    let mut s = "<window type=\"generic\" duration=\"00:00:03.00\" >\n".to_owned();
    for (a, b, t) in CUES {
        s += &format!("<time begin=\"{}\" end=\"{}\"/><clear/>{t}\n", centis(a), centis(b));
    }
    s + "</window>\n"
}

fn pjs() -> String {
    // 時間的單位是 0.1 秒
    CUES.iter()
        .map(|(a, b, t)| format!("{},{},\"{t}\"\n", (a * 10.0).round(), (b * 10.0).round()))
        .collect()
}

fn aqtitle() -> String {
    // 影格編號（25 fps）
    CUES.iter()
        .map(|(a, b, t)| format!("-->> {}\n{t}\n\n-->> {}\n\n", (a * 25.0).round(), (b * 25.0).round()))
        .collect()
}

/// SCC（CEA-608 pop-on 字幕，30 fps 的時間碼）。時間碼一定要遞增，不然時間會亂掉
fn scc() -> String {
    // 奇同位
    let parity = |b: u8| if b.count_ones().is_multiple_of(2) { b | 0x80 } else { b };
    let pairs = |text: &str| {
        let mut bytes: Vec<u8> = text.bytes().map(parity).collect();
        if bytes.len() % 2 == 1 {
            bytes.push(0x80);
        }
        bytes
            .chunks(2)
            .map(|p| format!("{:02x}{:02x}", p[0], p[1]))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let tc = |t: f64| {
        let f = (t * 30.0).round() as u64;
        let (s, fr) = (f / 30, f % 30);
        format!("{:02}:{:02}:{:02}:{fr:02}", s / 3600, s / 60 % 60, s % 60)
    };
    let mut lines = vec!["Scenarist_SCC V1.0".to_owned(), String::new()];
    for (a, b, t) in CUES {
        lines.push(format!(
            "{}\t9420 9420 94ae 94ae 9470 9470 {} 942f 942f",
            tc(a),
            pairs(t)
        ));
        lines.push(String::new());
        lines.push(format!("{}\t942c 942c", tc(b)));
        lines.push(String::new());
    }
    lines.join("\n")
}

/// 一種格式：說明、檔名、內容、mpv 回報的字幕編碼
struct Format {
    what: &'static str,
    name: &'static str,
    content: String,
    codec: &'static str,
    /// mpv 0.37 以前（Linux tar.gz 用的系統 libmpv）只顯示得出第一句：FFmpeg 的 SubViewer 分離器把每一句的檔案位置
    /// 都寫成 0，舊版 mpv 只靠檔案位置判斷「這一句看過了」，後面的句子都被當成重複的丟掉（新版還會比時間）
    first_cue_only_on_old_mpv: bool,
}

fn rare_formats() -> Vec<Format> {
    let f = |what, name, content, codec| Format {
        what,
        name,
        content,
        codec,
        first_cue_only_on_old_mpv: false,
    };
    vec![
        f("MPL2", "mpl2.mpl", mpl2(), "mpl2"),
        f("MPL2（.txt）", "mpl2.txt", mpl2(), "mpl2"),
        Format {
            first_cue_only_on_old_mpv: true,
            ..f("SubViewer 2", "subviewer.sub", subviewer(), "subviewer")
        },
        f("LRC", "lyrics.lrc", lrc(), "text"),
        f("JACOsub", "jacosub.jss", jacosub(), "jacosub"),
        f("RealText", "realtext.rt", realtext(), "realtext"),
        f("PJS", "pjs.pjs", pjs(), "pjs"),
        f("AQTitle", "aqtitle.aqt", aqtitle(), "text"),
        f("VPlayer", "vplayer.txt", vplayer(), "vplayer"),
        f("SCC", "captions.scc", scc(), "eia_608"),
    ]
}

/// mpv 的版本（主, 次）；看不懂的（自己建置的、git 版）當成新版
fn mpv_version(p: &Player) -> (u32, u32) {
    let text = p.get_string("mpv-version").unwrap_or_default();
    let v = text.trim_start_matches("mpv ").trim_start_matches('v');
    let mut parts = v.split(['.', '-']).map(|n| n.parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(a)), Some(Ok(b))) => (a, b),
        _ => (u32::MAX, 0),
    }
}

/// 暫停在第一格的 headless 播放器，開好 3 秒的測試畫面
fn paused_on_test_pattern() -> Player {
    let mut p = Player::new(Options::headless()).unwrap();
    p.set_pause(true).unwrap();
    p.open("av://lavfi:testsrc2=size=320x240:rate=30:duration=3").unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p
}

/// 跳到 `time`（暫停中），等到 mpv 的 sub-text 有字（或逾時）
fn sub_text_at(p: &mut Player, time: f64) -> String {
    p.seek_to(time, true).unwrap();
    let _ = p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart);
    let start = Instant::now();
    loop {
        let text = p.get_string("sub-text").unwrap_or_default();
        if !text.trim().is_empty() || start.elapsed() > TIMEOUT {
            return text.trim().to_owned();
        }
        let _ = p.wait(Duration::from_millis(50));
    }
}

/// 載入一個字幕檔（影戲自己的流程：讀檔、判斷編碼、寫成暫存檔再交給 mpv），檢查編碼名稱與 2 秒時的字幕
fn check_format(dir: &Path, format: &Format) -> Result<(), String> {
    let path = dir.join(format.name);
    std::fs::write(&path, &format.content).unwrap();
    let codec = format.codec;
    let mut p = paused_on_test_pattern();
    p.add_subtitle(&path.to_string_lossy())
        .map_err(|e| format!("載入失敗：{e}"))?;
    let track = p
        .get_string("track-list")
        .ok()
        .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
        .unwrap_or_default()
        .into_iter()
        .find(|t| t["type"] == "sub")
        .ok_or("沒有字幕軌")?;
    let got_codec = track["codec"].as_str().unwrap_or_default().to_owned();
    if got_codec != codec {
        return Err(format!("字幕編碼是 {got_codec:?}，預期 {codec:?}"));
    }
    let (time, want) = if format.first_cue_only_on_old_mpv && mpv_version(&p) < (0, 38) {
        eprintln!(
            "  {}：mpv 0.37 以前只看第一句（{:?}）",
            format.what,
            p.get_string("mpv-version")
        );
        (0.5, CUES[0].2)
    } else {
        (PROBE_TIME, PROBE_TEXT)
    };
    let text = sub_text_at(&mut p, time);
    if text != want {
        return Err(format!("{time} 秒的字幕是 {text:?}，預期 {want:?}"));
    }
    Ok(())
}

#[test]
fn rare_text_subtitles_show_the_cue() {
    let dir = TempDir::new("rare");
    let mut failed = Vec::new();
    for format in rare_formats() {
        let what = format.what;
        match check_format(&dir.0, &format) {
            Ok(()) => eprintln!("✔ {what}"),
            Err(e) => {
                eprintln!("✘ {what}：{e}");
                failed.push(format!("{what}：{e}"));
            }
        }
    }
    assert!(failed.is_empty(), "這些格式不行：\n{}", failed.join("\n"));
}

// ───────────── 歌詞 ─────────────

const LYRICS: &str = "[ti:測試歌曲]\n[00:00.20]第一句歌詞\n[00:01.50]影戲播放器測試\n[00:02.70]最後一句\n";

/// 暫存資料夾裡放一份 `general/audio_flac.flac`（取名「歌.flac」）和同名的 LRC 歌詞
fn song_with_lyrics(dir: &TempDir) -> PathBuf {
    let song = dir.0.join("歌.flac");
    std::fs::copy(sample("general/audio_flac.flac"), &song).unwrap();
    std::fs::write(dir.0.join("歌.lrc"), LYRICS).unwrap();
    song
}

#[test]
fn lrc_lyrics_on_audio_only() {
    let dir = TempDir::new("lyrics");
    let song = song_with_lyrics(&dir);
    let mut p = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    p.set_pause(true).unwrap();
    p.open(&song.to_string_lossy()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 同名的 .lrc 自動載入、選上
    let lyrics = p
        .state
        .tracks_of(TrackKind::Sub)
        .find(|t| t.external)
        .cloned()
        .expect("自動載入同名的歌詞");
    assert_eq!(lyrics.codec.as_deref(), Some("text"), "{lyrics:?}");
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Sub).is_some())
        .expect("選上歌詞");
    // mpv 畫在封面上的那份藏起來（介面自己畫）；只對這個檔案
    assert_eq!(p.get_string("sub-visibility").unwrap(), "no");

    // 跳到第二句、再跳回第一句（暫停中：純音訊檔的字幕照樣跟著位置更新）：狀態裡是目前那一句（介面畫在歌名下面）。
    // 不邊播邊等：mpv 的屬性通知不排隊，測試的執行緒慢一點就錯過只有 1 秒多的那一句
    for (time, want) in [(2.0, "影戲播放器測試"), (0.5, "第一句歌詞")] {
        p.seek_to(time, true).unwrap();
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
        p.wait_state(TIMEOUT, |s| s.sub_text.as_deref() == Some(want))
            .unwrap_or_else(|e| panic!("{e}：{time} 秒的歌詞 {:?}，預期 {want:?}", p.state.sub_text));
    }

    // 換成有字幕的影片：不再觀察 sub-text，mpv 自己畫字幕
    p.open(&sample("common/mkv_h264_aac_srt.mkv").to_string_lossy())
        .unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.state.sub_text, None, "換檔時清掉");
    assert_eq!(p.get_string("sub-visibility").unwrap(), "yes", "影片照常顯示字幕");
}

/// 一句從頭到尾的 ASS 字幕（卡拉 OK 歌詞常用 ASS：有樣式、位置、特效）
const ASS_LYRICS: &str = r"[Script Info]
ScriptType: v4.00+
PlayResX: 384
PlayResY: 288

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,8,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,{\k50}卡拉{\k50}歌詞
";

#[test]
fn ass_lyrics_over_a_cover_stay_with_mpv() {
    // 有封面的音樂檔：純文字的歌詞由介面畫在底色上（mpv 那份藏起來）；
    // 換成 ASS（樣式、位置、卡拉 OK 效果）就交還給 mpv 畫在封面上，換回來又藏起來
    let dir = TempDir::new("cover-ass");
    let song = dir.0.join("歌.mp3");
    std::fs::copy(sample("general/audio_mp3_cover.mp3"), &song).unwrap();
    std::fs::write(dir.0.join("歌.lrc"), LYRICS).unwrap();
    let ass = dir.0.join("卡拉.ass");
    std::fs::write(&ass, ASS_LYRICS).unwrap();
    let mut p = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    p.set_pause(true).unwrap();
    p.open(&song.to_string_lossy()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert!(p.state.tracks_of(TrackKind::Video).any(|t| t.albumart), "有專輯封面");
    let visibility = |p: &mut Player, want: &str| {
        let start = Instant::now();
        while p.get_string("sub-visibility").unwrap() != want {
            assert!(start.elapsed() < TIMEOUT, "sub-visibility 沒有變成 {want}");
            let _ = p.wait(Duration::from_millis(50));
        }
    };
    p.wait_state(TIMEOUT, |s| {
        s.selected(TrackKind::Sub)
            .is_some_and(|t| t.codec.as_deref() == Some("text"))
    })
    .expect("選上 LRC");
    visibility(&mut p, "no");
    assert!(p.state.lyrics_in_app());

    // 拖進 ASS：mpv 照常畫在封面上，介面不畫
    p.add_subtitle(&ass.to_string_lossy()).unwrap();
    p.wait_state(TIMEOUT, |s| {
        s.selected(TrackKind::Sub)
            .is_some_and(|t| t.codec.as_deref() == Some("ass"))
    })
    .expect("選上 ASS");
    visibility(&mut p, "yes");
    assert!(!p.state.lyrics_in_app());

    // 選回 LRC：又由介面畫
    let lrc = p
        .state
        .tracks_of(TrackKind::Sub)
        .find(|t| t.codec.as_deref() == Some("text"))
        .map(|t| t.id);
    p.select_track(TrackKind::Sub, lrc).unwrap();
    p.wait_state(TIMEOUT, |s| {
        s.selected(TrackKind::Sub)
            .is_some_and(|t| t.codec.as_deref() == Some("text"))
    })
    .expect("選回 LRC");
    visibility(&mut p, "no");
}

#[test]
fn video_subtitles_are_not_watched() {
    // 影片的字幕由 mpv 畫：不觀察 sub-text（每一格都要多算一次），狀態裡一直沒有
    let mut p = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    p.set_pause(true).unwrap();
    p.open(&sample("common/mkv_h264_aac_srt.mkv").to_string_lossy())
        .unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p.wait_state(TIMEOUT, |s| s.selected(TrackKind::Sub).is_some())
        .expect("選上內嵌字幕");
    assert_eq!(p.get_string("sub-visibility").unwrap(), "yes");
    // 畫面上真的有字幕（mpv 讀得到），狀態裡還是沒有
    assert_eq!(sub_text_at(&mut p, 2.0), "影戲播放器測試");
    p.set_pause(false).unwrap();
    assert!(
        p.wait_state(Duration::from_secs(2), |s| s.sub_text.is_some()).is_err(),
        "影片不觀察 sub-text：{:?}",
        p.state.sub_text
    );
}
