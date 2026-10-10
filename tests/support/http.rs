//! 測試用的本機 HTTP 伺服器（只用標準函式庫，只聽 127.0.0.1，不會碰到外面的網路）。
//!
//! - `/f/<路徑>`：`samples/generated` 底下的檔案；支援 `Range`（206）、HEAD，Content-Type 依副檔名
//! - `/status/<代碼>`：回應這個狀態碼
//! - `/slow`：收下請求、永遠不回應（測連線逾時）
//! - `/html`：一個網頁（不是影片）
//! - `/m3u`：網路上的播放清單：兩個 `/f/` 的網址，中間夾一個 `file:///` 的項目（匯入時要拿掉）
//! - `/hlslive/<路徑>`：像直播一樣的 HLS：`.m3u8` 拿掉結尾的 `#EXT-X-ENDLIST`（和 VOD 的標記），片段照常送。
//!   播放器當成直播（沒有總長度、不能跳轉），一直重新讀清單等新的片段
//! - `/stall/<百分比>/<路徑>`：跟 `/f/` 一樣，但檔案前面這個百分比之後的資料永遠不送（連線開著、一直等），
//!   從那之後開始的請求也一樣；從檔案後半開始的請求（MP4 放在最後的 moov）照常送。播放到那裡就會「緩衝中」
//! - `/norange/<路徑>`：跟 `/f/` 一樣的檔案，但不支援 `Range`（不管有沒有都送整個檔案、沒有 `Accept-Ranges`），
//!   像很多小型伺服器、NAS。播放器只能從頭讀（不能跳轉），但總長度照樣知道：不是直播
//! - 測試指定的固定回應（`put`）：假的 GitHub 發佈（轉址、檢查碼、要下載的檔案），可以送到一半就停住
//!
//! 每個請求的方法、路徑、標頭都記下來（`requests()`），測試用來確認播放器送了什麼。
//! 一個連線一個執行緒；每個回應之後關閉連線（`Connection: close`）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 收到的一個請求
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// 路徑（含查詢字串，照收到的樣子）
    pub path: String,
    /// 標頭（名稱、值；名稱照收到的大小寫）
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// 某個標頭的值（名稱不分大小寫）
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// 測試指定的固定回應
#[derive(Debug, Clone)]
pub struct Canned {
    pub status: u16,
    /// 另外的標頭（`Content-Length`、`Connection` 由伺服器加）
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 送了這麼多位元組之後就不再送（連線開著，直到伺服器關掉）：下載的進度、取消、沒有進度的測試用
    pub stall_after: Option<usize>,
    /// 不送 `Content-Length`（內容到連線關閉為止）：下載的那一邊事先不知道大小
    pub no_length: bool,
}

impl Canned {
    /// 200 與內容
    pub fn ok(body: impl Into<Vec<u8>>) -> Canned {
        Canned {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
            stall_after: None,
            no_length: false,
        }
    }

    /// 302 轉到 `location`
    pub fn redirect(location: &str) -> Canned {
        Canned {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
            stall_after: None,
            no_length: false,
        }
    }

    /// 只有狀態碼
    pub fn status(code: u16) -> Canned {
        Canned {
            status: code,
            headers: Vec::new(),
            body: format!("status {code}\n").into_bytes(),
            stall_after: None,
            no_length: false,
        }
    }

    /// 送了 `n` 個位元組就停住
    pub fn stall_after(mut self, n: usize) -> Canned {
        self.stall_after = Some(n);
        self
    }

    /// 不說大小（沒有 `Content-Length`，送完就關閉連線）
    pub fn without_length(mut self) -> Canned {
        self.no_length = true;
        self
    }
}

struct Shared {
    root: PathBuf,
    log: Mutex<Vec<Request>>,
    stop: AtomicBool,
    canned: Mutex<HashMap<String, Canned>>,
}

pub struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

/// `/slow` 最多拖多久（伺服器關掉時也會結束）
const SLOW_LIMIT: Duration = Duration::from_secs(120);

impl Server {
    /// 在 127.0.0.1 的任一個空的埠開始服務，檔案來自 `samples/generated`
    pub fn start() -> Server {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated");
        let listener = TcpListener::bind("127.0.0.1:0").expect("無法開啟測試用的 HTTP 伺服器");
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Shared {
            root,
            log: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            canned: Mutex::new(HashMap::new()),
        });
        let s = shared.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let s = s.clone();
                std::thread::spawn(move || {
                    let _ = serve(conn, &s);
                });
            }
        });
        Server { addr, shared }
    }

    /// 完整的網址，例如 `url("/f/common/a.mp4")`
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// `samples/generated` 底下的檔案的網址
    pub fn file_url(&self, rel: &str) -> String {
        let path = self.shared.root.join(rel);
        assert!(
            path.exists(),
            "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
            path.display()
        );
        self.url(&format!("/f/{rel}"))
    }

    /// 路徑 `path`（不含查詢字串）固定回應 `c`（取代同一個路徑之前指定的）
    pub fn put(&self, path: &str, c: Canned) {
        self.shared.canned.lock().unwrap().insert(path.to_owned(), c);
    }

    /// 拿掉 `put` 指定的回應（之後回應 404）
    pub fn remove(&self, path: &str) {
        self.shared.canned.lock().unwrap().remove(path);
    }

    /// 到目前為止收到的請求
    pub fn requests(&self) -> Vec<Request> {
        self.shared.log.lock().unwrap().clone()
    }

    /// 路徑開頭是 `prefix` 的請求
    pub fn requests_to(&self, prefix: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.starts_with(prefix))
            .collect()
    }

    /// 等到有路徑開頭是 `prefix` 的請求（最多 `timeout`）
    pub fn wait_request(&self, prefix: &str, timeout: Duration) -> Option<Request> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.requests_to(prefix).into_iter().next() {
                return Some(r);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // 叫醒還在等連線的 accept
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
    }
}

fn serve(conn: TcpStream, s: &Shared) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((n, v)) = h.split_once(':') {
            headers.push((n.to_owned(), v.trim_start().to_owned()));
        }
    }
    let req = Request { method, path, headers };
    s.log.lock().unwrap().push(req.clone());
    let mut out = conn;
    let head = req.method == "HEAD";
    // 查詢字串、網頁裡的位置（`#…`，播放器不一定會拿掉）不算路徑
    let route = req.path.split(['?', '#']).next().unwrap_or("");
    let canned = s.canned.lock().unwrap().get(route).cloned();
    if let Some(c) = canned {
        return send_canned(&mut out, &c, head, s);
    }
    if let Some(rel) = route.strip_prefix("/f/") {
        return send_file(&mut out, &s.root, rel, Some(req.header("Range")), head, None);
    }
    if let Some(rel) = route.strip_prefix("/norange/") {
        return send_file(&mut out, &s.root, rel, None, head, None);
    }
    if let Some(rel) = route.strip_prefix("/hlslive/") {
        if !rel.ends_with(".m3u8") {
            return send_file(&mut out, &s.root, rel, Some(req.header("Range")), head, None);
        }
        let Some(text) = sample_path(&s.root, rel).and_then(|p| std::fs::read_to_string(p).ok()) else {
            return send(&mut out, 404, "text/plain", b"not found\n", head);
        };
        let live: String = text
            .lines()
            .filter(|l| !l.starts_with("#EXT-X-ENDLIST") && !l.starts_with("#EXT-X-PLAYLIST-TYPE"))
            .map(|l| format!("{l}\n"))
            .collect();
        return send(&mut out, 200, content_type(Path::new(rel)), live.as_bytes(), head);
    }
    if let Some((pct, rel)) = route.strip_prefix("/stall/").and_then(|r| r.split_once('/')) {
        let pct: u64 = pct.parse().unwrap_or(50);
        return send_file(&mut out, &s.root, rel, Some(req.header("Range")), head, Some((pct, s)));
    }
    if let Some(code) = route.strip_prefix("/status/") {
        let code: u16 = code.parse().unwrap_or(500);
        return send(
            &mut out,
            code,
            "text/plain",
            format!("status {code}\n").as_bytes(),
            head,
        );
    }
    match route {
        "/slow" => {
            // 收下請求，一直不回應（直到逾時或伺服器關掉）
            let start = Instant::now();
            while !s.stop.load(Ordering::SeqCst) && start.elapsed() < SLOW_LIMIT {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(())
        }
        "/html" => send(
            &mut out,
            200,
            "text/html; charset=utf-8",
            b"<!doctype html><html><head><title>Not a video</title></head><body><p>hello</p></body></html>\n",
            head,
        ),
        "/m3u" => {
            let base = format!("http://{}", out.local_addr()?);
            let body = format!(
                "#EXTM3U\n#EXTINF:-1,第一個\n{base}/f/common/mp4_h264_aac.mp4\n#EXTINF:-1,本機檔案\nfile:///etc/passwd\n\
                 #EXTINF:-1,第二個\n{base}/f/common/mkv_h264_aac_srt.mkv\n"
            );
            send(&mut out, 200, "audio/x-mpegurl", body.as_bytes(), head)
        }
        _ => send(&mut out, 404, "text/plain", b"not found\n", head),
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v" | "m4s") => "video/mp4",
        Some("m4a") => "audio/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("ts") => "video/mp2t",
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("mpd") => "application/dash+xml",
        Some("vtt") => "text/vtt",
        Some("srt") => "application/x-subrip",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn send(out: &mut TcpStream, code: u16, ctype: &str, body: &[u8], head: bool) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(code),
        body.len()
    )?;
    if !head {
        out.write_all(body)?;
    }
    out.flush()
}

/// 測試指定的固定回應；`stall_after` 的位置之後不再送，連線開著到伺服器關掉
fn send_canned(out: &mut TcpStream, c: &Canned, head: bool, s: &Shared) -> std::io::Result<()> {
    let mut text = format!("HTTP/1.1 {} {}\r\nConnection: close\r\n", c.status, reason(c.status));
    if !c.no_length {
        text += &format!("Content-Length: {}\r\n", c.body.len());
    }
    for (n, v) in &c.headers {
        text += &format!("{n}: {v}\r\n");
    }
    text += "\r\n";
    out.write_all(text.as_bytes())?;
    if head {
        return out.flush();
    }
    let n = c.stall_after.unwrap_or(c.body.len()).min(c.body.len());
    out.write_all(&c.body[..n])?;
    out.flush()?;
    if c.stall_after.is_some() {
        let begin = Instant::now();
        while !s.stop.load(Ordering::SeqCst) && begin.elapsed() < SLOW_LIMIT {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

/// `Range: bytes=a-b`、`bytes=a-`、`bytes=-n` → (起點, 終點（含）)；看不懂時 None（整個檔案）
fn parse_range(range: &str, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = range.trim().strip_prefix("bytes=")?;
    let (a, b) = spec.split(',').next()?.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let r = match (a.parse::<u64>().ok(), b.parse::<u64>().ok()) {
        (Some(a), Some(b)) if a <= b => (a, b.min(len.saturating_sub(1))),
        (Some(a), None) if b.is_empty() => (a, len.saturating_sub(1)),
        (None, Some(n)) if a.is_empty() && n > 0 => (len.saturating_sub(n), len.saturating_sub(1)),
        _ => return None,
    };
    Some(if r.0 >= len { Err(()) } else { Ok(r) })
}

/// 樣本資料夾裡的檔案（路徑裡有 `..`、空的一段的不算）
fn sample_path(root: &Path, rel: &str) -> Option<PathBuf> {
    (!rel.split('/').any(|seg| seg == ".." || seg.is_empty())).then(|| root.join(rel))
}

/// `range` = 請求的 `Range`：外面的 None 是不支援 Range（不送 `Accept-Ranges`），`Some(None)` 是支援但這次沒有要求。
/// `stall` = (百分比, 伺服器)：檔案前面這個百分比之後的資料不送，連線開著等到伺服器關掉
fn send_file(
    out: &mut TcpStream,
    root: &Path,
    rel: &str,
    range: Option<Option<&str>>,
    head: bool,
    stall: Option<(u64, &Shared)>,
) -> std::io::Result<()> {
    // 只服務樣本資料夾裡的檔案
    let Some(path) = sample_path(root, rel) else {
        return send(out, 404, "text/plain", b"not found\n", head);
    };
    let Ok(mut file) = std::fs::File::open(&path) else {
        return send(out, 404, "text/plain", b"not found\n", head);
    };
    let len = file.metadata()?.len();
    let ctype = content_type(&path);
    let (code, start, end) = match range.flatten().and_then(|r| parse_range(r, len)) {
        Some(Ok((a, b))) => (206, a, b),
        Some(Err(())) => {
            write!(
                out,
                "HTTP/1.1 416 {}\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                reason(416)
            )?;
            return out.flush();
        }
        None => (200, 0, len.saturating_sub(1)),
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut head_text = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {count}\r\nConnection: close\r\n",
        reason(code)
    );
    if range.is_some() {
        head_text += "Accept-Ranges: bytes\r\n";
    }
    if code == 206 {
        head_text += &format!("Content-Range: bytes {start}-{end}/{len}\r\n");
    }
    head_text += "\r\n";
    out.write_all(head_text.as_bytes())?;
    if head {
        return out.flush();
    }
    file.seek(SeekFrom::Start(start))?;
    // 卡住：從檔案後半開始的請求照常送（MP4 放在最後的 moov），其他的送到卡住的位置就停
    let limit = stall
        .filter(|_| start < len / 2)
        .map(|(pct, _)| (len * pct / 100).saturating_sub(start).min(count));
    // 播放器跳轉時會中途關掉連線：寫不出去就結束
    std::io::copy(&mut file.take(limit.unwrap_or(count)), out)?;
    out.flush()?;
    if let (Some(_), Some((_, s))) = (limit, stall) {
        // 不關連線、也不再送：播放器一直等（直到逾時、跳轉關掉連線、或伺服器關掉）
        let begin = Instant::now();
        while !s.stop.load(Ordering::SeqCst) && begin.elapsed() < SLOW_LIMIT {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}
