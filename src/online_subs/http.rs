//! 線上字幕的 HTTP：介面（[`Http`]）、真的連線（[`UreqHttp`]）、離線（[`OfflineHttp`]，自動測試的預設）
//! 與測試用的假伺服器（[`FakeHttp`]）。
//!
//! - 4xx / 5xx 不當成錯誤：兩個 API 的錯誤原因都在回應內容裡（`http_status_as_error(false)`），由各來源自己解讀。
//! - 兩種連線（R-B7）：
//!   - API（[`Kind::Api`]）：最多轉址 [`MAX_API_REDIRECTS`] 次；整個請求（含轉址）20 秒。
//!   - 暫時的下載連結（[`Kind::Download`]）：最多轉址 [`MAX_DOWNLOAD_REDIRECTS`] 次；整個請求 30 秒。
//! - **轉址自己跟**（ureq 不跟：它換主機時只拿掉 `Authorization`，影戲的 `Api-Key` 會跟著送到別的主機）：
//!   - API 的轉址目的地跟原本的請求是同一個來源（https、主機、埠都一樣）時，標頭照舊（OpenSubtitles 的 vip-api
//!     一定要 JWT；射手網（偽）的 Token 也在 `Authorization`）；
//!   - 換了來源（API），或下載連結的任何轉址：只留 [`SAFE_ON_REDIRECT`] 的標頭（`Accept`），金鑰、登入憑證、
//!     Token 一律不送，之後再轉回原本的主機也不再加回來。新加的秘密標頭不用另外登記，預設就不會跟著轉址；
//!   - 303、以及 301 / 302 的 POST、DELETE 改成不帶內容的 GET（跟瀏覽器、curl 一樣）；307 / 308 照原本的方法與內容，
//!     但只在同一個來源裡跟：POST 的內容（登入的帳號密碼）、DELETE 不送到別的來源，3xx 直接交回去（ureq 也不跟）。
//!   - 兩種都只能用 https（射手網（偽）的 `http://` 下載網址先改成 https，不退回 http：R-C5）；連線最多等 10 秒；
//!     User-Agent 是 OpenSubtitles 規定的 `VitaScope vX.Y.Z`；proxy 照環境變數。這些共同的設定來自
//!     `crate::web::agent_base`（跟下載 yt-dlp 的連線同一份）。
//! - 回應內容最多讀 `limit` 位元組，超過就是 [`Transport::TooLarge`]。
//! - `VITASCOPE_DEBUG_ONLINE=1`：印出方法、主機、路徑與狀態碼（不印查詢字串、標頭：裡面有檔名、金鑰、登入憑證）。

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// 連線（含 TLS 交握）最多等多久（跟下載 yt-dlp 的連線一樣：`crate::web::agent_base`）
pub const CONNECT_TIMEOUT: Duration = crate::web::CONNECT_TIMEOUT;
/// API 的請求整個最多多久
pub const API_TIMEOUT: Duration = Duration::from_secs(20);
/// 下載字幕檔整個最多多久
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// API 最多跟著轉址幾次（OpenSubtitles 的查詢不照規定排序時會轉址一次）
pub const MAX_API_REDIRECTS: u32 = 3;
/// 下載連結最多轉址幾次
pub const MAX_DOWNLOAD_REDIRECTS: u32 = 5;
/// 轉址到別的來源（或下載連結的轉址）時還帶著的標頭（名稱小寫）：都不是秘密
pub const SAFE_ON_REDIRECT: [&str; 1] = ["accept"];
/// OpenSubtitles 規定的 User-Agent 寫法（`App vX.Y.Z`；跟檢查更新的 `VitaScope/x.y.z` 不同）
pub const USER_AGENT: &str = concat!("VitaScope v", env!("CARGO_PKG_VERSION"));

/// HTTP 方法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// 用哪一種連線
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// API：轉址到同一個來源（https、主機、埠）時標頭照舊，換了來源就只留 [`SAFE_ON_REDIRECT`]
    Api,
    /// 暫時的下載連結：任何轉址都只留 [`SAFE_ON_REDIRECT`]
    Download,
}

/// 一個請求
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    /// 另外的標頭（User-Agent 由連線加）
    pub headers: Vec<(String, String)>,
    /// POST 的內容（JSON）
    pub body: Option<String>,
    /// 回應內容最多讀多少位元組
    pub limit: u64,
    pub kind: Kind,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>, kind: Kind, limit: u64) -> Request {
        Request {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            limit,
            kind,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Request {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn json(mut self, body: String) -> Request {
        self.body = Some(body);
        self.header("Content-Type", "application/json")
    }

    /// 某個標頭的值（名稱不分大小寫）
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// 回應
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// 標頭（名稱是小寫）
    pub headers: Vec<(String, String)>,
}

impl Reply {
    /// 狀態碼與內容
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    /// 200 與內容
    pub fn ok(body: impl Into<Vec<u8>>) -> Reply {
        Reply::new(200, body)
    }

    /// 加一個標頭
    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    /// 某個標頭的值（名稱不分大小寫）
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 內容當成文字（不是 UTF-8 的部分換掉）
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// 連線層的失敗（還沒有回應）；各來源再換成 [`super::NetError`]（加上是哪個網站）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// 離線（自動測試的預設：絕不連網）
    Offline,
    /// 逾時
    Timeout,
    /// 連不上、連線中斷：原文（技術細節，不翻譯）
    Connect(String),
    /// 回應內容比上限大
    TooLarge,
    /// 網址（或轉址的目的地）不是 https
    NotHttps,
}

/// 送出請求、拿回狀態碼與內容（4xx、5xx 也是 `Ok`）。在背景執行緒呼叫
pub trait Http: Send + Sync {
    fn send(&self, req: &Request) -> Result<Reply, Transport>;

    /// 是不是離線的（自動測試的預設）
    fn is_offline(&self) -> bool {
        false
    }
}

/// 離線：每個請求都回傳 [`Transport::Offline`]，不碰網路（`Launch` 的預設、介面測試）
#[derive(Debug, Default, Clone, Copy)]
pub struct OfflineHttp;

impl Http for OfflineHttp {
    fn send(&self, _req: &Request) -> Result<Reply, Transport> {
        Err(Transport::Offline)
    }

    fn is_offline(&self) -> bool {
        true
    }
}

/// 真的連線（ureq）
pub struct UreqHttp {
    api: ureq::Agent,
    download: ureq::Agent,
    /// 整個請求（含轉址、讀內容）最多多久
    api_timeout: Duration,
    download_timeout: Duration,
    debug: bool,
}

/// 連線的選項（測試用：本機的測試伺服器是 http、不用 proxy、逾時短一點）
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct UreqOptions {
    pub https_only: bool,
    pub proxy_from_env: bool,
    pub api_timeout: Duration,
    pub download_timeout: Duration,
}

impl Default for UreqOptions {
    fn default() -> Self {
        UreqOptions {
            https_only: true,
            proxy_from_env: true,
            api_timeout: API_TIMEOUT,
            download_timeout: DOWNLOAD_TIMEOUT,
        }
    }
}

impl Default for UreqHttp {
    fn default() -> Self {
        UreqHttp::new()
    }
}

impl UreqHttp {
    /// 正式的設定：只能 https、proxy 照環境變數
    pub fn new() -> UreqHttp {
        UreqHttp::with_options(UreqOptions::default())
    }

    /// 測試用：自己指定選項
    #[doc(hidden)]
    pub fn with_options(o: UreqOptions) -> UreqHttp {
        // 共同的部分（連線逾時、https、proxy、狀態碼）跟 web.rs 一樣；這裡加上 OpenSubtitles 規定的 User-Agent。
        // ureq 不跟轉址（`max_redirects(0)`：3xx 直接回來），由 `call` 自己跟，才管得到每一站送哪些標頭；
        // 整個請求的時間限制也在 `call` 每一站設剩下的時間（含讀內容）
        let agent = || -> ureq::Agent {
            crate::web::agent_base(o.proxy_from_env, o.https_only)
                .user_agent(USER_AGENT)
                .max_redirects(0)
                .build()
                .into()
        };
        UreqHttp {
            api: agent(),
            download: agent(),
            api_timeout: o.api_timeout,
            download_timeout: o.download_timeout,
            debug: std::env::var_os("VITASCOPE_DEBUG_ONLINE").is_some(),
        }
    }

    /// 送出一站（不跟轉址）：`timeout` = 這一站（含讀內容）最多多久
    fn hop(
        agent: &ureq::Agent,
        method: Method,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
        timeout: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        macro_rules! go {
            ($b:expr, $send:ident $(, $arg:expr)?) => {{
                let mut b = $b;
                for (n, v) in headers {
                    b = b.header(n.as_str(), v.as_str());
                }
                b.config().timeout_global(Some(timeout)).build().$send($($arg)?)
            }};
        }
        match method {
            Method::Get => go!(agent.get(url), call),
            Method::Delete => go!(agent.delete(url), call),
            Method::Post => go!(agent.post(url), send, body.unwrap_or_default().as_bytes()),
        }
    }

    /// 送出請求、自己跟轉址（見模組說明）：每一站決定還帶哪些標頭，次數、總時間有上限
    fn call(&self, req: &Request) -> Result<ureq::http::Response<ureq::Body>, Transport> {
        let (agent, max_redirects, budget) = match req.kind {
            Kind::Api => (&self.api, MAX_API_REDIRECTS, self.api_timeout),
            Kind::Download => (&self.download, MAX_DOWNLOAD_REDIRECTS, self.download_timeout),
        };
        let deadline = Instant::now() + budget;
        let first = url::Url::parse(&req.url).map_err(|e| Transport::Connect(e.to_string()))?;
        let mut url = first.clone();
        let mut method = req.method;
        let mut body = req.body.clone();
        let mut headers = req.headers.clone();
        let mut hops = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Transport::Timeout);
            }
            let resp =
                Self::hop(agent, method, url.as_str(), &headers, body.as_deref(), left).map_err(transport_error)?;
            let status = resp.status().as_u16();
            let location = matches!(status, 301 | 302 | 303 | 307 | 308)
                .then(|| resp.headers().get("location").and_then(|v| v.to_str().ok()))
                .flatten();
            let Some(location) = location else {
                return Ok(resp);
            };
            if hops >= max_redirects {
                return Err(Transport::Connect(format!("too many redirects (> {max_redirects})")));
            }
            let next = url.join(location).map_err(|e| Transport::Connect(e.to_string()))?;
            let leaves_home = req.kind == Kind::Download || !same_origin(&first, &next);
            // 307 / 308 要照原本的方法與內容再送一次：POST 的內容（登入的帳號密碼）、DELETE 只送到原本的來源。
            // 換了來源就不跟，把 3xx 交回去（跟 ureq 一樣：它從不跟著轉送有內容的請求）
            if matches!(status, 307 | 308) && method != Method::Get && leaves_home {
                return Ok(resp);
            }
            hops += 1;
            // 303、301 / 302 的 POST、DELETE：改成不帶內容的 GET
            if status == 303 || (matches!(status, 301 | 302) && method != Method::Get) {
                method = Method::Get;
                body = None;
                headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
            }
            // 金鑰、登入憑證、Token 只送到原本的來源（API）；下載連結的轉址一律不送。拿掉之後不再加回來
            if leaves_home {
                headers.retain(|(n, _)| SAFE_ON_REDIRECT.contains(&n.to_ascii_lowercase().as_str()));
            }
            url = next;
        }
    }
}

/// 同一個來源：協定、主機（不分大小寫）、埠（沒寫的照協定的預設）都一樣
pub fn same_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

fn transport_error(e: ureq::Error) -> Transport {
    match e {
        ureq::Error::Timeout(_) => Transport::Timeout,
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => Transport::Timeout,
        ureq::Error::BodyExceedsLimit(_) => Transport::TooLarge,
        ureq::Error::RequireHttpsOnly(_) => Transport::NotHttps,
        e => Transport::Connect(e.to_string()),
    }
}

impl Http for UreqHttp {
    fn send(&self, req: &Request) -> Result<Reply, Transport> {
        let result = self.call(req).and_then(|mut resp| {
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .filter_map(|(n, v)| Some((n.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_owned())))
                .collect();
            // ureq 的上限是「讀到這麼多就算超過」（剛好這麼大的內容讀到結尾時也會失敗）：多給一個位元組再自己比
            let body = resp
                .body_mut()
                .with_config()
                .limit(req.limit.saturating_add(1))
                .read_to_vec()
                .map_err(transport_error)?;
            if body.len() as u64 > req.limit {
                return Err(Transport::TooLarge);
            }
            Ok(Reply { status, body, headers })
        });
        if self.debug {
            // 查詢字串有檔名，標頭有金鑰、登入憑證：只印主機與路徑
            let target = url::Url::parse(&req.url)
                .map(|u| format!("{}{}", u.host_str().unwrap_or_default(), u.path()))
                .unwrap_or_default();
            let outcome = match &result {
                Ok(r) => r.status.to_string(),
                Err(e) => format!("{e:?}"),
            };
            eprintln!("[vitascope] 線上字幕：{} {target} → {outcome}", req.method.as_str());
        }
        result
    }
}

/// 測試用的假伺服器：依「方法 + 網址包含的文字」回應準備好的內容，記下每個請求。
///
/// - 同一個路由準備了好幾個回應時依序用，最後一個之後一直用最後一個。
/// - 符合好幾個路由時用文字最長的那一個；沒有符合的回應 404。
/// - `hold(文字)`：網址包含這段文字的請求先等著，直到 `release(文字)`（下載到一半換影片之類的測試）。
#[doc(hidden)]
#[derive(Default)]
pub struct FakeHttp {
    routes: Mutex<Vec<Route>>,
    log: Mutex<Vec<Request>>,
    held: Mutex<Vec<String>>,
    released: Condvar,
}

struct Route {
    method: Method,
    pattern: String,
    replies: VecDeque<Result<Reply, Transport>>,
}

/// `hold` 最多等多久（測試忘了放行時不會永遠卡住）
const HOLD_LIMIT: Duration = Duration::from_secs(60);

impl FakeHttp {
    pub fn new() -> std::sync::Arc<FakeHttp> {
        std::sync::Arc::new(FakeHttp::default())
    }

    /// 網址包含 `pattern` 的 `method` 請求回應 `reply`（接在這個路由之前準備的回應後面）
    pub fn on(&self, method: Method, pattern: &str, reply: Reply) -> &Self {
        self.push(method, pattern, Ok(reply));
        self
    }

    /// 同上，但是連線失敗
    pub fn fail(&self, method: Method, pattern: &str, error: Transport) -> &Self {
        self.push(method, pattern, Err(error));
        self
    }

    fn push(&self, method: Method, pattern: &str, reply: Result<Reply, Transport>) {
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        match routes.iter_mut().find(|r| r.method == method && r.pattern == pattern) {
            Some(r) => r.replies.push_back(reply),
            None => routes.push(Route {
                method,
                pattern: pattern.to_owned(),
                replies: VecDeque::from([reply]),
            }),
        }
    }

    /// 到目前為止收到的請求
    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 網址包含 `pattern` 的請求
    pub fn requests_to(&self, pattern: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.url.contains(pattern))
            .collect()
    }

    /// 網址包含 `pattern` 的請求先等著（直到 `release`）
    pub fn hold(&self, pattern: &str) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(pattern.to_owned());
    }

    /// 放行 `hold` 的請求
    pub fn release(&self, pattern: &str) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| p != pattern);
        self.released.notify_all();
    }
}

impl Http for FakeHttp {
    fn send(&self, req: &Request) -> Result<Reply, Transport> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(req.clone());
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let start = std::time::Instant::now();
        while held.iter().any(|p| req.url.contains(p.as_str())) && start.elapsed() < HOLD_LIMIT {
            held = self
                .released
                .wait_timeout(held, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(held);
        let reply = {
            let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
            let route = routes
                .iter_mut()
                .filter(|r| r.method == req.method && req.url.contains(r.pattern.as_str()))
                .max_by_key(|r| r.pattern.len());
            match route {
                Some(r) if r.replies.len() > 1 => r.replies.pop_front().unwrap_or(Err(Transport::Offline)),
                Some(r) => r.replies.front().cloned().unwrap_or(Err(Transport::Offline)),
                None => Ok(Reply::new(404, "no fake route")),
            }
        }?;
        if reply.body.len() as u64 > req.limit {
            return Err(Transport::TooLarge);
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_never_answers() {
        let req = Request::new(
            Method::Get,
            "https://api.opensubtitles.com/api/v1/subtitles",
            Kind::Api,
            10,
        );
        assert_eq!(OfflineHttp.send(&req), Err(Transport::Offline));
        assert!(OfflineHttp.is_offline());
        assert!(!FakeHttp::default().is_offline());
    }

    #[test]
    fn fake_routes_queue_and_limit() {
        let fake = FakeHttp::new();
        fake.on(Method::Get, "/a", Reply::ok("1"))
            .on(Method::Get, "/a", Reply::ok("2"))
            .on(Method::Get, "/a/b", Reply::new(410, "gone"))
            .fail(Method::Post, "/a", Transport::Timeout);
        let get = |url: &str| fake.send(&Request::new(Method::Get, url, Kind::Api, 100));
        assert_eq!(get("https://x/a").unwrap().text(), "1");
        assert_eq!(get("https://x/a").unwrap().text(), "2");
        assert_eq!(get("https://x/a").unwrap().text(), "2", "最後一個回應一直用");
        assert_eq!(get("https://x/a/b").unwrap().status, 410, "最長的符合文字優先");
        assert_eq!(get("https://x/c").unwrap().status, 404);
        let post = Request::new(Method::Post, "https://x/a", Kind::Api, 100).json("{}".into());
        assert_eq!(fake.send(&post), Err(Transport::Timeout));
        assert_eq!(fake.requests().len(), 6);
        assert_eq!(fake.requests_to("/a/b").len(), 1);
        assert_eq!(
            fake.requests()[5].header_value("content-type"),
            Some("application/json")
        );
        fake.on(Method::Get, "/big", Reply::ok(vec![0u8; 101]));
        assert_eq!(get("https://x/big"), Err(Transport::TooLarge));
    }

    #[test]
    fn fake_hold_waits_for_release() {
        let fake = FakeHttp::new();
        fake.on(Method::Get, "/slow", Reply::ok("done"));
        fake.hold("/slow");
        let f = fake.clone();
        let t = std::thread::spawn(move || f.send(&Request::new(Method::Get, "https://x/slow", Kind::Download, 100)));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!t.is_finished(), "放行之前要一直等");
        fake.release("/slow");
        assert_eq!(t.join().unwrap().unwrap().text(), "done");
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let api = u("https://api.opensubtitles.com/api/v1/subtitles");
        assert!(same_origin(&api, &u("https://API.OpenSubtitles.com:443/api/v1/x")));
        for other in [
            "https://vip-api.opensubtitles.com/api/v1/x",
            "http://api.opensubtitles.com/api/v1/x",
            "https://api.opensubtitles.com:8443/api/v1/x",
            "https://api.opensubtitles.com.evil.example/api/v1/x",
            "https://evil.example/?api.opensubtitles.com",
        ] {
            assert!(!same_origin(&api, &u(other)), "{other}");
        }
        assert!(!same_origin(
            &u("http://127.0.0.1:1000/a"),
            &u("http://127.0.0.1:1001/a")
        ));
    }

    #[test]
    fn user_agent_follows_the_opensubtitles_rule() {
        // OpenSubtitles：`App vX.Y.Z`，不然回應 403
        assert_eq!(USER_AGENT, format!("VitaScope v{}", env!("CARGO_PKG_VERSION")));
    }
}
