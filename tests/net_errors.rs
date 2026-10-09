//! 開網址失敗時的說明（HTTP 404、找不到伺服器、逾時、網頁不是影片）。只連本機的測試伺服器 127.0.0.1。
//!
//! 這些原因大多只出現在 FFmpeg 的記錄裡，而 FFmpeg 的記錄只送到「行程裡第一個建立、還在的 mpv」
//! （mpv 的 common/av_log.c）。其他測試程式平行建立很多播放器，誰是第一個不一定；所以這些測試放在自己的測試程式裡，
//! 每個測試從建立播放器到釋放都拿著同一把鎖：同一時間只有一個播放器，它一定是第一個。

mod support;

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use support::http::Server;
use vitascope::net::{self, NetSettings};
use vitascope::player::{Options, Player, PlayerEvent};

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
