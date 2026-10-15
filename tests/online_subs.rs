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
    API_TIMEOUT, CONNECT_TIMEOUT, DOWNLOAD_TIMEOUT, Http, Kind, MAX_API_REDIRECTS, MAX_DOWNLOAD_REDIRECTS, Method,
    Request, Transport, USER_AGENT, UreqHttp, UreqOptions,
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

/// 轉址到別的來源（這裡是同一台電腦的另一個埠）：影戲的 API 金鑰（`Api-Key`）與登入憑證（`Authorization`）
/// 一律不跟著送過去（ureq 自己跟轉址時只拿掉 `Authorization`，`Api-Key` 會送到別的主機）；之後再轉回原本的主機也不加回來。
/// 同一個來源的轉址照舊帶著（vip-api 一定要 JWT）；POST 經過 302 變成不帶內容的 GET，307 照原本的方法與內容
#[test]
fn secrets_never_follow_a_redirect_to_another_origin() {
    let a = Server::start();
    let b = Server::start();
    assert_ne!(a.url(""), b.url(""), "兩個伺服器要在不同的埠");
    a.put("/api/v1/subtitles", Canned::redirect(&b.url("/landing")));
    b.put("/landing", Canned::ok("landed"));
    a.put("/api/v1/download", Canned::redirect(&b.url("/dl")));
    b.put("/dl", Canned::ok("dl"));
    // A → B → A：回到原本的主機也不再帶
    a.put("/bounce", Canned::redirect(&b.url("/back")));
    b.put("/back", Canned::redirect(&a.url("/final")));
    a.put("/final", Canned::ok("final"));
    // 同一個來源：照舊帶著；307 照原本的方法與內容
    a.put("/same", Canned::redirect(&a.url("/same-target")));
    a.put("/same-target", Canned::ok("same"));
    let temporary = |to: &str| Canned {
        status: 307,
        headers: vec![("Location".into(), to.into())],
        ..Canned::ok("")
    };
    a.put("/post307", temporary(&a.url("/post307-target")));
    a.put("/post307-target", Canned::ok("posted"));
    // 下載連結：同一個來源的轉址也不帶
    a.put("/files/x", Canned::redirect(&a.url("/files/y")));
    a.put("/files/y", Canned::ok("file"));
    let http = local();
    let secret = |r: Request| {
        r.header("Api-Key", "the-app-key")
            .header("Authorization", "Bearer jwt")
            .header("Accept", "*/*")
    };
    let text = |r: Request| http.send(&r).unwrap().text();
    assert_eq!(
        text(secret(get(a.url("/api/v1/subtitles?query=x"), Kind::Api))),
        "landed"
    );
    let post =
        |url: String| secret(Request::new(Method::Post, url, Kind::Api, 1 << 20)).json(r#"{"file_id":7}"#.into());
    assert_eq!(text(post(a.url("/api/v1/download"))), "dl");
    assert_eq!(text(secret(get(a.url("/bounce"), Kind::Api))), "final");
    assert_eq!(text(secret(get(a.url("/same"), Kind::Api))), "same");
    assert_eq!(text(post(a.url("/post307"))), "posted");
    assert_eq!(text(secret(get(a.url("/files/x"), Kind::Download))), "file");

    let only = |s: &Server, path: &str| {
        let got = s.requests_to(path);
        assert_eq!(got.len(), 1, "{path}：{got:?}");
        got.into_iter().next().unwrap()
    };
    let no_secrets = |r: &support::http::Request, what: &str| {
        assert_eq!(r.header("Api-Key"), None, "{what}：金鑰送到別的地方了");
        assert_eq!(r.header("Authorization"), None, "{what}：登入憑證送到別的地方了");
        assert_eq!(r.header("Accept"), Some("*/*"), "{what}：不是秘密的標頭照舊");
        assert_eq!(r.header("User-Agent"), Some(USER_AGENT), "{what}");
    };
    no_secrets(&only(&b, "/landing"), "API 轉到別的埠");
    let dl = only(&b, "/dl");
    no_secrets(&dl, "POST 轉到別的埠");
    assert_eq!(dl.method, "GET", "302 的 POST 變成 GET");
    assert!(dl.body.is_empty() && dl.header("Content-Type").is_none(), "{dl:?}");
    no_secrets(&only(&b, "/back"), "A → B");
    no_secrets(&only(&a, "/final"), "A → B → A：回到原本的主機也不加回來");
    let files_y = only(&a, "/files/y");
    no_secrets(&files_y, "下載連結的轉址");
    // 同一個來源的 API 轉址：照舊帶著
    let same = only(&a, "/same-target");
    assert_eq!(same.header("Api-Key"), Some("the-app-key"));
    assert_eq!(same.header("Authorization"), Some("Bearer jwt"));
    let p307 = only(&a, "/post307-target");
    assert_eq!(
        (p307.method.as_str(), p307.body.as_slice()),
        ("POST", br#"{"file_id":7}"#.as_slice())
    );
    assert_eq!(p307.header("Api-Key"), Some("the-app-key"));
    assert_eq!(p307.header("Content-Type"), Some("application/json"));
}

/// 307 / 308 會照原本的方法與內容再送一次：有內容的請求（登入的帳號密碼）、DELETE 只在同一個來源裡跟，
/// 要轉到別的來源時不跟，把 3xx 交回去（別的來源什麼都收不到）。303 與 301 / 302 的 POST、DELETE 變成不帶內容的 GET
#[test]
fn bodies_never_follow_a_redirect_to_another_origin() {
    let a = Server::start();
    let b = Server::start();
    let redirect = |status: u16, to: &str| Canned {
        status,
        headers: vec![("Location".into(), to.into())],
        ..Canned::ok("")
    };
    a.put("/login307", redirect(307, &b.url("/stolen")));
    a.put("/login308", redirect(308, &b.url("/stolen")));
    a.put("/logout307", redirect(307, &b.url("/stolen")));
    b.put("/stolen", Canned::ok("stolen"));
    // 下載連結：同一個來源的 307 POST 也不跟（下載連結的轉址一律當成離開原本的地方）
    a.put("/files/post", redirect(307, &a.url("/files/post-target")));
    a.put("/files/post-target", Canned::ok("file"));
    // 同一個來源：303 的 POST、302 的 DELETE 變成不帶內容的 GET，秘密的標頭照舊
    a.put("/see", redirect(303, &a.url("/seen")));
    a.put("/seen", Canned::ok("seen"));
    a.put("/del302", redirect(302, &a.url("/deleted")));
    a.put("/deleted", Canned::ok("deleted"));
    let http = local();
    let login = |url: String, kind: Kind| {
        Request::new(Method::Post, url, kind, 1 << 20)
            .header("Api-Key", "the-app-key")
            .json(r#"{"username":"me","password":"secret"}"#.into())
    };
    for (path, status) in [("/login307", 307), ("/login308", 308)] {
        let reply = http.send(&login(a.url(path), Kind::Api)).unwrap();
        assert_eq!(reply.status, status, "{path}：3xx 交回去");
    }
    let logout =
        Request::new(Method::Delete, a.url("/logout307"), Kind::Api, 1 << 20).header("Authorization", "Bearer jwt");
    assert_eq!(http.send(&logout).unwrap().status, 307);
    assert_eq!(
        http.send(&login(a.url("/files/post"), Kind::Download)).unwrap().status,
        307
    );
    assert!(b.requests().is_empty(), "別的來源收到了：{:?}", b.requests());
    assert!(a.requests_to("/files/post-target").is_empty());

    assert_eq!(http.send(&login(a.url("/see"), Kind::Api)).unwrap().text(), "seen");
    let del = Request::new(Method::Delete, a.url("/del302"), Kind::Api, 1 << 20).header("Api-Key", "the-app-key");
    assert_eq!(http.send(&del).unwrap().text(), "deleted");
    for path in ["/seen", "/deleted"] {
        let got = a.requests_to(path);
        assert_eq!(got.len(), 1, "{path}：{got:?}");
        let r = &got[0];
        assert_eq!(r.method, "GET", "{path}");
        assert!(r.body.is_empty() && r.header("Content-Type").is_none(), "{path}：{r:?}");
        assert_eq!(r.header("Api-Key"), Some("the-app-key"), "{path}：同一個來源照舊帶著");
    }
}

/// 轉址的每一站共用一個時間限制（不是每一站重新算）：兩站各慢 1.2 秒，限制 2 秒就逾時
#[test]
fn a_redirect_chain_shares_one_deadline() {
    let server = Server::start();
    let slow = Duration::from_millis(1200);
    server.put("/d1", Canned::redirect(&server.url("/d2")).after(slow));
    server.put("/d2", Canned::redirect(&server.url("/d3")).after(slow));
    server.put("/d3", Canned::ok("end"));
    for kind in [Kind::Api, Kind::Download] {
        let http = UreqHttp::with_options(UreqOptions {
            https_only: false,
            proxy_from_env: false,
            api_timeout: Duration::from_secs(2),
            download_timeout: Duration::from_secs(2),
        });
        assert_eq!(
            http.send(&get(server.url("/d1"), kind)),
            Err(Transport::Timeout),
            "{kind:?}"
        );
    }
    // 一站很慢：下一站只剩下的時間（不是重新給一整段）。3 秒 + 4 秒 = 7 秒才逾時就是每一站重新算（只比上限、留 2 秒餘裕）
    server.put(
        "/e1",
        Canned::redirect(&server.url("/slow")).after(Duration::from_secs(3)),
    );
    let http = UreqHttp::with_options(UreqOptions {
        https_only: false,
        proxy_from_env: false,
        api_timeout: Duration::from_secs(4),
        download_timeout: Duration::from_secs(4),
    });
    let start = Instant::now();
    assert_eq!(http.send(&get(server.url("/e1"), Kind::Api)), Err(Transport::Timeout));
    assert!(start.elapsed() < Duration::from_secs(6), "{:?}", start.elapsed());
    // 時間夠就跟到底
    let roomy = UreqHttp::with_options(UreqOptions {
        https_only: false,
        proxy_from_env: false,
        api_timeout: Duration::from_secs(60),
        download_timeout: Duration::from_secs(60),
    });
    assert_eq!(roomy.send(&get(server.url("/d1"), Kind::Api)).unwrap().text(), "end");
}

/// 下載連結最多轉址 [`MAX_DOWNLOAD_REDIRECTS`] 次（比 API 多）
#[test]
fn download_redirects_are_capped() {
    let server = Server::start();
    let hops = MAX_DOWNLOAD_REDIRECTS + 1;
    for i in 0..hops {
        let next = if i + 1 == hops {
            "/end".to_owned()
        } else {
            format!("/r{}", i + 1)
        };
        server.put(&format!("/r{i}"), Canned::redirect(&server.url(&next)));
    }
    server.put("/end", Canned::ok("end"));
    const { assert!(MAX_DOWNLOAD_REDIRECTS > MAX_API_REDIRECTS) };
    let http = local();
    assert_eq!(
        http.send(&get(server.url("/r1"), Kind::Download)).unwrap().text(),
        "end"
    );
    let too_many = http.send(&get(server.url("/r0"), Kind::Download));
    assert!(matches!(too_many, Err(Transport::Connect(_))), "{too_many:?}");
    // 同一串轉址，API 跟不完
    let api = http.send(&get(server.url("/r1"), Kind::Api));
    assert!(matches!(api, Err(Transport::Connect(_))), "{api:?}");
}

/// 整個流程：OpenSubtitles 的搜尋被轉到別的主機（例如被竄改的回應、公共網路的登入頁）時，金鑰與登入憑證不會送過去
#[test]
fn opensubtitles_search_redirected_elsewhere_keeps_the_key_home() {
    let api = Server::start();
    let elsewhere = Server::start();
    let dir = scratch("redirect-key");
    api.put(
        "/api/v1/login",
        Canned::ok(r#"{"user":{"allowed_downloads":20},"base_url":"api.opensubtitles.com","token":"jwt-y"}"#),
    );
    api.put("/api/v1/subtitles", Canned::redirect(&elsewhere.url("/subtitles")));
    elsewhere.put(
        "/subtitles",
        Canned::ok(r#"{"total_pages":1,"total_count":0,"page":1,"data":[]}"#),
    );
    let mut env = OnlineEnv::testing(Arc::new(local()), Some(dir.join("cache")));
    env.os_root = api.url("/api/v1");
    let account = Account::new(None);
    let JobResult::Login(info) = run(
        Job::Login {
            user: "me".into(),
            password: "pw".into(),
        },
        &env,
        &account,
    ) else {
        panic!()
    };
    info.unwrap();
    let job = SearchJob::new(
        Provider::Opensubtitles,
        VideoRef::new("https://example.com/a.mp4"),
        "a movie",
        &OnlineSubSettings::default(),
        vitascope::i18n::Lang::En,
    );
    let JobResult::Search { result, .. } = run(Job::Search(job), &env, &account) else {
        panic!()
    };
    assert!(result.unwrap().results.is_empty());
    let sent = &api.requests_to("/api/v1/subtitles")[0];
    assert_eq!(sent.header("Api-Key"), Some("test-key"));
    assert_eq!(sent.header("Authorization"), Some("Bearer jwt-y"));
    let got = &elsewhere.requests_to("/subtitles")[0];
    assert_eq!(got.header("Api-Key"), None, "金鑰送到別的主機了");
    assert_eq!(got.header("Authorization"), None, "登入憑證送到別的主機了");
    std::fs::remove_dir_all(&dir).unwrap();
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
