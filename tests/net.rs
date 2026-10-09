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
use support::http::Server;
use vitascope::mpv::{Event, Mpv, Node};
use vitascope::net::{self, HlsBitrate, NetSettings};
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};

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
