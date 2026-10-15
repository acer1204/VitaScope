//! OpenSubtitles.com REST API v1：搜尋、下載、登入、登出。
//!
//! - 每個請求都帶 `Api-Key`（影戲的金鑰，[`api_key`]）、OpenSubtitles 規定的 User-Agent（`VitaScope vX.Y.Z`）；
//!   登入後帶 `Authorization: Bearer <JWT>`，之後的請求送到登入回應的 `base_url`（vip-api 一定要 JWT）。
//! - 搜尋的參數照官方建議排好（字母順序、小寫、空白寫成 `+`、不送預設值），伺服器才不用轉址（[`query_string`]）。
//! - **登入是下載的前提**（R-A1）：未登入時伺服器拒絕下載（401 / 403）就是 [`NetError::NeedLogin`]；
//!   登入憑證過期（401，或 500 說 invalid）就登出、回傳 [`NetError::LoginExpired`]，**不改用未登入的身分再試**。
//!   `/login` 回 401 時不自動重試（官方規定）。
//! - 下載：`POST /download` 拿暫時的連結（這一步算一次下載次數），再用另一種連線（[`super::http::Kind::Download`]）
//!   抓檔案，不帶金鑰、登入憑證。連結 3 小時內可以再用：之前拿到的連結先試，失效才再要一個（R-B11）。

use super::http::{Kind, Method, Reply, Request};
use super::{
    Account, LoginInfo, NetError, Provider, Quota, ResultId, SearchJob, SearchPage, Session, Site, SubResult, VideoRef,
};
use crate::subs::SubLang;
use std::sync::atomic::{AtomicBool, Ordering};

/// 官方的 API 位址（登入前一律用這個）
pub const OFFICIAL_ROOT: &str = "https://api.opensubtitles.com/api/v1";
/// 登入回應的 `base_url` 只接受這兩個（不讓回應把之後的請求、登入憑證送到別的地方）
pub const BASE_HOSTS: [&str; 2] = ["api.opensubtitles.com", "vip-api.opensubtitles.com"];
/// API 回應（JSON）最多讀多少
pub const API_LIMIT: u64 = 4 * 1024 * 1024;

/// build.rs 產生：`MASK`、`KEY`（XOR 過的金鑰；沒設定時是空的）
mod key {
    include!(concat!(env!("OUT_DIR"), "/opensubtitles_key.rs"));
}

/// 自動測試要不要連到真的網站（`VITASCOPE_ONLINE_TESTS=1`）
pub fn online_tests_enabled() -> bool {
    std::env::var("VITASCOPE_ONLINE_TESTS").is_ok_and(|v| v.trim() == "1")
}

/// 影戲的 OpenSubtitles API 金鑰：建置時放進正式版的（build.rs）；這個版本沒有時 None。
///
/// 開發版（debug）在 `VITASCOPE_ONLINE_TESTS=1` 時，執行時的環境變數 `VITASCOPE_OPENSUBTITLES_KEY` 優先
/// （連真的網站的 `#[ignore]` 測試用）。正式版不看執行時的環境變數：使用者沒有辦法填自己的金鑰（OpenSubtitles 的規定）
pub fn api_key() -> Option<String> {
    choose_key(
        cfg!(debug_assertions),
        online_tests_enabled(),
        std::env::var("VITASCOPE_OPENSUBTITLES_KEY").ok().as_deref(),
        built_key(),
    )
}

/// 建置時放進來的金鑰（build.rs 寫的 `KEY` 用 `MASK` 還原；沒設定時 None）
fn built_key() -> Option<String> {
    unmask(key::KEY, key::MASK)
}

/// 用哪一個金鑰（R-B9）：只有開發版（`debug`）、而且 `VITASCOPE_ONLINE_TESTS=1`（`online_tests`）時，
/// 執行時的環境變數（`runtime`，空白的不算）才優先；其他情況一律是建置時放進來的（`built`）
pub fn choose_key(debug: bool, online_tests: bool, runtime: Option<&str>, built: Option<String>) -> Option<String> {
    match runtime.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) if debug && online_tests => Some(k.to_owned()),
        _ => built,
    }
}

/// 跟 `mask` 逐位元組 XOR 還原金鑰；空的、還原後不是文字的回傳 None
pub fn unmask(masked: &[u8], mask: &[u8]) -> Option<String> {
    if masked.is_empty() || mask.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = masked.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m).collect();
    String::from_utf8(bytes)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

/// 跟 [`unmask`] 相反（測試用；build.rs 做一樣的事）
#[doc(hidden)]
pub fn mask(key: &str, mask: &[u8]) -> Vec<u8> {
    key.bytes().zip(mask.iter().cycle()).map(|(b, m)| b ^ m).collect()
}

/// 搜尋的參數
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub languages: Vec<String>,
    pub moviehash: Option<String>,
    pub query: String,
    pub page: u32,
    pub hearing_impaired: bool,
    pub machine_translated: bool,
}

/// 官方建議的查詢字串：參數依字母排序、全部小寫、空白寫成 `+`、語言碼排序後用逗號接起來、預設值不送
/// （包含聽障字幕、排除機器翻譯、第 1 頁都是預設值）。例：`languages=en,zh-cn,zh-tw&moviehash=a65c16d28845ff55&query=movie+name+2019`
pub fn query_string(q: &Query) -> String {
    let mut params: Vec<(&str, String)> = Vec::new();
    if !q.hearing_impaired {
        params.push(("hearing_impaired", "exclude".into()));
    }
    let mut langs: Vec<String> = q
        .languages
        .iter()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    langs.sort();
    langs.dedup();
    if !langs.is_empty() {
        let encoded: Vec<String> = langs.iter().map(|l| super::encode_query(l, true)).collect();
        params.push(("languages", encoded.join(",")));
    }
    if q.machine_translated {
        params.push(("machine_translated", "include".into()));
    }
    if let Some(h) = &q.moviehash {
        params.push(("moviehash", h.to_ascii_lowercase()));
    }
    if q.page > 1 {
        params.push(("page", q.page.to_string()));
    }
    let text = q.query.trim().to_lowercase();
    if !text.is_empty() {
        params.push(("query", super::encode_query(&text, true)));
    }
    params.sort_by_key(|(k, _)| *k);
    params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// 這次請求送到哪裡：登入後是登入回應的主機；測試把位址指到本機時一律用測試的位址（不會連到真的網站）
fn root_for(env: &super::OnlineEnv, session: Option<&Session>) -> String {
    let root = env.os_root.trim_end_matches('/');
    match session {
        Some(s) if root == OFFICIAL_ROOT => format!("https://{}/api/v1", valid_base(&s.base_url)),
        _ => root.to_owned(),
    }
}

/// 登入回應的 `base_url`：只接受官方的兩台主機，其他的都用 `api.opensubtitles.com`
pub fn valid_base(base: &str) -> &'static str {
    let host = base.trim().trim_start_matches("https://").trim_end_matches('/');
    let host = host.strip_suffix("/api/v1").unwrap_or(host);
    BASE_HOSTS
        .into_iter()
        .find(|h| h.eq_ignore_ascii_case(host))
        .unwrap_or(BASE_HOSTS[0])
}

/// 送出 API 請求：加上金鑰、登入憑證，照規定間隔
fn send(
    env: &super::OnlineEnv,
    req: Request,
    key: &str,
    session: Option<&Session>,
    login: bool,
) -> Result<Reply, NetError> {
    let mut req = req.header("Api-Key", key).header("Accept", "*/*");
    if let Some(s) = session {
        req = req.header("Authorization", format!("Bearer {}", s.token));
    }
    env.throttle.wait(login);
    env.http
        .send(&req)
        .map_err(|e| NetError::transport(Site::OpenSubtitles, e))
}

/// 哪一個 API
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Search,
    Download,
    Login,
}

/// 錯誤的狀態碼換成 [`NetError`]
fn api_error(endpoint: Endpoint, reply: &Reply, had_token: bool) -> NetError {
    let body = reply.text().to_lowercase();
    match reply.status {
        401 if endpoint == Endpoint::Login => NetError::BadLogin,
        // 未登入時拒絕下載：要登入（403 也可能是 User-Agent 不對，那是這個版本的問題）
        401 | 403 if endpoint == Endpoint::Download && !had_token && !body.contains("user-agent") => {
            NetError::NeedLogin
        }
        401 if had_token => NetError::LoginExpired,
        401 | 403 => NetError::Forbidden,
        406 => NetError::Quota {
            reset_at: reset_at(&reply.body),
            signed_in: had_token,
        },
        410 => NetError::LinkExpired,
        429 => NetError::RateLimited(Site::OpenSubtitles),
        // 登入憑證過期或不對：伺服器回 500，JSON 的訊息說 invalid。只認 500 與 JSON 的 `message` / `errors`：
        // 代理伺服器、CDN 的 502 / 503 或錯誤網頁剛好有 invalid 這個字時，不能把使用者登出
        500 if had_token && says_invalid(&reply.body) => NetError::LoginExpired,
        code => NetError::Server {
            site: Site::OpenSubtitles,
            code: u32::from(code),
        },
    }
}

/// JSON 回應的 `message` 或 `errors`（文字或文字的陣列）有 invalid 這個字
fn says_invalid(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let invalid = |v: &serde_json::Value| v.as_str().is_some_and(|t| t.to_ascii_lowercase().contains("invalid"));
    v.get("message").is_some_and(invalid)
        || v.get("errors")
            .is_some_and(|e| invalid(e) || e.as_array().is_some_and(|a| a.iter().any(invalid)))
}

/// 登入過期時登出（記憶體與檔案），錯誤照樣回傳。只登出用 `session` 這個登入憑證的登入：
/// 同時進行的請求晚一點才失敗時，不會把使用者剛剛重新登入的狀態清掉
fn signed_out_if_expired(account: &Account, session: Option<&Session>, e: NetError) -> NetError {
    if e == NetError::LoginExpired
        && let Some(s) = session
    {
        account.expire(&s.token);
    }
    e
}

/// 回應裡的重置時間（`reset_time_utc`）
fn reset_at(body: &[u8]) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    super::parse_utc(v.get("reset_time_utc")?.as_str()?)
}

/// 搜尋（`video.hash` 已經算好）
pub(super) fn search(
    env: &super::OnlineEnv,
    account: &Account,
    job: &SearchJob,
    video: &VideoRef,
) -> Result<SearchPage, NetError> {
    let key = env.os_key.as_deref().ok_or(NetError::NoKey)?;
    let q = Query {
        languages: job.languages.clone(),
        moviehash: video.hash.map(|h| h.hex()),
        query: job.query.clone(),
        page: job.page.max(1),
        hearing_impaired: job.hearing_impaired,
        machine_translated: job.machine_translated,
    };
    if q.query.trim().is_empty() && q.moviehash.is_none() {
        return Err(NetError::QueryTooShort);
    }
    let session = account.session();
    let url = format!("{}/subtitles?{}", root_for(env, session.as_ref()), query_string(&q));
    let reply = send(
        env,
        Request::new(Method::Get, url, Kind::Api, API_LIMIT),
        key,
        session.as_ref(),
        false,
    )?;
    if reply.status != 200 {
        return Err(signed_out_if_expired(
            account,
            session.as_ref(),
            api_error(Endpoint::Search, &reply, session.is_some()),
        ));
    }
    let parsed = parse_search(&reply.body, &job.languages).ok_or(NetError::BadResponse(Site::OpenSubtitles))?;
    Ok(SearchPage {
        provider: Provider::Opensubtitles,
        video: video.clone(),
        query: job.query.clone(),
        page: parsed.page.max(1),
        total_pages: parsed.total_pages.max(parsed.page).max(1),
        total: Some(parsed.total),
        results: parsed.results,
    })
}

/// 解析好的搜尋結果
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub results: Vec<SubResult>,
    pub page: u32,
    pub total_pages: u32,
    pub total: u64,
}

/// OpenSubtitles 的語言碼 → 語言
pub fn lang_of(code: &str) -> SubLang {
    match code.trim().to_ascii_lowercase().as_str() {
        "zh-tw" | "zh-hk" => SubLang::Traditional,
        "zh-cn" => SubLang::Simplified,
        "ze" | "zh" => SubLang::Chinese,
        "en" => SubLang::English,
        "ja" => SubLang::Japanese,
        _ => SubLang::Unknown,
    }
}

/// `/subtitles` 的回應 → 每個檔案一列。排序：跟這個檔案完全相符的在前面，其次照使用者的語言順序，其他照伺服器的順序。
/// 整份不是 JSON（例如公共網路的登入頁）回傳 None；個別看不懂的項目略過
pub fn parse_search(body: &[u8], languages: &[String]) -> Option<Parsed> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let data = v.get("data")?.as_array()?;
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_u64);
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let flag = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_bool).unwrap_or(false);
    let mut results = Vec::new();
    for item in data {
        let Some(a) = item.get("attributes") else { continue };
        let Some(files) = a.get("files").and_then(serde_json::Value::as_array) else {
            continue;
        };
        let code = text(a, "language").to_ascii_lowercase();
        let feature = a
            .get("feature_details")
            .map(|f| {
                let mut s = text(f, "title");
                if s.is_empty() {
                    s = text(f, "movie_name");
                }
                if let Some(y) = num(f, "year").filter(|y| *y > 0) {
                    s.push_str(&format!(" ({y})"));
                }
                if let (Some(se), Some(ep)) = (num(f, "season_number"), num(f, "episode_number"))
                    && (se, ep) != (0, 0)
                {
                    s.push_str(&format!(" S{se:02}E{ep:02}"));
                }
                s
            })
            .unwrap_or_default();
        let uploader = a.get("uploader").map(|u| text(u, "name")).unwrap_or_default();
        let many = files.len() > 1;
        for f in files {
            let Some(file_id) = num(f, "file_id") else { continue };
            let mut r = SubResult::new(Provider::Opensubtitles, ResultId::Os { file_id });
            r.lang = lang_of(&code);
            r.lang_code = code.clone();
            r.name = text(a, "release");
            if r.name.is_empty() {
                r.name = text(f, "file_name");
            }
            r.format = "SRT".into();
            r.downloads = num(a, "download_count");
            r.rating = a.get("ratings").and_then(serde_json::Value::as_f64);
            r.hash_match = flag(a, "moviehash_match");
            r.hearing_impaired = flag(a, "hearing_impaired");
            r.auto_translated = flag(a, "ai_translated");
            r.machine_translated = flag(a, "machine_translated");
            r.trusted = flag(a, "from_trusted");
            r.cd = num(f, "cd_number").filter(|_| many).and_then(|n| u32::try_from(n).ok());
            r.feature = feature.clone();
            r.uploader = uploader.clone();
            r.upload_date = text(a, "upload_date");
            r.page_url = Some(text(a, "url")).filter(|u| u.starts_with("https://"));
            results.push(r);
        }
    }
    let rank = |code: &str| {
        languages
            .iter()
            .position(|l| l.eq_ignore_ascii_case(code))
            .unwrap_or(languages.len())
    };
    results.sort_by_key(|r| (!r.hash_match, rank(&r.lang_code)));
    let page = num(&v, "page").and_then(|n| u32::try_from(n).ok()).unwrap_or(1);
    Some(Parsed {
        total: num(&v, "total_count").unwrap_or(results.len() as u64),
        total_pages: num(&v, "total_pages")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(page),
        page,
        results,
    })
}

/// 下載的結果（連結、次數要跟成功或失敗一起回傳給介面）
pub(super) struct Got {
    pub link: Option<String>,
    pub quota: Option<Quota>,
    pub bytes: Result<Vec<u8>, NetError>,
}

/// 下載一個檔案：先試之前拿到的連結（不花下載次數；上次 410 的連結也先重試一次：R-B11），
/// 失效才 `POST /download` 要一個新的。剛拿到的連結 410 時不馬上再要（那又會花一次）：回傳
/// [`NetError::LinkExpired`] 與這個連結，下一次先重試它
pub(super) fn download(
    env: &super::OnlineEnv,
    account: &Account,
    file_id: u64,
    known_link: Option<&str>,
    cancel: &AtomicBool,
) -> Got {
    if let Some(link) = known_link {
        match fetch_link(env, link) {
            // 過期了：再要一個（這次才算一次下載）
            Err(NetError::LinkExpired) => {}
            bytes => {
                return Got {
                    link: Some(link.to_owned()),
                    quota: None,
                    bytes,
                };
            }
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Got {
            link: None,
            quota: None,
            bytes: Err(NetError::Cancelled),
        };
    }
    match request_link(env, account, file_id) {
        Ok((link, quota)) => {
            if let Some(q) = quota {
                account.set_quota(q);
            }
            let bytes = fetch_link(env, &link);
            Got {
                link: Some(link),
                quota,
                bytes,
            }
        }
        Err((e, quota)) => {
            if let Some(q) = quota {
                account.set_quota(q);
            }
            Got {
                link: None,
                quota,
                bytes: Err(e),
            }
        }
    }
}

/// `POST /download`：拿暫時的下載連結與剩下的次數
fn request_link(
    env: &super::OnlineEnv,
    account: &Account,
    file_id: u64,
) -> Result<(String, Option<Quota>), (NetError, Option<Quota>)> {
    let key = env.os_key.as_deref().ok_or((NetError::NoKey, None))?;
    let session = account.session();
    let url = format!("{}/download", root_for(env, session.as_ref()));
    let body = serde_json::json!({ "file_id": file_id }).to_string();
    let req = Request::new(Method::Post, url, Kind::Api, API_LIMIT).json(body);
    let reply = send(env, req, key, session.as_ref(), false).map_err(|e| (e, None))?;
    let v: Option<serde_json::Value> = serde_json::from_slice(&reply.body).ok();
    let reset = reset_at(&reply.body);
    if reply.status != 200 {
        let e = signed_out_if_expired(
            account,
            session.as_ref(),
            api_error(Endpoint::Download, &reply, session.is_some()),
        );
        // 次數用完：伺服器說的剩下次數可能是負的或沒有，一律當成 0
        let quota = matches!(e, NetError::Quota { .. }).then_some(Quota {
            remaining: 0,
            reset_at: reset,
        });
        return Err((e, quota));
    }
    let link = v
        .as_ref()
        .and_then(|v| v.get("link"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|l| l.starts_with("https://") || l.starts_with("http://"))
        .map(str::to_owned)
        .ok_or((NetError::BadResponse(Site::OpenSubtitles), None))?;
    let remaining = v
        .as_ref()
        .and_then(|v| v.get("remaining"))
        .and_then(serde_json::Value::as_i64);
    let quota = remaining.map(|remaining| Quota {
        remaining,
        reset_at: reset,
    });
    Ok((link, quota))
}

/// 抓暫時的下載連結（不帶金鑰、登入憑證；另一種連線：轉址一律拿掉 `Authorization`）
fn fetch_link(env: &super::OnlineEnv, link: &str) -> Result<Vec<u8>, NetError> {
    let req = Request::new(Method::Get, link, Kind::Download, super::MAX_SUBTITLE_SIZE);
    let reply = env
        .http
        .send(&req)
        .map_err(|e| NetError::transport(Site::OpenSubtitles, e))?;
    match reply.status {
        200..=299 => Ok(reply.body),
        404 | 410 => Err(NetError::LinkExpired),
        429 => Err(NetError::RateLimited(Site::OpenSubtitles)),
        code => Err(NetError::Server {
            site: Site::OpenSubtitles,
            code: u32::from(code),
        }),
    }
}

/// 登入：成功時記住登入憑證（記憶體與快取資料夾的檔案；密碼不存）
pub(super) fn login(
    env: &super::OnlineEnv,
    account: &Account,
    user: &str,
    password: &str,
) -> Result<LoginInfo, NetError> {
    let key = env.os_key.as_deref().ok_or(NetError::NoKey)?;
    let user = user.trim();
    if user.is_empty() || password.is_empty() {
        return Err(NetError::BadLogin);
    }
    // 登入一律送到官方的位址（之前登入的主機不一定還對）
    let url = format!("{}/login", env.os_root.trim_end_matches('/'));
    let body = serde_json::json!({ "username": user, "password": password }).to_string();
    let req = Request::new(Method::Post, url, Kind::Api, API_LIMIT).json(body);
    let reply = send(env, req, key, None, true)?;
    if reply.status != 200 {
        return Err(api_error(Endpoint::Login, &reply, false));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&reply.body).map_err(|_| NetError::BadResponse(Site::OpenSubtitles))?;
    let token = v
        .get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(NetError::BadResponse(Site::OpenSubtitles))?;
    let base = valid_base(
        v.get("base_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    );
    let info = v.get("user");
    let info = LoginInfo {
        user: user.to_owned(),
        allowed_downloads: info
            .and_then(|u| u.get("allowed_downloads"))
            .and_then(serde_json::Value::as_i64),
        vip: info
            .and_then(|u| u.get("vip"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    };
    account.sign_in(Session {
        token: token.to_owned(),
        base_url: base.to_owned(),
        user: user.to_owned(),
        expires: super::now_unix() + super::SESSION_LIFETIME.as_secs(),
    });
    Ok(info)
}

/// 登出：通知伺服器（盡力而為，失敗也算登出），清掉記憶體與檔案
pub(super) fn logout(env: &super::OnlineEnv, account: &Account) {
    if let (Some(session), Some(key)) = (account.session(), env.os_key.as_deref()) {
        let url = format!("{}/logout", root_for(env, Some(&session)));
        let req = Request::new(Method::Delete, url, Kind::Api, API_LIMIT);
        let _ = send(env, req, key, Some(&session), false);
    }
    account.sign_out();
}

#[cfg(test)]
mod tests {
    /// build.rs 寫出來的 `KEY` / `MASK` 還原得出建置時的 `VITASCOPE_OPENSUBTITLES_KEY`（去掉前後空白；沒設定是 None）。
    /// 平常的 `cargo test` 沒有金鑰；要測有金鑰的路徑：`VITASCOPE_OPENSUBTITLES_KEY=" fake-key-for-tests " cargo test --lib online_subs`
    #[test]
    fn the_built_key_is_the_build_time_variable() {
        let want = option_env!("VITASCOPE_OPENSUBTITLES_KEY")
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned);
        let got = super::built_key();
        // 不用 assert_eq!：失敗時不能把金鑰印到 CI 的記錄裡
        assert!(
            got == want,
            "建置時的金鑰還原不出來（build.rs 寫的 MASK、KEY 對不起來）"
        );
        assert_eq!(super::key::KEY.is_empty(), want.is_none());
        if let Some(k) = &want {
            assert!(super::key::KEY != k.as_bytes(), "執行檔裡不能直接看到金鑰");
            assert!(!super::key::MASK.is_empty());
        }
    }
}
