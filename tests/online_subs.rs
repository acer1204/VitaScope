//! 線上字幕的真實連線（`UreqHttp`）：連到本機的測試伺服器（只聽 127.0.0.1），不碰外面的網路（R-B8）。
//!
//! 最後兩個是連到真的 OpenSubtitles / 射手網（偽）的測試（`#[ignore]`）：要 `VITASCOPE_ONLINE_TESTS=1`，
//! OpenSubtitles 另外要開發版能用的金鑰（`VITASCOPE_OPENSUBTITLES_KEY`），射手網（偽）要 `VITASCOPE_ASSRT_TOKEN`：
//! `VITASCOPE_ONLINE_TESTS=1 cargo test --test online_subs -- --ignored --nocapture`

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::http::{Canned, Server};
use vitascope::online_subs::http::{
    API_TIMEOUT, CONNECT_TIMEOUT, DOWNLOAD_TIMEOUT, Http, Kind, MAX_API_REDIRECTS, Method, Request, Transport,
    USER_AGENT, UreqHttp, UreqOptions,
};
use vitascope::online_subs::{
    Account, DownloadJob, Job, JobResult, OnlineEnv, OnlineSubSettings, Provider, SearchJob, VideoRef, run,
};

/// 連本機的測試伺服器：http、不用 proxy
fn local() -> UreqHttp {
    UreqHttp::with_options(UreqOptions {
        https_only: false,
        proxy_from_env: false,
        ..Default::default()
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vitascope-online-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn get(url: String, kind: Kind) -> Request {
    Request::new(Method::Get, url, kind, 1 << 20)
}

#[test]
fn error_status_keeps_its_body() {
    let server = Server::start();
    let body = r#"{"message":"You have downloaded your allowed 5 subtitles","remaining":0}"#;
    server.put(
        "/api/v1/download",
        Canned {
            status: 406,
            ..Canned::ok(body)
        },
    );
    let req = Request::new(Method::Post, server.url("/api/v1/download"), Kind::Api, 1 << 20)
        .json(r#"{"file_id":22}"#.into())
        .header("Api-Key", "k");
    let reply = local().send(&req).unwrap();
    assert_eq!(reply.status, 406, "4xx 不是錯誤：原因在內容裡");
    assert_eq!(reply.text(), body);
    let got = server.requests_to("/api/v1/download");
    assert_eq!(got[0].method, "POST");
    assert_eq!(got[0].body, br#"{"file_id":22}"#);
    assert_eq!(got[0].header("Content-Type"), Some("application/json"));
    assert_eq!(got[0].header("Api-Key"), Some("k"));
    // 5xx 也一樣
    let reply = local().send(&get(server.url("/status/503"), Kind::Api)).unwrap();
    assert_eq!((reply.status, reply.text().as_str()), (503, "status 503\n"));
}

#[test]
fn size_limit_gives_too_large() {
    let server = Server::start();
    server.put("/big", Canned::ok(vec![b'x'; 5000]));
    server.put("/big-nolen", Canned::ok(vec![b'x'; 5000]).without_length());
    for path in ["/big", "/big-nolen"] {
        let mut req = get(server.url(path), Kind::Download);
        req.limit = 1000;
        assert_eq!(local().send(&req), Err(Transport::TooLarge), "{path}");
        req.limit = 5000;
        assert_eq!(local().send(&req).unwrap().body.len(), 5000, "{path}");
    }
}

#[test]
fn redirects_keep_authorization_only_on_the_same_host_for_the_api() {
    let server = Server::start();
    server.put("/same", Canned::redirect(&server.url("/target-same")));
    server.put("/target-same", Canned::ok("same"));
    // 換主機：127.0.0.1 → localhost（一樣是本機）
    let other_host = server.url("/target-other").replacen("127.0.0.1", "localhost", 1);
    server.put("/other", Canned::redirect(&other_host));
    server.put("/target-other", Canned::ok("other"));
    server.put("/dl", Canned::redirect(&server.url("/target-dl")));
    server.put("/target-dl", Canned::ok("dl"));
    let http = local();
    let with_auth = |path: &str, kind: Kind| {
        let req = get(server.url(path), kind).header("Authorization", "Bearer jwt");
        http.send(&req).unwrap().text()
    };
    assert_eq!(with_auth("/same", Kind::Api), "same");
    assert_eq!(with_auth("/other", Kind::Api), "other");
    assert_eq!(with_auth("/dl", Kind::Download), "dl");
    let auth = |path: &str| server.requests_to(path)[0].header("Authorization").map(str::to_owned);
    assert_eq!(
        auth("/target-same").as_deref(),
        Some("Bearer jwt"),
        "API：同一台主機保留登入憑證"
    );
    assert_eq!(auth("/target-other"), None, "換主機：拿掉登入憑證");
    assert_eq!(auth("/target-dl"), None, "下載連結：轉址一律拿掉");
}

#[test]
fn the_user_agent_is_sent() {
    let server = Server::start();
    server.put("/ua", Canned::ok("ok"));
    for kind in [Kind::Api, Kind::Download] {
        local().send(&get(server.url("/ua"), kind)).unwrap();
    }
    for r in server.requests_to("/ua") {
        assert_eq!(r.header("User-Agent"), Some(USER_AGENT));
    }
    assert_eq!(USER_AGENT, concat!("VitaScope v", env!("CARGO_PKG_VERSION")));
}

#[test]
fn the_real_client_refuses_plain_http() {
    // 正式的設定只能 https：http 的網址在連線之前就拒絕（射手網（偽）不退回 http：R-C5）
    let server = Server::start();
    server.put("/plain", Canned::ok("x"));
    let real = UreqHttp::new();
    for kind in [Kind::Api, Kind::Download] {
        assert_eq!(real.send(&get(server.url("/plain"), kind)), Err(Transport::NotHttps));
    }
    assert!(server.requests_to("/plain").is_empty());
}

#[test]
fn production_limits_follow_the_design() {
    // R-B7：連線 10 秒、API 20 秒、下載字幕檔 30 秒；API 的轉址只跟幾次；正式的設定只能 https、proxy 照環境變數
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(API_TIMEOUT, Duration::from_secs(20));
    assert_eq!(DOWNLOAD_TIMEOUT, Duration::from_secs(30));
    assert!((1..=3).contains(&MAX_API_REDIRECTS), "{MAX_API_REDIRECTS}");
    let o = UreqOptions::default();
    assert_eq!((o.api_timeout, o.download_timeout), (API_TIMEOUT, DOWNLOAD_TIMEOUT));
    assert!(o.https_only && o.proxy_from_env);
}

#[test]
fn api_redirects_are_capped() {
    let server = Server::start();
    // /r0 → /r1 → /r2 → /r3 → /end：3 次可以，4 次就失敗（設計：API 只跟少數幾次轉址；長度固定，常數改大時這裡會失敗）
    let hops = 4;
    for i in 0..hops {
        let next = if i + 1 == hops {
            "/end".to_owned()
        } else {
            format!("/r{}", i + 1)
        };
        server.put(&format!("/r{i}"), Canned::redirect(&server.url(&next)));
    }
    server.put("/end", Canned::ok("end"));
    let http = local();
    assert_eq!(http.send(&get(server.url("/r1"), Kind::Api)).unwrap().text(), "end");
    let too_many = http.send(&get(server.url("/r0"), Kind::Api));
    assert!(matches!(too_many, Err(Transport::Connect(_))), "{too_many:?}");
}

#[test]
fn timeouts_and_refused_connections() {
    let server = Server::start();
    let quick = UreqHttp::with_options(UreqOptions {
        https_only: false,
        proxy_from_env: false,
        api_timeout: Duration::from_secs(1),
        download_timeout: Duration::from_secs(1),
    });
    let start = Instant::now();
    assert_eq!(
        quick.send(&get(server.url("/slow"), Kind::Api)),
        Err(Transport::Timeout)
    );
    assert!(start.elapsed() < Duration::from_secs(60));
    // 兩種連線各用自己的時間限制（只比下限：慢的機器只會更久）
    for (api, download, kind) in [(1, 3, Kind::Download), (3, 1, Kind::Api)] {
        let http = UreqHttp::with_options(UreqOptions {
            https_only: false,
            proxy_from_env: false,
            api_timeout: Duration::from_secs(api),
            download_timeout: Duration::from_secs(download),
        });
        let start = Instant::now();
        assert_eq!(
            http.send(&get(server.url("/slow"), kind)),
            Err(Transport::Timeout),
            "{kind:?}"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(2500),
            "{kind:?} 要用自己的時間限制：{:?}",
            start.elapsed()
        );
    }
    // 沒有人在聽的埠
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let refused = local().send(&get(format!("http://127.0.0.1:{port}/x"), Kind::Api));
    assert!(matches!(refused, Err(Transport::Connect(_))), "{refused:?}");
}

/// 整個流程（真的連線 → 本機的假 OpenSubtitles）：登入、搜尋（照規定的參數、特徵碼、登入憑證）、下載、存到影片旁邊
#[test]
fn opensubtitles_flow_over_real_http() {
    let server = Server::start();
    let dir = scratch("flow");
    let video_path = dir.join("Movie.Name.2019.1080p.mkv");
    std::fs::write(
        &video_path,
        (0..204_800u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    server.put(
        "/api/v1/login",
        Canned::ok(r#"{"user":{"allowed_downloads":20,"vip":false},"base_url":"vip-api.opensubtitles.com","token":"jwt-x","status":200}"#),
    );
    server.put(
        "/api/v1/subtitles",
        Canned::ok(
            r#"{"total_pages":1,"total_count":1,"page":1,"data":[{"attributes":{"language":"zh-tw","release":"r",
                "moviehash_match":true,"files":[{"file_id":7,"file_name":"x.srt"}]}}]}"#,
        ),
    );
    let link = server.url("/files/x.srt");
    server.put(
        "/api/v1/download",
        Canned::ok(format!(
            r#"{{"link":"{link}","remaining":19,"reset_time_utc":"2030-01-01T00:00:00.000Z"}}"#
        )),
    );
    let srt = "1\n00:00:01,000 --> 00:00:02,000\n這是我們的時間，說話會讓你覺得還沒來過這裡\n";
    server.put("/files/x.srt", Canned::ok(srt));

    let mut env = OnlineEnv::testing(Arc::new(local()), Some(dir.join("cache")));
    env.os_root = server.url("/api/v1");
    env.session_file = Some(dir.join("session.json"));
    let account = Account::new(env.session_file.clone());
    let JobResult::Login(info) = run(
        Job::Login {
            user: "me".into(),
            password: "secret".into(),
        },
        &env,
        &account,
    ) else {
        panic!()
    };
    assert_eq!(info.unwrap().user, "me");
    let settings = OnlineSubSettings {
        languages: vec!["zh-tw".into(), "en".into()],
        ..Default::default()
    };
    let video = VideoRef::new(video_path.to_string_lossy().into_owned());
    let job = SearchJob::new(
        Provider::Opensubtitles,
        video.clone(),
        "Movie Name 2019",
        &settings,
        vitascope::i18n::Lang::ZhTw,
    );
    let JobResult::Search { result, video, .. } = run(Job::Search(job), &env, &account) else {
        panic!()
    };
    let page = result.unwrap();
    assert_eq!(page.matched(), 1);
    let search = &server.requests_to("/api/v1/subtitles")[0];
    // 登入回應的主機只用在正式的位址：測試的位址照舊（不會連到真的網站）
    assert_eq!(
        search.path,
        "/api/v1/subtitles?languages=en,zh-tw&moviehash=a65c16d28845ff55&query=movie+name+2019"
    );
    assert_eq!(search.header("Authorization"), Some("Bearer jwt-x"));
    assert_eq!(search.header("Api-Key"), Some("test-key"));
    let job = DownloadJob::for_result(&page.results[0], video, true).unwrap();
    let JobResult::Download {
        link: used,
        quota,
        result,
        ..
    } = run(Job::Download(job), &env, &account)
    else {
        panic!()
    };
    let saved = result.unwrap();
    assert_eq!(saved.path, dir.join("Movie.Name.2019.1080p.zh-TW.srt"));
    assert_eq!(std::fs::read_to_string(&saved.path).unwrap(), srt);
    assert_eq!(used.as_deref(), Some(link.as_str()));
    assert_eq!(quota.map(|q| q.remaining), Some(19));
    let file_get = &server.requests_to("/files/x.srt")[0];
    assert_eq!(file_get.header("Authorization"), None, "暫時的連結不帶登入憑證");
    assert_eq!(file_get.header("Api-Key"), None);
    assert_eq!(server.requests_to("/api/v1/download")[0].body, br#"{"file_id":7}"#);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// SR0：連到真的 OpenSubtitles（主人的金鑰；預設不跑）。回答設計上還不確定的四件事，結果記在 ROADMAP §7：
/// 1. 未登入能不能下載（會用掉這個 IP 一天 5 次中的 1 次）；
/// 2. 次數用完（406）的回應長什麼樣子：**另外要 `VITASCOPE_SR0_EXHAUST=1`**，會一直未登入下載同一個檔案
///    直到不是 200（最多 7 次，用完這個 IP 今天的次數）；沒設定時只印「沒有看到 406」；
/// 3. 回應有哪些 `ratelimit-*` 標頭；
/// 4. 參數沒照規定排好時會不會轉址。
#[test]
#[ignore = "連到真的 OpenSubtitles：VITASCOPE_ONLINE_TESTS=1，開發版要 VITASCOPE_OPENSUBTITLES_KEY"]
fn live_opensubtitles_spike() {
    use vitascope::online_subs::opensubtitles;
    if !opensubtitles::online_tests_enabled() {
        println!("略過：沒有設定 VITASCOPE_ONLINE_TESTS=1");
        return;
    }
    let Some(key) = opensubtitles::api_key() else {
        println!("略過：這個版本沒有 OpenSubtitles 的金鑰（VITASCOPE_OPENSUBTITLES_KEY）");
        return;
    };
    let dir = scratch("live");
    let mut env = OnlineEnv::real(&dir);
    env.session_file = None;
    let account = Account::new(None);
    let settings = OnlineSubSettings {
        languages: vec!["en".into()],
        ..Default::default()
    };
    let job = SearchJob::new(
        Provider::Opensubtitles,
        VideoRef::new("https://example.com/big_buck_bunny.mp4"),
        "big buck bunny",
        &settings,
        vitascope::i18n::Lang::En,
    );
    let JobResult::Search { result, .. } = run(Job::Search(job), &env, &account) else {
        panic!()
    };
    let page = result.expect("搜尋失敗");
    println!("搜尋：{} 個結果、{} 頁", page.results.len(), page.total_pages);
    assert!(!page.results.is_empty());

    // 照規定排好的參數：看回應的標頭（ratelimit-*）
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into();
    let call = |url: &str| agent.get(url).header("Api-Key", key.as_str()).call().expect("連線失敗");
    let canonical = call("https://api.opensubtitles.com/api/v1/subtitles?languages=en&query=big+buck+bunny");
    println!("照規定的參數：HTTP {}", canonical.status());
    for (name, value) in canonical.headers() {
        let name = name.as_str().to_ascii_lowercase();
        if name.contains("ratelimit") {
            println!("  {name}: {}", value.to_str().unwrap_or_default());
        }
    }
    // 沒排好、有大寫、空白是 %20：會不會轉址
    let messy = call("https://api.opensubtitles.com/api/v1/subtitles?query=Big%20Buck%20Bunny&languages=en");
    println!(
        "沒排好的參數：HTTP {}，轉到 {:?}",
        messy.status(),
        messy.headers().get("location").and_then(|v| v.to_str().ok())
    );

    // 未登入能不能下載（用掉 1 次）
    let first = page.results.iter().find_map(|r| match r.id {
        vitascope::online_subs::ResultId::Os { file_id } => Some(file_id),
        _ => None,
    });
    if let Some(file_id) = first {
        let req = Request::new(
            Method::Post,
            "https://api.opensubtitles.com/api/v1/download",
            Kind::Api,
            1 << 20,
        )
        .json(format!(r#"{{"file_id":{file_id}}}"#))
        .header("Api-Key", key.as_str())
        .header("Accept", "*/*");
        // 第 2 題（406）要用完今天的次數：主人另外同意才做
        let exhaust = std::env::var("VITASCOPE_SR0_EXHAUST").is_ok_and(|v| v.trim() == "1");
        let tries = if exhaust { 7 } else { 1 };
        let mut saw_406 = false;
        for n in 1..=tries {
            std::thread::sleep(Duration::from_millis(1100));
            match env.http.send(&req) {
                Ok(r) => {
                    // 連結本身不印（暫時的下載網址）
                    let body = r.text();
                    let shown: String = if r.status == 200 {
                        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                        format!(
                            "remaining={:?} reset_time_utc={:?} message={:?}",
                            v.get("remaining"),
                            v.get("reset_time_utc"),
                            v.get("message")
                        )
                    } else {
                        body.chars().take(600).collect()
                    };
                    println!("未登入的 /download 第 {n} 次：HTTP {} {shown}", r.status);
                    if r.status != 200 {
                        saw_406 |= r.status == 406;
                        for (name, value) in &r.headers {
                            if name.contains("ratelimit") || name.contains("retry") || name.starts_with("x-") {
                                println!("  {name}: {value}");
                            }
                        }
                        break;
                    }
                }
                Err(e) => {
                    println!("未登入的 /download 第 {n} 次：{e:?}");
                    break;
                }
            }
        }
        if !saw_406 {
            println!(
                "沒有看到 406（次數還沒用完）：406 的內容要另外設 VITASCOPE_SR0_EXHAUST=1 再跑一次（會用完這個 IP 今天的次數）"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 連到真的射手網（偽）（預設不跑）：`VITASCOPE_ONLINE_TESTS=1`、`VITASCOPE_ASSRT_TOKEN`
#[test]
#[ignore = "連到真的射手網（偽）：VITASCOPE_ONLINE_TESTS=1、VITASCOPE_ASSRT_TOKEN"]
fn live_assrt_search() {
    if !vitascope::online_subs::opensubtitles::online_tests_enabled() {
        println!("略過：沒有設定 VITASCOPE_ONLINE_TESTS=1");
        return;
    }
    let Ok(token) = std::env::var("VITASCOPE_ASSRT_TOKEN") else {
        println!("略過：沒有 VITASCOPE_ASSRT_TOKEN");
        return;
    };
    let env = OnlineEnv::real(&scratch("live-assrt"));
    let settings = OnlineSubSettings {
        assrt_token: token,
        ..Default::default()
    };
    let job = SearchJob::new(
        Provider::Assrt,
        VideoRef::new("https://example.com/a.mp4"),
        "big buck bunny",
        &settings,
        vitascope::i18n::Lang::ZhTw,
    );
    let JobResult::Search { result, .. } = run(Job::Search(job), &env, &Account::new(None)) else {
        panic!()
    };
    let page = result.expect("搜尋失敗");
    println!("射手網（偽）：{} 個結果", page.results.len());
    for r in page.results.iter().take(5) {
        println!("  {:?} {} [{}] {}", r.lang, r.lang_code, r.format, r.name);
    }
}
