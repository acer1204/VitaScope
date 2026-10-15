//! 線上字幕核心的單元測試：全部用 [`FakeHttp`]（不連網）與暫存資料夾

use super::http::{FakeHttp, Kind, Method, Reply, Transport};
use super::*;
use crate::i18n::Lang;

/// 繁體的字幕內容（繁體專用字夠多，內容判斷得出來）
const TRAD_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\n這是我們的時間，說話會讓你覺得還沒來過這裡\n";
/// 簡體的字幕內容
const SIMP_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\n这是我们的时间，说话会让你觉得还没来过这里\n";
/// 分不出語言的內容
const PLAIN_SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nVITASCOPE TEST\n";
/// 日文的字幕內容：用到的漢字（時、間、見、話、開、問、頭、東、無、現、愛）都在繁體的判斷字裡
const JA_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nもう時間がないよ。ちょっと見て。話があるんだけど\n\n\
                      2\n00:00:04,000 --> 00:00:06,000\nドアを開けてくれ！問題ないよ。頭がいたいんだ\n\n\
                      3\n00:00:07,000 --> 00:00:09,000\n東へいこう。無理だよ。現実を見ろ。愛してるよ\n";

/// 這個測試專用的暫存資料夾（每次重建）
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vitascope-online-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 資料夾裡的檔名（排序）
fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// 一部 200 KiB 的「影片」（特徵碼 a65c16d28845ff55）
fn video_in(dir: &Path) -> VideoRef {
    let path = dir.join("Movie.Name.2019.1080p.mkv");
    let bytes: Vec<u8> = (0..204_800u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, bytes).unwrap();
    VideoRef::new(path.to_string_lossy().into_owned())
}

fn env_with(fake: &Arc<FakeHttp>, cache: Option<PathBuf>) -> OnlineEnv {
    OnlineEnv::testing(fake.clone(), cache)
}

fn session(token: &str) -> Session {
    Session {
        token: token.into(),
        base_url: "api.opensubtitles.com".into(),
        user: "me".into(),
        expires: now_unix() + 3600,
    }
}

fn signed_in(token: &str) -> Arc<Account> {
    let a = Account::new(None);
    a.sign_in(session(token));
    a
}

fn os_job(video: VideoRef) -> SearchJob {
    let s = OnlineSubSettings {
        languages: vec!["zh-tw".into(), "zh-cn".into(), "en".into()],
        ..Default::default()
    };
    SearchJob::new(Provider::Opensubtitles, video, "movie name 2019", &s, Lang::ZhTw)
}

fn os_download(video: VideoRef, file_id: u64, lang: &str) -> DownloadJob {
    DownloadJob {
        video,
        item: DownloadItem::Os {
            file_id,
            lang_code: lang.into(),
        },
        next_to_video: true,
        cancel: Arc::new(AtomicBool::new(false)),
        known_link: None,
        saved_before: None,
    }
}

fn download_result(r: JobResult) -> (Option<String>, Option<Quota>, Result<Downloaded, NetError>) {
    match r {
        JobResult::Download {
            link, quota, result, ..
        } => (link, quota, result),
        other => panic!("不是下載的結果：{other:?}"),
    }
}

/// OpenSubtitles `/subtitles` 的回應（照 OpenAPI 的格式寫的）：伺服器的順序是英文、繁體（相符）、簡體（兩片）、沒有檔案的一筆
const OS_SEARCH: &str = r#"{
  "total_pages": 3, "total_count": 61, "per_page": 50, "page": 1,
  "data": [
    {"id": "1", "type": "subtitle", "attributes": {
      "language": "en", "release": "Movie.Name.2019.WEB-DL", "download_count": 880, "hearing_impaired": true,
      "ai_translated": false, "machine_translated": false, "from_trusted": false, "ratings": 6.5,
      "moviehash_match": false, "upload_date": "2020-01-02T00:00:00Z", "uploader": {"name": "someone"},
      "feature_details": {"title": "Movie Name", "year": 2019}, "url": "https://www.opensubtitles.com/en/subtitles/x",
      "files": [{"file_id": 11, "cd_number": 1, "file_name": "a.srt"}]}},
    {"id": "2", "type": "subtitle", "attributes": {
      "language": "zh-tw", "release": "[Nekomoe] Movie Name [1080p]", "download_count": 1203,
      "ai_translated": true, "from_trusted": true, "moviehash_match": true,
      "feature_details": {"title": "Show", "year": 2019, "season_number": 1, "episode_number": 2},
      "files": [{"file_id": 22, "cd_number": 1, "file_name": "b.srt"}]}},
    {"id": "3", "type": "subtitle", "attributes": {
      "language": "zh-cn", "release": null, "machine_translated": true,
      "files": [{"file_id": 33, "cd_number": 1, "file_name": "c1.srt"}, {"file_id": 34, "cd_number": 2, "file_name": "c2.srt"}]}},
    {"id": "4", "type": "subtitle", "attributes": {"language": "ja", "release": "no files"}},
    {"id": "5", "type": "subtitle"}
  ]
}"#;

// ───────────────────────────── 設定 ─────────────────────────────

#[test]
fn settings_defaults_and_sanitize() {
    let s = OnlineSubSettings::default();
    assert_eq!(s.provider, Provider::Opensubtitles);
    assert!(s.languages.is_empty() && s.save_next_to_video && s.hearing_impaired && s.auto_search);
    assert!(!s.machine_translated);
    let s = OnlineSubSettings {
        languages: vec![
            " ZH-TW ".into(),
            "zh-tw".into(),
            "en".into(),
            "english".into(),
            "pt-br".into(),
            "zh-".into(),
            "e1".into(),
            "".into(),
        ],
        assrt_token: "  not a valid token  ".into(),
        opensubtitles_user: " me ".into(),
        ..Default::default()
    }
    .sanitized();
    assert_eq!(s.languages, ["zh-tw", "en", "pt-br"]);
    assert_eq!(
        s.assrt_token, "  not a valid token  ",
        "Token 照使用者輸入的保存（R-C7）"
    );
    assert_eq!(s.opensubtitles_user, "me");
    let many = OnlineSubSettings {
        languages: (b'a'..=b'z').map(|c| format!("{}{}", c as char, c as char)).collect(),
        ..Default::default()
    }
    .sanitized();
    assert_eq!(many.languages.len(), MAX_LANGUAGES);
}

#[test]
fn languages_follow_the_ui_language_when_unset() {
    let s = OnlineSubSettings::default();
    assert_eq!(s.effective_languages(Lang::ZhTw), ["zh-tw", "zh-cn", "ze", "en"]);
    assert_eq!(s.effective_languages(Lang::En), ["en"]);
    let s = OnlineSubSettings {
        languages: vec!["ja".into()],
        ..Default::default()
    };
    assert_eq!(s.effective_languages(Lang::ZhTw), ["ja"]);
}

#[test]
fn secrets_are_not_printed() {
    let s = OnlineSubSettings {
        assrt_token: "0123456789abcdef0123456789abcdef".into(),
        ..Default::default()
    };
    assert!(!format!("{s:?}").contains("0123456789"));
    assert!(!format!("{:?}", session("eyJ.secret.jwt")).contains("secret"));
    let jobs = [
        Job::Login {
            user: "me".into(),
            password: "hunter2".into(),
        },
        Job::AssrtDetail {
            sub_id: 1,
            token: "0123456789abcdef0123456789abcdef".into(),
        },
        Job::Search(SearchJob::new(Provider::Assrt, VideoRef::new("x"), "q", &s, Lang::ZhTw)),
    ];
    for job in &jobs {
        let text = format!("{job:?}");
        assert!(!text.contains("hunter2") && !text.contains("0123456789"), "{text}");
    }
    let mut env = OnlineEnv::testing(FakeHttp::new(), None);
    env.os_key = Some("KEY-123".into());
    assert!(!format!("{env:?}").contains("KEY-123"));
}

// ───────────────────────────── 錯誤、時間 ─────────────────────────────

#[test]
fn every_error_has_a_message_in_both_languages() {
    let all = [
        NetError::Offline,
        NetError::NoKey,
        NetError::NoToken,
        NetError::InvalidToken,
        NetError::NeedLogin,
        NetError::LoginExpired,
        NetError::BadLogin,
        NetError::Forbidden,
        NetError::Quota {
            reset_at: Some(now_unix() + 3 * 3600 + 120),
            signed_in: false,
        },
        NetError::Quota {
            reset_at: None,
            signed_in: true,
        },
        NetError::LinkExpired,
        NetError::RateLimited(Site::Assrt),
        NetError::RateLimited(Site::OpenSubtitles),
        NetError::QueryTooShort,
        NetError::NotFound(Site::Assrt),
        NetError::Server {
            site: Site::OpenSubtitles,
            code: 503,
        },
        NetError::Server {
            site: Site::Assrt,
            code: 30000,
        },
        NetError::Connect {
            site: Site::Assrt,
            detail: "refused".into(),
        },
        NetError::Timeout(Site::OpenSubtitles),
        NetError::BadResponse(Site::OpenSubtitles),
        NetError::TooLarge,
        NetError::NotSubtitle,
        NetError::Archive { page: None },
        NetError::Disk("full".into()),
        NetError::Cancelled,
        NetError::NoSaveFolder,
        NetError::NoFreeName,
        NetError::NotHttps(Site::Assrt),
    ];
    for lang in [Lang::ZhTw, Lang::En] {
        crate::i18n::set_lang(lang);
        for e in &all {
            let m = e.message();
            assert!(!m.is_empty(), "{e:?}");
            // 介面文字不能有兩個字母的「人工智慧」縮寫（§1.14；OpenSubtitles 的 ai_translated 寫成「自動翻譯」）
            let banned = [0x41u8, 0x49];
            assert!(
                !m.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w.as_bytes().eq_ignore_ascii_case(&banned)),
                "{m}"
            );
            // 英文介面裡沒有中文（錯誤只帶資料，文字都由 message 依介面語言產生）
            if lang == Lang::En {
                assert!(!m.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "{m}");
            }
        }
    }
    crate::i18n::set_lang(Lang::ZhTw);
    let quota = all[8].message();
    assert!(quota.contains("3 小時") && quota.contains("登入"), "{quota}");
    assert!(!all[9].message().contains("登入"), "已經登入時不再叫人登入");
    assert!(all[15].message().contains("HTTP 503"));
    assert!(all[16].message().contains("30000") && !all[16].message().contains("HTTP"));
    assert!(NetError::NeedLogin.offers_login() && NetError::LoginExpired.offers_login());
    assert!(all[8].offers_login() && !all[9].offers_login() && !NetError::Forbidden.offers_login());
}

#[test]
fn worker_errors_carry_no_text() {
    // 背景執行緒的介面語言一律是預設的中文：錯誤裡不能有那邊產生的文字（R-C3、§1.8）
    let dir = scratch("worker-text");
    let stuck = dir.join("not-a-dir");
    std::fs::write(&stuck, b"x").unwrap();
    let video = VideoRef::new(stuck.join("Movie.mkv").to_string_lossy().into_owned());
    let e = std::thread::spawn(move || {
        save_subtitle(
            TRAD_SRT.as_bytes(),
            &video,
            "zh-TW",
            "srt",
            true,
            None,
            &AtomicBool::new(false),
        )
        .unwrap_err()
    })
    .join()
    .unwrap();
    assert_eq!(e, NetError::NoSaveFolder);
    crate::i18n::set_lang(Lang::En);
    assert_eq!(
        e.message(),
        "Couldn't save the subtitle: there is no folder to save it in"
    );
    assert_eq!(
        NetError::transport(Site::OpenSubtitles, Transport::NotHttps),
        NetError::NotHttps(Site::OpenSubtitles)
    );
    crate::i18n::set_lang(Lang::ZhTw);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn parses_utc_times() {
    assert_eq!(parse_utc("2022-04-08T13:03:15.000Z"), Some(1_649_422_995));
    assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
    assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_utc("2022-13-08T13:03:15Z"), None);
    assert_eq!(parse_utc("07 hours and 40 minutes"), None);
    assert_eq!(parse_utc(""), None);
    assert_eq!(parse_utc("9999-12-31T23:59:59Z"), Some(253_402_300_799));
    // 不合理的年份、秒數：不溢位（開發版會 panic），當成看不懂
    assert_eq!(parse_utc("9999999999999999-01-01T00:00:00Z"), None);
    assert_eq!(parse_utc("-9999999999999999-01-01T00:00:00Z"), None);
    assert_eq!(parse_utc("1969-12-31T23:59:59Z"), None);
    assert_eq!(parse_utc("2022-04-08T13:03:1e300Z"), None);
    crate::i18n::set_lang(Lang::ZhTw);
    assert_eq!(wait_text(3 * 3600 + 30 * 60), "3 小時 30 分");
    assert_eq!(wait_text(10), "1 分鐘");
}

// ───────────────────────────── 搜尋文字 ─────────────────────────────

#[test]
fn clean_query_table() {
    for (name, want) in [
        ("Movie.Name.2019.1080p.BluRay.x264-GROUP", "movie name 2019"),
        ("Show.S02E05.720p.WEB-DL", "show s02e05"),
        (
            "[Nekomoe kissaten][Sousou no Frieren][01][1080p][CHS]",
            "sousou no frieren 01",
        ),
        ("[Group] Show Name - 01 [1080p][HEVC]", "show name 01"),
        ("影片", "影片"),
        ("Movie.Name.2019.1080p.BluRay.x264-GROUP.mkv", "movie name 2019"),
        ("The.Web.2020.1080p", "the web 2020"),
        ("Show Name 01 WEB DL", "show name 01"),
        ("葬送的芙莉蓮 第01話 [1080p]", "葬送的芙莉蓮 第01話"),
        ("1080p", "1080p"),
        ("[OnlyGroup]", "onlygroup"),
    ] {
        assert_eq!(clean_query(name), want, "{name}");
    }
}

#[test]
fn query_encoding() {
    assert_eq!(encode_query("movie name 2019", true), "movie+name+2019");
    assert_eq!(encode_query("a b&c=d/é", false), "a%20b%26c%3Dd%2F%C3%A9");
    assert_eq!(encode_query("葬", true), "%E8%91%AC");
    assert_eq!(encode_query("a-b._~", true), "a-b._~");
}

// ───────────────────────────── OpenSubtitles：參數、解析 ─────────────────────────────

#[test]
fn opensubtitles_query_is_canonical() {
    use opensubtitles::{Query, query_string};
    let q = Query {
        languages: vec!["zh-TW".into(), "en".into(), "zh-cn".into(), "en".into()],
        moviehash: Some("A65C16D28845FF55".into()),
        query: "Movie Name 2019".into(),
        page: 1,
        hearing_impaired: true,
        machine_translated: false,
    };
    // 字母順序、小寫、空白寫成 +、語言碼排序用逗號、預設值不送
    assert_eq!(
        query_string(&q),
        "languages=en,zh-cn,zh-tw&moviehash=a65c16d28845ff55&query=movie+name+2019"
    );
    let q2 = Query {
        page: 2,
        hearing_impaired: false,
        machine_translated: true,
        moviehash: None,
        ..q.clone()
    };
    assert_eq!(
        query_string(&q2),
        "hearing_impaired=exclude&languages=en,zh-cn,zh-tw&machine_translated=include&page=2&query=movie+name+2019"
    );
    let q3 = Query {
        languages: Vec::new(),
        query: String::new(),
        ..q
    };
    assert_eq!(query_string(&q3), "moviehash=a65c16d28845ff55");
}

#[test]
fn opensubtitles_results_are_ordered_and_split_per_file() {
    let langs: Vec<String> = ["zh-tw", "zh-cn", "en"].map(String::from).to_vec();
    let p = opensubtitles::parse_search(OS_SEARCH.as_bytes(), &langs).unwrap();
    assert_eq!((p.page, p.total_pages, p.total), (1, 3, 61));
    let ids: Vec<&ResultId> = p.results.iter().map(|r| &r.id).collect();
    // 相符的先，再照語言順序（繁、簡、英），同一筆的兩片照順序
    assert_eq!(
        ids,
        [
            &ResultId::Os { file_id: 22 },
            &ResultId::Os { file_id: 33 },
            &ResultId::Os { file_id: 34 },
            &ResultId::Os { file_id: 11 }
        ]
    );
    let trad = &p.results[0];
    assert!(trad.hash_match && trad.auto_translated && trad.trusted);
    assert_eq!(trad.lang, SubLang::Traditional);
    assert_eq!(trad.lang_code, "zh-tw");
    assert_eq!(trad.feature, "Show (2019) S01E02");
    assert_eq!(trad.cd, None, "只有一片時不標第幾片");
    let simp = &p.results[1];
    assert_eq!((simp.cd, p.results[2].cd), (Some(1), Some(2)));
    assert!(simp.machine_translated && !simp.hash_match);
    assert_eq!(simp.name, "c1.srt", "沒有版本名稱時用檔名");
    assert_eq!(simp.downloads, None);
    let en = &p.results[3];
    assert_eq!(en.lang, SubLang::English);
    assert_eq!((en.downloads, en.rating), (Some(880), Some(6.5)));
    assert!(en.hearing_impaired);
    assert_eq!(en.uploader, "someone");
    assert_eq!(
        en.page_url.as_deref(),
        Some("https://www.opensubtitles.com/en/subtitles/x")
    );
    assert!(opensubtitles::parse_search(b"<html>login</html>", &langs).is_none());
    assert!(opensubtitles::parse_search(b"{\"data\": 3}", &langs).is_none());
}

#[test]
fn login_base_is_one_of_the_official_hosts() {
    assert_eq!(
        opensubtitles::valid_base("vip-api.opensubtitles.com"),
        "vip-api.opensubtitles.com"
    );
    assert_eq!(
        opensubtitles::valid_base("https://VIP-API.opensubtitles.com/api/v1"),
        "vip-api.opensubtitles.com"
    );
    assert_eq!(opensubtitles::valid_base("evil.example.com"), "api.opensubtitles.com");
    assert_eq!(opensubtitles::valid_base(""), "api.opensubtitles.com");
}

#[test]
fn runtime_key_only_overrides_in_debug_online_tests() {
    use opensubtitles::choose_key;
    let built = || Some("built".to_owned());
    // （開發版, VITASCOPE_ONLINE_TESTS=1, 執行時的變數, 建置時的金鑰）→ 用哪一個（R-B9）
    assert_eq!(choose_key(true, true, Some(" run "), built()).as_deref(), Some("run"));
    assert_eq!(choose_key(true, false, Some("run"), built()).as_deref(), Some("built"));
    assert_eq!(
        choose_key(false, true, Some("run"), built()).as_deref(),
        Some("built"),
        "正式版不看執行時的變數"
    );
    assert_eq!(choose_key(false, false, Some("run"), None), None);
    assert_eq!(
        choose_key(false, true, Some("run"), None),
        None,
        "正式版沒有金鑰就是沒有"
    );
    assert_eq!(choose_key(true, true, Some("   "), built()).as_deref(), Some("built"));
    assert_eq!(choose_key(true, true, None, None), None);
}

#[test]
fn api_key_mask_round_trip() {
    let mask = b"VitaScope:online-subtitles:mask!";
    let masked = opensubtitles::mask("AbCdEf0123456789AbCdEf0123456789-long-key", mask);
    assert_ne!(masked, b"AbCdEf0123456789AbCdEf0123456789-long-key");
    assert_eq!(
        opensubtitles::unmask(&masked, mask).as_deref(),
        Some("AbCdEf0123456789AbCdEf0123456789-long-key")
    );
    assert_eq!(opensubtitles::unmask(&[], mask), None, "沒有金鑰的版本");
    assert_eq!(opensubtitles::unmask(&opensubtitles::mask("  ", mask), mask), None);
}

// ───────────────────────────── OpenSubtitles：搜尋 ─────────────────────────────

#[test]
fn search_sends_the_canonical_request_with_moviehash() {
    let dir = scratch("os-search");
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/subtitles", Reply::ok(OS_SEARCH));
    let env = env_with(&fake, None);
    let account = Account::new(None);
    let video = video_in(&dir);
    let JobResult::Search {
        video: searched,
        result,
        ..
    } = run(Job::Search(os_job(video.clone())), &env, &account)
    else {
        panic!()
    };
    let page = result.unwrap();
    assert_eq!(page.matched(), 1);
    assert_eq!(page.results.len(), 4);
    let hash = searched.hash.expect("本機檔案要算特徵碼");
    assert_eq!((hash.hex().as_str(), hash.size), ("a65c16d28845ff55", 204_800));
    assert_eq!(page.video, searched);
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].url,
        "https://os.test/api/v1/subtitles?languages=en,zh-cn,zh-tw&moviehash=a65c16d28845ff55&query=movie+name+2019"
    );
    assert_eq!(reqs[0].kind, Kind::Api);
    assert_eq!(reqs[0].header_value("Api-Key"), Some("test-key"));
    assert_eq!(reqs[0].header_value("Authorization"), None, "未登入不送登入憑證");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn search_without_key_or_offline_never_connects() {
    let fake = FakeHttp::new();
    let mut env = env_with(&fake, None);
    env.os_key = None;
    let account = Account::new(None);
    let JobResult::Search { result, .. } = run(Job::Search(os_job(VideoRef::new("https://x/a.mp4"))), &env, &account)
    else {
        panic!()
    };
    assert_eq!(result, Err(NetError::NoKey));
    assert!(fake.requests().is_empty());
    // 預設的環境（Launch 的預設）是離線的
    let offline = OnlineEnv::default();
    assert!(offline.is_offline() && offline.os_key.is_none() && offline.cache_dir.is_none());
    assert!(offline.session_file.is_none());
    let mut job = os_job(VideoRef::new("https://x/a.mp4"));
    job.provider = Provider::Assrt;
    job.assrt_token = "0123456789abcdef0123456789abcdef".into();
    let JobResult::Search { result, .. } = run(Job::Search(job), &offline, &account) else {
        panic!()
    };
    assert_eq!(result, Err(NetError::Offline));
}

#[test]
fn search_errors_are_mapped() {
    for (status, body, token, want) in [
        (403, "{}", false, NetError::Forbidden),
        (401, "{}", false, NetError::Forbidden),
        (401, "{}", true, NetError::LoginExpired),
        (500, r#"{"message":"invalid token"}"#, true, NetError::LoginExpired),
        (500, r#"{"errors":["Invalid token"]}"#, true, NetError::LoginExpired),
        (
            500,
            "oops",
            true,
            NetError::Server {
                site: Site::OpenSubtitles,
                code: 500,
            },
        ),
        // 代理伺服器、CDN 的錯誤頁剛好有 invalid：只是伺服器暫時有問題，不登出
        (
            502,
            "<html>invalid response from upstream</html>",
            true,
            NetError::Server {
                site: Site::OpenSubtitles,
                code: 502,
            },
        ),
        (
            503,
            r#"{"message":"invalid"}"#,
            true,
            NetError::Server {
                site: Site::OpenSubtitles,
                code: 503,
            },
        ),
        (
            500,
            "<html>invalid gateway config</html>",
            true,
            NetError::Server {
                site: Site::OpenSubtitles,
                code: 500,
            },
        ),
        (429, "{}", false, NetError::RateLimited(Site::OpenSubtitles)),
        (
            200,
            "<html>wifi login</html>",
            false,
            NetError::BadResponse(Site::OpenSubtitles),
        ),
    ] {
        let fake = FakeHttp::new();
        fake.on(Method::Get, "/subtitles", Reply::new(status, body));
        let env = env_with(&fake, None);
        let account = if token { signed_in("jwt") } else { Account::new(None) };
        let JobResult::Search { result, .. } = run(Job::Search(os_job(VideoRef::new("https://x/a"))), &env, &account)
        else {
            panic!()
        };
        assert_eq!(result, Err(want.clone()), "{status} {body}");
        // 登入過期：已經登出，沒有用未登入的身分再試一次
        assert_eq!(fake.requests().len(), 1, "{status}");
        if want == NetError::LoginExpired {
            assert_eq!(account.user(), None);
        } else if token {
            assert_eq!(account.user().as_deref(), Some("me"), "{status} {body}：不能登出");
        }
    }
    let fake = FakeHttp::new();
    fake.fail(Method::Get, "/subtitles", Transport::Timeout);
    let JobResult::Search { result, .. } = run(
        Job::Search(os_job(VideoRef::new("https://x/a"))),
        &env_with(&fake, None),
        &Account::new(None),
    ) else {
        panic!()
    };
    assert_eq!(result, Err(NetError::Timeout(Site::OpenSubtitles)));
}

#[test]
fn signed_in_requests_go_to_the_login_host_with_the_token() {
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/subtitles", Reply::ok(OS_SEARCH));
    // 官方的位址（假的連線：不會真的連出去）
    let mut env = env_with(&fake, None);
    env.os_root = opensubtitles::OFFICIAL_ROOT.into();
    let account = Account::new(None);
    account.sign_in(Session {
        base_url: "vip-api.opensubtitles.com".into(),
        ..session("jwt-1")
    });
    let _ = run(Job::Search(os_job(VideoRef::new("https://x/a"))), &env, &account);
    let req = &fake.requests()[0];
    assert!(
        req.url
            .starts_with("https://vip-api.opensubtitles.com/api/v1/subtitles?"),
        "{}",
        req.url
    );
    assert_eq!(req.header_value("Authorization"), Some("Bearer jwt-1"));
    // 測試指定的位址：一律用它（登入的主機不會把測試送到真的網站）
    let fake = FakeHttp::new();
    let _ = run(
        Job::Search(os_job(VideoRef::new("https://x/a"))),
        &env_with(&fake, None),
        &account,
    );
    assert!(fake.requests()[0].url.starts_with("https://os.test/api/v1/"));
}

#[test]
fn a_late_expiry_does_not_sign_out_a_new_login() {
    // 用舊憑證的搜尋還在等網路時，使用者重新登入了；舊的請求晚一點才收到 401：新的登入要留著
    let dir = scratch("os-race");
    let file = dir.join("online").join(SESSION_FILE);
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/subtitles", Reply::new(401, "{}"));
    fake.hold("/subtitles");
    let mut env = env_with(&fake, None);
    env.session_file = Some(file.clone());
    let account = Account::new(Some(file.clone()));
    account.sign_in(session("jwt-old"));
    let rx = spawn(Job::Search(os_job(VideoRef::new("https://x/a"))), &env, &account, || {});
    let deadline = Instant::now() + Duration::from_secs(30);
    while fake.requests_to("/subtitles").is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fake.requests_to("/subtitles")[0].header_value("Authorization"),
        Some("Bearer jwt-old")
    );
    account.sign_in(Session {
        user: "new".into(),
        ..session("jwt-new")
    });
    fake.release("/subtitles");
    let JobResult::Search { result, .. } = rx.recv_timeout(Duration::from_secs(30)).unwrap() else {
        panic!()
    };
    assert_eq!(result, Err(NetError::LoginExpired));
    assert_eq!(account.user().as_deref(), Some("new"), "新的登入不受影響");
    assert_eq!(account.session().unwrap().token, "jwt-new");
    assert!(file.is_file(), "新的登入狀態檔案還在");
    // 同一個憑證過期：照樣登出
    account.expire("jwt-new");
    assert_eq!(account.user(), None);
    assert!(!file.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn requests_are_spaced_by_the_throttle() {
    // 正式的間隔照 OpenSubtitles 的規定（每秒 5 次、登入每秒 1 次），真的環境用的就是它
    assert!(REQUEST_GAP >= Duration::from_millis(200), "{REQUEST_GAP:?}");
    assert!(LOGIN_GAP >= Duration::from_secs(1), "{LOGIN_GAP:?}");
    assert_eq!(Throttle::opensubtitles().gaps(), (REQUEST_GAP, LOGIN_GAP));
    let dir = scratch("os-throttle");
    assert_eq!(OnlineEnv::real(&dir).throttle.gaps(), (REQUEST_GAP, LOGIN_GAP));
    // 經過真正送請求的路徑：搜尋 → 登入（至少隔登入的間隔）→ 搜尋（至少隔一般的間隔）。只比下限，慢的機器不會失敗
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/subtitles", Reply::ok(OS_SEARCH));
    fake.on(
        Method::Post,
        "/login",
        Reply::ok(r#"{"user":{"allowed_downloads":20},"base_url":"api.opensubtitles.com","token":"t"}"#),
    );
    let mut env = env_with(&fake, None);
    env.throttle = Arc::new(Throttle::new(Duration::from_millis(60), Duration::from_millis(300)));
    let account = Account::new(None);
    let search = || run(Job::Search(os_job(VideoRef::new("https://x/a"))), &env, &account);
    let start = Instant::now();
    let _ = search();
    let JobResult::Login(r) = run(
        Job::Login {
            user: "me".into(),
            password: "pw".into(),
        },
        &env,
        &account,
    ) else {
        panic!()
    };
    r.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(300), "登入前等登入的間隔");
    let _ = search();
    assert!(
        start.elapsed() >= Duration::from_millis(360),
        "登入之後的請求也要隔開：{:?}",
        start.elapsed()
    );
    assert_eq!(fake.requests().len(), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────────────────────── OpenSubtitles：下載 ─────────────────────────────

const LINK: &str = "https://dl.test/download/ABC/subfile/movie.srt";

fn download_ok(remaining: i64) -> Reply {
    Reply::ok(format!(
        r#"{{"link":"{LINK}","file_name":"movie.srt","requests":1,"remaining":{remaining},
            "message":"ok","reset_time":"07 hours","reset_time_utc":"2022-04-08T13:03:15.000Z"}}"#
    ))
}

#[test]
fn download_saves_next_to_the_video_with_a_content_tag() {
    let dir = scratch("os-dl");
    let cache = dir.join("cache");
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(17));
    // 網站標 zh-tw，內容是簡體（R-B12）
    fake.on(Method::Get, "dl.test/download", Reply::ok(SIMP_SRT));
    let env = env_with(&fake, Some(cache.clone()));
    let account = signed_in("jwt-1");
    let video = video_in(&dir);
    let (link, quota, result) = download_result(run(
        Job::Download(os_download(video.clone(), 22, "zh-tw")),
        &env,
        &account,
    ));
    let d = result.unwrap();
    assert_eq!(d.path, dir.join("Movie.Name.2019.1080p.zh-CN.srt"));
    assert!(d.in_video_dir && !d.fell_back && !d.reused);
    assert_eq!(std::fs::read_to_string(&d.path).unwrap(), SIMP_SRT);
    assert_eq!(link.as_deref(), Some(LINK));
    assert_eq!(
        quota,
        Some(Quota {
            remaining: 17,
            reset_at: Some(1_649_422_995)
        })
    );
    assert_eq!(account.quota(), quota, "介面看得到剩下的次數");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].body.as_deref(), Some(r#"{"file_id":22}"#));
    assert_eq!(reqs[0].header_value("Authorization"), Some("Bearer jwt-1"));
    assert_eq!(reqs[0].header_value("Api-Key"), Some("test-key"));
    // 暫時的連結：另一種連線，不帶金鑰、登入憑證
    assert_eq!(reqs[1].kind, Kind::Download);
    assert_eq!(reqs[1].header_value("Authorization"), None);
    assert_eq!(reqs[1].header_value("Api-Key"), None);
    assert_eq!(
        names_in(&dir),
        ["Movie.Name.2019.1080p.mkv", "Movie.Name.2019.1080p.zh-CN.srt"],
        "沒有留下暫存檔，也沒有用到快取資料夾"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn no_quota_number_unless_the_server_sends_remaining() {
    // 回應沒有 `remaining`：不能自己編一個數字（R-A1、R-C14），之前伺服器說的也不改
    let dir = scratch("os-no-remaining");
    let fake = FakeHttp::new();
    fake.on(
        Method::Post,
        "/download",
        Reply::ok(format!(r#"{{"link":"{LINK}","file_name":"movie.srt","message":"ok"}}"#)),
    );
    fake.on(Method::Get, "dl.test/download", Reply::ok(PLAIN_SRT));
    let env = env_with(&fake, Some(dir.join("cache")));
    let account = signed_in("jwt-1");
    let (_, quota, result) = download_result(run(
        Job::Download(os_download(video_in(&dir), 22, "en")),
        &env,
        &account,
    ));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(quota, None, "伺服器沒說剩下幾個");
    assert_eq!(account.quota(), None, "介面不能顯示數字");
    let earlier = Quota {
        remaining: 9,
        reset_at: None,
    };
    account.set_quota(earlier);
    let (_, quota, result) = download_result(run(
        Job::Download(os_download(video_in(&dir), 23, "en")),
        &env,
        &account,
    ));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(quota, None);
    assert_eq!(account.quota(), Some(earlier), "之前伺服器說的次數不能被蓋成別的數字");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn anonymous_download_refused_asks_for_login() {
    let dir = scratch("os-anon");
    for (status, body, want) in [
        (401, "{\"message\":\"login required\"}", NetError::NeedLogin),
        (403, "{\"message\":\"forbidden\"}", NetError::NeedLogin),
        // 403 是 User-Agent 不對：那是這個版本的問題，不是要登入
        (
            403,
            "User-Agent header is wrong; set it to App name with version",
            NetError::Forbidden,
        ),
    ] {
        let fake = FakeHttp::new();
        fake.on(Method::Post, "/download", Reply::new(status, body));
        let env = env_with(&fake, Some(dir.join("cache")));
        let (link, _, result) = download_result(run(
            Job::Download(os_download(VideoRef::new("https://x/v.mp4"), 1, "en")),
            &env,
            &Account::new(None),
        ));
        assert_eq!(result, Err(want), "{status} {body}");
        assert_eq!(link, None);
        assert_eq!(fake.requests().len(), 1, "沒有拿到連結就不抓檔案");
        assert!(fake.requests()[0].header_value("Authorization").is_none());
    }
    assert!(!dir.join("cache").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn quota_and_expired_login() {
    // 次數用完：重置時間、剩 0 個；未登入時提供登入
    let fake = FakeHttp::new();
    fake.on(
        Method::Post,
        "/download",
        Reply::new(
            406,
            r#"{"requests":6,"remaining":-1,"message":"You have downloaded your allowed 5 subtitles",
                "reset_time":"03 hours","reset_time_utc":"2022-04-08T13:03:15.000Z"}"#,
        ),
    );
    let account = Account::new(None);
    let (_, quota, result) = download_result(run(
        Job::Download(os_download(VideoRef::new("https://x/v"), 1, "en")),
        &env_with(&fake, None),
        &account,
    ));
    let e = result.unwrap_err();
    assert_eq!(
        e,
        NetError::Quota {
            reset_at: Some(1_649_422_995),
            signed_in: false
        }
    );
    assert!(e.offers_login());
    assert_eq!(
        quota,
        Some(Quota {
            remaining: 0,
            reset_at: Some(1_649_422_995)
        })
    );
    // 登入過期（401、500 invalid）：登出（檔案也刪掉），只送一次，不改用未登入的身分再試
    for (status, body) in [(401, "{}"), (500, r#"{"message":"invalid token"}"#)] {
        let dir = scratch("os-expired");
        let file = dir.join("online").join(SESSION_FILE);
        let account = Account::new(Some(file.clone()));
        account.sign_in(session("jwt-old"));
        assert!(file.is_file());
        let fake = FakeHttp::new();
        fake.on(Method::Post, "/download", Reply::new(status, body));
        let (_, _, result) = download_result(run(
            Job::Download(os_download(VideoRef::new("https://x/v"), 1, "en")),
            &env_with(&fake, None),
            &account,
        ));
        assert_eq!(result, Err(NetError::LoginExpired), "{status}");
        assert_eq!(fake.requests().len(), 1);
        assert_eq!(account.user(), None);
        assert!(!file.exists(), "過期的登入狀態要刪掉");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn known_link_and_saved_file_save_the_quota() {
    let dir = scratch("os-reuse");
    let video = video_in(&dir);
    // 之前拿到的連結還有效：不再 POST /download
    let fake = FakeHttp::new();
    fake.on(Method::Get, "dl.test", Reply::ok(TRAD_SRT));
    let env = env_with(&fake, None);
    let mut job = os_download(video.clone(), 22, "zh-tw");
    job.known_link = Some(LINK.into());
    let (link, quota, result) = download_result(run(Job::Download(job), &env, &signed_in("t")));
    let saved = result.unwrap();
    assert_eq!(saved.path, dir.join("Movie.Name.2019.1080p.zh-TW.srt"));
    assert_eq!((link.as_deref(), quota), (Some(LINK), None));
    assert!(fake.requests_to("/download").iter().all(|r| r.method != Method::Post));
    // 之前的連結過期：要一個新的（只要一次）
    let fake = FakeHttp::new();
    fake.on(Method::Get, "old.test", Reply::new(410, "gone"));
    fake.on(Method::Post, "/download", download_ok(4));
    fake.on(Method::Get, "dl.test", Reply::ok(TRAD_SRT));
    let mut job = os_download(video.clone(), 22, "zh-tw");
    job.known_link = Some("https://old.test/x".into());
    let (link, quota, result) = download_result(run(Job::Download(job), &env_with(&fake, None), &signed_in("t")));
    let again = result.unwrap();
    assert!(again.reused, "內容一樣：直接用已經存的檔案");
    assert_eq!(again.path, saved.path);
    assert_eq!(link.as_deref(), Some(LINK));
    assert_eq!(quota.map(|q| q.remaining), Some(4));
    let posts = fake.requests().into_iter().filter(|r| r.method == Method::Post).count();
    assert_eq!(posts, 1);
    // 這次已經存過、檔案還在：完全不連網
    let fake = FakeHttp::new();
    let mut job = os_download(video, 22, "zh-tw");
    job.saved_before = Some(saved.path.clone());
    let (_, _, result) = download_result(run(Job::Download(job), &env_with(&fake, None), &signed_in("t")));
    let d = result.unwrap();
    assert!(d.reused && d.in_video_dir);
    assert_eq!(d.path, saved.path);
    assert!(fake.requests().is_empty());
    assert_eq!(
        names_in(&dir),
        ["Movie.Name.2019.1080p.mkv", "Movie.Name.2019.1080p.zh-TW.srt"]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn downloads_memory_prepares_jobs() {
    let dir = scratch("os-memory");
    let video = video_in(&dir);
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(9));
    fake.on(Method::Get, "dl.test", Reply::ok(PLAIN_SRT));
    let env = env_with(&fake, None);
    let account = signed_in("t");
    let mut memory = Downloads::default();
    let mut job = os_download(video.clone(), 5, "en");
    memory.prepare(&mut job);
    assert_eq!((job.known_link.as_ref(), job.saved_before.as_ref()), (None, None));
    let result = run(Job::Download(job), &env, &account);
    memory.record(&result);
    let mut job = os_download(video.clone(), 5, "en");
    memory.prepare(&mut job);
    assert_eq!(job.known_link.as_deref(), Some(LINK));
    assert_eq!(job.saved_before, Some(dir.join("Movie.Name.2019.1080p.en.srt")));
    // 別的影片：存的位置不算（連結還是可以用）
    let mut other = os_download(VideoRef::new("https://x/other"), 5, "en");
    memory.prepare(&mut other);
    assert_eq!((other.known_link.as_deref(), other.saved_before), (Some(LINK), None));
    // 連結 410：先留著，下一次先重試同一個連結（R-B11）
    let failed = |link: Option<&str>, e: NetError| JobResult::Download {
        video: video.clone(),
        item: DownloadItem::Os {
            file_id: 5,
            lang_code: "en".into(),
        },
        link: link.map(str::to_owned),
        quota: None,
        result: Err(e),
    };
    memory.record(&failed(Some(LINK), NetError::LinkExpired));
    let mut job = os_download(video.clone(), 5, "en");
    memory.prepare(&mut job);
    assert_eq!(job.known_link.as_deref(), Some(LINK));
    // 重試過也失效、又沒拿到新的連結（例如次數用完）：忘掉它
    memory.record(&failed(
        None,
        NetError::Quota {
            reset_at: None,
            signed_in: true,
        },
    ));
    let mut job = os_download(video, 5, "en");
    memory.prepare(&mut job);
    assert_eq!(job.known_link, None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_410_link_is_retried_before_spending_another_download() {
    let dir = scratch("os-410");
    let video = video_in(&dir);
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(9));
    // 剛拿到的連結第一次 410，再抓一次就好了
    fake.on(Method::Get, "dl.test", Reply::new(410, "gone"));
    fake.on(Method::Get, "dl.test", Reply::ok(TRAD_SRT));
    let env = env_with(&fake, None);
    let account = signed_in("t");
    let mut memory = Downloads::default();
    let attempt = |memory: &mut Downloads| {
        let mut job = os_download(video.clone(), 22, "zh-tw");
        memory.prepare(&mut job);
        let known = job.known_link.clone();
        let result = run(Job::Download(job), &env, &account);
        memory.record(&result);
        (known, download_result(result).2)
    };
    let (known, first) = attempt(&mut memory);
    assert_eq!((known, first), (None, Err(NetError::LinkExpired)));
    let posts = || fake.requests().iter().filter(|r| r.method == Method::Post).count();
    assert_eq!(posts(), 1);
    // 第二次：先用同一個連結（不花下載次數）
    let (known, second) = attempt(&mut memory);
    assert_eq!(known.as_deref(), Some(LINK), "410 的連結要先重試");
    assert_eq!(second.unwrap().path, dir.join("Movie.Name.2019.1080p.zh-TW.srt"));
    assert_eq!(posts(), 1, "重試同一個連結就好了：沒有再 POST /download");
    assert_eq!(fake.requests_to("dl.test").len(), 2);
    // 重試也 410：這時才要新的連結（同一次按下載裡，只多花一次）
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(8));
    fake.on(Method::Get, "dl.test", Reply::new(410, "gone"));
    fake.on(Method::Get, "dl.test", Reply::new(410, "gone"));
    fake.on(Method::Get, "dl.test", Reply::ok(SIMP_SRT));
    let env = env_with(&fake, None);
    let mut memory = Downloads::default();
    let mut attempt = || {
        let mut job = os_download(video.clone(), 23, "zh-cn");
        memory.prepare(&mut job);
        let result = run(Job::Download(job), &env, &account);
        memory.record(&result);
        download_result(result).2
    };
    assert_eq!(attempt(), Err(NetError::LinkExpired));
    assert!(attempt().is_ok());
    let order: Vec<Method> = fake.requests().iter().map(|r| r.method).collect();
    assert_eq!(
        order,
        [Method::Post, Method::Get, Method::Get, Method::Post, Method::Get],
        "先重試同一個連結，失效才再要一個"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_downloads_are_not_saved() {
    let dir = scratch("os-bad");
    let video = video_in(&dir);
    for (body, want) in [
        (b"<!DOCTYPE html><html>portal</html>".to_vec(), NetError::NotSubtitle),
        (b"PK\x03\x04zipzip".to_vec(), NetError::Archive { page: None }),
        (vec![b'a'; MAX_SUBTITLE_SIZE as usize + 1], NetError::TooLarge),
    ] {
        let fake = FakeHttp::new();
        fake.on(Method::Post, "/download", download_ok(3));
        fake.on(Method::Get, "dl.test", Reply::ok(body));
        let (_, _, result) = download_result(run(
            Job::Download(os_download(video.clone(), 1, "en")),
            &env_with(&fake, Some(dir.join("cache"))),
            &signed_in("t"),
        ));
        assert_eq!(result, Err(want));
    }
    // 連結剛拿到就失效
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(3));
    fake.on(Method::Get, "dl.test", Reply::new(410, ""));
    let (link, _, result) = download_result(run(
        Job::Download(os_download(video, 1, "en")),
        &env_with(&fake, None),
        &signed_in("t"),
    ));
    assert_eq!((link.as_deref(), result), (Some(LINK), Err(NetError::LinkExpired)));
    assert_eq!(names_in(&dir), ["Movie.Name.2019.1080p.mkv"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancelled_download_leaves_nothing() {
    let dir = scratch("os-cancel");
    let video = video_in(&dir);
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(3));
    fake.on(Method::Get, "dl.test", Reply::ok(TRAD_SRT));
    let job = os_download(video, 1, "zh-tw");
    job.cancel.store(true, Ordering::SeqCst);
    let (_, _, result) = download_result(run(Job::Download(job), &env_with(&fake, None), &signed_in("t")));
    assert_eq!(result, Err(NetError::Cancelled));
    assert!(fake.requests().is_empty(), "取消了就不再花下載次數");
    assert_eq!(names_in(&dir), ["Movie.Name.2019.1080p.mkv"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_while_the_file_is_downloading() {
    // 下載到一半關掉視窗（G6 設 cancel）：不存檔、資料夾裡只有影片
    let dir = scratch("os-cancel-flight");
    let video = video_in(&dir);
    let fake = FakeHttp::new();
    fake.on(Method::Post, "/download", download_ok(3));
    fake.on(Method::Get, "dl.test", Reply::ok(TRAD_SRT));
    fake.hold("dl.test");
    let job = os_download(video, 1, "zh-tw");
    let cancel = job.cancel.clone();
    let rx = spawn(Job::Download(job), &env_with(&fake, None), &signed_in("t"), || {});
    let deadline = Instant::now() + Duration::from_secs(30);
    while fake.requests_to("dl.test").is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.requests_to("dl.test").len(), 1, "已經在下載檔案");
    cancel.store(true, Ordering::SeqCst);
    fake.release("dl.test");
    let (_, _, result) = download_result(rx.recv_timeout(Duration::from_secs(30)).unwrap());
    assert_eq!(result, Err(NetError::Cancelled));
    assert_eq!(names_in(&dir), ["Movie.Name.2019.1080p.mkv"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────────────────────── 登入、登出 ─────────────────────────────

#[test]
fn login_keeps_the_token_never_the_password() {
    let dir = scratch("os-login");
    let file = dir.join("online").join(SESSION_FILE);
    let fake = FakeHttp::new();
    fake.on(
        Method::Post,
        "/login",
        Reply::ok(
            r#"{"user":{"allowed_downloads":20,"level":"Sub leecher","user_id":66,"ext_installed":false,"vip":false},
                "base_url":"vip-api.opensubtitles.com","token":"eyJ.jwt.token","status":200}"#,
        ),
    );
    let mut env = env_with(&fake, None);
    env.session_file = Some(file.clone());
    let account = Account::new(env.session_file.clone());
    let JobResult::Login(info) = run(
        Job::Login {
            user: " me ".into(),
            password: "pa\"ss".into(),
        },
        &env,
        &account,
    ) else {
        panic!()
    };
    let info = info.unwrap();
    assert_eq!(
        info,
        LoginInfo {
            user: "me".into(),
            allowed_downloads: Some(20),
            vip: false
        }
    );
    assert_eq!(account.user().as_deref(), Some("me"));
    assert_eq!(account.quota(), None, "還沒下載前不顯示剩下的次數（R-C14）");
    let req = &fake.requests()[0];
    let body: serde_json::Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"username": "me", "password": "pa\"ss"}));
    assert_eq!(req.header_value("Authorization"), None);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("eyJ.jwt.token") && text.contains("vip-api.opensubtitles.com"));
    assert!(
        !text.contains("pa\\\"ss") && !text.contains("password"),
        "密碼不存：{text}"
    );
    // 關掉影戲再開：還是登入狀態
    let again = Account::new(Some(file.clone()));
    assert_eq!(again.user(), None, "讀檔之前不知道");
    again.restore();
    assert_eq!(again.user().as_deref(), Some("me"));
    let s = again.session().unwrap();
    assert!(s.expires > now_unix() + 23 * 3600 && s.expires <= now_unix() + 24 * 3600);
    // 登出：通知伺服器、刪掉檔案（伺服器失敗也一樣）
    fake.fail(Method::Delete, "/logout", Transport::Timeout);
    assert!(matches!(run(Job::Logout, &env, &again), JobResult::Logout));
    let logout = fake.requests_to("/logout");
    assert_eq!(logout.len(), 1);
    assert_eq!(logout[0].method, Method::Delete);
    assert_eq!(logout[0].header_value("Authorization"), Some("Bearer eyJ.jwt.token"));
    assert_eq!(again.user(), None);
    assert!(!file.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn wrong_password_is_not_retried() {
    let fake = FakeHttp::new();
    fake.on(
        Method::Post,
        "/login",
        Reply::new(401, r#"{"message":"Error, invalid username/password"}"#),
    );
    let account = Account::new(None);
    let login = |user: &str, pass: &str| {
        let JobResult::Login(r) = run(
            Job::Login {
                user: user.into(),
                password: pass.into(),
            },
            &env_with(&fake, None),
            &account,
        ) else {
            panic!()
        };
        r
    };
    assert_eq!(login("me", "bad"), Err(NetError::BadLogin));
    assert_eq!(fake.requests().len(), 1, "401 之後不自動重試");
    assert_eq!(login("me", ""), Err(NetError::BadLogin));
    assert_eq!(login("  ", "x"), Err(NetError::BadLogin));
    assert_eq!(fake.requests().len(), 1, "空的帳號密碼不送出");
    assert_eq!(account.user(), None);
}

#[test]
fn session_file_round_trip_and_expiry() {
    let dir = scratch("session");
    let file = dir.join("online").join(SESSION_FILE);
    let s = session("jwt");
    write_session(&file, &s).unwrap();
    assert_eq!(read_session(&file, now_unix()), Some(s.clone()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // 過期：刪掉
    assert_eq!(read_session(&file, s.expires), None);
    assert!(!file.exists());
    // 看不懂、到期時間不合理（超過 24 小時之後）：刪掉
    for text in [
        "not json",
        r#"{"token":"","base_url":"x","user":"u","expires":99999999999}"#,
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(read_session(&file, now_unix()), None, "{text}");
        assert!(!file.exists());
    }
    let far = Session {
        expires: now_unix() + 30 * 24 * 3600,
        ..s
    };
    write_session(&file, &far).unwrap();
    assert_eq!(read_session(&file, now_unix()), None);
    // 帳號在記憶體裡過期：當成沒登入
    let account = Account::new(Some(file.clone()));
    account.sign_in(Session {
        expires: now_unix() + 1,
        ..session("jwt")
    });
    assert!(file.exists());
    {
        account.state().session.as_mut().unwrap().expires = now_unix();
    }
    assert_eq!(account.user(), None);
    assert!(!file.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn throttle_spaces_requests() {
    let t = Throttle::new(Duration::from_millis(60), Duration::from_millis(150));
    let start = Instant::now();
    t.wait(false);
    t.wait(false);
    assert!(start.elapsed() >= Duration::from_millis(60));
    let before_login = Instant::now();
    t.wait(true);
    assert!(before_login.elapsed() >= Duration::from_millis(100), "登入要隔久一點");
    let none = Throttle::none();
    let start = Instant::now();
    for _ in 0..20 {
        none.wait(false);
    }
    assert!(start.elapsed() < Duration::from_millis(500));
}

// ───────────────────────────── 射手網（偽） ─────────────────────────────

const TOKEN: &str = "0123456789abcdefABCDEF0123456789";

/// `sub/search` 的回應（照網站文件的範例寫的）
const ASSRT_SEARCH: &str = r#"{"sub":{"subs":[
  {"lang":{"langlist":{"langcht":true,"langeng":true},"desc":"繁 英"},"id":602333,"native_name":"電影名稱",
   "upload_time":"2016-01-26 15:42:07","subtype":"SRT","vote_score":0,"release_site":"YYeTs","videoname":"Movie.Name.2019"},
  {"lang":{"langlist":{"langchs":true},"desc":"简"},"id":602334,"native_name":"","videoname":"Movie.Name.2019.WEB",
   "subtype":"ASS","vote_score":5,"release_site":"字幕组"},
  {"lang":{"langlist":{"langdou":true},"desc":"双语"},"id":1234,"native_name":"VobSub 版","subtype":"VobSub"},
  {"lang":{"desc":"日"},"id":99,"native_name":"x","subtype":"其他"},
  {"native_name":"no id"}
 ],"action":"search","keyword":"movie","result":"succeed"},"status":0}"#;

/// `sub/detail` 的回應：壓縮檔裡的每個檔案都有網址
const ASSRT_DETAIL: &str = r#"{"sub":{"subs":[{"id":602333,"subtype":"SRT","release_site":"YYeTs",
  "lang":{"langlist":{"langcht":true,"langchs":true},"desc":"简 繁"},
  "filename":"Movie.rar","url":"http://file0.assrt.net/download/602333/Movie.rar?_=1",
  "filelist":[
    {"url":"http://file0.assrt.net/onthefly/602333/-/1/Movie.chs.srt?_=1","f":"Movie.chs.srt","s":"48KB"},
    {"url":"http://file1.assrt.net/onthefly/602333/-/1/Movie.cht.srt?_=1","f":"Movie.cht.srt","s":"48KB"},
    {"url":"http://file1.assrt.net/onthefly/602333/-/1/readme.txt.nfo","f":"readme.nfo","s":"1KB"},
    {"url":"","f":"empty.srt","s":"0KB"}
  ]}]},"status":0}"#;

fn assrt_job(query: &str, token: &str) -> SearchJob {
    let s = OnlineSubSettings {
        assrt_token: token.into(),
        ..Default::default()
    };
    SearchJob::new(Provider::Assrt, VideoRef::new("C:/v/Movie.mkv"), query, &s, Lang::ZhTw)
}

fn assrt_search(fake: &Arc<FakeHttp>, job: SearchJob) -> Result<SearchPage, NetError> {
    let JobResult::Search { result, .. } = run(Job::Search(job), &env_with(fake, None), &Account::new(None)) else {
        panic!()
    };
    result
}

#[test]
fn assrt_token_and_urls() {
    assert_eq!(assrt::check_token(&format!("  {TOKEN} ")), Ok(TOKEN.to_owned()));
    assert_eq!(assrt::check_token(""), Err(NetError::NoToken));
    assert_eq!(assrt::check_token("   "), Err(NetError::NoToken));
    assert_eq!(assrt::check_token("short"), Err(NetError::InvalidToken));
    assert_eq!(
        assrt::check_token(&format!("{}!", &TOKEN[..31])),
        Err(NetError::InvalidToken)
    );
    assert_eq!(assrt::page_url(602_333), "https://assrt.net/xml/sub/602/602333.xml");
    assert_eq!(
        assrt::https_url("http://file0.assrt.net/onthefly/1/a.srt?_=1"),
        "https://file0.assrt.net/onthefly/1/a.srt?_=1"
    );
    assert_eq!(
        assrt::https_url("http://evil.example/a.srt"),
        "http://evil.example/a.srt"
    );
    assert_eq!(
        assrt::https_url("https://file2.assrt.net/x"),
        "https://file2.assrt.net/x"
    );
    assert_eq!(assrt::name_tag("Movie.2019.chs&eng.srt"), "chs&eng");
    assert_eq!(assrt::name_tag("dir/Movie.cht.ass"), "cht");
    assert_eq!(assrt::name_tag("Movie.2019.srt"), "");
    assert_eq!(assrt::name_tag("cht.srt"), "");
    for (code, want) in [
        (1, NetError::InvalidToken),
        (20001, NetError::InvalidToken),
        (101, NetError::QueryTooShort),
        (20900, NetError::NotFound(Site::Assrt)),
        (30900, NetError::RateLimited(Site::Assrt)),
        (
            30100,
            NetError::Server {
                site: Site::Assrt,
                code: 30100,
            },
        ),
    ] {
        assert_eq!(assrt::status_error(code), want, "{code}");
    }
}

#[test]
fn assrt_search_sends_the_token_in_a_header() {
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/sub/search", Reply::ok(ASSRT_SEARCH));
    let page = assrt_search(&fake, assrt_job("葬送 芙莉蓮", TOKEN)).unwrap();
    let req = &fake.requests()[0];
    assert_eq!(
        req.url,
        "https://assrt.test/v1/sub/search?q=%E8%91%AC%E9%80%81%20%E8%8A%99%E8%8E%89%E8%93%AE&cnt=15&pos=0"
    );
    assert!(!req.url.contains(TOKEN), "Token 不能放進網址");
    assert_eq!(
        req.header_value("Authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!((page.page, page.total_pages, page.total), (1, 1, None));
    let r = &page.results;
    assert_eq!(r.len(), 4, "沒有 id 的略過");
    assert_eq!(
        r[0].id,
        ResultId::Assrt {
            sub_id: 602_333,
            file: None
        }
    );
    assert_eq!((r[0].lang, r[0].lang_code.as_str()), (SubLang::Traditional, "繁 英"));
    assert_eq!(r[0].name, "電影名稱 / Movie.Name.2019");
    assert_eq!((r[0].format.as_str(), r[0].release_site.as_str()), ("SRT", "YYeTs"));
    assert_eq!(
        r[0].page_url.as_deref(),
        Some("https://assrt.net/xml/sub/602/602333.xml")
    );
    assert!(!r[0].downloadable(), "要先展開");
    assert_eq!(
        (r[1].lang, r[1].name.as_str()),
        (SubLang::Simplified, "Movie.Name.2019.WEB")
    );
    assert_eq!(r[1].rating, Some(5.0));
    assert_eq!(r[2].lang, SubLang::Chinese);
    assert!(r[2].archive_only, "VobSub 要到網頁下載");
    assert_eq!(r[3].lang, SubLang::Japanese, "沒有 langlist：看說明");
    // 第 2 頁
    let mut job = assrt_job("movie", TOKEN);
    job.page = 2;
    let _ = assrt_search(&fake, job);
    assert!(fake.requests()[1].url.ends_with("q=movie&cnt=15&pos=15"));
}

#[test]
fn assrt_errors_and_no_client_side_length_check() {
    // 沒有 Token、格式不對：不連線
    let fake = FakeHttp::new();
    assert_eq!(assrt_search(&fake, assrt_job("movie", "")), Err(NetError::NoToken));
    assert_eq!(
        assrt_search(&fake, assrt_job("movie", "abc")),
        Err(NetError::InvalidToken)
    );
    assert!(fake.requests().is_empty());
    // 兩個字的中文片名照樣送出，伺服器說太短才是太短（R-C6）
    fake.on(
        Method::Get,
        "/sub/search",
        Reply::new(400, r#"{"status":101,"errmsg":"too short"}"#),
    );
    assert_eq!(
        assrt_search(&fake, assrt_job("英雄", TOKEN)),
        Err(NetError::QueryTooShort)
    );
    assert_eq!(fake.requests().len(), 1);
    for (status, body, want) in [
        (
            400,
            r#"{"status":20001,"errmsg":"invalid token"}"#,
            Err(NetError::InvalidToken),
        ),
        (200, r#"{"status":30900}"#, Err(NetError::RateLimited(Site::Assrt))),
        (
            200,
            r#"{"status":30000}"#,
            Err(NetError::Server {
                site: Site::Assrt,
                code: 30000,
            }),
        ),
        (
            502,
            "<html>bad gateway</html>",
            Err(NetError::Server {
                site: Site::Assrt,
                code: 502,
            }),
        ),
        (200, "<html>wifi login</html>", Err(NetError::BadResponse(Site::Assrt))),
        // 找不到 = 沒有結果
        (200, r#"{"status":20900}"#, Ok(0)),
    ] {
        let fake = FakeHttp::new();
        fake.on(Method::Get, "/sub/search", Reply::new(status, body));
        let got = assrt_search(&fake, assrt_job("movie", TOKEN)).map(|p| p.results.len());
        assert_eq!(got, want, "{status} {body}");
    }
}

#[test]
fn assrt_tries_the_other_domain_when_unreachable() {
    let fake = FakeHttp::new();
    fake.fail(
        Method::Get,
        "assrt.test/v1/sub/search",
        Transport::Connect("refused".into()),
    );
    fake.on(Method::Get, "alt.test/v1/sub/search", Reply::ok(ASSRT_SEARCH));
    let mut env = env_with(&fake, None);
    env.assrt_alt_root = Some("https://alt.test/v1".into());
    let JobResult::Search { result, .. } = run(Job::Search(assrt_job("movie", TOKEN)), &env, &Account::new(None))
    else {
        panic!()
    };
    assert_eq!(result.unwrap().results.len(), 4);
    assert_eq!(fake.requests().len(), 2);
    // 逾時不換網域（不是連不上）
    let fake = FakeHttp::new();
    fake.fail(Method::Get, "/sub/search", Transport::Timeout);
    let mut env = env_with(&fake, None);
    env.assrt_alt_root = Some("https://alt.test/v1".into());
    let JobResult::Search { result, .. } = run(Job::Search(assrt_job("movie", TOKEN)), &env, &Account::new(None))
    else {
        panic!()
    };
    assert_eq!(result, Err(NetError::Timeout(Site::Assrt)));
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn assrt_detail_lists_files_traditional_first() {
    let v: serde_json::Value = serde_json::from_str(ASSRT_DETAIL).unwrap();
    let rows = assrt::parse_detail(&v, 602_333).unwrap();
    let files: Vec<(&str, &str, SubLang)> = rows
        .iter()
        .map(|r| match &r.id {
            ResultId::Assrt { file: Some(f), .. } => (f.name.as_str(), f.url.as_str(), r.lang),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        files,
        [
            (
                "Movie.cht.srt",
                "https://file1.assrt.net/onthefly/602333/-/1/Movie.cht.srt?_=1",
                SubLang::Traditional
            ),
            (
                "Movie.chs.srt",
                "https://file0.assrt.net/onthefly/602333/-/1/Movie.chs.srt?_=1",
                SubLang::Simplified
            ),
        ],
        "不是字幕的檔案、沒有網址的略過；網址改成 https"
    );
    assert!(rows.iter().all(SubResult::downloadable));
    // 只有壓縮檔：請到網頁下載
    let only_rar = serde_json::json!({"status":0,"sub":{"subs":[{"id":5,"filename":"a.rar","url":"http://file0.assrt.net/a.rar","filelist":[]}]}});
    assert_eq!(
        assrt::parse_detail(&only_rar, 5),
        Err(NetError::Archive {
            page: Some(assrt::page_url(5))
        })
    );
    let vobsub = serde_json::json!({"status":0,"sub":{"subs":[{"id":6,"subtype":"VobSub","filelist":[{"f":"a.sub","url":"https://x/a.sub"}]}]}});
    assert!(matches!(assrt::parse_detail(&vobsub, 6), Err(NetError::Archive { .. })));
    // 單一檔案
    let single = serde_json::json!({"status":0,"sub":{"subs":[{"id":7,"filename":"a.cht.ass","url":"http://file0.assrt.net/a.ass"}]}});
    let rows = assrt::parse_detail(&single, 7).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].format, "ASS");
    assert_eq!(
        assrt::parse_detail(&serde_json::json!({"status":0}), 8),
        Err(NetError::NotFound(Site::Assrt))
    );
}

#[test]
fn assrt_detail_and_download_end_to_end() {
    let dir = scratch("assrt-dl");
    let video = video_in(&dir);
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/sub/detail?id=602333", Reply::ok(ASSRT_DETAIL));
    fake.on(Method::Get, "Movie.cht.srt", Reply::ok(TRAD_SRT));
    let env = env_with(&fake, None);
    let account = Account::new(None);
    let JobResult::Detail { sub_id, result } = run(
        Job::AssrtDetail {
            sub_id: 602_333,
            token: TOKEN.into(),
        },
        &env,
        &account,
    ) else {
        panic!()
    };
    assert_eq!(sub_id, 602_333);
    let rows = result.unwrap();
    assert_eq!(
        fake.requests()[0].header_value("Authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    let job = DownloadJob::for_result(&rows[0], video.clone(), true).unwrap();
    let (link, quota, result) = download_result(run(Job::Download(job), &env, &account));
    assert_eq!((link, quota), (None, None));
    let d = result.unwrap();
    assert_eq!(d.path, dir.join("Movie.Name.2019.1080p.zh-TW.srt"));
    let get = fake.requests_to("Movie.cht.srt");
    assert_eq!(
        get[0].url,
        "https://file1.assrt.net/onthefly/602333/-/1/Movie.cht.srt?_=1"
    );
    assert_eq!(get[0].kind, Kind::Download);
    assert_eq!(get[0].header_value("Authorization"), None, "下載檔案不帶 Token");
    // 檔名分不出語言、內容也分不出：用檔名的標記
    let file = AssrtFile {
        name: "Movie.2019.chs&eng.srt".into(),
        url: "https://file0.assrt.net/x/plain.srt".into(),
        size: String::new(),
    };
    fake.on(Method::Get, "plain.srt", Reply::ok(PLAIN_SRT));
    let job = DownloadJob {
        video,
        item: DownloadItem::Assrt { file },
        next_to_video: true,
        cancel: Arc::new(AtomicBool::new(false)),
        known_link: None,
        saved_before: None,
    };
    let (_, _, result) = download_result(run(Job::Download(job.clone()), &env, &account));
    assert_eq!(result.unwrap().path, dir.join("Movie.Name.2019.1080p.chs&eng.srt"));
    // 不是字幕的副檔名：不下載
    let mut bad = job;
    bad.item = DownloadItem::Assrt {
        file: AssrtFile {
            name: "setup.exe".into(),
            url: "https://file0.assrt.net/x/setup.exe".into(),
            size: String::new(),
        },
    };
    let (_, _, result) = download_result(run(Job::Download(bad), &env, &account));
    assert_eq!(result, Err(NetError::NotSubtitle));
    assert!(fake.requests_to("setup.exe").is_empty());
    // 還沒展開的一筆不能直接下載
    let entry = SubResult::new(Provider::Assrt, ResultId::Assrt { sub_id: 1, file: None });
    assert!(DownloadJob::for_result(&entry, VideoRef::new("x"), true).is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

// ───────────────────────────── 內容、存檔 ─────────────────────────────

#[test]
fn sniff_rejects_what_is_not_a_subtitle() {
    assert_eq!(sniff(b"<!DOCTYPE html><p>", "srt"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"\xEF\xBB\xBF  <HTML><body>", "srt"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"PK\x03\x04....", "srt"), Err(NetError::Archive { page: None }));
    assert_eq!(sniff(b"Rar!\x1a\x07\x00", "srt"), Err(NetError::Archive { page: None }));
    assert_eq!(
        sniff(b"7z\xBC\xAF\x27\x1C\x00", "srt"),
        Err(NetError::Archive { page: None })
    );
    // VobSub 的 .sub（MPEG-PS）
    assert_eq!(sniff(b"\x00\x00\x01\xBA\x44\x00", "sub"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"\x1F\x8B\x08\x00", "srt"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"", "srt"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"   \n", "srt"), Err(NetError::NotSubtitle));
    assert_eq!(
        sniff(&vec![b'1'; MAX_SUBTITLE_SIZE as usize + 1], "srt"),
        Err(NetError::TooLarge)
    );
    // UTF-16 的 SRT、ASS、PGS
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(PLAIN_SRT.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    assert_eq!(sniff(&utf16, "srt"), Ok(()));
    assert_eq!(sniff(b"[Script Info]\nTitle: x", "ass"), Ok(()));
    assert_eq!(sniff(b"PG\x00\x01", "sup"), Ok(()));
    assert_eq!(sniff(b"1\n00:00", "sup"), Err(NetError::NotSubtitle), ".sup 要是 PGS");
    // `.txt`：壓縮檔裡的說明檔不是字幕；有時間碼的才是
    let readme = "本字幕由某某字幕組製作\n僅供學習交流，請勿用於商業用途\nhttps://example.com\n";
    assert_eq!(sniff(readme.as_bytes(), "txt"), Err(NetError::NotSubtitle));
    assert_eq!(sniff(b"Version 1.0\n12:30\nThanks", "txt"), Err(NetError::NotSubtitle));
    for sub in [
        "{1}{25}Hello|World\n{30}{60}Bye\n",
        "[10][25]Hello\n",
        "00:00:01:Hello\n00:00:03:Bye\n",
        "0:00:01=Hello\n",
        "\u{feff}1\n00:00:01,000 --> 00:00:02,000\nHello\n",
    ] {
        assert_eq!(sniff(sub.as_bytes(), "txt"), Ok(()), "{sub}");
    }
}

#[test]
fn tags_come_from_the_content_first() {
    assert_eq!(content_tag(SIMP_SRT.as_bytes(), "srt", "zh-TW"), "zh-CN");
    assert_eq!(content_tag(TRAD_SRT.as_bytes(), "srt", "zh-CN"), "zh-TW");
    assert_eq!(content_tag(PLAIN_SRT.as_bytes(), "srt", "en"), "en");
    assert_eq!(content_tag(b"PG", "sup", "zh-TW"), "zh-TW", "圖形字幕看不出內容");
    // 日文字幕：常用的漢字很多也是繁體的判斷字，但標的是日文就照網站的；沒標的也看假名，不改成中文
    let ja = JA_SRT.as_bytes();
    assert_eq!(
        crate::subs::classify_content(JA_SRT),
        Some(SubLang::Traditional),
        "這段日文只看判斷字會被當成繁體（測試的前提）"
    );
    assert_eq!(content_tag(ja, "srt", "ja"), "ja");
    assert_eq!(content_tag(ja, "srt", "jpn"), "jpn");
    assert_eq!(content_tag(ja, "srt", ""), "");
    assert_eq!(content_tag(SIMP_SRT.as_bytes(), "srt", "en"), "en", "標英文的不改");
    assert_eq!(content_tag(SIMP_SRT.as_bytes(), "srt", "zh"), "zh-CN");
    assert_eq!(content_tag(SIMP_SRT.as_bytes(), "srt", "chs&eng"), "zh-CN");
    assert_eq!(content_tag(TRAD_SRT.as_bytes(), "srt", ""), "zh-TW");
    assert_eq!(os_tag("zh-tw"), "zh-TW");
    assert_eq!(os_tag("ZH-CN"), "zh-CN");
    assert_eq!(os_tag("ze"), "zh");
    assert_eq!(os_tag("pt-br"), "pt-BR");
    assert_eq!(os_tag("en"), "en");
    assert_eq!(sanitize_tag("../../evil"), "evil");
    assert_eq!(sanitize_tag("chs&eng"), "chs&eng");
    assert_eq!(sanitize_tag("a/b\\c:d"), "abcd");
    assert_eq!(sanitize_tag("x".repeat(40).as_str()).len(), 16);
}

#[test]
fn saving_numbers_names_reuses_identical_and_never_overwrites() {
    let dir = scratch("save");
    let video = video_in(&dir);
    let no = AtomicBool::new(false);
    let save = |bytes: &str| save_subtitle(bytes.as_bytes(), &video, "zh-TW", "srt", true, None, &no).unwrap();
    let first = save(TRAD_SRT);
    assert_eq!(first.path, dir.join("Movie.Name.2019.1080p.zh-TW.srt"));
    assert!(!first.reused);
    // 內容一樣：直接用那個檔案
    let same = save(TRAD_SRT);
    assert_eq!(same.path, first.path);
    assert!(same.reused);
    // 不一樣：編號，不覆蓋
    let other = save("1\n00:00:01,000 --> 00:00:02,000\n另一個\n");
    assert_eq!(other.path, dir.join("Movie.Name.2019.1080p.zh-TW.2.srt"));
    let third = save("1\n00:00:01,000 --> 00:00:02,000\n第三個\n");
    assert_eq!(third.path, dir.join("Movie.Name.2019.1080p.zh-TW.3.srt"));
    assert_eq!(std::fs::read_to_string(&first.path).unwrap(), TRAD_SRT);
    // 沒有標記
    let untagged = save_subtitle(b"x", &video, "", "srt", true, None, &no).unwrap();
    assert_eq!(untagged.path, dir.join("Movie.Name.2019.1080p.srt"));
    // 存好的會被自動載入（下次開啟）
    let found = crate::subs::find_external(Path::new(&video.path));
    assert!(found.contains(&first.path) && found.contains(&other.path), "{found:?}");
    assert!(
        names_in(&dir).iter().all(|n| !n.contains("vitascope-part")),
        "{:?}",
        names_in(&dir)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saved_next_to_the_video_with_its_own_name() {
    // 影片名稱原本的樣子（很長的中文名稱、Linux / macOS 允許的 `:`）：存好的字幕下次開啟要自動載入
    let dir = scratch("own-stem");
    let no = AtomicBool::new(false);
    let mut stems = vec!["葬送的芙莉蓮".repeat(12) + " 第01話 1080p"];
    if cfg!(unix) {
        stems.push("Blade Runner: 2049?".to_owned());
    }
    for stem in stems {
        assert!(
            stem.len() > 150 || stem.contains(':'),
            "測試的前提：整理過的名稱會不一樣"
        );
        let path = dir.join(format!("{stem}.mkv"));
        std::fs::write(&path, b"video").unwrap();
        let video = VideoRef::new(path.to_string_lossy().into_owned());
        let d = save_subtitle(TRAD_SRT.as_bytes(), &video, "zh-TW", "srt", true, None, &no).unwrap();
        assert_eq!(d.path, dir.join(format!("{stem}.zh-TW.srt")));
        assert!(d.in_video_dir);
        assert_eq!(
            crate::subs::find_external(&path),
            std::slice::from_ref(&d.path),
            "{stem}"
        );
        std::fs::remove_file(&d.path).unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_falls_back_to_the_cache_folder() {
    let dir = scratch("fallback");
    let cache = dir.join("cache").join("subs").join("online");
    let no = AtomicBool::new(false);
    // 影片的「資料夾」其實是一個檔案：寫不進去 → 快取資料夾
    std::fs::write(dir.join("not-a-dir"), b"x").unwrap();
    let stuck = VideoRef::new(dir.join("not-a-dir").join("Movie.mkv").to_string_lossy().into_owned());
    let d = save_subtitle(b"1", &stuck, "zh-TW", "srt", true, Some(&cache), &no).unwrap();
    assert_eq!(d.path, cache.join("Movie.zh-TW.srt"));
    assert!(d.fell_back && !d.in_video_dir);
    // 設定不存到影片旁邊：快取資料夾，不是「改放」
    let video = video_in(&dir);
    let d = save_subtitle(b"2", &video, "en", "srt", false, Some(&cache), &no).unwrap();
    assert_eq!(d.path, cache.join("Movie.Name.2019.1080p.en.srt"));
    assert!(!d.fell_back && !d.in_video_dir);
    // 網路影片：用網址的最後一段
    let url = VideoRef::new("https://example.com/videos/Clip.Name.mp4?token=1");
    let d = save_subtitle(b"3", &url, "en", "srt", true, Some(&cache), &no).unwrap();
    assert_eq!(d.path, cache.join("Clip.Name.en.srt"));
    let bare = VideoRef::new("https://example.com/");
    let d = save_subtitle(b"4", &bare, "", "srt", true, Some(&cache), &no).unwrap();
    assert_eq!(d.path, cache.join("subtitle.srt"));
    // 沒有地方放
    assert_eq!(
        save_subtitle(b"5", &stuck, "en", "srt", true, None, &no),
        Err(NetError::NoSaveFolder)
    );
    // 快取資料夾的名稱整理過（各系統都能用）
    let odd = VideoRef::new("https://example.com/a%3Ab/Clip:Name.mp4");
    let d = save_subtitle(b"6", &odd, "en", "srt", true, Some(&cache), &no).unwrap();
    assert_eq!(d.path, cache.join("Clip_Name.en.srt"));
    assert!(names_in(&cache).iter().all(|n| !n.contains("vitascope-part")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_is_checked_before_the_final_rename() {
    let dir = scratch("cancel");
    // 暫存檔已經寫好之後才取消（寫到很慢的網路磁碟時按了關閉）：還是不能換成正式名稱
    let asked_after_write = std::cell::Cell::new(false);
    let cancelled_once_written = || {
        let written = names_in(&dir).iter().any(|n| crate::save::is_part(n));
        asked_after_write.set(asked_after_write.get() || written);
        written
    };
    let r = save_in(&dir, "Movie", "zh-TW", "srt", b"x", &cancelled_once_written);
    assert!(matches!(r, Err(SaveError::Cancelled)));
    assert!(asked_after_write.get(), "暫存檔寫好之後要再問一次取消");
    assert!(names_in(&dir).is_empty(), "暫存檔要刪掉：{:?}", names_in(&dir));
    let cancel = AtomicBool::new(true);
    let video = video_in(&dir);
    assert_eq!(
        save_subtitle(b"x", &video, "zh-TW", "srt", true, Some(&dir.join("c")), &cancel),
        Err(NetError::Cancelled)
    );
    assert_eq!(names_in(&dir), ["Movie.Name.2019.1080p.mkv"]);
    // 已經有內容一樣的檔案：取消了也回傳取消（介面不再等這個結果），檔案留著
    let no = AtomicBool::new(false);
    let saved = save_subtitle(b"x", &video, "zh-TW", "srt", true, None, &no).unwrap();
    assert_eq!(
        save_subtitle(b"x", &video, "zh-TW", "srt", true, None, &cancel),
        Err(NetError::Cancelled)
    );
    assert!(saved.path.is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn video_identity() {
    let a = VideoRef {
        path: "C:/v/a.mkv".into(),
        hash: Some(FileHash { size: 1, hash: 2 }),
    };
    assert!(a.same_video(&VideoRef::new("C:/v/a.mkv")), "還沒算特徵碼的：看路徑");
    assert!(!a.same_video(&VideoRef {
        hash: Some(FileHash { size: 1, hash: 3 }),
        ..a.clone()
    }));
    assert!(!a.same_video(&VideoRef::new("C:/v/b.mkv")));
    assert_eq!(a.local_path(), Some(Path::new("C:/v/a.mkv")));
    assert_eq!(VideoRef::new("https://x/a.mkv").local_path(), None);
    assert_eq!(VideoRef::new("").local_path(), None);
}

#[test]
fn spawn_returns_through_the_channel_and_wakes() {
    let fake = FakeHttp::new();
    fake.on(Method::Get, "/subtitles", Reply::ok(OS_SEARCH));
    let env = env_with(&fake, None);
    let account = Account::new(None);
    let woke = Arc::new(AtomicBool::new(false));
    let w = woke.clone();
    let rx = spawn(
        Job::Search(os_job(VideoRef::new("https://x/a"))),
        &env,
        &account,
        move || w.store(true, Ordering::SeqCst),
    );
    let JobResult::Search { result, provider, .. } = rx.recv_timeout(Duration::from_secs(30)).unwrap() else {
        panic!()
    };
    assert_eq!(provider, Provider::Opensubtitles);
    assert_eq!(result.unwrap().results.len(), 4);
    // wake 在送出結果之後
    let deadline = Instant::now() + Duration::from_secs(10);
    while !woke.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(woke.load(Ordering::SeqCst));
}
