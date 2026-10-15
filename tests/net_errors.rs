//! 開網址失敗時的說明（HTTP 404、找不到伺服器、逾時、網頁不是影片；網站影片快取的網址失效時重新問 yt-dlp）。只連本機的測試伺服器 127.0.0.1。
//!
//! 這些原因大多只出現在 FFmpeg 的記錄裡，而 FFmpeg 的記錄只送到「行程裡第一個建立、還在的 mpv」
//! （mpv 的 common/av_log.c）。其他測試程式平行建立很多播放器，誰是第一個不一定；所以這些測試放在自己的測試程式裡，
//! 每個測試從建立播放器到釋放都拿著同一把鎖：同一時間只有一個播放器，它一定是第一個。

mod support;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use support::app_ui::{Seen, pick_site_quality, step_until_seen};
use support::fake_ytdl::{
    FakeResolver, expire_signed, page_never_fetched, resolved, signed_site, site_options, site_player,
    site_player_with, site_video_json, two_quality_json,
};
use support::http::Server;
use vitascope::app::{Launch, VitascopeApp};
use vitascope::i18n::Lang;
use vitascope::mpv::{EndReason, Event};
use vitascope::net::{self, NetSettings};
use vitascope::player::{Options, Player, PlayerEvent, State, TrackKind};
use vitascope::settings::Settings;
use vitascope::ytdl::YtdlError;
use vitascope::ytdl::plan::Choice;

const TIMEOUT: Duration = Duration::from_secs(30);

static ONE_PLAYER: Mutex<()> = Mutex::new(());

/// 拿到鎖才能建立播放器（前一個測試失敗時鎖會「中毒」，照樣可以用）。
/// 鎖要比播放器先宣告：變數照宣告的相反順序釋放，播放器先釋放、才放開鎖
fn lock() -> MutexGuard<'static, ()> {
    ONE_PLAYER.lock().unwrap_or_else(|e| e.into_inner())
}

/// headless 播放器，網路設定照預設值同步套用（跟介面啟動時一樣）；`extra` 是額外的 mpv 選項
fn player(extra: &[(&str, &str)]) -> Player {
    let mut p = Player::new(Options {
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗");
    let opts = net::mpv_options(&NetSettings::default(), &p.net_defaults());
    for (name, _, r) in p.apply_net(&opts, true) {
        r.unwrap_or_else(|e| panic!("無法設定 {name}：{e}"));
    }
    p
}

/// 開 `url`，等到開檔失敗，回傳（失敗花的時間, 最後的說明）。
/// 詳細原因的記錄比開檔失敗晚到（mpv 先送一般事件），說明會跟著更新：等到說明裡有 `want`，等不到就回傳最後看到的
fn failure_of(p: &mut Player, url: &str, want: &str) -> (Duration, String) {
    let start = Instant::now();
    p.open(url).unwrap();
    loop {
        match p.wait(TIMEOUT.saturating_sub(start.elapsed())) {
            Some(PlayerEvent::EndFile { error: Some(_), .. }) => break,
            Some(PlayerEvent::FileLoaded) => panic!("{url} 不該開得起來"),
            Some(_) => {}
            None => panic!("等不到 {url} 開檔失敗"),
        }
    }
    let took = start.elapsed();
    let _ = p.wait_state(TIMEOUT, |s| s.last_error.as_deref().is_some_and(|e| e.contains(want)));
    let last = p.state.last_error.clone().unwrap_or_default();
    (took, last)
}

/// HTTP 的錯誤是 FFmpeg 的「警告」：播放器要收警告，才說得出是 404 還是 403
#[test]
fn http_errors_are_explained() {
    let _one = lock();
    let server = Server::start();
    let mut p = player(&[]);
    for (code, want) in [
        (404, "無法開啟網址：找不到這個網址的內容（HTTP 404）"),
        (403, "無法開啟網址：伺服器拒絕存取（HTTP 403）：網址可能過期或需要登入"),
        (500, "無法開啟網址：伺服器回應錯誤（HTTP 500）"),
    ] {
        let url = server.url(&format!("/status/{code}"));
        let (_, err) = failure_of(&mut p, &url, want);
        assert_eq!(err, want, "記錄：{:#?}", p.recent_errors());
    }
    // 網頁：mpv 讀得到內容，但不是影片
    let (_, err) = failure_of(&mut p, &server.url("/html"), "網頁");
    assert_eq!(
        err,
        "無法開啟網址：這個網址是網頁，不是影片檔",
        "記錄：{:#?}",
        p.recent_errors()
    );
    // 英文介面
    vitascope::i18n::set_lang(vitascope::i18n::Lang::En);
    let (_, err) = failure_of(&mut p, &server.url("/status/410"), "HTTP 410");
    vitascope::i18n::set_lang(vitascope::i18n::Lang::ZhTw);
    assert_eq!(err, "Can't open the URL: Nothing found at this address (HTTP 410)");
}

/// 伺服器收下連線卻一直不回應：照設定的逾時（這裡 2 秒）放棄，說明是逾時。
/// 不能每次失敗都重試（FFmpeg 的 reconnect_on_network_error 會重試 4 次、中間還等 4 秒，要十幾秒才放棄）
#[test]
fn network_timeout_applies() {
    let _one = lock();
    let server = Server::start();
    let mut p = player(&[("network-timeout", "2")]);
    let (took, err) = failure_of(&mut p, &server.url("/slow"), "逾時");
    assert_eq!(
        err,
        "無法開啟網址：連線逾時（2 秒內沒有回應）",
        "記錄：{:#?}",
        p.recent_errors()
    );
    // 逾時 2 秒，慢的 CI 也不會超過 8 秒；一直重試的話至少 12 秒
    assert!(took >= Duration::from_millis(1500), "還沒逾時就放棄了：{took:?}");
    assert!(took < Duration::from_secs(8), "逾時 2 秒，卻花了 {took:?}（重試了？）");
    assert_eq!(server.requests_to("/slow").len(), 1, "連線失敗不重試");
}

/// 找不到伺服器。主機名稱裡有超過 63 個字元的一段（DNS 不允許），系統的解析器在本機就拒絕，
/// 不會真的去問 DNS 伺服器（測試不能連到外面的網路）
#[test]
fn dns_failure_is_explained() {
    let _one = lock();
    let mut p = player(&[]);
    let url = format!("http://{}.invalid/video.mp4", "a".repeat(64));
    let (_, err) = failure_of(&mut p, &url, "找不到伺服器");
    assert_eq!(
        err,
        "無法開啟網址：找不到伺服器（請確認網址與網路連線）",
        "記錄：{:#?}",
        p.recent_errors()
    );
}

/// 介面：網址開不了時，起始畫面寫出原因（HTTP 404；英文介面也是）
#[test]
fn start_screen_explains_a_404() {
    let _one = lock();
    let server = Server::start();
    let url = server.url("/status/404");
    for (lang, want) in [
        (Lang::ZhTw, "無法開啟網址：找不到這個網址的內容（HTTP 404）"),
        (Lang::En, "Can't open the URL: Nothing found at this address (HTTP 404)"),
    ] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        let player = Player::new(Options {
            keep_open: true,
            ..Options::headless()
        })
        .expect("建立 mpv 失敗");
        let launch = Launch {
            files: vec![std::path::PathBuf::from(&url)],
            ..Default::default()
        };
        let mut h = Harness::builder()
            .with_size([960.0, 600.0])
            .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch));
        // 詳細的原因（FFmpeg 的記錄）比開檔失敗晚到，說明會跟著更新
        let start = Instant::now();
        while h.query_by_label(want).is_none() {
            assert!(
                start.elapsed() < TIMEOUT,
                "起始畫面沒有寫出原因：{:?}（記錄：{:#?}）",
                h.state().player().state.last_error,
                h.state().player().recent_errors()
            );
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    vitascope::i18n::set_lang(Lang::ZhTw);
}

// ───────────── 網站影片：快取的網址失效（假的 yt-dlp；不執行程式、不連網） ─────────────

/// 同 `fake_ytdl::site_options`（本機的測試伺服器 127.0.0.1 當成影片網站），跟介面一樣播完停在最後一格
fn kept_site_options(fake: &Arc<FakeResolver>) -> Options {
    Options {
        keep_open: true,
        ..site_options(fake, true, &[])
    }
}

/// `kept_site_options` 的 headless 播放器，網路設定照預設值同步套用（跟介面啟動時一樣）
fn kept_site_player(fake: &Arc<FakeResolver>) -> Player {
    site_player_with(kept_site_options(fake), &NetSettings::default())
}

/// 介面：有網路 hook、用假的 yt-dlp 的播放器（`kept_site_options`），播完不自動接下一個
fn site_harness(fake: &Arc<FakeResolver>) -> Harness<'static, VitascopeApp> {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let player = Player::new(kept_site_options(fake)).expect("建立 mpv 失敗");
    Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, Launch::default()))
}

/// 網路測試的樣本（`python scripts/gen_samples.py` 產生；這台的 FFmpeg 做不出來時略過）
fn net_sample(test: &str, rel: &str) -> bool {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    if !p.exists() {
        eprintln!("略過 {test}：沒有樣本 {}（python scripts/gen_samples.py）", p.display());
    }
    p.exists()
}

fn sample(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

fn loaded(p: &mut Player, url: &str) {
    p.open(url).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {url}：{e}"));
}

/// 介面：一直跑介面幀，直到條件成立
fn step_until_net(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&VitascopeApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}
目前狀態：{:#?}",
            h.state().player().state
        );
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn playing_url(s: &State, url: &str) -> bool {
    s.loaded && s.path.as_deref() == Some(url)
}

/// 正在播的網站影片用的格式是這個
fn playing_format(app: &VitascopeApp, page: &str, id: &str) -> bool {
    let s = &app.player().state;
    playing_url(s, page)
        && s.net
            .as_ref()
            .is_some_and(|n| !n.audio_only && n.chosen.iter().any(|f| f.format_id.as_deref() == Some(id)))
}

/// 讀過這個網址（伺服器收到過；查詢字串也比對）
fn fetched(server: &Server, path: &str) -> bool {
    !server.requests_to(path).is_empty()
}

/// 介面：暫停中精準跳到 `t`，等畫面跟上
fn paused_at(h: &mut Harness<'_, VitascopeApp>, t: f64) {
    h.state().player().seek_to(t, true).unwrap();
    let start = Instant::now();
    loop {
        h.step();
        let s = &h.state().player().state;
        if s.paused && (s.time_pos - t).abs() < 0.05 {
            break;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：跳到指定的時間
目前狀態：{s:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    h.run_steps(3);
}

const STALE_403: &str = "無法開啟網址：伺服器拒絕存取（HTTP 403）：網址可能過期或需要登入";

/// 主人回報的情形：播過網站影片，過一會兒再貼同一個網址，起始畫面寫「伺服器拒絕存取（HTTP 403）」，
/// 關掉影戲重開才能播（20 分鐘內用之前解析的網址，網站已經讓它們失效）。現在照樣播得起來，畫面上沒有錯誤
#[test]
fn start_screen_replays_a_site_video_whose_urls_expired() {
    if !net_sample(
        "start_screen_replays_a_site_video_whose_urls_expired",
        "net/video_only.mp4",
    ) {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=JpQC0W91E6k");
    let fake = signed_site(&server.url(""), &page, "JpQC0W91E6k").arc();
    let mut settings = Settings::default();
    settings.auto_next = false;
    let player = Player::new(kept_site_options(&fake)).expect("建立 mpv 失敗");
    let mut h = Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, Launch::default()));
    let plays = |h: &mut Harness<'_, VitascopeApp>, what: &str| {
        let start = Instant::now();
        loop {
            h.step();
            let s = &h.state().player().state;
            if s.loaded && s.path.as_deref() == Some(page.as_str()) && s.net.is_some() {
                return;
            }
            assert!(
                h.query_by_label_contains("無法開啟網址").is_none(),
                "{what}：起始畫面顯示錯誤 {:?}",
                s.last_error
            );
            assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}\n目前狀態：{s:#?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    h.step();
    h.event(egui::Event::Paste(page.clone()));
    plays(&mut h, "第一次播");
    h.state_mut().player_mut().stop().unwrap();
    let start = Instant::now();
    while h.state().player().state.loaded {
        assert!(start.elapsed() < TIMEOUT, "停不下來");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    expire_signed(&server, 1);
    let stale = "/signed/1/f/net/video_only.mp4";
    let before = server.requests_to(stale).len();
    h.event(egui::Event::Paste(page.clone()));
    plays(&mut h, "網址失效後再貼一次");
    assert_eq!(fake.calls(), 2);
    // 再貼一次先用了快取的結果（失效的網址試了一次，不是快取剛好沒有），之後才重新問
    assert_eq!(
        server.requests_to(stale).len() - before,
        1,
        "失效的網址先試一次：{:#?}",
        server.requests_to("/signed/1/")
    );
    assert!(!server.requests_to("/signed/2/f/net/video_only.mp4").is_empty());
}

/// 重新問 yt-dlp 只做一次：新的網址也開不起來時顯示原本的錯誤（HTTP 403），不再問。
/// 開不起來的結果不留在快取裡：下一次開又重新問（拿到能用的網址就播得起來）。
/// 重新問的時候 yt-dlp 自己失敗：說明 yt-dlp 的原因
#[test]
fn stale_site_urls_are_resolved_again_only_once() {
    if !net_sample("stale_site_urls_are_resolved_again_only_once", "net/video_only.mp4") {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = signed_site(&server.url(""), &page, "vid1").arc();
    let mut p = kept_site_player(&fake);
    p.open(&page).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 之前的、重新問拿到的都失效
    expire_signed(&server, 1);
    expire_signed(&server, 2);
    let (_, err) = failure_of(&mut p, &page, "403");
    assert_eq!(err, STALE_403, "記錄：{:#?}", p.recent_errors());
    assert_eq!(fake.calls(), 2, "只重新問一次");
    assert!(p.state.net_failure.is_none(), "不是 yt-dlp 的錯誤");
    assert_eq!(p.mpv().hooks_pending(), 0);
    assert!(
        server
            .requests_to("/signed/2/")
            .iter()
            .any(|r| r.path.contains("video_only"))
    );
    // 開不起來的結果丟掉了：再開一次直接重新問（不再試那些網址），這次的網址能用
    let tried = server.requests_to("/signed/2/").len();
    p.open(&page).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(fake.calls(), 3);
    assert!(p.state.last_error.is_none());
    assert_eq!(
        server.requests_to("/signed/2/").len(),
        tried,
        "開不起來的結果不留在快取裡"
    );

    // 重新問的時候 yt-dlp 失敗（影片被刪了之類）：說明 yt-dlp 的原因
    let page = server.url("/watch?v=gone");
    let base = server.url("");
    let (json_page, n) = (page.clone(), std::sync::atomic::AtomicUsize::new(0));
    let flaky = FakeResolver::new(move |_, _| {
        if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(resolved(&site_video_json(&base, &json_page, "gone"), Vec::new()))
        } else {
            Err(YtdlError::Unavailable.into())
        }
    })
    .arc();
    drop(p);
    let mut p = kept_site_player(&flaky);
    p.open(&page).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    for rel in ["net/video_only.mp4", "net/audio_only.m4a", "net/sub.vtt"] {
        server.put(&format!("/f/{rel}"), support::http::Canned::status(403));
    }
    let want = YtdlError::Unavailable.message();
    let (_, err) = failure_of(&mut p, &page, &want);
    assert_eq!(err, want, "記錄：{:#?}", p.recent_errors());
    assert_eq!(flaky.calls(), 2);
    assert_eq!(
        p.state.net_failure.as_ref().map(|f| &f.error),
        Some(&YtdlError::Unavailable)
    );
    assert_eq!(p.mpv().hooks_pending(), 0);
}

/// 20 分鐘內重開同一部網站影片用之前解析的結果，但網站可能已經讓那些網址失效（YouTube 提早撤銷，回應 403）。
/// 用快取的結果開不起來：丟掉那個結果、安靜地重新問 yt-dlp 一次再開（不顯示錯誤）。
/// 舊的網址只試一次（mpv 自己的重試不再去讀它），網站的字幕換成新的（不會有兩條）
#[test]
fn stale_cached_site_urls_are_resolved_again() {
    if !net_sample("stale_cached_site_urls_are_resolved_again", "net/video_only.mp4") {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = support::fake_ytdl::signed_site(&server.url(""), &page, "vid1").arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 1);
    support::fake_ytdl::expire_signed(&server, 1);
    let before = server.requests_to("/signed/1/").len();
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 2, "用快取的結果開不起來：重新問一次");
    assert!(p.state.last_error.is_none() && p.state.net_failure.is_none());
    assert_eq!(p.mpv().hooks_pending(), 0);
    let stale: Vec<_> = server.requests_to("/signed/1/").split_off(before);
    assert!(!stale.is_empty(), "先用了快取的結果（不是快取剛好過期）");
    let videos = stale.iter().filter(|r| r.path.contains("video_only")).count();
    assert_eq!(videos, 1, "失效的網址只試一次：{stale:#?}");
    assert!(!server.requests_to("/signed/2/f/net/video_only.mp4").is_empty());
    let subs: Vec<_> = p.state.tracks_of(TrackKind::Sub).collect();
    assert_eq!(subs.len(), 1, "{:#?}", p.state.tracks);
    assert!(
        subs[0]
            .external_filename
            .as_deref()
            .is_some_and(|f| f.contains("/signed/2/")),
        "字幕用新的網址：{:?}",
        subs[0].external_filename
    );
    p.wait_state(TIMEOUT, |s| s.chapters.len() == 3).unwrap();
    // 新的結果放進快取：再開一次直接用（不再問）
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 2);
    page_never_fetched(&server);
}

/// 換畫質（選單選的格式）用到失效的快取：重新問 yt-dlp，從原本的位置接著播、照樣暫停，選的格式不變
#[test]
fn quality_change_with_stale_cache_keeps_the_position() {
    if !net_sample(
        "quality_change_with_stale_cache_keeps_the_position",
        "net/video_only.mp4",
    ) {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = support::fake_ytdl::signed_site(&server.url(""), &page, "vid1").arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &page);
    p.seek_to(1.5, true).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 1.5).abs() < 0.1).unwrap();
    support::fake_ytdl::expire_signed(&server, 1);
    let stale = "/signed/1/f/net/video_only.mp4";
    let before = server.requests_to(stale).len();
    p.reload_net(Choice::Format {
        video: "v1".into(),
        audio: Some("a1".into()),
    })
    .unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p.wait_state(TIMEOUT, |s| (s.time_pos - 1.5).abs() < 0.25).unwrap();
    assert_eq!(fake.calls(), 2, "快取的格式清單失效了：重新問一次");
    assert_eq!(
        server.requests_to(stale).len() - before,
        1,
        "先用了快取的格式清單（失效的網址試一次）"
    );
    assert_eq!(p.get_string("pause").unwrap(), "yes");
    let info = p.state.net.clone().unwrap();
    assert!(
        info.start_at.is_some_and(|t| (t - 1.5).abs() < 0.1),
        "{:?}",
        info.start_at
    );
    let ids: Vec<_> = info.chosen.iter().filter_map(|f| f.format_id.as_deref()).collect();
    assert_eq!(ids, ["v1", "a1"]);
    assert!(!server.requests_to("/signed/2/f/net/video_only.mp4").is_empty());
    assert_eq!(p.mpv().hooks_pending(), 0);
    page_never_fetched(&server);
}

/// 用快取的結果載入完成、還沒開始播才失敗（mpv 0.37 影像解不了時整個停下；解碼器的記錄比結束晚到）：
/// 網址那時候還能用，不重新問 yt-dlp、快取不丟，照樣只播聲音重開（`audio_only_retry`）。
/// 這裡的引擎不會停下：照 0.37 的順序送結束與記錄（同 tests/decoders.rs 的 `late_video_failure_log_still_retries_with_audio_only`）
#[test]
fn cached_site_video_failing_after_load_reopens_audio_only() {
    if !net_sample(
        "cached_site_video_failing_after_load_reopens_audio_only",
        "net/video_only.mp4",
    ) {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = FakeResolver::json(site_video_json(&server.url(""), &page, "vid1")).arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("vd", "-"), ("pause", "yes")]);
    let version = p.get_string("mpv-version").unwrap();
    if version.starts_with("mpv 0.3") {
        eprintln!("略過：{version} 自己就會停下，再送一次假的會跟真的混在一起");
        return;
    }
    loaded(&mut p, &page);
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 1, "第二次用快取的結果");
    // 真的記錄可能已經收到了：清掉，照「記錄晚到」的順序重送
    p.state.missing_decoders.clear();
    let stopped = Event::EndFile {
        reason: EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    assert_eq!(p.inject_event(stopped), None, "結束先留著");
    let _ = p.inject_event(Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: "Failed to initialize a decoder for codec 'h264'.\n".into(),
    });
    let mut events = Vec::new();
    loop {
        match p.wait(TIMEOUT) {
            Some(PlayerEvent::FileLoaded) => break,
            Some(ev) => events.push(ev),
            None => panic!("只播聲音重開沒有載入：{events:?}"),
        }
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. })),
        "{events:?}"
    );
    assert!(p.reopened_audio_only());
    assert_eq!(p.get_string("vid").unwrap(), "no");
    assert_eq!(fake.calls(), 1, "網址能用：不重新問 yt-dlp");
    assert!(p.state.last_error.is_none() && p.state.net_failure.is_none());
    assert!(p.state.net.is_some());
    // 快取沒有丟掉
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 1);
    assert_eq!(p.mpv().hooks_pending(), 0);
    page_never_fetched(&server);
}

/// 其他網頁（不是已知的影片網站：先照原樣開，認不出內容才問 yt-dlp）用快取的結果開不起來：重開也照同樣的順序
///（先照原樣開，不是直接問 yt-dlp）。重新問的時候 yt-dlp 不認得了：說明照 mpv 原本的（是網頁，不是影片檔）
#[test]
fn stale_cached_fallback_page_reopens_the_same_way() {
    if !net_sample("stale_cached_fallback_page_reopens_the_same_way", "net/video_only.mp4") {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let html = server.url("/html");
    let (base, json_page, n) = (server.url(""), html.clone(), AtomicUsize::new(0));
    let fake = FakeResolver::new(move |_, _| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 {
            let json = site_video_json(&format!("{base}/signed/1"), &json_page, "h1");
            Ok(support::fake_ytdl::resolved(&json, Vec::new()))
        } else {
            Err(YtdlError::Unsupported.into())
        }
    })
    .arc();
    let mut p = site_player(&fake, false, &NetSettings::default(), &[("pause", "yes")]);
    loaded(&mut p, &html);
    assert_eq!(fake.calls(), 1);
    let native = server.requests_to("/html").len();
    assert!(native > 0, "其他網頁先照原樣開");
    // 網站的字幕選到才下載：讀完了、停下來再讓網址失效。不然第一次播的字幕晚一點才讀到 403，
    // 那筆「Failed to open」的記錄晚到、算到下一次開檔，其他網頁就當成連不上、不問 yt-dlp（跟這裡要測的無關）
    assert!(
        server.wait_request("/signed/1/f/net/sub.vtt", TIMEOUT).is_some(),
        "沒有讀網站的字幕"
    );
    p.stop().unwrap();
    p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
        .unwrap();
    support::fake_ytdl::expire_signed(&server, 1);
    let stale = "/signed/1/f/net/video_only.mp4";
    let before = server.requests_to(stale).len();
    let err = failure_of(&mut p, &html, "網頁").1;
    assert_eq!(err, "無法開啟網址：這個網址是網頁，不是影片檔");
    assert_eq!(fake.calls(), 2, "重新問一次");
    assert!(server.requests_to(stale).len() > before, "先用了快取的結果");
    // 第一次開讀了 native 次；再開：先照原樣開、用快取的結果開不起來，重開又先照原樣開
    assert_eq!(
        server.requests_to("/html").len(),
        3 * native,
        "重開也先照原樣開：{:#?}",
        server.requests_to("/html")
    );
    assert_eq!(p.mpv().hooks_pending(), 0);
}

/// 用快取的結果開不起來、正在重新問 yt-dlp（連線中的畫面顯示「正在取得網站影片」）的時候：
/// - 取消：跟一般的取消一樣（結束的原因是 stop、沒有錯誤）
/// - 開別的檔案：新的檔案照常開始（介面收得到開始）、載入
/// - 再開同一個網頁：照一般的開檔（開始照常通知），重新問、播得起來
///
/// 之後才到的 yt-dlp 結果都不理（假的 yt-dlp 不理會取消，等測試放行才回傳）
#[test]
fn retry_waiting_for_ytdl_can_be_cancelled_or_replaced() {
    if !net_sample(
        "retry_waiting_for_ytdl_can_be_cancelled_or_replaced",
        "net/video_only.mp4",
    ) {
        return;
    }
    let _one = lock();
    for (case, what) in ["取消", "開別的檔案", "再開同一個網頁"].into_iter().enumerate() {
        let server = Server::start();
        let page = server.url(&format!("/watch?v=r{case}"));
        let gate = Arc::new(AtomicBool::new(false));
        let (base, json_page, held, n) = (server.url(""), page.clone(), gate.clone(), AtomicUsize::new(0));
        let fake = FakeResolver::new(move |_, _| {
            let i = n.fetch_add(1, Ordering::SeqCst) + 1;
            // 第一次馬上回答；之後的等測試放行（不理會取消）
            while i > 1 && !held.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let json = site_video_json(&format!("{base}/signed/{i}"), &json_page, "r");
            Ok(support::fake_ytdl::resolved(&json, Vec::new()))
        })
        .arc();
        let mut p = site_player(&fake, true, &NetSettings::default(), &[("pause", "yes")]);
        loaded(&mut p, &page);
        support::fake_ytdl::expire_signed(&server, 1);
        p.open(&page).unwrap();
        // 處理事件，直到重新問 yt-dlp（用快取的結果開不起來的結束、重開的開始都不通知）
        let mut events = Vec::new();
        let deadline = Instant::now() + TIMEOUT;
        while fake.calls() < 2 || !p.site_resolving() {
            assert!(Instant::now() < deadline, "{what}：沒有重新問 yt-dlp：{events:?}");
            events.extend(p.wait(Duration::from_millis(50)));
        }
        let starts = |ev: &[PlayerEvent]| ev.iter().filter(|e| **e == PlayerEvent::StartFile).count();
        assert_eq!(starts(&events), 1, "{what}：對介面來說是同一次開檔：{events:?}");
        // 被換掉的第一次播放照常結束（stop）；這次開檔的結束（快取的網址開不起來）不通知
        let opened = events.iter().position(|e| *e == PlayerEvent::StartFile).unwrap();
        assert!(
            !events[opened..]
                .iter()
                .any(|e| matches!(e, PlayerEvent::EndFile { .. })),
            "{what}：{events:?}"
        );
        assert!(
            p.state.net_busy.as_ref().is_some_and(|b| b.url == page),
            "{what}：正在取得網站影片"
        );
        assert_eq!(
            p.net_loading().map(|(u, _)| u.to_owned()),
            Some(page.clone()),
            "{what}：連線中的畫面"
        );
        let local = sample("common/mp4_h264_aac.mp4");
        let until_loaded = |p: &mut Player| {
            let mut events = Vec::new();
            loop {
                match p.wait(TIMEOUT) {
                    Some(PlayerEvent::FileLoaded) => return events,
                    Some(ev) => events.push(ev),
                    None => panic!("{what}：沒有載入：{events:?}"),
                }
            }
        };
        match case {
            0 => {
                p.cancel_loading().unwrap();
                assert!(!p.loading_now() && p.state.net_busy.is_none());
                match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
                    Ok(PlayerEvent::EndFile {
                        reason: EndReason::Stop,
                        error: None,
                    }) => {}
                    other => panic!("取消 = 停止、沒有錯誤：{other:?}"),
                }
            }
            1 => {
                p.open(&local).unwrap();
                assert!(!p.site_resolving() && p.state.net_busy.is_none());
                let events = until_loaded(&mut p);
                assert_eq!(starts(&events), 1, "新的檔案照常開始：{events:?}");
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. })),
                    "{events:?}"
                );
            }
            _ => {
                p.open(&page).unwrap();
                gate.store(true, Ordering::SeqCst);
                let events = until_loaded(&mut p);
                assert!(starts(&events) >= 1, "使用者自己開的照常開始：{events:?}");
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. })),
                    "{events:?}"
                );
                assert_eq!(fake.calls(), 3, "再開一次重新問（快取已經丟掉了）");
                assert!(!server.requests_to("/signed/3/f/net/video_only.mp4").is_empty());
            }
        }
        // 放行假的 yt-dlp：之後才到的結果不理
        gate.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + TIMEOUT;
        while fake.running() > 0 {
            assert!(Instant::now() < deadline, "{what}：假的 yt-dlp 沒有結束");
            p.wait(Duration::from_millis(100));
        }
        let quiet = Instant::now() + Duration::from_millis(500);
        let mut late = Vec::new();
        while Instant::now() < quiet {
            late.extend(p.wait(Duration::from_millis(50)));
        }
        assert!(
            !late.iter().any(|e| matches!(
                e,
                PlayerEvent::StartFile | PlayerEvent::FileLoaded | PlayerEvent::EndFile { .. }
            )),
            "{what}：之後才到的結果不理：{late:?}"
        );
        assert!(p.state.last_error.is_none() && p.state.net_failure.is_none(), "{what}");
        assert_eq!(p.mpv().hooks_pending(), 0, "{what}");
        assert!(
            server.requests_to("/signed/2/").is_empty(),
            "{what}：被取消的結果沒有用到"
        );
        match case {
            0 => assert!(!p.state.loaded),
            1 => {
                let path = p.state.path.clone().map(std::path::PathBuf::from);
                assert_eq!(path, Some(local.clone().into()));
                assert!(p.state.net.is_none());
            }
            _ => {
                assert!(p.state.loaded && p.state.path.as_deref() == Some(page.as_str()));
                assert!(p.state.net.is_some());
            }
        }
        page_never_fetched(&server);
    }
}

/// 重新問 yt-dlp 每次開檔最多一次：重開後拿到的新網址載入了、只播聲音重開（mpv 0.37 影像解不了）時又失效了，
/// 只播聲音重開用的是快取裡的新結果，開不起來就照常說明，不再問（同一次開檔不會一直重開）
#[test]
fn site_retry_happens_once_per_open_even_after_audio_only_reopen() {
    if !net_sample(
        "site_retry_happens_once_per_open_even_after_audio_only_reopen",
        "net/video_only.mp4",
    ) {
        return;
    }
    let _one = lock();
    let server = Server::start();
    let page = server.url("/watch?v=vid1");
    let fake = support::fake_ytdl::signed_site(&server.url(""), &page, "vid1").arc();
    let mut p = site_player(&fake, true, &NetSettings::default(), &[("vd", "-"), ("pause", "yes")]);
    let version = p.get_string("mpv-version").unwrap();
    if version.starts_with("mpv 0.3") {
        eprintln!("略過：{version} 自己就會停下，再送一次假的會跟真的混在一起");
        return;
    }
    loaded(&mut p, &page);
    support::fake_ytdl::expire_signed(&server, 1);
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 2, "快取的網址失效：重新問了一次");
    // 新的網址也失效了，這時才照 0.37 的順序停下（影像解不了、記錄晚到）：只播聲音重開
    support::fake_ytdl::expire_signed(&server, 2);
    p.state.missing_decoders.clear();
    let stopped = Event::EndFile {
        reason: EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    assert_eq!(p.inject_event(stopped), None, "結束先留著");
    let _ = p.inject_event(Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: "Failed to initialize a decoder for codec 'h264'.\n".into(),
    });
    let ended = p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. }));
    assert!(ended.is_ok(), "只播聲音重開也開不起來：照常說明（{ended:?}）");
    assert!(p.reopened_audio_only());
    assert_eq!(fake.calls(), 2, "同一次開檔不再問 yt-dlp");
    assert!(p.state.last_error.is_some());
    assert_eq!(p.mpv().hooks_pending(), 0);
    // 開不起來的結果丟掉了：下一次開檔重新問（這次的網址能用）
    loaded(&mut p, &page);
    assert_eq!(fake.calls(), 3);
    page_never_fetched(&server);
}

/// 換畫質時之前解析的網址已經失效（網站提早撤銷，回應 403）：安靜地重新問 yt-dlp 一次，跟一般的換畫質一樣
/// 從現在的位置接著播，暫停、字幕延遲、A-B 重播、選的字幕都保留，視窗不調整，畫面上沒有錯誤
#[test]
fn site_quality_switch_with_expired_urls_resolves_again() {
    if !net_sample("site_quality_switch_with_expired_urls_resolves_again", "net/sub.vtt") {
        return;
    }
    let _one = lock();
    let server = Server::start();
    // 每次解析都給新的、有簽名的網址（`/signed/<第幾次>/f/…`）
    let base = server.url("");
    let n = std::sync::atomic::AtomicUsize::new(0);
    let fake = FakeResolver::new(move |_, _| {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let json = two_quality_json(&format!("{base}/signed/{i}"));
        Ok(support::fake_ytdl::resolved(&json, Vec::new()))
    })
    .arc();
    let mut h = site_harness(&fake);
    h.step();
    let page = server.url("/watch?v=two");
    h.event(egui::Event::Paste(page.clone()));
    step_until_net(&mut h, "播網站影片", |app| {
        let s = &app.player().state;
        playing_url(s, &page) && s.net.is_some() && s.seekable && s.video_size.is_some()
    });
    let ja = h
        .state()
        .player()
        .state
        .tracks_of(TrackKind::Sub)
        .find(|t| t.lang.as_deref() == Some("ja"))
        .map(|t| t.id)
        .expect("網站的日文字幕");
    h.state_mut()
        .player_mut()
        .select_track(TrackKind::Sub, Some(ja))
        .unwrap();
    h.state_mut().player_mut().set_sub_delay(0.5).unwrap();
    h.state_mut().player_mut().set_ab_loop(Some(5.0), Some(15.0)).unwrap();
    h.key_press(egui::Key::Space);
    step_until_net(&mut h, "暫停", |app| app.player().state.paused);
    paused_at(&mut h, 10.0);
    // 第一次解析給的網址都失效了
    for path in ["/signed/1/f/common/mp4_long.mp4", "/signed/1/f/net/sub.vtt"] {
        server.put(path, support::http::Canned::status(403));
    }

    pick_site_quality(&mut h, "360p · AVC");
    let mut seen = Seen::default();
    seen.record(&h);
    seen.sizes.clear();
    h.step();
    step_until_seen(&mut h, &mut seen, "換成 360p", |app| playing_format(app, &page, "lo"));
    step_until_seen(&mut h, &mut seen, "還原日文字幕", |app| {
        app.player()
            .state
            .selected(TrackKind::Sub)
            .and_then(|t| t.lang.as_deref())
            == Some("ja")
    });
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) {
        h.step();
        seen.record(&h);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls(), 2, "之前的網址失效了：重新問一次");
    assert!(
        fetched(&server, "/signed/1/f/common/mp4_long.mp4?q=lo"),
        "先用了之前的結果"
    );
    assert!(fetched(&server, "/signed/2/f/common/mp4_long.mp4?q=lo"));
    let p = h.state().player();
    assert!(p.state.last_error.is_none(), "{:?}", p.state.last_error);
    assert!(h.query_by_label_contains("無法開啟網址").is_none());
    assert_eq!(p.get_string("pause").unwrap(), "yes", "暫停中換畫質：還是暫停");
    let t = p.get_f64("time-pos").unwrap();
    assert!((t - 10.0).abs() < 1.0, "接著 10 秒播：{t}");
    assert_eq!(p.get_f64("sub-delay").unwrap(), 0.5);
    assert_eq!(p.ab_loop_points(), [Some(5.0), Some(15.0)]);
    assert!(seen.sizes.is_empty(), "換畫質不調整視窗大小：{:?}", seen.sizes);
    assert_eq!(seen.osd, ["畫質：360p · AVC"]);
    assert_eq!(h.state().playlist().map(|l| l.len()), Some(1), "播放清單不變");
    page_never_fetched(&server);
}
