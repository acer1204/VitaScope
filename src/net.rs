//! 網路：開啟網址（HTTP、HLS、DASH）的設定、對應的 mpv 選項、網址的檢查與顯示、連線失敗的說明。
//!
//! 這裡只有純邏輯，不碰 mpv：選項由 `Player::apply_net` 送出，失敗的說明由 `Player` 開檔失敗時呼叫
//! [`classify_failure`]（mpv 的記錄裡有網路的錯誤時，換成看得懂的原因）。
//!
//! 網址的來源不同，能開的網址也不同（[`Origin`]）：使用者自己輸入、從命令列給的照舊什麼都能開；
//! 從檔案（本機的 .m3u）、網路上的播放清單、網站影片的資料讀到的網址，只開網路串流，
//! 不開 `edl://`、`av://` 這類能讀本機檔案或執行濾鏡的特殊網址（mpv 只依「誰開的」限制，我們自己開的它都接受）。

use crate::ytdl::{Browser, CodecPref, ListMode, SitePrefs, SiteQuality};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 網路快取的選項（MB）：demuxer-max-bytes；往回的快取（demuxer-max-back-bytes）是它的三分之一
pub const CACHE_SIZES: [u32; 4] = [64, 150, 400, 1000];
/// 連線逾時（秒）的範圍
pub const TIMEOUT_RANGE: std::ops::RangeInclusive<u32> = 5..=120;

/// 網路：開啟網址（HTTP、HLS、DASH 的串流）與網站影片（yt-dlp）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetSettings {
    /// 用 yt-dlp 播放網站影片（關掉時影片網站的網址說明要打開，其他網頁照一般的網址開）
    pub ytdl: bool,
    /// 使用者指定的 yt-dlp；None = 自動尋找（影戲下載的 → 影戲旁邊 → PATH → 常見位置）
    pub ytdl_path: Option<PathBuf>,
    /// 網站影片的預設畫質
    pub quality: SiteQuality,
    /// 優先的影像編碼
    pub codec: CodecPref,
    /// 載入網站提供的字幕
    pub site_subs: bool,
    /// 也載入網站自動產生的字幕
    pub auto_subs: bool,
    /// 從哪個瀏覽器讀 Cookie（要登入才能看的影片）；None = 不使用
    pub cookies_from: Option<Browser>,
    /// 網址同時是影片和播放清單時播哪個
    pub list_mode: ListMode,
    /// HLS / DASH 一開始選哪個畫質（mpv 的 hls-bitrate）
    pub hls_bitrate: HlsBitrate,
    /// 連線中斷時自動重新連線
    pub reconnect: bool,
    /// 網路快取（MB；只能是 [`CACHE_SIZES`] 之一）
    pub cache_mb: u32,
    /// 連線逾時（秒，5–120）
    pub timeout_secs: u32,
    /// 檢查網站憑證（tls-verify；Linux tar.gz 用的系統 libmpv 預設不檢查，一律明確設定）
    pub tls_verify: bool,
    /// 記住開啟過的網址（最近開啟、續播）
    pub remember_urls: bool,
    /// User-Agent；空白 = 播放引擎預設（libmpv）
    pub user_agent: String,
    /// Referer；空白 = 不送
    pub referrer: String,
    /// 其他 HTTP 標頭，每一項「名稱: 值」
    pub headers: Vec<String>,
    /// HTTP proxy（http://主機:埠）；空白 = 系統的 http_proxy 環境變數
    pub proxy: String,
}

impl Default for NetSettings {
    fn default() -> Self {
        Self {
            ytdl: true,
            ytdl_path: None,
            quality: SiteQuality::Best,
            codec: CodecPref::Auto,
            site_subs: true,
            auto_subs: false,
            cookies_from: None,
            list_mode: ListMode::Video,
            hls_bitrate: HlsBitrate::Max,
            reconnect: true,
            cache_mb: 150,
            timeout_secs: 30,
            tls_verify: true,
            remember_urls: true,
            user_agent: String::new(),
            referrer: String::new(),
            headers: Vec::new(),
            proxy: String::new(),
        }
    }
}

/// HLS / DASH 有好幾種畫質時，一開始選哪一個
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HlsBitrate {
    /// 最高
    #[default]
    Max,
    /// 最低（網路慢的時候）
    Min,
}

impl HlsBitrate {
    pub const ALL: [HlsBitrate; 2] = [HlsBitrate::Max, HlsBitrate::Min];

    pub fn label(self) -> &'static str {
        match self {
            HlsBitrate::Max => crate::tr!("最高", "Highest"),
            HlsBitrate::Min => crate::tr!("最低", "Lowest"),
        }
    }

    /// mpv 的 hls-bitrate 值
    fn mpv(self) -> &'static str {
        match self {
            HlsBitrate::Max => "max",
            HlsBitrate::Min => "min",
        }
    }
}

impl NetSettings {
    /// 讀檔後、套用前整理：快取對齊選項、逾時拉回範圍；含換行之類控制字元的文字整項拿掉
    /// （HTTP 標頭用換行分隔，值裡有換行就能多塞一個標頭）
    pub fn sanitized(mut self) -> Self {
        self.cache_mb = snap_cache(self.cache_mb);
        self.timeout_secs = self.timeout_secs.clamp(*TIMEOUT_RANGE.start(), *TIMEOUT_RANGE.end());
        for text in [&mut self.user_agent, &mut self.referrer, &mut self.proxy] {
            *text = clean_line(text).unwrap_or_default();
        }
        self.headers = self.headers.iter().filter_map(|h| clean_header(h)).collect();
        // 空白的路徑 = 自動尋找
        if self.ytdl_path.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
            self.ytdl_path = None;
        }
        self
    }

    /// 網站影片的偏好（交給 yt-dlp 的部分）
    pub fn site_prefs(&self) -> SitePrefs {
        SitePrefs {
            quality: self.quality,
            codec: self.codec,
            site_subs: self.site_subs,
            auto_subs: self.auto_subs,
            cookies_from: self.cookies_from,
            list_mode: self.list_mode,
        }
    }
}

/// 最接近的快取選項（一樣近時取小的）
fn snap_cache(mb: u32) -> u32 {
    CACHE_SIZES.into_iter().min_by_key(|c| c.abs_diff(mb)).unwrap_or(150)
}

/// 去掉前後空白；有控制字元（換行、NUL…）時 None
fn clean_line(text: &str) -> Option<String> {
    let t = text.trim();
    (!t.chars().any(char::is_control)).then(|| t.to_owned())
}

/// 「設定 → 網路 → 進階」的一行標頭能不能送出（是「名稱: 值」、沒有控制字元）；空白行不算錯
pub fn header_line_ok(line: &str) -> bool {
    line.trim().is_empty() || clean_header(line).is_some()
}

/// proxy 是 SOCKS（播放引擎只支援 HTTP proxy）
pub fn is_socks_proxy(proxy: &str) -> bool {
    proxy.trim().get(..5).is_some_and(|p| p.eq_ignore_ascii_case("socks"))
}

/// 一行 HTTP 標頭「名稱: 值」整理成固定的寫法；名稱不是合法的標頭名稱、沒有冒號、有控制字元時 None
fn clean_header(line: &str) -> Option<String> {
    let line = clean_line(line)?;
    let (name, value) = line.split_once(':')?;
    let name = name.trim();
    // RFC 9110 的 token：英數字和這些符號（不能有空白）
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) {
        return None;
    }
    Some(format!("{name}: {}", value.trim()))
}

// ───────────── mpv 選項 ─────────────

/// 這個播放引擎的預設值（啟動時讀 `option-info/…/default-value`；使用者清空設定時還原成這些）
#[derive(Debug, Clone, PartialEq)]
pub struct NetDefaults {
    pub user_agent: String,
}

impl Default for NetDefaults {
    fn default() -> Self {
        Self {
            user_agent: "libmpv".into(),
        }
    }
}

/// 影戲管理的單一值網路選項（只送有變的，見 `Player::apply_net`）
pub const MANAGED: [&str; 7] = [
    "user-agent",
    "http-proxy",
    "network-timeout",
    "tls-verify",
    "demuxer-max-bytes",
    "demuxer-max-back-bytes",
    "hls-bitrate",
];
/// 影戲管理的清單選項：HTTP 標頭（字串清單；項目裡可能有逗號）、交給 FFmpeg 的連線選項（鍵值清單）
pub const HEADERS: &str = "http-header-fields";
pub const LAVF: &str = "stream-lavf-o";

/// 網路設定對應的 mpv 選項（整組；每次都是全部的受管理選項，清空的設定送回預設值）
#[derive(Debug, Clone, PartialEq)]
pub struct NetOptions {
    /// 單一值的選項（[`MANAGED`]，照這個順序）
    pub scalars: Vec<(&'static str, String)>,
    /// http-header-fields：Referer 在第一個，接著其他標頭
    pub headers: Vec<String>,
    /// stream-lavf-o 的值（`名稱=值,名稱=值`；名稱和值都沒有逗號）
    pub lavf: String,
}

/// FFmpeg 的重新連線（stream-lavf-o）：
/// - 連線中斷時重連，直播之類不能跳轉的串流也是（reconnect_streamed），重試之間最多等 5 秒。
/// - 不用 `reconnect_on_network_error`：它連「一開始就連不上」也重試（找不到伺服器、被拒絕、逾時、憑證錯誤各重試 4 次、
///   中間共等 4 秒以上），每次都可能等滿連線逾時，連不上的網址要等好幾分鐘才說明原因。
/// - 關掉時要明確寫 `reconnect=0`：mpv 自己預設就開著重新連線（stream_lavf.c），清空這個選項不會關掉
const RECONNECT_ON: &str = "reconnect=1,reconnect_streamed=1,reconnect_delay_max=5";
const RECONNECT_OFF: &str = "reconnect=0";

pub fn mpv_options(s: &NetSettings, d: &NetDefaults) -> NetOptions {
    let s = s.clone().sanitized();
    let yes_no = |b: bool| if b { "yes" } else { "no" }.to_owned();
    let user_agent = if s.user_agent.is_empty() {
        d.user_agent.clone()
    } else {
        s.user_agent
    };
    let scalars = vec![
        ("user-agent", user_agent),
        // 空字串 mpv 不用（FFmpeg 照樣看 http_proxy 環境變數）
        ("http-proxy", s.proxy),
        ("network-timeout", s.timeout_secs.to_string()),
        // 一律明確設定：系統的 libmpv 0.37（FFmpeg 6.1）預設不檢查憑證
        ("tls-verify", yes_no(s.tls_verify)),
        ("demuxer-max-bytes", format!("{}MiB", s.cache_mb)),
        ("demuxer-max-back-bytes", format!("{}MiB", s.cache_mb / 3)),
        ("hls-bitrate", s.hls_bitrate.mpv().to_owned()),
    ];
    // Referer 放進標頭清單、不用 mpv 的 referrer 選項：那個選項是空字串時 mpv 照樣送出空的「Referer:」
    let headers = (!s.referrer.is_empty())
        .then(|| format!("Referer: {}", s.referrer))
        .into_iter()
        .chain(s.headers)
        .collect();
    NetOptions {
        scalars,
        headers,
        lavf: if s.reconnect { RECONNECT_ON } else { RECONNECT_OFF }.to_owned(),
    }
}

// ───────────── 網址 ─────────────

/// 網址是從哪裡來的：決定能開哪些網址
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 使用者自己給的：命令列、單一執行個體轉送、開檔對話框、「開啟網址」、貼上。什麼都能開（跟以前一樣）
    User,
    /// 本機播放清單檔（.m3u）裡的項目：本機檔案、`file://`、網路串流，加上 IPTV 的 udp / rtp 群播
    LocalFile,
    /// 網路上的播放清單、網址捷徑檔：只有網路串流
    Remote,
    /// 網站影片（yt-dlp）資料裡的網址：只有 http、https
    Site,
}

/// 網路串流的協定（mpv 認為「安全」、播放引擎有的協定，不含 data、httpproxy）
pub const NETWORK: [&str; 18] = [
    "http", "https", "dav", "davs", "webdav", "webdavs", "mms", "mmsh", "mmshttp", "mmst", "rtmp", "rtmps", "rtmpt",
    "rtmpts", "rtsp", "rtsps", "rtp", "srtp",
];

/// 非使用者給的網址最長多少（位元組）
const MAX_URL: usize = 8 * 1024;
/// 存進播放紀錄的網址最長多少（位元組）
const MAX_STORED_URL: usize = 4 * 1024;

/// `scheme://` 的 scheme（小寫）；不是網址（本機路徑、`C:\`）時 None
pub fn scheme(s: &str) -> Option<String> {
    if !crate::m3u::is_url(s) {
        return None;
    }
    s.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase())
}

/// 網路串流（[`NETWORK`] 的協定）；`av://`、`edl://`、`memory://` 之類 mpv 自己的網址不算
pub fn is_network(s: &str) -> bool {
    scheme(s).is_some_and(|sc| NETWORK.contains(&sc.as_str()))
}

/// 從 `origin` 來的這個網址（或路徑）能不能開
pub fn allowed(s: &str, origin: Origin) -> bool {
    if origin == Origin::User {
        return true;
    }
    // 檔案、網路上讀到的：有控制字元的一律不開
    if s.is_empty() || s.chars().any(char::is_control) {
        return false;
    }
    let sc = scheme(s);
    // 網址裡有空白（多半是解析錯了）、太長的不開；本機路徑本來就常有空白（「My Videos」、全形空白），照開
    if sc.is_some() && (s.len() > MAX_URL || s.chars().any(char::is_whitespace)) {
        return false;
    }
    let network = sc.as_deref().is_some_and(|sc| NETWORK.contains(&sc));
    match origin {
        Origin::User => true,
        // 本機路徑（不是網址）、file://、網路串流、IPTV 群播
        Origin::LocalFile => match sc.as_deref() {
            None => true,
            Some(sc) => network || matches!(sc, "file" | "udp"),
        },
        Origin::Remote => network,
        Origin::Site => matches!(sc.as_deref(), Some("http" | "https")),
    }
}

/// 「開啟網址」對話框、貼上的文字不是能開的網址
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// 沒有內容
    Empty,
    /// 不是網址
    NotUrl,
    /// 不支援的協定（寫在文字裡的樣子，例如 `ftp://`、`javascript:`）
    Unsupported(String),
}

impl InputError {
    pub fn message(&self) -> String {
        match self {
            InputError::Empty => String::new(),
            InputError::NotUrl => crate::tr!("這不是網址", "This is not a URL").to_owned(),
            InputError::Unsupported(s) => crate::tf!("不支援 {s} 網址", "{s} URLs are not supported"),
        }
    }
}

/// 使用者輸入、貼上的文字 → 網址（一行一個或以空白分隔的好幾個）。
/// 前後的引號、`<>` 拿掉；沒寫協定、看起來像網址（`youtube.com/watch?v=…`、`www.…`）的補上 `https://`。
/// 整行是一個網址（路徑裡有空白，例如 `https://x/My Video.mp4`，後面沒有別的網址）就當一個，空白換成 `%20`；
/// 不然每一個都要是網路串流（或 IPTV 的 udp），有一個不是就整個不接受
pub fn parse_input(text: &str) -> Result<Vec<String>, InputError> {
    let text = unwrap_quotes(text.trim());
    if text.is_empty() {
        return Err(InputError::Empty);
    }
    if let Some(url) = one_url_with_spaces(text) {
        return Ok(vec![url]);
    }
    text.split_whitespace().map(|t| one_url(unwrap_quotes(t))).collect()
}

/// 只有一行、開頭是有協定的網址、後面以空白分開的部分都不像網址：是路徑裡有空白的一個網址。
/// 空白照網址的寫法換成 `%xx`（FFmpeg 不會自己換，原樣送出的請求會壞掉）
fn one_url_with_spaces(text: &str) -> Option<String> {
    if text.contains(['\n', '\r']) {
        return None;
    }
    let mut tokens = text.split_whitespace();
    let first = tokens.next()?;
    let rest: Vec<&str> = tokens.collect();
    if rest.is_empty() || !one_url(first).is_ok_and(|u| u == first) {
        return None;
    }
    let other_url = |t: &str| {
        let t = unwrap_quotes(t);
        scheme(t).is_some()
            || t.split_once(':')
                .is_some_and(|(h, _)| h.len() >= 2 && is_scheme_name(h))
            || looks_like_host(t)
    };
    if rest.iter().any(|t| other_url(t)) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_whitespace() {
            let mut buf = [0; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// 一對包住整段的引號或 `<>` 拿掉
fn unwrap_quotes(s: &str) -> &str {
    for (open, close) in [('"', '"'), ('\'', '\''), ('<', '>'), ('“', '”'), ('「', '」')] {
        if let Some(inner) = s.strip_prefix(open).and_then(|r| r.strip_suffix(close)) {
            return inner.trim();
        }
    }
    s
}

fn one_url(t: &str) -> Result<String, InputError> {
    if t.is_empty() {
        return Err(InputError::NotUrl);
    }
    if let Some(sc) = scheme(t) {
        return if NETWORK.contains(&sc.as_str()) || sc == "udp" {
            Ok(t.to_owned())
        } else {
            Err(InputError::Unsupported(format!("{sc}://")))
        };
    }
    // 「名稱:」開頭、後面不是埠號：mailto:、javascript: 這類沒有「//」的網址
    // （一個字母的是 Windows 的磁碟代號 C:）
    if let Some((head, rest)) = t.split_once(':')
        && head.len() >= 2
        && is_scheme_name(head)
        && !rest.starts_with(|c: char| c.is_ascii_digit())
    {
        return Err(InputError::Unsupported(format!("{}:", head.to_ascii_lowercase())));
    }
    if looks_like_host(t) {
        return Ok(format!("https://{t}"));
    }
    Err(InputError::NotUrl)
}

/// RFC 3986 的 scheme：英文字母開頭，接著英數字、`+`、`-`、`.`
fn is_scheme_name(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// 沒寫協定的網址：開頭是主機名稱（有點的網域、`localhost`）或 IPv4，後面可以有埠號、路徑。
/// 只有「名稱.mp4」這樣、最後一段是影音、字幕、清單副檔名的是檔名，不是網址（`a.mp4` 不變成 `https://a.mp4`）
fn looks_like_host(t: &str) -> bool {
    let end = t.find(['/', '?', '#']).unwrap_or(t.len());
    let authority = &t[..end];
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (h, true),
        Some(_) => return false,
        None => (authority, false),
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let file_name = {
        let p = std::path::Path::new(host);
        crate::formats::media_kind(p).is_some() || crate::formats::is_subtitle(p) || crate::formats::is_playlist(p)
    };
    if file_name && end == t.len() && !port && !host.to_ascii_lowercase().starts_with("www.") {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_alphanumeric() || c == '-'))
        // 最後一段（頂級網域）不能全是數字，除非整個是 IPv4（1.2.3.4）
        && (labels.last().is_some_and(|tld| tld.chars().any(|c| !c.is_ascii_digit()))
            || (labels.len() == 4 && labels.iter().all(|l| l.parse::<u8>().is_ok())))
}

/// 網址的主機名稱（小寫）；看不懂時 None
pub fn host(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    u.host_str()
        .filter(|h| !h.is_empty())
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase())
}

/// 顯示用的名稱：知道標題就用標題，不然用網址路徑的最後一段（解開 %xx），再不然用主機名稱
pub fn display_name(url: &str, title: Option<&str>) -> String {
    if let Some(t) = title.map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        && !t.is_empty()
    {
        return t;
    }
    let parsed = url::Url::parse(url).ok();
    let last = parsed
        .as_ref()
        .and_then(|u| u.path_segments())
        .and_then(|mut segs| segs.rfind(|s| !s.is_empty()))
        .map(crate::m3u::percent_decode)
        .filter(|s| !s.trim().is_empty());
    last.or_else(|| host(url)).unwrap_or_else(|| url.to_owned())
}

/// 查詢參數的名稱像帳號密碼、權杖、簽章（不記進播放紀錄）
fn secret_query_key(key: &str) -> bool {
    const PARTS: [&str; 7] = [
        "password",
        "passwd",
        "token",
        "secret",
        "signature",
        "apikey",
        "session",
    ];
    const WORDS: [&str; 6] = ["pass", "pwd", "key", "sig", "auth", "sid"];
    let k = key.to_ascii_lowercase();
    PARTS.iter().any(|p| k.contains(p))
        || k.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| WORDS.contains(&w))
}

/// 能不能記進播放紀錄（最近開啟、續播）：網址裡有帳號密碼（`user:pass@`、像密碼或權杖的查詢參數、
/// Xtream 類 IPTV 的 `/live/帳號/密碼/頻道` 路徑）、`data:` 網址、太長的不記（照樣能播）
pub fn storable(url: &str) -> bool {
    if url.len() > MAX_STORED_URL {
        return false;
    }
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    if u.scheme() == "data" || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    if u.query_pairs().any(|(k, _)| secret_query_key(&k)) {
        return false;
    }
    // Xtream Codes 的串流網址：/live|movie|series/帳號/密碼/編號.副檔名
    let segs: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
    !(segs.len() >= 4
        && ["live", "movie", "series"].contains(&segs[0].to_ascii_lowercase().as_str())
        && segs[1..].iter().all(|s| !s.is_empty()))
}

/// 續播、書籤用的代號（播放紀錄的鍵）：網路串流的網址去掉 `#` 之後的部分（只是網頁裡的位置，伺服器收不到）。
/// `site` = 網站影片的（擷取器, 影片代號），yt-dlp 解析出來時傳入（`NetInfo::resume_key`）：同一部影片的不同網址
/// （`youtu.be/x`、`watch?v=x&t=90`）是同一個代號 `ytdl://擷取器/代號`。
/// 不是網路串流（本機檔案、`av://` 之類）時 None：本機檔案用路徑本身，mpv 自己的網址不續播
pub fn resume_key(url: &str, site: Option<(&str, &str)>) -> Option<String> {
    if let Some((extractor, id)) = site.filter(|(e, i)| !e.is_empty() && !i.is_empty()) {
        return Some(format!("ytdl://{}/{id}", extractor.to_ascii_lowercase()));
    }
    if !is_network(url) {
        return None;
    }
    Some(url.split_once('#').map_or(url, |(base, _)| base).to_owned())
}

/// 只能即時收看的串流協定（RTSP、RTMP、MMS、RTP）：不能跳轉就是直播
const LIVE_SCHEMES: [&str; 11] = [
    "mms", "mmsh", "mmshttp", "mmst", "rtmp", "rtmps", "rtmpt", "rtmpts", "rtsp", "rtsps", "rtp",
];

/// 網路串流載入時判斷是不是直播。`duration` = mpv 的總長度（沒有、不是正數時 None），`seekable` = 能不能跳轉，
/// `file_format` = mpv 的 `file-format`（`hls`、`dash`、`mkv`…）。
/// - 沒有總長度：直播（網路電台、直播的 RTSP 之類）。
/// - 有總長度但不能跳轉：只有 HLS、DASH 與 RTSP、RTMP 之類才是直播。直播的 HLS 也有 FFmpeg 用開頭那一段估的很短的總長度，
///   只能靠「不能跳轉」分辨。一般的 HTTP 檔案不能跳轉是伺服器不支援 Range（很多小型伺服器、NAS），總長度是真的：不是直播
pub fn is_live(url: &str, duration: Option<f64>, seekable: bool, file_format: Option<&str>) -> bool {
    if duration.is_none_or(|d| !d.is_finite() || d <= 0.0) {
        return true;
    }
    if seekable {
        return false;
    }
    let adaptive = file_format
        .and_then(|f| f.split(',').next())
        .is_some_and(|f| matches!(f.trim(), "hls" | "applehttp" | "dash"));
    adaptive || scheme(url).is_some_and(|sc| LIVE_SCHEMES.contains(&sc.as_str()))
}

/// 網路上的播放清單（IPTV 的 `.m3u` 網址之類）展開後的項目：mpv 讀完清單、結束這個網址（EndFile 的原因是 redirect）時，
/// 從 mpv 的 `playlist` 讀出來、只留網路串流（[`Origin::Remote`]）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemotePlaylist {
    /// 播放清單本身的網址
    pub source: String,
    /// （網址, 清單寫的標題），照清單的順序
    pub entries: Vec<(String, Option<String>)>,
    /// 從第幾個開始播（mpv 選的那一個；它被拿掉了就是它後面第一個留下的）
    pub start: usize,
    /// 拿掉了幾個（本機檔案、`edl://` 之類不能從網路上的清單開的）
    pub dropped: usize,
}

/// mpv 的 `playlist`（JSON：`[{filename, title, current, playlist-path}]`）→ 留下能開的項目。看不懂的 JSON 當成空的清單。
/// 清單本身的網址看項目的 `playlist-path`（mpv 0.37 起有）：mpv 讀完清單馬上就開第一個項目，
/// 處理到開始的事件時讀的 `path` 可能已經是第一個項目。沒有 `playlist-path` 時用 `fallback`
pub fn remote_playlist(fallback: &str, playlist_json: &str) -> RemotePlaylist {
    #[derive(Deserialize)]
    struct Entry {
        filename: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        current: bool,
        #[serde(default, rename = "playlist-path")]
        playlist_path: Option<String>,
    }
    let all: Vec<Entry> = serde_json::from_str(playlist_json).unwrap_or_default();
    let current = all.iter().position(|e| e.current).unwrap_or(0);
    let source = all
        .get(current)
        .and_then(|e| e.playlist_path.clone())
        .or_else(|| all.iter().find_map(|e| e.playlist_path.clone()))
        .unwrap_or_else(|| fallback.to_owned());
    let mut out = RemotePlaylist {
        source,
        ..Default::default()
    };
    let mut start = None;
    for (i, e) in all.into_iter().enumerate() {
        if !allowed(&e.filename, Origin::Remote) {
            out.dropped += 1;
            continue;
        }
        if i >= current && start.is_none() {
            start = Some(out.entries.len());
        }
        let title = e.title.filter(|t| !t.trim().is_empty());
        out.entries.push((e.filename, title));
    }
    out.start = start.unwrap_or(0);
    out
}

/// 標題最長幾個字（太長的截掉；選單、清單放不下）
const MAX_TITLE_CHARS: usize = 300;

/// 值得記下的標題（m3u 的 `#EXTINF`、mpv 的 media-title）：空白整理成一個、太長的截掉；
/// 空的、跟沒有標題時顯示的名稱一樣（例如 m3u 存檔時自動寫的檔名）的不算。
/// v0.3.0 存清單時網址寫的是網址最後一段去掉副檔名（`…/index.m3u8` 寫 `index`），那也不算：
/// 不然還原舊的清單後，這個自動寫的名稱會蓋過影片真正的標題
pub fn useful_title(url: &str, title: &str) -> Option<String> {
    let t: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    let old_auto = std::path::Path::new(url)
        .file_stem()
        .is_some_and(|stem| stem.to_string_lossy() == t);
    (!t.is_empty() && t != display_name(url, None) && !old_auto).then_some(t)
}

/// 網路串流的標題（mpv 的 media-title）。網路電台（Icecast、Shoutcast）沒有標題時，media-title 是正在播的歌名
/// （`icy-title`，每首歌都會變），不能當成電台的標題：改用電台名稱（`icy-name`），沒有就不記
pub fn stream_title<'a>(media_title: &'a str, icy_title: Option<&str>, icy_name: Option<&'a str>) -> Option<&'a str> {
    match icy_title.map(str::trim) {
        Some(song) if !song.is_empty() && song == media_title.trim() => {
            icy_name.map(str::trim).filter(|n| !n.is_empty())
        }
        _ => Some(media_title),
    }
}

/// 貼上的文字只有一個檔名（沒有資料夾，副檔名是影音、字幕或播放清單）：macOS 的 Finder 複製檔案時
/// 另外放的文字就是這樣，開不了（不知道在哪個資料夾）
pub fn bare_file_name(text: &str) -> bool {
    let t = unwrap_quotes(text.trim());
    let p = std::path::Path::new(t);
    !t.is_empty()
        && !t.contains(['\n', '\r', '/', '\\'])
        && (crate::formats::media_kind(p).is_some() || crate::formats::is_subtitle(p) || crate::formats::is_playlist(p))
}

/// 貼上的文字是本機檔案的路徑（一行一個，前後的引號拿掉；也接受 `file://` 網址）。
/// 不檢查檔案在不在：網路磁碟連不上時檢查會卡住畫面，打不開時 mpv 會說明原因。
/// 看起來是完整路徑才算：Windows 是磁碟代號（`C:\`）或網路路徑（`\\server\share`），其他系統是 `/` 或 `~/` 開頭。
/// 有一行不是就整個不算（None）
pub fn pasted_paths(text: &str) -> Option<Vec<std::path::PathBuf>> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from);
    pasted_paths_for(text, cfg!(windows), home.as_deref())
}

fn pasted_paths_for(text: &str, windows: bool, home: Option<&std::path::Path>) -> Option<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let line = unwrap_quotes(line);
        if line.is_empty() || line.chars().any(char::is_control) {
            return None;
        }
        if line.len() > 7 && line.get(..7).is_some_and(|h| h.eq_ignore_ascii_case("file://")) {
            paths.push(crate::m3u::file_url_to_path(&line[7..]));
            continue;
        }
        let b = line.as_bytes();
        let path = if windows {
            let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
            let unc = b.len() > 2 && b.starts_with(b"\\\\") && b[2] != b'\\';
            (drive || unc).then(|| std::path::PathBuf::from(line))?
        } else if line.starts_with('/') {
            std::path::PathBuf::from(line)
        } else if line == "~" || line.starts_with("~/") {
            home?.join(line.trim_start_matches('~').trim_start_matches('/'))
        } else {
            return None;
        };
        paths.push(path);
    }
    (!paths.is_empty()).then_some(paths)
}

// ───────────── 連線失敗的說明 ─────────────

/// 開網址失敗的原因（從 mpv、FFmpeg 的記錄判斷）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetFailure {
    /// 找不到伺服器（DNS）
    HostNotFound,
    /// 伺服器拒絕連線
    Refused,
    /// 連線逾時
    TimedOut,
    /// 伺服器回應 HTTP 錯誤
    Http(u16),
    /// Windows（Schannel）查不到憑證是否已被撤銷：網路擋住了憑證的撤銷清單
    RevocationOffline,
    /// 網站的憑證不受信任
    Untrusted,
    /// 播放引擎不支援這個協定（小寫的 scheme）
    Protocol(String),
    /// 這個網址是網頁，不是影片檔
    WebPage,
}

/// 記錄裡的文字 → 原因。依序比對，第一個符合的算數
const UNTRUSTED: [&str; 6] = [
    "SNI or certificate check failed",
    "0x80090325",
    "0x800b0109",
    "certificate verify failed",
    "Invalid certificate chain",
    "NOT trusted",
];

/// 開網址失敗時，記錄（`log`，mpv 的錯誤、FFmpeg 的 HTTP 錯誤，一行一筆）裡看得出的原因；
/// `url` 是開的網址。不是網路的錯誤時 None（照一般的開檔失敗說明）
pub fn classify_failure(log: &str, url: &str) -> Option<NetFailure> {
    let has = |s: &str| log.contains(s);
    let sc = scheme(url)?;
    if has("0x80092013") || has("0x80092012") {
        return Some(NetFailure::RevocationOffline);
    }
    if UNTRUSTED.iter().any(|s| has(s)) {
        return Some(NetFailure::Untrusted);
    }
    if has("Failed to resolve hostname") {
        return Some(NetFailure::HostNotFound);
    }
    if has("Connection refused") || has("Error number -10061") {
        return Some(NetFailure::Refused);
    }
    if has("timed out") || has("Error number -138") || has("Error number -10060") {
        return Some(NetFailure::TimedOut);
    }
    if let Some(code) = http_error(log) {
        return Some(NetFailure::Http(code));
    }
    if has("Protocol not found") || has("No protocol handler found") {
        return Some(NetFailure::Protocol(sc));
    }
    // 網頁：mpv 認不出格式（0.37、0.41 都是這一句）。「No video or audio streams」不算：
    // 字幕檔之類能讀、只是沒有影音的網址也是這一句，照一般的「沒有可播放的影像或聲音」
    if matches!(sc.as_str(), "http" | "https") && has("Failed to recognize file format") {
        return Some(NetFailure::WebPage);
    }
    None
}

/// 記錄裡第一個「HTTP error 404 Not Found」的狀態碼
fn http_error(log: &str) -> Option<u16> {
    log.split("HTTP error ").skip(1).find_map(|rest| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok().filter(|c| (100..1000).contains(c))
    })
}

impl NetFailure {
    /// 給使用者看的說明；`timeout_secs` 是目前的連線逾時（逾時的說明用）
    pub fn message(&self, timeout_secs: f64) -> String {
        let reason = match self {
            NetFailure::HostNotFound => crate::tr!(
                "找不到伺服器（請確認網址與網路連線）",
                "Server not found (check the address and your connection)"
            )
            .to_owned(),
            NetFailure::Refused => crate::tr!("伺服器拒絕連線", "The server refused the connection").to_owned(),
            NetFailure::TimedOut => crate::tf!(
                "連線逾時（{timeout_secs} 秒內沒有回應）",
                "The connection timed out (no response within {timeout_secs} s)"
            ),
            NetFailure::Http(code @ (401 | 403)) => crate::tf!(
                "伺服器拒絕存取（HTTP {code}）：網址可能過期或需要登入",
                "Access denied (HTTP {code}): the link may have expired or need a login"
            ),
            NetFailure::Http(code @ (404 | 410)) => crate::tf!(
                "找不到這個網址的內容（HTTP {code}）",
                "Nothing found at this address (HTTP {code})"
            ),
            NetFailure::Http(429) => crate::tr!(
                "伺服器暫時限制連線次數（HTTP 429），請稍後再試",
                "The server is rate-limiting (HTTP 429); try again later"
            )
            .to_owned(),
            NetFailure::Http(code) => {
                crate::tf!(
                    "伺服器回應錯誤（HTTP {code}）",
                    "The server returned an error (HTTP {code})"
                )
            }
            NetFailure::RevocationOffline => crate::tr!(
                "無法確認網站憑證是否已被撤銷：網路可能擋住了憑證檢查（可以在「設定 → 網路」暫時關閉「檢查網站憑證」）",
                "Couldn't check whether the site's certificate was revoked; your network may block the check \
                 (you can turn off \"Check website certificates\" in Settings → Network for now)"
            )
            .to_owned(),
            NetFailure::Untrusted => {
                crate::tr!("網站的憑證不受信任", "The site's certificate isn't trusted").to_owned()
            }
            NetFailure::Protocol(sc) => crate::tf!(
                "播放引擎不支援 {sc}:// 網址",
                "The playback engine doesn't support {sc}:// URLs"
            ),
            NetFailure::WebPage => crate::tr!(
                "這個網址是網頁，不是影片檔",
                "This address is a web page, not a video file"
            )
            .to_owned(),
        };
        crate::tf!("無法開啟網址：{reason}", "Can't open the URL: {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_sanitizing() {
        let d = NetSettings::default();
        assert_eq!(d.hls_bitrate, HlsBitrate::Max);
        assert!(d.reconnect && d.tls_verify && d.remember_urls);
        assert_eq!((d.cache_mb, d.timeout_secs), (150, 30));
        assert!(d.user_agent.is_empty() && d.referrer.is_empty() && d.headers.is_empty() && d.proxy.is_empty());
        assert_eq!(d.clone().sanitized(), d, "預設值整理後不變");
        let s = NetSettings {
            cache_mb: 300,
            timeout_secs: 1,
            user_agent: "  Agent/1 \r\n".into(),
            referrer: "http://a/\r\nX-Evil: 1".into(),
            proxy: "http://p:8080\nX-Evil: 1".into(),
            headers: vec![
                "X-Ok:  a, b ".into(),
                "X-Bad: 1\r\nX-Injected: 2".into(),
                "no colon".into(),
                "Bad Name: 1".into(),
                ": empty name".into(),
                "X-Nul: a\0b".into(),
                "  Cookie:c=d".into(),
            ],
            ..NetSettings::default()
        }
        .sanitized();
        assert_eq!(s.cache_mb, 400);
        assert_eq!(s.timeout_secs, 5);
        assert_eq!(s.user_agent, "Agent/1", "前後的空白、換行拿掉");
        assert_eq!(s.referrer, "", "有換行的整項拿掉");
        assert_eq!(s.proxy, "");
        assert_eq!(s.headers, ["X-Ok: a, b", "Cookie: c=d"]);
        for (mb, want) in [(0, 64), (64, 64), (107, 64), (108, 150), (150, 150), (5000, 1000)] {
            assert_eq!(snap_cache(mb), want, "{mb}");
        }
        let s = NetSettings {
            timeout_secs: 999,
            ..NetSettings::default()
        };
        assert_eq!(s.sanitized().timeout_secs, 120);
    }

    #[test]
    fn mpv_options_cover_every_setting() {
        let d = NetDefaults {
            user_agent: "libmpv-x".into(),
        };
        let o = mpv_options(&NetSettings::default(), &d);
        assert_eq!(
            o.scalars,
            [
                ("user-agent", "libmpv-x".to_owned()),
                ("http-proxy", String::new()),
                ("network-timeout", "30".into()),
                ("tls-verify", "yes".into()),
                ("demuxer-max-bytes", "150MiB".into()),
                ("demuxer-max-back-bytes", "50MiB".into()),
                ("hls-bitrate", "max".into()),
            ]
        );
        let names: Vec<&str> = o.scalars.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, MANAGED, "每個受管理的選項都送，照 MANAGED 的順序");
        assert!(o.headers.is_empty());
        assert_eq!(o.lavf, "reconnect=1,reconnect_streamed=1,reconnect_delay_max=5");
        let s = NetSettings {
            hls_bitrate: HlsBitrate::Min,
            reconnect: false,
            cache_mb: 1000,
            timeout_secs: 7,
            tls_verify: false,
            user_agent: "VitaScope-Test".into(),
            referrer: "https://ref.example/page?a=1,2".into(),
            headers: vec!["X-A: 1, 2".into(), "bad\nheader: x".into()],
            proxy: "http://127.0.0.1:3128".into(),
            ..NetSettings::default()
        };
        let o = mpv_options(&s, &d);
        let get = |n: &str| o.scalars.iter().find(|(k, _)| *k == n).unwrap().1.clone();
        assert_eq!(get("user-agent"), "VitaScope-Test");
        assert_eq!(get("http-proxy"), "http://127.0.0.1:3128");
        assert_eq!(get("network-timeout"), "7");
        assert_eq!(get("tls-verify"), "no");
        assert_eq!(get("demuxer-max-bytes"), "1000MiB");
        assert_eq!(get("demuxer-max-back-bytes"), "333MiB");
        assert_eq!(get("hls-bitrate"), "min");
        // Referer 放在標頭清單的第一個（不用 mpv 的 referrer 選項）；壞掉的標頭不送
        assert_eq!(o.headers, ["Referer: https://ref.example/page?a=1,2", "X-A: 1, 2"]);
        assert_eq!(o.lavf, "reconnect=0", "關掉要明確寫 0（mpv 預設就開著）");
    }

    #[test]
    fn mpv_options_never_retry_failed_connects() {
        // reconnect_on_network_error 會讓連不上的網址重試好幾次（每次都可能等滿逾時）
        for reconnect in [true, false] {
            let s = NetSettings {
                reconnect,
                ..NetSettings::default()
            };
            let o = mpv_options(&s, &NetDefaults::default());
            assert!(!o.lavf.contains("reconnect_on_network_error"), "{}", o.lavf);
            assert!(!o.lavf.contains("reconnect_on_http_error"), "{}", o.lavf);
            // 重試之間最多等 5 秒
            if let Some(max) = o.lavf.split(',').find_map(|kv| kv.strip_prefix("reconnect_delay_max=")) {
                assert!(max.parse::<u32>().unwrap() <= 5, "{max}");
            }
            // 名稱、值都不能有逗號（stream-lavf-o 用逗號分隔）
            assert!(o.lavf.split(',').all(|kv| kv.split_once('=').is_some()), "{}", o.lavf);
        }
    }

    #[test]
    fn origins_allow_different_urls() {
        use Origin::*;
        let cases: &[(&str, [bool; 4])] = &[
            // User、LocalFile、Remote、Site
            ("https://example.com/a.mp4", [true, true, true, true]),
            ("HTTP://EXAMPLE.COM/A.MP4", [true, true, true, true]),
            ("rtmp://live.example/app/key", [true, true, true, false]),
            ("rtsp://cam.local/stream", [true, true, true, false]),
            ("udp://239.0.0.1:1234", [true, true, false, false]),
            ("rtp://239.0.0.1:5004", [true, true, true, false]),
            ("file:///C:/a.mp4", [true, true, false, false]),
            ("C:\\影片\\a.mp4", [true, true, false, false]),
            ("/home/me/a.mp4", [true, true, false, false]),
            // 本機路徑有空白（半形、全形）照開
            ("C:\\Users\\me\\My Videos\\a b.mp4", [true, true, false, false]),
            ("/home/me/影片\u{3000}第1集.mp4", [true, true, false, false]),
            ("D:\\a\tb.mp4", [true, false, false, false]),
            ("edl://%3%a.mp4", [true, false, false, false]),
            ("av://lavfi:testsrc", [true, false, false, false]),
            ("memory://#EXTM3U", [true, false, false, false]),
            ("lavf://file:/etc/passwd", [true, false, false, false]),
            ("ftp://example.com/a.mp4", [true, false, false, false]),
            ("data:text/plain,hi", [true, true, false, false]),
            ("https://example.com/a b.mp4", [true, false, false, false]),
            ("https://example.com/a\r\n.mp4", [true, false, false, false]),
            ("", [true, false, false, false]),
        ];
        for (url, want) in cases {
            let got = [User, LocalFile, Remote, Site].map(|o| allowed(url, o));
            assert_eq!(&got, want, "{url:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(9000));
        assert!(allowed(&long, User) && !allowed(&long, Remote));
        assert!(is_network("https://x/") && is_network("RTSP://x/") && !is_network("av://lavfi:x"));
        assert!(!is_network("C:\\a.mp4") && !is_network("edl://x"));
        assert_eq!(scheme("HTTPS://x"), Some("https".into()));
        assert_eq!(scheme("C:\\x"), None);
    }

    #[test]
    fn parse_input_accepts_urls_and_rejects_the_rest() {
        let ok = |t: &str| parse_input(t).unwrap_or_else(|e| panic!("{t:?}: {e:?}"));
        assert_eq!(ok("https://example.com/v.m3u8"), ["https://example.com/v.m3u8"]);
        assert_eq!(ok("  \"https://example.com/a.mp4\"  "), ["https://example.com/a.mp4"]);
        assert_eq!(ok("<http://x.example/a>"), ["http://x.example/a"]);
        // 沒寫協定、看起來像網址
        assert_eq!(ok("youtube.com/watch?v=abc"), ["https://youtube.com/watch?v=abc"]);
        assert_eq!(ok("www.example.com"), ["https://www.example.com"]);
        assert_eq!(ok("localhost:8080/a.mp4"), ["https://localhost:8080/a.mp4"]);
        assert_eq!(ok("192.168.1.5:8000/live"), ["https://192.168.1.5:8000/live"]);
        // 好幾行、以空白分隔：照順序
        assert_eq!(
            ok("https://a.example/1.mp4\r\nhttps://b.example/2.mp4\n\n rtmp://c.example/live "),
            [
                "https://a.example/1.mp4",
                "https://b.example/2.mp4",
                "rtmp://c.example/live"
            ]
        );
        assert_eq!(ok("udp://239.0.0.1:1234"), ["udp://239.0.0.1:1234"]);
        assert_eq!(parse_input("   "), Err(InputError::Empty));
        assert_eq!(parse_input("hello world"), Err(InputError::NotUrl));
        assert_eq!(parse_input("影片"), Err(InputError::NotUrl));
        assert_eq!(parse_input("1.5"), Err(InputError::NotUrl));
        assert_eq!(parse_input("C:\\影片\\a.mp4"), Err(InputError::NotUrl));
        assert_eq!(parse_input("/home/a.mp4"), Err(InputError::NotUrl));
        assert_eq!(
            parse_input("ftp://x.example/a"),
            Err(InputError::Unsupported("ftp://".into()))
        );
        assert_eq!(
            parse_input("javascript:alert(1)"),
            Err(InputError::Unsupported("javascript:".into()))
        );
        assert_eq!(parse_input("edl://x"), Err(InputError::Unsupported("edl://".into())));
        assert_eq!(
            parse_input("file:///C:/a.mp4"),
            Err(InputError::Unsupported("file://".into()))
        );
        // 整行是一個路徑裡有空白的網址：一個，空白換成 %20（全形空白照 UTF-8）
        assert_eq!(
            ok("https://example.com/My Video.mp4"),
            ["https://example.com/My%20Video.mp4"]
        );
        assert_eq!(
            ok("\"https://x.example/影片 第1集\u{3000}上.mp4\""),
            ["https://x.example/影片%20第1集%E3%80%80上.mp4"]
        );
        // 後面還有別的網址：照空白分開
        assert_eq!(
            ok("https://a.example/1 b.example/2"),
            ["https://a.example/1", "https://b.example/2"]
        );
        // 有一個不是網址就整個不接受
        assert_eq!(parse_input("hello https://a.example/1"), Err(InputError::NotUrl));
        assert_eq!(
            parse_input("https://a.example/1 ftp://b.example/2"),
            Err(InputError::Unsupported("ftp://".into()))
        );
        assert_eq!(
            parse_input("https://a.example/1\nhttps://b.example/my video.mp4"),
            Err(InputError::NotUrl),
            "好幾行時每一個都要是網址"
        );
        // 檔名不是網址（不會變成 https://a.mp4 去查 DNS）
        for name in ["a.mp4", "第1集.mp4", "Movie.MKV", "list.m3u8", "sub.srt", "clip.mkv"] {
            assert_eq!(parse_input(name), Err(InputError::NotUrl), "{name}");
        }
        // 但有路徑、埠號或 www. 的照樣是網址
        assert_eq!(ok("example.mov/watch"), ["https://example.mov/watch"]);
        assert_eq!(ok("www.example.mov"), ["https://www.example.mov"]);
        assert_eq!(InputError::Unsupported("ftp://".into()).message(), "不支援 ftp:// 網址");
        assert_eq!(InputError::NotUrl.message(), "這不是網址");
    }

    #[test]
    fn hosts_and_display_names() {
        assert_eq!(
            host("https://WWW.Example.com:8443/a").as_deref(),
            Some("www.example.com")
        );
        assert_eq!(host("rtmp://Live.Example/app").as_deref(), Some("live.example"));
        assert_eq!(host("http://[::1]:8080/a").as_deref(), Some("::1"));
        assert_eq!(host("not a url"), None);
        assert_eq!(
            display_name("https://x.example/dir/%E5%BD%B1%E7%89%87.mp4?a=1", None),
            "影片.mp4"
        );
        assert_eq!(
            display_name("https://x.example/dir/live/", None),
            "live",
            "最後一段是空的就往前找"
        );
        assert_eq!(display_name("https://x.example/", None), "x.example");
        assert_eq!(
            display_name("https://x.example/a.mp4", Some("  標題\n第二行 ")),
            "標題 第二行"
        );
        assert_eq!(display_name("https://x.example/a.mp4", Some("  ")), "a.mp4");
        assert_eq!(display_name("???", None), "???");
    }

    #[test]
    fn secrets_are_not_stored() {
        for url in [
            "https://example.com/watch?v=abc",
            "https://example.com/a.mp4",
            "https://example.com/live/news/index.m3u8",
            "http://iptv.example/movie/2024/x.mp4",
            "https://drive.example/v?resourcekey=abc",
        ] {
            assert!(storable(url), "{url}");
        }
        for url in [
            "https://user:pass@example.com/a.mp4",
            "https://user@example.com/a.mp4",
            "http://iptv.example/get.php?username=u&password=p&type=m3u",
            "https://cdn.example/a.m3u8?token=abc",
            "https://cdn.example/a.m3u8?access_token=abc",
            "https://s3.example/a.mp4?X-Amz-Signature=abc&X-Amz-Credential=x",
            "https://cdn.example/a.mp4?sig=abc",
            "https://cdn.example/a.mp4?api_key=abc",
            "https://cdn.example/a.mp4?Auth=abc",
            "https://cdn.example/a.mp4?pwd=abc",
            "http://iptv.example/live/user/pass/123.ts",
            "http://iptv.example/series/user/pass/9.mkv",
            "data:video/mp4;base64,AAAA",
            "not a url",
        ] {
            assert!(!storable(url), "{url}");
        }
        let long = format!("https://example.com/{}", "a".repeat(5000));
        assert!(!storable(&long));
    }

    #[test]
    fn titles_worth_keeping() {
        let url = "https://x.example/dir/a%20b.mp4";
        assert_eq!(useful_title(url, "  我的\n影片 "), Some("我的 影片".to_owned()));
        assert_eq!(useful_title(url, "   "), None);
        // 跟沒有標題時顯示的一樣（m3u 存檔時自動寫的）：不算
        assert_eq!(useful_title(url, "a b.mp4"), None);
        let long = "長".repeat(500);
        assert_eq!(useful_title(url, &long).unwrap().chars().count(), MAX_TITLE_CHARS);
        // v0.3.0 存清單時自動寫的（網址最後一段去掉副檔名）：不算，影片真正的標題才記得到
        assert_eq!(useful_title("https://x.example/live/index.m3u8", "index"), None);
        assert_eq!(useful_title("https://x.example/watch?v=abc.def", "watch?v=abc"), None);
        assert_eq!(useful_title("https://x.example/a%20b.mp4", "a%20b"), None);
        assert_eq!(
            useful_title("https://x.example/live/index.m3u8", "新聞台"),
            Some("新聞台".to_owned())
        );
    }

    #[test]
    fn radio_song_is_not_the_station_title() {
        // 一般的串流：media-title 就是標題
        assert_eq!(stream_title("新聞台", None, None), Some("新聞台"));
        assert_eq!(stream_title("新聞台", Some(""), None), Some("新聞台"));
        // 有標題（title 標籤排在 icy-title 前面）時，歌名不一樣：照用標題
        assert_eq!(stream_title("台北電台", Some("某首歌"), Some("電台")), Some("台北電台"));
        // media-title 是正在播的歌：改用電台名稱，沒有就不記
        assert_eq!(
            stream_title("某首歌", Some("某首歌"), Some(" 古典電台 ")),
            Some("古典電台")
        );
        assert_eq!(stream_title("某首歌", Some("某首歌"), None), None);
        assert_eq!(stream_title("某首歌", Some("某首歌"), Some("  ")), None);
    }

    #[test]
    fn bare_file_names_from_finder() {
        for t in ["a.mp4", " 影片 1.MKV\n", "\"字幕.srt\"", "清單.m3u8"] {
            assert!(bare_file_name(t), "{t:?}");
        }
        for t in [
            "",
            "hello",
            "a.docx",
            "dir/a.mp4",
            r"C:\a.mp4",
            "a.mp4\nb.mp4",
            "https://x.example/a.mp4",
        ] {
            assert!(!bare_file_name(t), "{t:?}");
        }
    }

    #[test]
    fn pasted_text_that_looks_like_paths() {
        use std::path::{Path, PathBuf};
        let win = |t: &str| pasted_paths_for(t, true, None);
        let unix = |t: &str| pasted_paths_for(t, false, Some(Path::new("/home/me")));
        // Windows：磁碟代號、網路路徑；檔案總管的「複製路徑」會加引號，選好幾個是一行一個
        assert_eq!(win(r"C:\影片\a b.mp4"), Some(vec![PathBuf::from(r"C:\影片\a b.mp4")]));
        assert_eq!(win("d:/a.mkv"), Some(vec![PathBuf::from("d:/a.mkv")]));
        assert_eq!(
            win(r"\\nas\share\a.mp4"),
            Some(vec![PathBuf::from(r"\\nas\share\a.mp4")])
        );
        assert_eq!(
            win("\"C:\\a.mp4\"\r\n\"C:\\b c.mp4\"\r\n"),
            Some(vec![PathBuf::from(r"C:\a.mp4"), PathBuf::from(r"C:\b c.mp4")])
        );
        for t in [
            "a.mp4",
            r"影片\a.mp4",
            "C:",
            "C:a.mp4",
            "/home/a.mp4",
            r"\\\x",
            "~/a.mp4",
            "",
            "  \n ",
        ] {
            assert_eq!(win(t), None, "{t:?}");
        }
        assert_eq!(win("C:\\a.mp4\nhello"), None, "有一行不是路徑就整個不算");
        // 其他系統：/、~/ 開頭
        assert_eq!(
            unix("/home/me/影片/a b.mp4"),
            Some(vec![PathBuf::from("/home/me/影片/a b.mp4")])
        );
        assert_eq!(unix("'~/a.mp4'"), Some(vec![PathBuf::from("/home/me/a.mp4")]));
        assert_eq!(unix("~"), Some(vec![PathBuf::from("/home/me")]));
        assert_eq!(pasted_paths_for("~/a.mp4", false, None), None, "不知道家目錄");
        for t in [
            "a.mp4",
            r"C:\a.mp4",
            "~me/a.mp4",
            "hello world",
            "/a.mp4\nb.mp4",
            "/a\u{0}b",
        ] {
            assert_eq!(unix(t), None, "{t:?}");
        }
        // file:// 網址照 m3u 的規則轉成路徑（照執行的平台）
        let got = pasted_paths("file:///C:/%E5%BD%B1%E7%89%87/a%20b.mp4").unwrap();
        if cfg!(windows) {
            assert_eq!(got, [PathBuf::from(r"C:\影片\a b.mp4")]);
        } else {
            assert_eq!(got, [PathBuf::from("/C:/影片/a b.mp4")]);
        }
        assert_eq!(pasted_paths("file://"), None);
    }

    #[test]
    fn failures_from_real_log_lines() {
        let url = "https://example.com/a.mp4";
        let cases: &[(&str, Option<NetFailure>)] = &[
            (
                "[ffmpeg] tcp: Failed to resolve hostname nonexistent.invalid: No such host is known. ",
                Some(NetFailure::HostNotFound),
            ),
            (
                "[ffmpeg] tcp: Connection to tcp://127.0.0.1:9 failed: Connection refused",
                Some(NetFailure::Refused),
            ),
            (
                "[ffmpeg] tcp: Connection to tcp://127.0.0.1:9 failed: Error number -10061 occurred",
                Some(NetFailure::Refused),
            ),
            (
                "[ffmpeg] tcp: Connection to tcp://10.255.255.1:80 failed: Connection timed out",
                Some(NetFailure::TimedOut),
            ),
            // Windows 的寫法（收下連線、一直不回應；tests/net_errors.rs 實際看到的）
            (
                "[ffmpeg] http: Error reading HTTP response: Error number -138 occurred",
                Some(NetFailure::TimedOut),
            ),
            (
                "[ffmpeg] http: Error reading HTTP response: Operation timed out",
                Some(NetFailure::TimedOut),
            ),
            ("[ffmpeg] http: HTTP error 404 Not Found", Some(NetFailure::Http(404))),
            (
                "[ffmpeg] https: HTTP error 403 Forbidden\n[stream] Failed to open https://example.com/a.mp4.",
                Some(NetFailure::Http(403)),
            ),
            (
                "[ffmpeg] http: HTTP error 503 Service Unavailable",
                Some(NetFailure::Http(503)),
            ),
            (
                "[ffmpeg] tls: Creating security context failed (0x80092013)",
                Some(NetFailure::RevocationOffline),
            ),
            (
                "[ffmpeg] tls: Creating security context failed (0x80090325)",
                Some(NetFailure::Untrusted),
            ),
            (
                "[ffmpeg] tls: error:0A000086:SSL routines::certificate verify failed",
                Some(NetFailure::Untrusted),
            ),
            (
                "[stream] No protocol handler found to open URL https://example.com/a.mp4",
                Some(NetFailure::Protocol("https".into())),
            ),
            ("[cplayer] Failed to recognize file format.", Some(NetFailure::WebPage)),
            ("[cplayer] No video or audio streams selected.", None),
            (
                "[ffmpeg] tls: Creating security context failed (0x80092012)",
                Some(NetFailure::RevocationOffline),
            ),
            (
                "[ffmpeg] tls: Creating security context failed (0x800b0109)",
                Some(NetFailure::Untrusted),
            ),
            (
                "[ffmpeg] tls: SNI or certificate check failed: The target principal name is incorrect.",
                Some(NetFailure::Untrusted),
            ),
            (
                "[ffmpeg] tls: Unable to verify certificate: Invalid certificate chain",
                Some(NetFailure::Untrusted),
            ),
            (
                "[ffmpeg] tls: The certificate is NOT trusted. The certificate issuer is unknown.",
                Some(NetFailure::Untrusted),
            ),
            (
                "[ffmpeg] Protocol not found",
                Some(NetFailure::Protocol("https".into())),
            ),
            (
                "[ffmpeg] tcp: Connection to tcp://10.0.0.1:80 failed: Error number -10060 occurred",
                Some(NetFailure::TimedOut),
            ),
            (
                "[ffmpeg] http: HTTP error 401 Unauthorized",
                Some(NetFailure::Http(401)),
            ),
            ("[ffmpeg] http: HTTP error 410 Gone", Some(NetFailure::Http(410))),
            (
                "[ffmpeg] http: HTTP error 429 Too Many Requests",
                Some(NetFailure::Http(429)),
            ),
            // 只有一般的開檔失敗：沒有網路的原因
            ("[stream] Failed to open https://example.com/a.mp4.", None),
            ("", None),
        ];
        for (log, want) in cases {
            assert_eq!(&classify_failure(log, url), want, "{log}");
        }
        // 網頁的判斷只對 http(s)；不是網址不判斷
        assert_eq!(classify_failure("Failed to recognize file format.", "rtsp://x/a"), None);
        assert_eq!(classify_failure("HTTP error 404 Not Found", "C:\\a.mp4"), None);
        assert_eq!(http_error("HTTP error 4044"), None);
        assert_eq!(http_error("HTTP error abc\nHTTP error 410 Gone"), Some(410));
    }

    #[test]
    fn live_streams() {
        let http = "https://x.example/a.mkv";
        // 沒有總長度：直播（網路電台之類）
        assert!(is_live(http, None, false, Some("mp3")));
        assert!(is_live(http, Some(0.0), true, None));
        assert!(is_live(http, Some(f64::NAN), true, None));
        // 直播的 HLS / DASH：FFmpeg 估了很短的總長度，但不能跳轉
        let hls = "https://x.example/live/index.m3u8";
        assert!(is_live(hls, Some(0.9), false, Some("hls")));
        assert!(is_live(hls, Some(0.9), false, Some("hls,applehttp")));
        assert!(is_live(hls, Some(0.9), false, Some("applehttp")));
        assert!(is_live("https://x.example/a.mpd", Some(4.0), false, Some("dash")));
        assert!(!is_live(hls, Some(120.0), true, Some("hls")), "HLS 的 VOD 能跳轉");
        // RTSP、RTMP 之類不能跳轉：直播
        assert!(is_live("rtsp://cam.local/stream", Some(3.0), false, Some("rtsp")));
        assert!(is_live("RTMP://x.example/app/key", Some(3.0), false, Some("flv")));
        // 伺服器不支援 Range 的一般檔案：不能跳轉，但總長度是真的，不是直播
        assert!(!is_live(http, Some(90.0), false, Some("mkv")));
        assert!(!is_live(http, Some(90.0), false, Some("mov,mp4,m4a,3gp,3g2,mj2")));
        assert!(!is_live(http, Some(90.0), false, None));
        assert!(!is_live(http, Some(90.0), true, Some("mkv")));
    }

    #[test]
    fn resume_keys() {
        // 網路串流：去掉 # 之後的部分（大小寫、查詢字串照舊：YouTube 的影片代號分大小寫）
        assert_eq!(
            resume_key("https://x.example/v.mp4?id=AbC#t=90", None).as_deref(),
            Some("https://x.example/v.mp4?id=AbC")
        );
        assert_eq!(
            resume_key("https://x.example/v.mp4", None).as_deref(),
            Some("https://x.example/v.mp4")
        );
        assert_ne!(
            resume_key("https://x.example/v?id=abc", None),
            resume_key("https://x.example/v?id=ABC", None)
        );
        // 網站影片：同一部影片的不同網址是同一個代號
        let a = resume_key("https://youtu.be/BaW_jenozKc", Some(("Youtube", "BaW_jenozKc")));
        let b = resume_key(
            "https://www.youtube.com/watch?v=BaW_jenozKc&t=90",
            Some(("Youtube", "BaW_jenozKc")),
        );
        assert_eq!(a.as_deref(), Some("ytdl://youtube/BaW_jenozKc"));
        assert_eq!(a, b);
        assert_eq!(
            resume_key("https://x.example/v.mp4", Some(("", ""))).as_deref(),
            Some("https://x.example/v.mp4"),
            "沒有代號時照網址"
        );
        // 不是網路串流：沒有代號（本機檔案用路徑，mpv 自己的網址不續播）
        for p in [
            "av://lavfi:testsrc",
            "C:\\a.mp4",
            "/home/a.mp4",
            "edl://x",
            "memory://x",
        ] {
            assert_eq!(resume_key(p, None), None, "{p}");
        }
    }

    #[test]
    fn remote_playlists_keep_only_network_entries() {
        let json = r#"[
            {"filename": "http://h/1.mp4", "title": "第一台", "id": 1},
            {"filename": "file:///etc/passwd", "title": "本機", "current": true, "playing": true, "id": 2},
            {"filename": "edl://%3%abc", "id": 3},
            {"filename": "https://h/2.m3u8", "title": "  ", "id": 4},
            {"filename": "rtmp://h/live", "title": "第三台", "id": 5}
        ]"#;
        let p = remote_playlist("http://h/list.m3u", json);
        assert_eq!(p.source, "http://h/list.m3u", "沒有 playlist-path 時用給的網址");
        assert_eq!(
            p.entries,
            [
                ("http://h/1.mp4".to_owned(), Some("第一台".to_owned())),
                ("https://h/2.m3u8".to_owned(), None),
                ("rtmp://h/live".to_owned(), Some("第三台".to_owned())),
            ]
        );
        assert_eq!(p.dropped, 2);
        assert_eq!(p.start, 1, "mpv 選的被拿掉了：從它後面第一個留下的開始");
        let first = r#"[{"filename": "http://h/1.mp4", "current": true}, {"filename": "http://h/2.mp4"}]"#;
        assert_eq!(remote_playlist("u", first).start, 0);
        let second = r#"[{"filename": "http://h/1.mp4"}, {"filename": "http://h/2.mp4", "current": true}]"#;
        assert_eq!(remote_playlist("u", second).start, 1);
        // 最後幾個都被拿掉：從第一個開始
        let tail = r#"[{"filename": "http://h/1.mp4"}, {"filename": "file:///x", "current": true}]"#;
        let p = remote_playlist("u", tail);
        assert_eq!((p.entries.len(), p.start, p.dropped), (1, 0, 1));
        assert_eq!(
            remote_playlist(
                "u",
                r#"[{"filename": "http://h/1.mp4", "current": true, "playlist-path": "http://h/real.m3u"}]"#
            )
            .source,
            "http://h/real.m3u",
            "清單本身的網址：mpv 記在每個項目的 playlist-path"
        );
        // mpv 把 file:// 換成本機路徑（Windows 是 \etc\passwd）：一樣拿掉
        let local = r#"[{"filename": "\\etc\\passwd", "current": true}, {"filename": "/etc/passwd"}, {"filename": "http://h/1.mp4"}]"#;
        let p = remote_playlist("u", local);
        assert_eq!(p.entries, [("http://h/1.mp4".to_owned(), None)]);
        assert_eq!((p.start, p.dropped), (0, 2));
        assert_eq!(
            remote_playlist("u", "not json"),
            RemotePlaylist {
                source: "u".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn header_lines_and_socks() {
        assert!(header_line_ok("X-A: 1, 2") && header_line_ok("  ") && header_line_ok(""));
        assert!(!header_line_ok("no colon") && !header_line_ok("Bad Name: 1") && !header_line_ok("X: a\u{0}b"));
        assert!(is_socks_proxy("socks5://127.0.0.1:1080") && is_socks_proxy(" SOCKS5H://h:1"));
        assert!(!is_socks_proxy("http://h:3128") && !is_socks_proxy("") && !is_socks_proxy("sock"));
    }

    #[test]
    fn failure_messages() {
        let m = |f: NetFailure| f.message(30.0);
        assert_eq!(
            m(NetFailure::HostNotFound),
            "無法開啟網址：找不到伺服器（請確認網址與網路連線）"
        );
        assert_eq!(m(NetFailure::TimedOut), "無法開啟網址：連線逾時（30 秒內沒有回應）");
        assert_eq!(
            NetFailure::TimedOut.message(2.5),
            "無法開啟網址：連線逾時（2.5 秒內沒有回應）"
        );
        assert_eq!(
            m(NetFailure::Http(403)),
            "無法開啟網址：伺服器拒絕存取（HTTP 403）：網址可能過期或需要登入"
        );
        assert_eq!(
            m(NetFailure::Http(410)),
            "無法開啟網址：找不到這個網址的內容（HTTP 410）"
        );
        assert!(m(NetFailure::Http(429)).contains("HTTP 429"));
        assert_eq!(m(NetFailure::Http(500)), "無法開啟網址：伺服器回應錯誤（HTTP 500）");
        assert!(m(NetFailure::RevocationOffline).contains("檢查網站憑證"));
        assert_eq!(
            m(NetFailure::Protocol("xyz".into())),
            "無法開啟網址：播放引擎不支援 xyz:// 網址"
        );
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            m(NetFailure::Http(404)),
            "Can't open the URL: Nothing found at this address (HTTP 404)"
        );
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }
}
