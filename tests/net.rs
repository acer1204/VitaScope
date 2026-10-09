//! 網路功能的基礎：mpv 的 hook、wakeup、用 node 設定屬性（headless：不出畫面、不出聲音，三個平台的 CI 都跑）
//!
//! 網站影片（yt-dlp）要在 mpv 開檔前（`on_load` hook）換掉要開的網址、設定這個檔案專用的標頭，
//! 載入後再設定章節；這裡先確認 mpv 包裝層的這些功能本身是對的。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vitascope::mpv::{Event, Mpv, Node};
use vitascope::player::{Options, Player, PlayerEvent};

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
