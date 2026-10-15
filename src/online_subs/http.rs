//! 線上字幕的 HTTP：介面（[`Http`]）、真的連線（[`UreqHttp`]）、離線（[`OfflineHttp`]，自動測試的預設）
//! 與測試用的假伺服器（[`FakeHttp`]）。
//!
//! - 4xx / 5xx 不當成錯誤：兩個 API 的錯誤原因都在回應內容裡（`http_status_as_error(false)`），由各來源自己解讀。
//! - 兩種連線（R-B7）：
//!   - API（[`Kind::Api`]）：同一台主機的轉址保留 `Authorization`（OpenSubtitles 的 vip-api 一定要 JWT；
//!     射手網（偽）的 Token 也在這個標頭），換主機就拿掉；最多轉址 [`MAX_API_REDIRECTS`] 次；整個請求 20 秒。
//!   - 暫時的下載連結（[`Kind::Download`]）：轉址一律拿掉 `Authorization`（本來也不送），整個請求 30 秒。
//!   - 兩種都只能用 https（射手網（偽）的 `http://` 下載網址先改成 https，不退回 http：R-C5）；連線最多等 10 秒；
//!     User-Agent 是 OpenSubtitles 規定的 `VitaScope vX.Y.Z`；proxy 照環境變數。這些共同的設定來自
//!     `crate::web::agent_base`（跟下載 yt-dlp 的連線同一份）。
//! - 回應內容最多讀 `limit` 位元組，超過就是 [`Transport::TooLarge`]。
//! - `VITASCOPE_DEBUG_ONLINE=1`：印出方法、主機、路徑與狀態碼（不印查詢字串、標頭：裡面有檔名、金鑰、登入憑證）。

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

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
    /// API：同一台主機的轉址保留 `Authorization`
    Api,
    /// 暫時的下載連結：轉址一律拿掉 `Authorization`
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
        let agent = |kind: Kind| -> ureq::Agent {
            let (timeout, redirects, auth) = match kind {
                Kind::Api => (
                    o.api_timeout,
                    MAX_API_REDIRECTS,
                    ureq::config::RedirectAuthHeaders::SameHost,
                ),
                Kind::Download => (
                    o.download_timeout,
                    MAX_DOWNLOAD_REDIRECTS,
                    ureq::config::RedirectAuthHeaders::Never,
                ),
            };
            // 共同的部分（連線逾時、https、proxy、狀態碼）跟 web.rs 一樣；這裡加上整個請求的時間限制
            // （API 的回應很小，不像 web.rs 的大檔案下載要用「沒有進度」判斷）、轉址規則與 OpenSubtitles 規定的 User-Agent
            crate::web::agent_base(o.proxy_from_env, o.https_only)
                .timeout_global(Some(timeout))
                .user_agent(USER_AGENT)
                .max_redirects(redirects)
                .redirect_auth_headers(auth)
                .build()
                .into()
        };
        UreqHttp {
            api: agent(Kind::Api),
            download: agent(Kind::Download),
            debug: std::env::var_os("VITASCOPE_DEBUG_ONLINE").is_some(),
        }
    }

    fn call(&self, req: &Request) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let agent = match req.kind {
            Kind::Api => &self.api,
            Kind::Download => &self.download,
        };
        match req.method {
            Method::Get => {
                let mut b = agent.get(&req.url);
                for (n, v) in &req.headers {
                    b = b.header(n.as_str(), v.as_str());
                }
                b.call()
            }
            Method::Delete => {
                let mut b = agent.delete(&req.url);
                for (n, v) in &req.headers {
                    b = b.header(n.as_str(), v.as_str());
                }
                b.call()
            }
            Method::Post => {
                let mut b = agent.post(&req.url);
                for (n, v) in &req.headers {
                    b = b.header(n.as_str(), v.as_str());
                }
                b.send(req.body.as_deref().unwrap_or_default().as_bytes())
            }
        }
    }
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
        let result = self.call(req).map_err(transport_error).and_then(|mut resp| {
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
    fn user_agent_follows_the_opensubtitles_rule() {
        // OpenSubtitles：`App vX.Y.Z`，不然回應 403
        assert_eq!(USER_AGENT, format!("VitaScope v{}", env!("CARGO_PKG_VERSION")));
    }
}
