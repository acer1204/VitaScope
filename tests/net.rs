//! 網路功能（headless：不出畫面、不出聲音，三個平台的 CI 都跑；只連本機的測試伺服器 127.0.0.1）
//!
//! - mpv 包裝層的基礎：hook、wakeup、用 node 設定屬性。網站影片（yt-dlp）要在 mpv 開檔前（`on_load` hook）
//!   換掉要開的網址、設定這個檔案專用的標頭，載入後再設定章節。
//! - 播放網址：HTTP 的檔案（能跳轉）、HLS、DASH（多畫質），網路設定（User-Agent、標頭、憑證、逾時…）真的送到 mpv、送到伺服器。
//!
//! 開網址失敗的說明要看 FFmpeg 的記錄，在 tests/net_errors.rs（那些測試一次只能有一個播放器）。

mod support;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::fake_ytdl::{
    FakeResolver, SITE_COOKIE, SITE_REFERER, SITE_TITLE_TEXT, SITE_UA, site_playlist_json, site_video_json,
};
use support::http::Server;
use vitascope::mpv::{EndReason, Event, Mpv, Node};
use vitascope::net::{self, HlsBitrate, NetSettings};
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};
use vitascope::ytdl::plan::{Choice, Mode};
use vitascope::ytdl::{Failure, Hint, Resolve, SitePrefs, YtdlError};

const TIMEOUT: Duration = Duration::from_secs(15);
const LAVFI: &str = "av://lavfi:testsrc2=size=160x90:rate=10:duration=10";

/// 不出畫面、不出聲音、閒置時不結束的 mpv（直接用包裝層，不經過 Player）
fn raw_mpv() -> Mpv {
    Mpv::new(&[("vo", "null"), ("ao", "null"), ("idle", "yes"), ("ytdl", "no")]).expect("建立 mpv 失敗")
}

/// 等到符合條件的事件，回傳它；逾時就失敗
fn wait_event(mpv: &Mpv, what: &str, mut pred: impl FnMut(&Event) -> bool) -> Event {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "等不到 {what}");
        if let Some(ev) = mpv.wait_event(left.as_secs_f64().min(0.5))
            && pred(&ev)
        {
            return ev;
        }
    }
}

/// 在 `within` 之內收到的事件（不管是什麼）
fn events_for(mpv: &Mpv, within: Duration) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut out = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return out;
        }
        if let Some(ev) = mpv.wait_event(left.as_secs_f64()) {
            out.push(ev);
        }
    }
}

fn hook_id(ev: &Event) -> u64 {
    match ev {
        Event::Hook { id, .. } => *id,
        other => panic!("不是 hook：{other:?}"),
    }
}

fn sample(rel: &str) -> String {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

fn headless() -> Player {
    Player::new(Options::headless()).expect("建立 mpv 失敗")
}

/// 有這個屬性（舊的 libmpv 可能沒有）
fn has_property(p: &Player, name: &str) -> bool {
    p.get_string("property-list").unwrap().split(',').any(|x| x == name)
}

/// 字串清單的項目裡有逗號：用字串設定會被拆開，用 node 設定剛好兩項
#[test]
fn node_string_list_keeps_commas() {
    let p = headless();
    let mpv = p.mpv();
    let headers = ["X-Test: 1,2", "Cookie: a=b, c=d"];
    mpv.set_node("http-header-fields", &Node::strings(headers)).unwrap();
    assert_eq!(mpv.get_string_list("http-header-fields").unwrap(), headers);
    // 對照：當成逗號分隔的字串設定，就變成四項（這就是要用 node 的原因）
    mpv.set_property("http-header-fields", headers.join(",")).unwrap();
    assert_eq!(mpv.get_string_list("http-header-fields").unwrap().len(), 4);
    // 空的清單
    mpv.set_node("http-header-fields", &Node::Array(Vec::new())).unwrap();
    assert!(mpv.get_string_list("http-header-fields").unwrap().is_empty());
    // 單一字串的 node 也是照選項的字串格式解析
    mpv.set_node("user-agent", &Node::Str("VitaScope/test, 1".into()))
        .unwrap();
    assert_eq!(mpv.get_string("user-agent").unwrap(), "VitaScope/test, 1");
    // 型別不對：mpv 拒絕，回傳錯誤（不會當掉）
    assert!(mpv.set_node("volume", &Node::Array(vec![Node::Int(1)])).is_err());
}

/// 一整棵樹（巢狀的陣列、鍵值清單、各種型別、空的清單、中文）經過 mpv 複製再讀回來，內容一樣
#[test]
fn node_tree_round_trips_through_mpv() {
    let p = headless();
    if !has_property(&p, "user-data") {
        eprintln!("略過 node_tree_round_trips_through_mpv：這個 libmpv 沒有 user-data（0.36 起才有）");
        return;
    }
    let tree = Node::Map(vec![
        ("title".into(), Node::Str("影戲, \"引號\"".into())),
        ("count".into(), Node::Int(-3)),
        ("ratio".into(), Node::Double(0.5)),
        ("on".into(), Node::Flag(true)),
        ("off".into(), Node::Flag(false)),
        ("empty".into(), Node::Array(Vec::new())),
        ("none".into(), Node::Map(Vec::new())),
        (
            "list".into(),
            Node::Array(vec![
                Node::Str("a".into()),
                Node::Map(vec![("深".into(), Node::Array(vec![Node::Int(1), Node::Int(2)]))]),
            ]),
        ),
    ]);
    p.mpv().set_node("user-data/vitascope-test/tree", &tree).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&p.get_string("user-data/vitascope-test/tree").unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "title": "影戲, \"引號\"",
            "count": -3,
            "ratio": 0.5,
            "on": true,
            "off": false,
            "empty": [],
            "none": {},
            "list": ["a", {"深": [1, 2]}],
        })
    );
}

/// 播放中設定章節（網站影片的章節在載入後才設定）。mpv 只收 0 ≤ 時間 < 長度 的章節，
/// 所以用知道長度的 3 秒樣本（av://lavfi 的長度只是已經讀到的部分，後面的章節會被丟掉），先暫停才不會播完
#[test]
fn chapter_list_can_be_set_while_playing() {
    let mut p = Player::new(Options {
        extra: vec![("pause".into(), "yes".into())],
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p.wait_state(TIMEOUT, |s| s.duration.is_some_and(|d| d > 2.8)).unwrap();
    let chapter =
        |title: &str, time: Node| Node::Map(vec![("title".into(), Node::Str(title.into())), ("time".into(), time)]);
    p.mpv()
        .set_node(
            "chapter-list",
            &Node::Array(vec![
                chapter("開場", Node::Double(0.0)),
                chapter("第二段, 有逗號", Node::Int(1)),
                chapter("結尾", Node::Double(2.5)),
                // 超過長度：mpv 丟掉
                chapter("太晚", Node::Double(99.0)),
            ]),
        )
        .unwrap();
    p.wait_state(TIMEOUT, |s| !s.chapters.is_empty()).unwrap();
    let got: Vec<(Option<&str>, f64)> = p.state.chapters.iter().map(|c| (c.title.as_deref(), c.time)).collect();
    assert_eq!(
        got,
        [(Some("開場"), 0.0), (Some("第二段, 有逗號"), 1.0), (Some("結尾"), 2.5)]
    );
    // 章節真的生效：跳到第 3 章
    p.mpv().set_property("chapter", 2i64).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 2.5).abs() < 0.3).unwrap();
}

/// 在 av://lavfi 上註冊的 on_load hook：收到事件時 mpv 停著等，continue 之後才載入
#[test]
fn hook_on_lavfi_is_received_and_continued() {
    let mpv = raw_mpv();
    mpv.hook_add(42, "on_load", 0).unwrap();
    for round in 0..2 {
        mpv.command(&["loadfile", LAVFI]).unwrap();
        let ev = wait_event(&mpv, "on_load", |e| {
            // 第二次會先收到上一個檔案的 EndFile（被換掉），那不算
            assert!(
                !matches!(e, Event::FileLoaded),
                "第 {round} 次：hook 之前就載入了：{e:?}"
            );
            matches!(e, Event::Hook { .. })
        });
        let Event::Hook { ref name, userdata, .. } = ev else {
            unreachable!()
        };
        assert_eq!((name.as_str(), userdata), ("on_load", 42));
        assert_eq!(mpv.hooks_pending(), 1);
        // hook 等待中：mpv 不會自己載入；指令、屬性照常可以用
        let early = events_for(&mpv, Duration::from_millis(300));
        assert!(
            !early.iter().any(|e| matches!(e, Event::FileLoaded)),
            "第 {round} 次：沒有 continue 就載入了：{early:?}"
        );
        assert_eq!(mpv.get_string("path").unwrap(), LAVFI);
        let id = hook_id(&ev);
        mpv.hook_continue(id).unwrap();
        assert_eq!(mpv.hooks_pending(), 0);
        // 同一個 hook 再 continue 一次是未定義行為：包裝層擋下來，不交給 mpv
        // （有沒有真的沒交給 mpv，見 bad_hook_continue_never_reaches_mpv）
        assert!(mpv.hook_continue(id).is_err());
        assert!(mpv.hook_continue(id + 1000).is_err());
        wait_event(&mpv, "FileLoaded", |e| matches!(e, Event::FileLoaded));
    }
}

/// 錯誤的 continue（重複的、沒收到過的序號）在包裝層就擋下來，根本不交給 mpv。
/// mpv 自己也會回傳錯誤，所以只看回傳值分不出來；但 mpv 收到時會記一筆錯誤
/// 「invalid hook API usage」，包裝層擋下來就不會有這筆紀錄
#[test]
fn bad_hook_continue_never_reaches_mpv() {
    const MPV_COMPLAINT: &str = "invalid hook API usage";
    let mpv = raw_mpv();
    mpv.request_log_messages("error").unwrap();
    let complaints = |evs: &[Event]| -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Log { text, .. } if text.contains(MPV_COMPLAINT) => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    // 對照組：mpv 的錯誤紀錄確實會從 wait_event 收到（不然下面「沒有紀錄」的檢查一定通過）
    mpv.command(&["loadfile", "av://lavfi:no_such_filter_vitascope"])
        .unwrap();
    wait_event(
        &mpv,
        "error log",
        |e| matches!(e, Event::Log { level, .. } if level == "error"),
    );
    wait_event(&mpv, "EndFile", |e| matches!(e, Event::EndFile { .. }));

    mpv.hook_add(7, "on_load", 0).unwrap();
    mpv.command(&["loadfile", LAVFI]).unwrap();
    let ev = wait_event(&mpv, "on_load", |e| matches!(e, Event::Hook { .. }));
    let id = hook_id(&ev);
    // 從來沒收到過的序號
    assert!(mpv.hook_continue(id + 1000).is_err());
    assert_eq!(mpv.hooks_pending(), 1, "錯的序號不能把等待中的 hook 消掉");
    mpv.hook_continue(id).unwrap();
    // 已經 continue 過的序號
    assert!(mpv.hook_continue(id).is_err());
    let mut seen = Vec::new();
    wait_event(&mpv, "FileLoaded", |e| {
        seen.push(e.clone());
        matches!(e, Event::FileLoaded)
    });
    // mpv 的紀錄是非同步送來的，載入後再多等一下
    seen.extend(events_for(&mpv, Duration::from_millis(300)));
    let got = complaints(&seen);
    assert!(got.is_empty(), "錯誤的 continue 交給了 mpv：{got:?}");
}

/// 開檔失敗時的 on_load_fail；同一個 hook 名稱，priority 小的先執行
#[test]
fn hook_priority_and_on_load_fail() {
    let mpv = raw_mpv();
    mpv.hook_add(1, "on_load", 10).unwrap();
    mpv.hook_add(2, "on_load", -5).unwrap();
    mpv.hook_add(3, "on_load_fail", 0).unwrap();
    mpv.command(&["loadfile", "av://lavfi:no_such_filter_vitascope"])
        .unwrap();
    let mut order = Vec::new();
    loop {
        let ev = wait_event(&mpv, "hook", |e| {
            assert!(!matches!(e, Event::EndFile { .. }), "還沒處理 hook 就結束了：{e:?}");
            matches!(e, Event::Hook { .. })
        });
        let Event::Hook { ref name, userdata, .. } = ev else {
            unreachable!()
        };
        order.push((name.clone(), userdata));
        mpv.hook_continue(hook_id(&ev)).unwrap();
        if name == "on_load_fail" {
            break;
        }
    }
    assert_eq!(
        order,
        [
            ("on_load".to_owned(), 2),
            ("on_load".to_owned(), 1),
            ("on_load_fail".to_owned(), 3)
        ]
    );
    wait_event(&mpv, "EndFile", |e| matches!(e, Event::EndFile { error: Some(_), .. }));
    assert_eq!(mpv.hooks_pending(), 0);
}

/// 別的執行緒呼叫 wakeup：正在等的 wait_event 馬上回傳 None，也會呼叫 wakeup callback
#[test]
fn wakeup_interrupts_wait_event() {
    let mut mpv = raw_mpv();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    mpv.set_wakeup_callback(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let mpv = Arc::new(mpv);
    // 先把建立時的事件取完
    while mpv.wait_event(0.0).is_some() {}
    let before = calls.load(Ordering::SeqCst);
    let weak = Arc::downgrade(&mpv);
    let waker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(mpv) = weak.upgrade() {
            mpv.wakeup();
        }
    });
    let start = Instant::now();
    // 中間有別的事件也沒關係；沒被叫醒的話最後一次會等滿 10 秒
    while mpv.wait_event(10.0).is_some() {}
    let waited = start.elapsed();
    waker.join().unwrap();
    assert!(
        waited < Duration::from_secs(5),
        "wakeup 沒有叫醒 wait_event（等了 {waited:?}）"
    );
    assert!(calls.load(Ordering::SeqCst) > before, "wakeup 沒有呼叫 callback");
}

/// hook 還沒 continue 就釋放 mpv：mpv 自己放行，不會卡住
#[test]
fn dropping_mpv_with_a_pending_hook_does_not_hang() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mpv = raw_mpv();
        mpv.hook_add(1, "on_load", 0).unwrap();
        mpv.command(&["loadfile", LAVFI]).unwrap();
        wait_event(&mpv, "on_load", |e| matches!(e, Event::Hook { .. }));
        assert_eq!(mpv.hooks_pending(), 1);
        drop(mpv);
        let _ = tx.send(());
    });
    rx.recv_timeout(TIMEOUT).expect("hook 等待中釋放 mpv 卡住了");
}

/// 播放器收到沒人處理的 hook（還沒有網站影片的功能）：立刻放行，照常載入
#[test]
fn player_continues_hooks_nobody_handles() {
    let mut p = headless();
    p.mpv().hook_add(9, "on_load", 0).unwrap();
    for _ in 0..2 {
        p.open(LAVFI).unwrap();
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
        assert_eq!(p.mpv().hooks_pending(), 0);
    }
}

// ───────────── 播放網址、網路設定 ─────────────

/// headless 播放器，網路設定照 `s` 同步套用（跟介面啟動時一樣）；`extra` 是額外的 mpv 選項（算使用者自己指定的）
fn net_player(s: &NetSettings, extra: &[(&str, &str)]) -> Player {
    let mut p = Player::new(Options {
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    let opts = net::mpv_options(s, &p.net_defaults());
    for (name, _, r) in p.apply_net(&opts, true) {
        r.unwrap_or_else(|e| panic!("無法設定 {name}：{e}"));
    }
    p
}

/// 播放中改網路設定（非同步，跟介面一樣），等 mpv 全部回覆、都成功。
/// 一定要等：之後同步的 loadfile 可能插隊到還沒執行的非同步設定前面
fn apply_async(p: &mut Player, s: &NetSettings) {
    let opts = net::mpv_options(s, &p.net_defaults());
    let mut left: HashSet<u64> = p
        .apply_net(&opts, false)
        .into_iter()
        .map(|(name, _, r)| {
            r.unwrap_or_else(|e| panic!("無法送出 {name}：{e}"))
                .expect("播放中一定是非同步")
        })
        .collect();
    let deadline = Instant::now() + TIMEOUT;
    while !left.is_empty() {
        match p.wait(deadline.saturating_duration_since(Instant::now())) {
            Some(PlayerEvent::CommandReply { id, error }) if left.remove(&id) => {
                assert!(error.is_none(), "mpv 不接受網路設定：{error:?}");
            }
            Some(_) => {}
            None => panic!("等不到非同步設定的回覆（還有 {} 個）", left.len()),
        }
    }
}

fn loaded(p: &mut Player, url: &str) {
    p.open(url).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {url}：{e}"));
}

/// 選到的影片軌的寬度（從介面看到的軌道清單）
fn video_width(p: &Player) -> Option<i64> {
    p.state.selected(TrackKind::Video).and_then(|t| t.width)
}

/// mpv 的版本（主, 次）；看不懂的（自己建置的、git 版）當成新版
fn mpv_version(p: &Player) -> (u32, u32) {
    let text = p.get_string("mpv-version").unwrap();
    let v = text.trim_start_matches("mpv ").trim_start_matches('v');
    let mut parts = v.split(['.', '-']).map(|n| n.parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(a)), Some(Ok(b))) => (a, b),
        _ => (u32::MAX, 0),
    }
}

/// 含 L3 元件的播放引擎（本專案建置、components.json 列有 libxml2）。系統的 libmpv（FFmpeg 6.1）的 DASH 分離器
/// 被中斷時會卡死（ROADMAP §7），DASH 的測試只在本專案的引擎跑；不是的話印出原因、回傳 false
fn l3_engine(test: &str) -> bool {
    let has = option_env!("VITASCOPE_LIBMPV_MANIFEST")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|m| {
            m["components"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c["name"] == "libxml2"))
        });
    if !has {
        eprintln!("略過 {test}：這個播放引擎還不是含 L3 元件（libxml2）的版本");
    }
    has
}

/// 網路測試的樣本（`python scripts/gen_samples.py` 產生；這台的 FFmpeg 做不出來時略過）
fn net_sample(test: &str, rel: &str) -> bool {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    if !p.exists() {
        eprintln!("略過 {test}：沒有樣本 {}（python scripts/gen_samples.py）", p.display());
    }
    p.exists()
}

#[test]
fn http_mp4_plays_and_seeks() {
    let server = Server::start();
    let mut p = net_player(&NetSettings::default(), &[("pause", "yes")]);
    let url = server.file_url("common/mp4_h264_aac.mp4");
    loaded(&mut p, &url);
    // 伺服器支援 Range，mpv 判斷能跳轉；網路串流用快取（cache-buffering-state 有值）
    p.wait_state(TIMEOUT, |s| {
        s.duration.is_some_and(|d| d > 2.8) && s.seekable && s.cache_buffering.is_some()
    })
    .unwrap();
    assert_eq!(p.state.path.as_deref(), Some(url.as_str()));
    assert!(!p.state.paused_for_cache);
    let reqs = server.requests_to("/f/common/mp4_h264_aac.mp4");
    assert!(!reqs.is_empty());
    assert!(
        reqs.iter()
            .all(|r| r.header("Range").is_some_and(|v| v.starts_with("bytes="))),
        "每個請求都用 Range：{reqs:#?}"
    );
    // 跳到中間（暫停中，停在那一格）
    p.seek_to(1.5, true).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 1.5).abs() < 0.05).unwrap();
    p.set_pause(false).unwrap();
    p.wait_state(TIMEOUT, |s| s.time_pos > 1.7).unwrap();
}

#[test]
fn global_headers_reach_the_server() {
    let server = Server::start();
    let s = NetSettings {
        user_agent: "VitaScope-Test/1.0 (a, b)".into(),
        referrer: "http://ref.example/page?x=1,2".into(),
        headers: vec!["X-Test: a, b".into(), "X-Other:1".into()],
        ..NetSettings::default()
    };
    let mut p = net_player(&s, &[("pause", "yes")]);
    loaded(&mut p, &server.file_url("common/mp4_h264_aac.mp4"));
    let reqs = server.requests_to("/f/common/mp4_h264_aac.mp4");
    for r in &reqs {
        assert_eq!(r.header("User-Agent"), Some("VitaScope-Test/1.0 (a, b)"), "{r:#?}");
        assert_eq!(r.header("Referer"), Some("http://ref.example/page?x=1,2"), "{r:#?}");
        // 值裡的逗號不會把標頭拆開
        assert_eq!(r.header("X-Test"), Some("a, b"), "{r:#?}");
        assert_eq!(r.header("X-Other"), Some("1"), "{r:#?}");
    }
    // 播放中改設定（非同步）：下一次連線用新的值。User-Agent 清空 = 播放引擎預設；Referer 清空就不送
    let s = NetSettings {
        headers: vec!["X-Test: c, d".into()],
        ..NetSettings::default()
    };
    apply_async(&mut p, &s);
    assert_eq!(p.mpv().get_string_list("http-header-fields").unwrap(), ["X-Test: c, d"]);
    // 只看新檔案的請求：前一個檔案已經開著的連線（快取還在讀）照樣用開檔時的設定
    loaded(&mut p, &server.file_url("common/mkv_h264_aac_srt.mkv"));
    let reqs = server.requests_to("/f/common/mkv_h264_aac_srt.mkv");
    assert!(!reqs.is_empty());
    let engine_ua = p.net_defaults().user_agent;
    for r in &reqs {
        assert_eq!(r.header("User-Agent"), Some(engine_ua.as_str()), "{r:#?}");
        assert_eq!(r.header("Referer"), None, "{r:#?}");
        assert_eq!(r.header("X-Test"), Some("c, d"), "{r:#?}");
        assert_eq!(r.header("X-Other"), None, "{r:#?}");
    }
}

/// 啟動時同步套用、之後非同步改的網路設定，mpv 讀回來都是設定的值（兩種引擎；系統的 libmpv 0.37 預設不檢查憑證）
#[test]
fn net_options_reach_mpv() {
    let mut p = net_player(&NetSettings::default(), &[]);
    let mib = |n: i64| n * 1024 * 1024;
    assert_eq!(p.get_string("tls-verify").unwrap(), "yes");
    assert_eq!(p.get_f64("network-timeout").unwrap(), 30.0);
    assert_eq!(p.get_i64("demuxer-max-bytes").unwrap(), mib(150));
    assert_eq!(p.get_i64("demuxer-max-back-bytes").unwrap(), mib(50));
    assert_eq!(p.get_string("hls-bitrate").unwrap(), "max");
    assert_eq!(p.get_string("user-agent").unwrap(), p.net_defaults().user_agent);
    assert_eq!(p.get_string("http-proxy").unwrap(), "");
    let lavf = p.get_string("stream-lavf-o").unwrap();
    for kv in ["reconnect=1", "reconnect_streamed=1", "reconnect_delay_max=5"] {
        assert!(lavf.contains(kv), "{lavf}");
    }
    assert!(!lavf.contains("reconnect_on_network_error"), "{lavf}");
    assert!(p.mpv().get_string_list("http-header-fields").unwrap().is_empty());
    // 改設定（非同步）
    let s = NetSettings {
        hls_bitrate: HlsBitrate::Min,
        reconnect: false,
        cache_mb: 64,
        timeout_secs: 7,
        tls_verify: false,
        user_agent: "UA/2".into(),
        proxy: "http://127.0.0.1:3128".into(),
        headers: vec!["X-A: 1".into(), "X-B: 2, 3".into()],
        ..NetSettings::default()
    };
    apply_async(&mut p, &s);
    assert_eq!(p.get_string("tls-verify").unwrap(), "no");
    assert_eq!(p.get_f64("network-timeout").unwrap(), 7.0);
    assert_eq!(p.get_i64("demuxer-max-bytes").unwrap(), mib(64));
    assert_eq!(p.get_i64("demuxer-max-back-bytes").unwrap(), mib(21));
    assert_eq!(p.get_string("hls-bitrate").unwrap(), "min");
    assert_eq!(p.get_string("user-agent").unwrap(), "UA/2");
    assert_eq!(p.get_string("http-proxy").unwrap(), "http://127.0.0.1:3128");
    assert_eq!(p.get_string("stream-lavf-o").unwrap(), "reconnect=0");
    assert_eq!(
        p.mpv().get_string_list("http-header-fields").unwrap(),
        ["X-A: 1", "X-B: 2, 3"]
    );
    // 沒變的不再送
    let opts = net::mpv_options(&s, &p.net_defaults());
    assert!(p.apply_net(&opts, false).is_empty(), "沒變的設定又送了一次");
    // 標頭清空
    apply_async(&mut p, &NetSettings::default());
    assert!(p.mpv().get_string_list("http-header-fields").unwrap().is_empty());
    assert_eq!(p.get_string("tls-verify").unwrap(), "yes");

    // VITASCOPE_MPV_OPTS（或 Options.extra）指定的選項：影戲不改
    let s = NetSettings {
        user_agent: "UA/3".into(),
        headers: vec!["X-Ours: 1".into()],
        timeout_secs: 9,
        ..NetSettings::default()
    };
    let mut q = net_player(&s, &[("user-agent", "Mine/1"), ("http-header-fields", "X-Mine: 1")]);
    assert!(q.user_overrides().contains("user-agent") && q.user_overrides().contains("http-header-fields"));
    assert_eq!(q.get_string("user-agent").unwrap(), "Mine/1");
    assert_eq!(q.mpv().get_string_list("http-header-fields").unwrap(), ["X-Mine: 1"]);
    assert_eq!(q.get_f64("network-timeout").unwrap(), 9.0, "其他的照樣設定");
    apply_async(&mut q, &NetSettings::default());
    assert_eq!(q.get_string("user-agent").unwrap(), "Mine/1");
    assert_eq!(q.mpv().get_string_list("http-header-fields").unwrap(), ["X-Mine: 1"]);
}

#[test]
fn hls_vod_plays() {
    if !net_sample("hls_vod_plays", "net/hls_vod/index.m3u8") {
        return;
    }
    let server = Server::start();
    let mut p = net_player(&NetSettings::default(), &[]);
    loaded(&mut p, &server.file_url("net/hls_vod/index.m3u8"));
    p.wait_state(TIMEOUT, |s| s.duration.is_some_and(|d| d > 2.5)).unwrap();
    // 播到結尾（3 秒；不是逾時、不是錯誤）
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile { reason, error: None }) => {
            assert_eq!(reason, vitascope::mpv::EndReason::Eof)
        }
        other => panic!("HLS 沒有播到結尾：{other:?}"),
    }
    for seg in ["seg0.ts", "seg1.ts", "seg2.ts"] {
        assert!(
            !server.requests_to(&format!("/f/net/hls_vod/{seg}")).is_empty(),
            "沒有讀到片段 {seg}"
        );
    }
}

/// 多畫質的 HLS：一開始選哪個畫質照 hls-bitrate（設定的「HLS / DASH 畫質」）。
/// 本專案的引擎（mpv 0.41 起）每個畫質是一個 edition（換 edition 就換畫質，不用重開）；系統的 libmpv 0.37 是好幾條影片軌
#[test]
fn hls_variants() {
    if !net_sample("hls_variants", "net/hls_multi/master.m3u8") {
        return;
    }
    let server = Server::start();
    let url = server.file_url("net/hls_multi/master.m3u8");
    for (bitrate, want, other) in [(HlsBitrate::Max, 320, 160), (HlsBitrate::Min, 160, 320)] {
        let s = NetSettings {
            hls_bitrate: bitrate,
            ..NetSettings::default()
        };
        let mut p = net_player(&s, &[("pause", "yes")]);
        loaded(&mut p, &url);
        p.wait_state(TIMEOUT, |st| st.selected(TrackKind::Video).is_some())
            .unwrap();
        assert_eq!(video_width(&p), Some(want), "{bitrate:?}：{:#?}", p.state.tracks);
        if mpv_version(&p) >= (0, 41) {
            let editions: Vec<serde_json::Value> =
                serde_json::from_str(&p.get_string("edition-list").unwrap()).unwrap();
            assert_eq!(editions.len(), 2, "兩個畫質是兩個 edition：{editions:#?}");
            // 換畫質：換 edition，不用重開
            let now = p.get_i64("current-edition").unwrap();
            p.mpv().set_property("edition", 1 - now).unwrap();
            p.wait_state(TIMEOUT, |st| {
                st.selected(TrackKind::Video).and_then(|t| t.width) == Some(other)
            })
            .unwrap();
        } else {
            let widths: Vec<_> = p.state.tracks_of(TrackKind::Video).filter_map(|t| t.width).collect();
            assert_eq!(widths.len(), 2, "兩個畫質是兩條影片軌：{widths:?}");
        }
    }
}

/// 多畫質的 DASH：每個畫質是一條影片軌，一開始照 hls-bitrate 選（DASH 只在本專案的引擎測）
#[test]
fn dash_variants() {
    if !l3_engine("dash_variants") || !net_sample("dash_variants", "net/dash_multi/manifest.mpd") {
        return;
    }
    let server = Server::start();
    let url = server.file_url("net/dash_multi/manifest.mpd");
    for (bitrate, want) in [(HlsBitrate::Max, 320), (HlsBitrate::Min, 160)] {
        let s = NetSettings {
            hls_bitrate: bitrate,
            ..NetSettings::default()
        };
        let mut p = net_player(&s, &[("pause", "yes")]);
        loaded(&mut p, &url);
        p.wait_state(TIMEOUT, |st| st.selected(TrackKind::Video).is_some())
            .unwrap();
        let mut widths: Vec<_> = p.state.tracks_of(TrackKind::Video).filter_map(|t| t.width).collect();
        widths.sort_unstable();
        assert_eq!(widths, [160, 320], "兩個畫質是兩條影片軌");
        assert_eq!(video_width(&p), Some(want), "{bitrate:?}：{:#?}", p.state.tracks);
        assert_eq!(p.state.tracks_of(TrackKind::Audio).count(), 1);
    }
}

/// 播放器介面註冊的網路 hook（開檔前、開檔失敗）：本機檔案、開不起來的檔案、網址都馬上放行，沒有卡住的
#[test]
fn hooks_continue_for_local_files() {
    let server = Server::start();
    let mut p = Player::new(Options {
        net_hooks: true,
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.hooks_continued(), 1, "on_load");
    assert_eq!(p.mpv().hooks_pending(), 0);
    // 開不起來：on_load、on_load_fail 都放行，照常回報失敗
    let missing = std::env::temp_dir().join("vitascope-net-no-such-file.mp4");
    p.open(&missing.to_string_lossy()).unwrap();
    let err = p.wait_for(TIMEOUT, |_| false).unwrap_err();
    assert!(err.contains("無法載入檔案"), "{err}");
    assert_eq!(p.hooks_continued(), 3, "on_load + on_load_fail");
    assert_eq!(p.mpv().hooks_pending(), 0);
    // 網址
    p.open(&server.file_url("common/mp4_h264_aac.mp4")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.hooks_continued(), 4);
    assert_eq!(p.mpv().hooks_pending(), 0);

    // 自動測試用的 headless 預設不註冊
    let mut q = headless();
    q.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    q.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(q.hooks_continued(), 0);
}

/// 網路上的播放清單（IPTV 的 .m3u 網址）：mpv 讀完清單、結束這個網址（原因是 redirect）時，展開的項目只留網路串流
/// （清單裡的 file:/// 拿掉），標題照 #EXTINF。照一般的開檔重新開第一個之後，mpv 自己的清單只剩一個
#[test]
fn remote_m3u_import_filters_unsafe_entries() {
    let server = Server::start();
    let mut p = net_player(&NetSettings::default(), &[("pause", "yes")]);
    let list = server.url("/m3u");
    p.open(&list).unwrap();
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Redirect,
            error: None,
        }) => {}
        other => panic!("網路上的清單應該是 redirect：{other:?}"),
    }
    let imported = p.take_remote_playlist().expect("展開的項目");
    assert!(p.take_remote_playlist().is_none(), "只拿一次");
    let a = server.file_url("common/mp4_h264_aac.mp4");
    let b = server.file_url("common/mkv_h264_aac_srt.mkv");
    assert_eq!(imported.source, list);
    assert_eq!(
        imported.entries,
        [(a.clone(), Some("第一個".to_owned())), (b, Some("第二個".to_owned()))]
    );
    assert_eq!((imported.start, imported.dropped), (0, 1), "file:/// 拿掉了");
    loaded(&mut p, &a);
    assert_eq!(p.get_i64("playlist-count").unwrap(), 1, "mpv 自己的清單只剩一個");
    // 一般的網址結束（不是 redirect）不展開
    p.stop().unwrap();
    p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
        .unwrap();
    assert!(p.take_remote_playlist().is_none());
}

/// 取消正在連線的網址：mpv 中斷連線，結束的原因是 stop、沒有錯誤；之後不再是「載入中」
#[test]
fn cancel_loading_stops_cleanly() {
    let server = Server::start();
    let mut p = net_player(&NetSettings::default(), &[]);
    let url = server.url("/slow");
    p.open(&url).unwrap();
    assert!(p.loading_now());
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::StartFile).unwrap();
    server.wait_request("/slow", TIMEOUT).expect("連到伺服器");
    let (loading, _) = p.net_loading().expect("網址連線中");
    assert_eq!(loading, url);
    p.cancel_loading().unwrap();
    assert!(!p.loading_now(), "取消後馬上不算載入中");
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Stop,
            error: None,
        }) => {}
        other => panic!("取消 = 停止、沒有錯誤：{other:?}"),
    }
    assert!(p.state.last_error.is_none());
    assert!(p.net_loading().is_none());
    // 本機檔案不算「網址連線中」
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    assert!(p.net_loading().is_none());
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert!(!p.loading_now());
}

// ───────────── 網站影片（假的 yt-dlp；不執行程式、不連網） ─────────────

/// 有網路 hook、用假的 yt-dlp 的播放器（跟介面一樣）。`sites` = 本機的測試伺服器（127.0.0.1）當成影片網站（先問 yt-dlp）；
/// 不當成的話是「其他網頁」：先照原樣開，認不出內容才問
fn site_player(fake: &Arc<FakeResolver>, sites: bool, s: &NetSettings, extra: &[(&str, &str)]) -> Player {
    let resolver: Arc<dyn Resolve> = fake.clone();
    let mut p = Player::new(Options {
        net_hooks: true,
        net_resolver: Some(resolver),
        net_sites: if sites { vec!["127.0.0.1".into()] } else { Vec::new() },
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    let opts = net::mpv_options(s, &p.net_defaults());
    for (name, _, r) in p.apply_net(&opts, true) {
        r.unwrap_or_else(|e| panic!("無法設定 {name}：{e}"));
    }
    p.set_net_config(s, &SitePrefs::default());
    p
}

/// 等到開檔失敗，回傳最後的說明（詳細原因的記錄可能比失敗晚到：等到說明裡有 `want`，等不到就回傳最後看到的）
fn failure_text(p: &mut Player, url: &str, want: &str) -> String {
    p.open(url).unwrap();
    loop {
        match p.wait(TIMEOUT) {
            Some(PlayerEvent::EndFile { error: Some(_), .. }) => break,
            Some(PlayerEvent::FileLoaded) => panic!("{url} 不該開得起來"),
            Some(_) => {}
            None => panic!("等不到 {url} 開檔失敗"),
        }
    }
    let _ = p.wait_state(TIMEOUT, |s| s.last_error.as_deref().is_some_and(|e| e.contains(want)));
    p.state.last_error.clone().unwrap_or_default()
}

/// 處理事件，直到開始等 yt-dlp
fn until_resolving(p: &mut Player) {
    let deadline = Instant::now() + TIMEOUT;
    while !p.site_resolving() {
        assert!(Instant::now() < deadline, "沒有開始問 yt-dlp");
        p.wait(Duration::from_millis(50));
    }
}

/// 網頁本身（`/watch`、`/playlist`）一次都沒被讀：網站影片只讀 yt-dlp 給的網址
fn page_never_fetched(server: &Server) {
    let pages: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.starts_with("/watch") || r.path.starts_with("/playlist"))
        .collect();
    assert!(pages.is_empty(), "播放器自己去讀了網頁：{pages:#?}");
}

/// 網站影片：yt-dlp 給的影像、聲音合成一部（EDL），用網站要的 User-Agent、Referer、Cookie（memory:// 的 Cookie 檔）去讀；
/// 標題、章節（載入後才設定）、網站的字幕（選到才下載）都有，mpv 的 path 還是網頁的網址
#[test]
fn fake_site_video_edl() {
    if !net_sample("fake_site_video_edl", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "vid1")).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 1);
    assert_eq!(fake.requests()[0].url, page);
    assert_eq!(p.mpv().hooks_pending(), 0);
    p.wait_state(TIMEOUT, |s| s.chapters.len() == 3).unwrap();
    assert_eq!(p.state.path.as_deref(), Some(page.as_str()), "path 是網頁");
    assert_eq!(p.get_string("media-title").unwrap(), SITE_TITLE_TEXT);
    assert_eq!(p.state.tracks_of(TrackKind::Video).filter(|t| !t.albumart).count(), 1);
    assert_eq!(p.state.tracks_of(TrackKind::Audio).count(), 1);
    let subs: Vec<_> = p.state.tracks_of(TrackKind::Sub).collect();
    assert_eq!(subs.len(), 1, "{:#?}", p.state.tracks);
    assert_eq!(subs[0].lang.as_deref(), Some("en"));
    assert_eq!(subs[0].title.as_deref(), Some("English (site)"));
    // 沒有標題的章節：mpv 給空字串，選單上顯示「第 n 章」
    let titles: Vec<&str> = p
        .state
        .chapters
        .iter()
        .map(|c| c.title.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(titles, ["開頭", "中間", ""]);
    let info = p.state.net.clone().expect("網站影片的資料");
    assert_eq!(info.page_url, page);
    assert_eq!(info.resume_key().as_deref(), Some("ytdl://fakesite/vid1"));
    assert!(p.state.net_busy.is_none() && p.state.net_failure.is_none());
    for rel in ["/f/net/video_only.mp4", "/f/net/audio_only.m4a"] {
        let reqs = server.requests_to(rel);
        assert!(!reqs.is_empty(), "沒有讀 {rel}");
        for r in &reqs {
            assert_eq!(r.header("User-Agent"), Some(SITE_UA), "{r:#?}");
            assert_eq!(r.header("Referer"), Some(SITE_REFERER), "{r:#?}");
            assert!(
                r.header("Cookie").is_some_and(|c| c.contains(SITE_COOKIE)),
                "沒有送網站的 Cookie：{r:#?}"
            );
        }
    }
    page_never_fetched(&server);
}

/// 網站影片的標頭、Cookie 只用在那個檔案：下一個網址照全域的設定（引擎預設的 User-Agent、沒有 Cookie、沒有 Referer）
#[test]
fn file_local_options_do_not_leak() {
    if !net_sample("file_local_options_do_not_leak", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "vid1")).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    loaded(&mut p, &server.file_url("common/mp4_h264_aac.mp4"));
    assert!(p.state.net.is_none(), "不是網站影片了");
    let engine_ua = p.net_defaults().user_agent;
    let reqs = server.requests_to("/f/common/mp4_h264_aac.mp4");
    assert!(!reqs.is_empty());
    for r in &reqs {
        assert_eq!(r.header("User-Agent"), Some(engine_ua.as_str()), "{r:#?}");
        assert_eq!(r.header("Cookie"), None, "{r:#?}");
        assert_eq!(r.header("Referer"), None, "{r:#?}");
    }
    assert_eq!(p.get_string("user-agent").unwrap(), engine_ua);
    assert!(p.mpv().get_string_list("http-header-fields").unwrap().is_empty());
    assert_eq!(fake.calls(), 1, "媒體檔的網址不問 yt-dlp");
}

/// 網站影片播放中改了 User-Agent：網站影片用 file-local 蓋過了它，mpv 在檔案結束時會還原成改之前的值。
/// 播放器要再送一次：下一個網址用新的 User-Agent
#[test]
fn ua_change_during_site_video_reaches_next_url() {
    if !net_sample("ua_change_during_site_video_reaches_next_url", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "vid1")).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    let changed = NetSettings {
        user_agent: "Changed/2".into(),
        headers: vec!["X-After: 1".into()],
        ..NetSettings::default()
    };
    apply_async(&mut p, &changed);
    loaded(&mut p, &server.file_url("common/mp4_h264_aac.mp4"));
    let reqs = server.requests_to("/f/common/mp4_h264_aac.mp4");
    assert!(!reqs.is_empty());
    for r in &reqs {
        assert_eq!(r.header("User-Agent"), Some("Changed/2"), "{r:#?}");
        assert_eq!(r.header("X-After"), Some("1"), "{r:#?}");
        assert_eq!(r.header("Cookie"), None, "{r:#?}");
    }
    assert_eq!(p.get_string("user-agent").unwrap(), "Changed/2");
    // 記下的值跟 mpv 一致：同樣的設定不再送
    let opts = net::mpv_options(&changed, &p.net_defaults());
    assert!(p.apply_net(&opts, false).is_empty(), "沒變的設定又送了一次");
}

/// 網址指定的開始時間（yt-dlp 的 start_time）；換畫質時從現在的位置接著播，20 分鐘內不再問 yt-dlp（用之前的結果）
#[test]
fn start_time_and_reload_keep_position() {
    if !net_sample("start_time_and_reload_keep_position", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=vid1&t=1");
    let json = site_video_json(&server.url(""), &page, "vid1").replacen(
        "\"duration\": 3,",
        "\"duration\": 3, \"start_time\": 1.5,",
        1,
    );
    let fake = FakeResolver::json(json).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    p.wait_state(TIMEOUT, |s| (s.time_pos - 1.5).abs() < 0.25).unwrap();
    assert_eq!(p.state.net.as_ref().and_then(|n| n.start_at), Some(1.5));
    p.seek_to(0.5, true).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 0.5).abs() < 0.1).unwrap();
    p.reload_net(Choice::Format {
        video: "v1".into(),
        audio: Some("a1".into()),
    })
    .unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 0.5).abs() < 0.25).unwrap();
    assert_eq!(fake.calls(), 1, "換畫質用之前的結果，不再問 yt-dlp");
    let info = p.state.net.clone().unwrap();
    assert!(
        info.start_at.is_some_and(|t| (t - 0.5).abs() < 0.1),
        "{:?}",
        info.start_at
    );
    let ids: Vec<_> = info.chosen.iter().filter_map(|f| f.format_id.as_deref()).collect();
    assert_eq!(ids, ["v1", "a1"]);
    page_never_fetched(&server);
}

/// 等 yt-dlp 的時候開了別的檔案：不等了，新的檔案馬上開；之後才到的結果不理（不會換回網站影片）
#[test]
fn cancel_by_opening_another_file() {
    if !net_sample("cancel_by_opening_another_file", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=slow");
    let late = site_video_json(&server.url(""), &page, "slow");
    // 不理會取消，8 秒後照樣回傳結果
    let fake = FakeResolver::block(Duration::from_secs(8), true, move || {
        Ok(support::fake_ytdl::resolved(&late, Vec::new()))
    })
    .arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    p.open(&page).unwrap();
    until_resolving(&mut p);
    assert!(p.state.net_busy.as_ref().is_some_and(|b| b.url == page && !b.playlist));
    let local = sample("common/mp4_h264_aac.mp4");
    let start = Instant::now();
    p.open(&local).unwrap();
    assert!(!p.site_resolving() && p.state.net_busy.is_none());
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .expect("等 yt-dlp 的時候開別的檔案，要馬上開");
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "新的檔案等了 yt-dlp：{:?}",
        start.elapsed()
    );
    let is_local = |p: &Player| p.state.path.as_deref().map(std::path::Path::new) == Some(std::path::Path::new(&local));
    assert!(is_local(&p), "{:?}", p.state.path);
    // 等到假的 yt-dlp 真的回傳了（不理會取消的那一個），再處理一陣子事件：還是本機的檔案
    let deadline = Instant::now() + Duration::from_secs(30);
    while fake.running() > 0 {
        assert!(Instant::now() < deadline, "假的 yt-dlp 沒有結束");
        p.wait(Duration::from_millis(100));
    }
    p.wait(Duration::from_millis(500));
    assert!(p.state.loaded);
    assert!(is_local(&p), "{:?}", p.state.path);
    assert!(p.state.net.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    page_never_fetched(&server);
}

/// 等 yt-dlp 的時候取消（Esc、取消、停止）：結束的原因是 stop、沒有錯誤；hook 放行了，背景的解析也收到取消
#[test]
fn cancel_loading_during_resolve() {
    let server = Server::start();
    let page = server.url("/watch?v=slow");
    let fake = FakeResolver::block(Duration::from_secs(60), false, || Err(YtdlError::NoResponse.into())).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[]);
    p.open(&page).unwrap();
    until_resolving(&mut p);
    let (loading, _) = p.net_loading().expect("連線中");
    assert_eq!(loading, page);
    p.cancel_loading().unwrap();
    assert!(!p.loading_now() && p.state.net_busy.is_none());
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Stop,
            error: None,
        }) => {}
        other => panic!("取消 = 停止、沒有錯誤：{other:?}"),
    }
    assert!(p.state.last_error.is_none() && p.state.net_failure.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    let deadline = Instant::now() + TIMEOUT;
    while fake.cancelled() == 0 {
        assert!(Instant::now() < deadline, "背景的解析沒有收到取消");
        std::thread::sleep(Duration::from_millis(20));
    }
    page_never_fetched(&server);
}

/// 等 yt-dlp 的時候直接停止（不是 `cancel_loading`）、或還沒處理到 hook 就換了檔案：播放器發現那個檔案被拿掉了，
/// 不等 yt-dlp、放行 hook（被換掉的那一個根本不問）
#[test]
fn stop_or_replace_before_the_hook_never_waits_for_ytdl() {
    let server = Server::start();
    let page = server.url("/watch?v=slow");
    let fake = FakeResolver::block(Duration::from_secs(60), false, || Err(YtdlError::NoResponse.into())).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[]);
    p.open(&page).unwrap();
    until_resolving(&mut p);
    p.stop().unwrap();
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Stop,
            error: None,
        }) => {}
        other => panic!("停止、沒有錯誤：{other:?}"),
    }
    assert!(!p.site_resolving() && p.state.net_busy.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    assert_eq!(fake.calls(), 1);
    // 開了網站影片、還沒處理事件（hook 還沒收到）就換成本機的檔案：網站影片根本不問 yt-dlp
    p.open(&server.url("/watch?v=replaced")).unwrap();
    let local = sample("common/mp4_h264_aac.mp4");
    p.open(&local).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .expect("換掉的網站影片不該讓新的檔案等 yt-dlp");
    assert_eq!(fake.calls(), 1, "被換掉的網址不問 yt-dlp");
    assert_eq!(p.mpv().hooks_pending(), 0);
    page_never_fetched(&server);
}

/// yt-dlp 的錯誤：說明用 yt-dlp 的原因（不是「無法載入檔案」），提醒與建議也留著；網址換成空的 memory://，
/// mpv 不會再去讀網頁。沒有 yt-dlp、yt-dlp 一直不回應（看門狗）也說明原因
#[test]
fn resolver_timeout_and_failure_messages() {
    let server = Server::start();
    let fake = FakeResolver::new(|req, _| {
        let v = req.url.rsplit('=').next().unwrap_or_default();
        match v {
            "unsupported" => Err(YtdlError::Unsupported.into()),
            "bot" => Err(Failure {
                error: YtdlError::NotABot,
                hints: vec![Hint::NeedsJsRuntime],
            }),
            "drm" => Err(YtdlError::Drm.into()),
            // 第一次用的時候才找完：沒有 yt-dlp
            "gone" => Err(YtdlError::Missing.into()),
            // 不理會取消、一直不回應
            _ => {
                std::thread::sleep(Duration::from_secs(30));
                Err(YtdlError::NoResponse.into())
            }
        }
    })
    .with_watchdog(Duration::from_secs(1))
    .arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[]);
    for (v, want, hints) in [
        ("unsupported", "無法播放網站影片：yt-dlp 不支援這個網站", vec![]),
        (
            "bot",
            "無法播放網站影片：網站要求確認不是機器人",
            vec![Hint::NeedsJsRuntime],
        ),
        ("drm", "無法播放網站影片：影片有 DRM 保護，無法播放", vec![]),
    ] {
        let err = failure_text(&mut p, &server.url(&format!("/watch?v={v}")), want);
        assert_eq!(err, want, "記錄：{:#?}", p.recent_errors());
        let f = p.state.net_failure.clone().expect("網站影片的原因");
        assert_eq!(f.hints, hints);
        assert!(!p.state.net_need_ytdl);
        assert_eq!(p.mpv().hooks_pending(), 0);
    }
    assert_eq!(
        p.state.net_failure.as_ref().and_then(|f| f.remedy()),
        None,
        "DRM 沒有建議"
    );
    // 解析的時候才知道沒有 yt-dlp（第一次用、還沒找完）：跟一開始就知道一樣，說明要 yt-dlp
    let err = failure_text(&mut p, &server.url("/watch?v=gone"), "yt-dlp");
    assert_eq!(err, "網站影片需要 yt-dlp");
    assert!(p.state.net_need_ytdl);
    // 看門狗（這裡 1 秒）：不再等，說明 yt-dlp 沒有回應
    let start = Instant::now();
    let err = failure_text(&mut p, &server.url("/watch?v=hang"), "沒有回應");
    assert_eq!(err, "無法播放網站影片：yt-dlp 沒有回應");
    assert!(start.elapsed() >= Duration::from_millis(900), "{:?}", start.elapsed());
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
    assert_eq!(p.mpv().hooks_pending(), 0);
    page_never_fetched(&server);

    // 沒有 yt-dlp：影片網站的網址馬上說明要 yt-dlp（建議取得），不去讀網頁
    let missing = FakeResolver::missing().arc();
    let mut q = site_player(&missing, true, &NetSettings::default(), &[]);
    let err = failure_text(&mut q, &server.url("/watch?v=x"), "yt-dlp");
    assert_eq!(err, "網站影片需要 yt-dlp");
    assert!(q.state.net_need_ytdl);
    assert_eq!(
        q.state.net_failure.as_ref().and_then(|f| f.remedy()),
        Some(vitascope::ytdl::Remedy::GetYtdl)
    );
    assert_eq!(missing.calls(), 0);
    page_never_fetched(&server);
}

/// 網站資料裡的網址是本機檔案、edl:// 之類（不能從網站開的）：不開，說明沒有可以播放的格式
#[test]
fn unsafe_urls_from_site_are_dropped() {
    let server = Server::start();
    let page = server.url("/watch?v=bad");
    let fake = FakeResolver::json(
        r#"{"title": "bad", "requested_formats": [
             {"format_id": "v", "url": "file:///etc/passwd", "protocol": "https", "vcodec": "avc1", "acodec": "none"},
             {"format_id": "a", "url": "edl://%3%abc", "protocol": "http", "vcodec": "none", "acodec": "mp4a"}]}"#,
    )
    .arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[]);
    let err = failure_text(&mut p, &page, "格式");
    assert_eq!(err, "無法播放網站影片：沒有可以播放的格式");
    page_never_fetched(&server);
}

/// N-M4：mpv 不保證錯誤記錄比 on_load_fail 的 hook 事件先送到（平常是先到）。hook 先到、「Failed to open」後到時，
/// 也要等記錄都收到了才決定：連不上的網頁不問 yt-dlp。用 `inject_event` 照這個順序送，不靠 mpv 剛好怎麼送
#[test]
fn on_load_fail_waits_for_a_log_that_arrives_after_it() {
    let server = Server::start();
    let fake = FakeResolver::fail(YtdlError::Unsupported.into()).arc();
    let mut p = site_player(&fake, false, &NetSettings::default(), &[]);
    // 其他網頁：放行 on_load（照原樣開），mpv 連到伺服器後一直等（`/slow` 不回應）
    let slow = server.url("/slow");
    p.open(&slow).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while server.requests_to("/slow").is_empty() {
        assert!(Instant::now() < deadline, "沒有連到伺服器");
        let _ = p.wait(Duration::from_millis(20));
    }
    p.poll();
    assert!(p.loading_now() && !p.site_resolving());
    // hook 先到（序號是假的：放行時 mpv 說不是等待中的，沒關係），記錄後到，同一次 poll 處理
    p.inject_event(Event::Hook {
        name: "on_load_fail".into(),
        id: u64::MAX,
        userdata: 2,
    });
    p.inject_event(Event::Log {
        prefix: "stream".into(),
        level: "error".into(),
        text: format!(
            "Failed to open {slow}.
"
        ),
    });
    p.poll();
    assert!(
        !p.site_resolving(),
        "hook 一到就決定了（還沒看到記錄）：連不上的網頁去問了 yt-dlp"
    );
    assert_eq!(fake.calls(), 0);
    p.stop().unwrap();
    p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
        .expect("停止");
    let deadline = Instant::now() + TIMEOUT;
    while p.mpv().hooks_pending() > 0 {
        assert!(Instant::now() < deadline, "hook 沒有放行");
        let _ = p.wait(Duration::from_millis(20));
    }
    assert_eq!(fake.calls(), 0);
}

/// 網站的播放清單：交給介面（NetPlaylist：照順序、標題），這個網址安靜地停下（stop、沒有錯誤）。
/// 「載入整個播放清單」（只對這次有效的要求）從網址 v= 的那一部開始
#[test]
fn site_playlist_redirect_import() {
    let server = Server::start();
    let page = server.url("/playlist?list=PL1");
    let items = vec![(server.url("/watch?v=p1"), "p1"), (server.url("/watch?v=p2"), "p2")];
    let fake = FakeResolver::json(site_playlist_json(&page, &items)).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[]);
    p.open(&page).unwrap();
    let ev = p
        .wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::NetPlaylist { .. }))
        .unwrap();
    let want: Vec<(String, Option<String>)> = vec![
        (items[0].0.clone(), Some("第 1 部".into())),
        (items[1].0.clone(), Some("第 2 部".into())),
    ];
    assert_eq!(
        ev,
        PlayerEvent::NetPlaylist {
            source: page.clone(),
            entries: want.clone(),
            start: 0,
            dropped: 0,
            capped: false,
        }
    );
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile {
            reason: EndReason::Stop,
            error: None,
        }) => {}
        other => panic!("播放清單的網址安靜地停下：{other:?}"),
    }
    assert!(p.state.last_error.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    assert!(!fake.requests()[0].playlist, "預設只播這部影片（--no-playlist）");
    // 載入整個播放清單：從 v=p2 開始
    let both = server.url("/watch?v=p2&list=PL1");
    p.open_with_mode(
        &both,
        Mode {
            yes_playlist: true,
            ..Default::default()
        },
    )
    .unwrap();
    let ev = p
        .wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::NetPlaylist { .. }))
        .unwrap();
    assert_eq!(
        ev,
        PlayerEvent::NetPlaylist {
            source: both,
            entries: want,
            start: 1,
            dropped: 0,
            capped: false,
        }
    );
    assert!(fake.requests()[1].playlist, "--yes-playlist");
    page_never_fetched(&server);
}

/// 其他網頁：先照原樣開。mpv 認不出內容（是網頁）才問 yt-dlp，問得到就照樣播；連不上、HTTP 404 之類不問
/// （說明照原本的原因，不多等 yt-dlp）。要不要問看錯誤記錄（記錄比 hook 晚到時的順序見
/// `on_load_fail_waits_for_a_log_that_arrives_after_it`）
#[test]
fn fallback_only_for_unrecognized_content() {
    if !net_sample("fallback_only_for_unrecognized_content", "net/video_only.mp4") {
        return;
    }
    let server = Server::start();
    let html = server.url("/html");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &html, "h1")).arc();
    let mut p = site_player(&fake, false, &NetSettings::default(), &[("pause", "yes")]);
    // 404：不問 yt-dlp（mpv 在 on_load_fail 之前記下「Failed to open」）
    let not_found = server.url("/status/404");
    p.open(&not_found).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut ended = None;
    while server.requests_to("/status/404").is_empty() && ended.is_none() {
        assert!(Instant::now() < deadline, "沒有連到伺服器");
        // 放行 on_load（處理事件；很快的機器上可能這時就已經失敗了）
        ended = p
            .wait(Duration::from_millis(5))
            .filter(|e| matches!(e, PlayerEvent::EndFile { .. }));
    }
    if ended.is_none() {
        ended = p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })).ok();
    }
    assert!(
        matches!(ended, Some(PlayerEvent::EndFile { error: Some(_), .. })),
        "404 應該開檔失敗：{ended:?}"
    );
    assert_eq!(fake.calls(), 0, "HTTP 404 不該問 yt-dlp");
    assert!(p.state.net_failure.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    // 網頁：先自己讀（伺服器收到），認不出來才問 yt-dlp，照 yt-dlp 的結果播
    loaded(&mut p, &html);
    assert_eq!(fake.calls(), 1);
    assert!(!server.requests_to("/html").is_empty(), "其他網頁先照原樣開");
    assert_eq!(p.get_string("media-title").unwrap(), SITE_TITLE_TEXT);
    assert!(p.state.net.is_some());
    assert_eq!(p.mpv().hooks_pending(), 0);

    // yt-dlp 也不認得這個網頁：照 mpv 原本的說明（是網頁，不是影片檔）
    let unknown = FakeResolver::fail(YtdlError::Unsupported.into()).arc();
    let mut q = site_player(&unknown, false, &NetSettings::default(), &[]);
    let err = failure_text(&mut q, &html, "網頁");
    assert_eq!(err, "無法開啟網址：這個網址是網頁，不是影片檔");
    assert_eq!(unknown.calls(), 1);
    // 沒有 yt-dlp：說明裡提一句
    let missing = FakeResolver::missing().arc();
    let mut r = site_player(&missing, false, &NetSettings::default(), &[]);
    let err = failure_text(&mut r, &html, "yt-dlp");
    assert_eq!(
        err,
        "無法開啟網址：這個網址是網頁，不是影片檔（網站上的影片要用 yt-dlp 播放）"
    );
    assert!(!r.state.net_need_ytdl, "不是已知的影片網站：不另外提示取得 yt-dlp");
    assert_eq!(r.mpv().hooks_pending(), 0);
}

/// mpv 的事件佇列滿了、送不出 hook 時會把 hook 拿掉（「Removing hook」）：播放器重新註冊，之後的檔案照樣收得到
#[test]
fn removed_hook_is_registered_again() {
    let fake = FakeResolver::fail(YtdlError::Unsupported.into()).arc();
    let mut p = site_player(&fake, false, &NetSettings::default(), &[]);
    let mpv = p.mpv().clone();
    // 塞滿佇列（不讀事件）：每個非同步指令的回覆都先預留一個位置，塞到 mpv 說滿了為止
    let mut sent = 0u64;
    while sent < 5000 && mpv.command_async(sent + 1, &["ignore"]).is_ok() {
        sent += 1;
    }
    assert!(sent < 5000, "佇列一直沒滿");
    mpv.command(&["loadfile", LAVFI]).unwrap();
    // 不讀事件，等檔案載入：佇列是滿的，mpv 送不出 on_load，把 hook 拿掉、照樣載入。
    // （hook 送出去了的話，mpv 會停在 hook 等，永遠不會載入）
    let deadline = Instant::now() + TIMEOUT;
    while p.get_f64("duration").is_err() {
        assert!(
            Instant::now() < deadline,
            "佇列滿了，mpv 卻沒有拿掉 hook（塞了 {sent} 個指令）"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // 讀事件（包括「Removing hook」的記錄）：重新註冊
    while p.hooks_readded() == 0 {
        assert!(Instant::now() < deadline, "mpv 拿掉了 hook，播放器沒有重新註冊");
        p.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    // 新版的 mpv 說是哪一個（只拿掉了 on_load）；0.37 沒說，兩個都重新註冊
    assert!((1..=2).contains(&p.hooks_readded()), "{}", p.hooks_readded());
    // 之後的檔案照樣收到 on_load
    let before = p.hooks_continued();
    p.open(LAVFI).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert!(
        p.hooks_continued() > before,
        "hook 被拿掉之後沒有重新註冊（之後的檔案收不到 on_load）"
    );
    assert_eq!(p.mpv().hooks_pending(), 0);
}

/// 舊的 libmpv（0.37）拿掉 hook 時的記錄沒寫是哪一個：兩個都重新註冊，沒被拿掉的那一個就有兩份。
/// 同一個檔案收到兩次 on_load 只處理一次（字幕不會加兩次、不會問兩次 yt-dlp），兩次 on_load_fail 也照常放行
#[test]
fn duplicated_hooks_after_an_old_style_removal_are_handled_once() {
    if !net_sample(
        "duplicated_hooks_after_an_old_style_removal_are_handled_once",
        "net/video_only.mp4",
    ) {
        return;
    }
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "vid1")).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    // 0.37 的寫法（沒有「main/on_load」）
    p.inject_event(Event::Log {
        prefix: "cplayer".into(),
        level: "warn".into(),
        text: "Sending hook command failed. Removing hook.\n".into(),
    });
    p.poll();
    assert_eq!(p.hooks_readded(), 2, "看不出是哪一個：兩個都重新註冊");
    let before = p.hooks_continued();
    loaded(&mut p, &page);
    assert_eq!(p.hooks_continued() - before, 2, "on_load 有兩份，兩個都放行");
    assert_eq!(fake.calls(), 1, "只問一次 yt-dlp");
    assert_eq!(
        p.state.tracks_of(TrackKind::Sub).count(),
        1,
        "網站的字幕只加一次：{:#?}",
        p.state.tracks
    );
    assert_eq!(p.mpv().hooks_pending(), 0);
    // 其他網頁（不是影片網站）：認不出內容才問 yt-dlp。on_load_fail 也有兩份：第二份直接放行，只問一次
    let html = server.url("/html");
    let fallback = FakeResolver::json(site_video_json(&server.url(""), &html, "h1")).arc();
    let mut q = site_player(&fallback, false, &NetSettings::default(), &[("pause", "yes")]);
    q.inject_event(Event::Log {
        prefix: "cplayer".into(),
        level: "warn".into(),
        text: "Sending hook command failed. Removing hook.
"
        .into(),
    });
    q.poll();
    loaded(&mut q, &html);
    assert_eq!(fallback.calls(), 1, "只問一次 yt-dlp");
    assert_eq!(q.state.tracks_of(TrackKind::Sub).count(), 1, "{:#?}", q.state.tracks);
    assert_eq!(q.mpv().hooks_pending(), 0);
}
