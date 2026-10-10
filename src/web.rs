//! 從網路下載檔案（影戲按需求下載的 yt-dlp、deno；只在使用者按下「下載」「更新」時才連網）。
//!
//! - 連線最多等 [`CONNECT_TIMEOUT`]、回應的標頭最多等 [`RESPONSE_TIMEOUT`]。**沒有整個下載的時間限制**：
//!   檢查更新用的 10 秒整體限制會讓 40 MB 的下載在慢一點的網路上失敗。改成讀資料時多久沒收到東西就停（[`Web::stall`]），
//!   使用者也可以隨時取消（每 100 毫秒看一次）。
//! - 讀資料在另一個執行緒：取消、沒有進度時馬上回傳，不等卡住的連線。那個執行緒下一次收到資料、連線斷掉時就結束
//!   （送不出去）；不設讀取的時間上限，因為 ureq 的 `timeout_recv_body` 是整個內容的時間限制，還在下載的慢速網路也會被它切斷。
//!   半開的連線（對方消失、沒有斷線）會讓它一直等到影戲結束：只占一個執行緒與一個連線，不碰任何檔案。
//! - User-Agent `VitaScope/x.y.z`；proxy 照環境變數（`HTTPS_PROXY`、`HTTP_PROXY`、`ALL_PROXY`、`NO_PROXY`）。
//! - 不要求壓縮（`Accept-Encoding: identity`）：下載的大小就是 `Content-Length`，進度才對。
//!
//! 背景執行緒只回傳 [`WebError`]，給使用者看的文字由介面執行緒產生（介面語言是每個執行緒自己的）。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 連線（含 TLS 交握）最多等多久
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 送出請求後，回應的標頭最多等多久
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
/// 讀資料時多久沒收到任何東西就當成斷線
pub const STALL: Duration = Duration::from_secs(60);
/// 每次讀多少
const CHUNK: usize = 64 * 1024;
/// 多久看一次取消
const POLL: Duration = Duration::from_millis(100);

/// 下載失敗的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebError {
    /// 伺服器回應錯誤（HTTP 狀態碼）
    Status(u16),
    /// 連不上、連線中斷、逾時：原文（技術細節，不翻譯；介面加上說明）
    Network(String),
    /// 使用者取消了
    Cancelled,
    /// 很久沒有收到資料
    Stalled,
    /// 檔案比上限大（伺服器說的大小，或實際收到的）
    TooLarge,
    /// 寫入檔案失敗：原文
    Write(String),
}

impl WebError {
    /// 給使用者看的說明（介面執行緒呼叫）
    pub fn message(&self) -> String {
        use crate::{tf, tr};
        match self {
            // GitHub 對未登入的連線有次數限制（以 IP 計算，公司、宿舍的網路可能共用）
            WebError::Status(429) => tr!(
                "伺服器暫時限制連線次數，請稍後再試",
                "The server is rate-limiting; try again later"
            )
            .to_owned(),
            WebError::Status(code) => tf!(
                "伺服器回應錯誤（HTTP {code}）",
                "The server returned an error (HTTP {code})"
            ),
            WebError::Network(e) => tf!("無法連線：{e}", "Couldn't connect: {e}"),
            WebError::Cancelled => tr!("已取消", "Cancelled").to_owned(),
            WebError::Stalled => tr!(
                "下載很久沒有進度，已停止（請確認網路連線）",
                "The download stopped making progress (check your connection)"
            )
            .to_owned(),
            WebError::TooLarge => tr!(
                "檔案大得不合理，已停止下載",
                "The file is unreasonably large; download stopped"
            )
            .to_owned(),
            WebError::Write(e) => tf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"),
        }
    }
}

/// 下載的進度（背景執行緒寫，介面讀）
#[derive(Debug, Default)]
pub struct Progress {
    /// 已經收到幾個位元組
    pub done: AtomicU64,
    /// 總共幾個位元組；0 = 不知道
    pub total: AtomicU64,
}

impl Progress {
    /// (已收到, 總共)；總共不知道時 None
    pub fn get(&self) -> (u64, Option<u64>) {
        let total = self.total.load(Ordering::Relaxed);
        (self.done.load(Ordering::Relaxed), (total > 0).then_some(total))
    }
}

/// 下載用的連線設定
#[derive(Clone)]
pub struct Web {
    agent: ureq::Agent,
    stall: Duration,
}

/// 讀資料的執行緒送來的
enum Msg {
    /// 收到回應：總共幾個位元組（伺服器有說的話）
    Start(Option<u64>),
    Data(Vec<u8>),
    Done,
    Failed(WebError),
}

fn net_error(e: ureq::Error) -> WebError {
    match e {
        ureq::Error::StatusCode(code) => WebError::Status(code),
        e => WebError::Network(e.to_string()),
    }
}

impl Web {
    /// `proxy_from_env`：照環境變數用 proxy（自動測試連本機的伺服器時不用）；
    /// `https_only`：連線與轉址都只能是 https（真的下載時；本機的測試伺服器是 http）
    pub fn new(proxy_from_env: bool, https_only: bool) -> Web {
        let mut config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            // 不限制讀內容的總時間（N-L2）：慢速網路上還在下載的不能被切斷，卡住的由 `download` 的「沒有進度」處理
            .timeout_recv_body(None)
            .user_agent(concat!("VitaScope/", env!("CARGO_PKG_VERSION")))
            .https_only(https_only)
            // 狀態碼自己看（轉址的 3xx、錯誤的 4xx / 5xx）
            .http_status_as_error(false);
        if !proxy_from_env {
            config = config.proxy(None);
        }
        Web {
            agent: config.build().into(),
            stall: STALL,
        }
    }

    /// 測試用：多久沒收到資料就停
    #[doc(hidden)]
    pub fn with_stall(mut self, stall: Duration) -> Self {
        self.stall = stall;
        self
    }

    /// 不跟著轉址，回傳轉去哪裡（`Location`，相對的網址轉成完整的）；不是轉址時 None。4xx、5xx 是錯誤
    pub fn redirect_target(&self, url: &str) -> Result<Option<String>, WebError> {
        let resp = self
            .agent
            .get(url)
            .config()
            .max_redirects(0)
            .build()
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(net_error)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            return Err(WebError::Status(status));
        }
        if !(300..400).contains(&status) {
            return Ok(None);
        }
        let Some(location) = resp.headers().get("location").and_then(|v| v.to_str().ok()) else {
            return Ok(None);
        };
        let base = url::Url::parse(url).map_err(|e| WebError::Network(e.to_string()))?;
        Ok(base.join(location.trim()).ok().map(|u| u.to_string()))
    }

    /// 下載小檔案（檢查碼之類）到記憶體，最多 `cap` 位元組
    pub fn fetch(&self, url: &str, cap: u64, cancel: &AtomicBool) -> Result<Vec<u8>, WebError> {
        let mut out = Vec::new();
        self.download(url, &mut out, cap, &Progress::default(), cancel)?;
        Ok(out)
    }

    /// 下載到 `out`（跟著轉址），最多 `cap` 位元組；進度寫進 `progress`。
    /// `cancel` 變成 true 時馬上回傳 [`WebError::Cancelled`]。回傳總共寫了幾個位元組
    pub fn download(
        &self,
        url: &str,
        out: &mut dyn Write,
        cap: u64,
        progress: &Progress,
        cancel: &AtomicBool,
    ) -> Result<u64, WebError> {
        progress.done.store(0, Ordering::Relaxed);
        progress.total.store(0, Ordering::Relaxed);
        // 讀的執行緒最多先讀好幾塊（寫檔慢時不無限制地堆在記憶體裡）
        let (tx, rx) = mpsc::sync_channel::<Msg>(8);
        let agent = self.agent.clone();
        let target = url.to_owned();
        std::thread::Builder::new()
            .name("vitascope-web-read".into())
            .spawn(move || read_body(&agent, &target, cap, &tx))
            .map_err(|e| WebError::Network(e.to_string()))?;

        let mut written: u64 = 0;
        let mut last = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(WebError::Cancelled);
            }
            match rx.recv_timeout(POLL) {
                Ok(Msg::Start(total)) => {
                    if let Some(t) = total {
                        progress.total.store(t, Ordering::Relaxed);
                    }
                    last = Instant::now();
                }
                Ok(Msg::Data(chunk)) => {
                    written += chunk.len() as u64;
                    if written > cap {
                        return Err(WebError::TooLarge);
                    }
                    out.write_all(&chunk).map_err(|e| WebError::Write(e.to_string()))?;
                    progress.done.store(written, Ordering::Relaxed);
                    last = Instant::now();
                }
                Ok(Msg::Done) => {
                    out.flush().map_err(|e| WebError::Write(e.to_string()))?;
                    return Ok(written);
                }
                Ok(Msg::Failed(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // 連線、等標頭的時間 ureq 自己限制（10 + 20 秒，比這個短）；這裡管的是讀資料時卡住
                    if last.elapsed() >= self.stall {
                        return Err(WebError::Stalled);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(WebError::Network("connection closed".into()));
                }
            }
        }
    }
}

/// 讀資料的執行緒：連線、送出回應的大小、一塊一塊送資料。等的那一邊不要了（取消）就結束
fn read_body(agent: &ureq::Agent, url: &str, cap: u64, tx: &mpsc::SyncSender<Msg>) {
    let resp = match agent.get(url).header("Accept-Encoding", "identity").call() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Msg::Failed(net_error(e)));
            return;
        }
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let _ = tx.send(Msg::Failed(WebError::Status(status)));
        return;
    }
    let total = resp.body().content_length();
    if total.is_some_and(|t| t > cap) {
        let _ = tx.send(Msg::Failed(WebError::TooLarge));
        return;
    }
    if tx.send(Msg::Start(total)).is_err() {
        return;
    }
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(Msg::Done);
                return;
            }
            Ok(n) => {
                if tx.send(Msg::Data(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                let _ = tx.send(Msg::Failed(WebError::Network(e.to_string())));
                return;
            }
        }
    }
}
