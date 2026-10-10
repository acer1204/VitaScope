//! yt-dlp 的錯誤與提醒：從 yt-dlp 的輸出（stderr）判斷原因。
//!
//! 背景執行緒只回傳這裡的列舉（[`YtdlError`]、[`Hint`]），給使用者看的文字由介面執行緒呼叫 `message()` 產生：
//! 介面語言是每個執行緒自己的（`i18n`），在背景執行緒組文字會變成錯的語言，切換語言後也不會跟著換。

use super::Browser;

/// 錯誤訊息裡 yt-dlp 原文最多保留幾個字
const MAX_RAW_CHARS: usize = 200;

/// 網站影片播不了的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YtdlError {
    /// 找不到 yt-dlp（沒有安裝、指定的檔案不見了）
    Missing,
    /// 「設定 → 網路」關掉了「用 yt-dlp 播放網站影片」
    Disabled,
    /// yt-dlp 不支援這個網站
    Unsupported,
    /// 影片已刪除或設為私人
    Unavailable,
    /// 有年齡限制，要登入
    AgeRestricted,
    /// 網站要確認不是機器人
    NotABot,
    /// 只有頻道會員能看
    MembersOnly,
    /// 直播或首播還沒開始
    NotStarted,
    /// 網站暫時限制連線次數（HTTP 429）
    RateLimited,
    /// 網站拒絕存取（HTTP 403）
    Forbidden,
    /// 取不到影片，多半是 yt-dlp 太舊（網站改版）
    Outdated,
    /// YouTube 要 JavaScript 執行環境（deno）
    NeedsJsRuntime,
    /// 有 DRM 保護
    Drm,
    /// 連不到網站（沒有網路、找不到主機）
    Offline,
    /// 讀不到瀏覽器的 Cookie（`--cookies-from-browser`）；瀏覽器是設定裡選的那一個
    Cookies(Option<Browser>),
    /// yt-dlp 沒有回應（逾時、沒有輸出）
    NoResponse,
    /// Windows 擋下了 yt-dlp（Defender 誤判、Smart App Control、公司的應用程式控制原則）；系統的錯誤代碼
    Blocked(i32),
    /// 無法執行 yt-dlp（其他的系統錯誤）
    Spawn(std::io::ErrorKind),
    /// yt-dlp 的輸出不是 JSON（多半是 yt-dlp 設定檔裡的選項造成的）
    NotJson,
    /// 資料裡沒有能播放的格式（全部都是不安全、看不懂的網址）
    NoPlayable,
    /// 播放引擎不支援的串流方式（yt-dlp 的 protocol，例如 `websocket_frag`）
    UnsupportedProtocol(String),
    /// 播放清單是空的
    EmptyPlaylist,
    /// 選的畫質已經不在清單上
    FormatGone,
    /// 已取消（不顯示給使用者）
    Cancelled,
    /// 其他：yt-dlp 的原文（最後一行 `ERROR:`，去掉前面的 `[網站] 代號:`）
    Other(String),
}

/// 錯誤附帶的建議：介面在錯誤訊息旁邊放對應的按鈕或說明
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// 到「設定 → 網路」從瀏覽器讀 Cookie（登入後才能看的影片）
    UseCookies,
    /// 更新 yt-dlp 再試一次
    UpdateYtdl,
    /// 取得 deno
    GetDeno,
    /// 改用 Firefox 的 Cookie（Chrome、Edge 開著時讀不到，新版的還加了密）
    TryFirefox,
    /// 取得 yt-dlp
    GetYtdl,
    /// 到「設定 → 網路」打開「用 yt-dlp 播放網站影片」
    EnableYtdl,
}

impl Remedy {
    /// 錯誤訊息下面的建議（文字）。跟設定有關的建議，起始畫面另外放「網路設定…」按鈕（[`Self::in_settings`]）
    pub fn advice(self) -> &'static str {
        self.advice_on(crate::paths::Os::current())
    }

    /// 在 `os` 上的建議（測試在任何系統上檢查每個系統的說法）。
    /// 「放在影戲的資料夾」只有 Windows 說得通：macOS 的程式資料夾在 VitaScope.app 裡面（放進去會破壞簽章），
    /// Linux 的 AppImage 執行時的資料夾是唯讀的暫時資料夾；這兩個系統說常用的安裝方式（找得到的位置見 `locate`）
    pub fn advice_on(self, os: crate::paths::Os) -> &'static str {
        use crate::paths::Os;
        use crate::tr;
        match self {
            Remedy::GetYtdl => match os {
                Os::Windows => tr!(
                    "請安裝 yt-dlp（github.com/yt-dlp/yt-dlp，或 winget、scoop），或把 yt-dlp.exe 放在影戲的資料夾",
                    "Install yt-dlp (github.com/yt-dlp/yt-dlp, or winget, scoop), or put yt-dlp.exe in VitaScope's folder"
                ),
                Os::Macos => tr!(
                    "請安裝 yt-dlp（用 Homebrew：brew install yt-dlp，或 github.com/yt-dlp/yt-dlp）",
                    "Install yt-dlp (with Homebrew: brew install yt-dlp, or github.com/yt-dlp/yt-dlp)"
                ),
                Os::Linux => tr!(
                    "請安裝 yt-dlp（用套件管理員、pipx，或把 github.com/yt-dlp/yt-dlp 的 yt-dlp 放在 ~/.local/bin）",
                    "Install yt-dlp (with your package manager or pipx, or put yt-dlp from github.com/yt-dlp/yt-dlp in ~/.local/bin)"
                ),
            },
            Remedy::UpdateYtdl => tr!(
                "請更新 yt-dlp（yt-dlp -U，或用安裝它的方式更新）後再試一次",
                "Update yt-dlp (yt-dlp -U, or the way you installed it) and try again"
            ),
            Remedy::GetDeno => tr!(
                "請安裝 deno 2.3 以上（deno.com），YouTube 才能取得全部畫質",
                "Install deno 2.3 or newer (deno.com) so YouTube offers every quality"
            ),
            Remedy::UseCookies => tr!(
                "需要登入的影片：在「設定 → 網路」選擇從瀏覽器讀 Cookie（建議 Firefox）",
                "For videos that need a login, choose a browser to read cookies from in Settings → Network (Firefox recommended)"
            ),
            Remedy::TryFirefox => tr!(
                "Chrome、Edge 開著時常常讀不到 Cookie，建議在「設定 → 網路」改用 Firefox 的 Cookie",
                "Chrome and Edge cookies often can't be read while the browser is open; \
                 try Firefox's cookies in Settings → Network"
            ),
            Remedy::EnableYtdl => tr!(
                "要播放網站影片，請在「設定 → 網路」打開「用 yt-dlp 播放網站影片」",
                "To play website videos, turn on \"Play website videos with yt-dlp\" in Settings → Network"
            ),
        }
    }

    /// 建議的處理方式在「設定 → 網路」（起始畫面放「網路設定…」按鈕）
    pub fn in_settings(self) -> bool {
        matches!(
            self,
            Remedy::GetYtdl | Remedy::UseCookies | Remedy::TryFirefox | Remedy::EnableYtdl
        )
    }
}

impl YtdlError {
    /// 給使用者看的完整訊息（「無法播放網站影片：原因」）
    pub fn message(&self) -> String {
        if matches!(self, YtdlError::Missing | YtdlError::Disabled) {
            return self.reason();
        }
        let reason = self.reason();
        crate::tf!("無法播放網站影片：{reason}", "Can't play the website video: {reason}")
    }

    /// 原因本身（沒有前面的「無法播放網站影片：」）
    pub fn reason(&self) -> String {
        use crate::{tf, tr};
        match self {
            YtdlError::Missing => tr!("網站影片需要 yt-dlp", "Website videos need yt-dlp").to_owned(),
            YtdlError::Disabled => tr!(
                "網站影片需要 yt-dlp（已在「設定 → 網路」關閉）",
                "Website videos need yt-dlp (turned off in Settings → Network)"
            )
            .to_owned(),
            YtdlError::Unsupported => tr!("yt-dlp 不支援這個網站", "yt-dlp doesn't support this site").to_owned(),
            YtdlError::Unavailable => tr!(
                "影片無法觀看（已刪除或設為私人）",
                "The video isn't available (removed or private)"
            )
            .to_owned(),
            YtdlError::AgeRestricted => tr!(
                "影片有年齡限制，需要登入",
                "The video is age-restricted and needs a login"
            )
            .to_owned(),
            YtdlError::NotABot => {
                tr!("網站要求確認不是機器人", "The site wants to confirm you're not a bot").to_owned()
            }
            YtdlError::MembersOnly => tr!("只有頻道會員可以觀看", "Members-only video").to_owned(),
            YtdlError::NotStarted => {
                tr!("直播或首播還沒開始", "The live stream or premiere hasn't started yet").to_owned()
            }
            YtdlError::RateLimited => tr!(
                "網站暫時限制連線次數，請稍後再試",
                "The site is rate-limiting; try again later"
            )
            .to_owned(),
            YtdlError::Forbidden => tr!("網站拒絕存取（403）", "The site refused access (403)").to_owned(),
            YtdlError::Outdated => tr!(
                "無法取得影片（yt-dlp 可能需要更新）",
                "Couldn't get the video (yt-dlp may need an update)"
            )
            .to_owned(),
            YtdlError::NeedsJsRuntime => tr!(
                "YouTube 需要 JavaScript 執行環境（deno）",
                "YouTube needs a JavaScript runtime (deno)"
            )
            .to_owned(),
            YtdlError::Drm => tr!(
                "影片有 DRM 保護，無法播放",
                "The video is DRM-protected and can't be played"
            )
            .to_owned(),
            YtdlError::Offline => tr!(
                "無法連線到網站（請確認網路連線）",
                "Can't reach the site (check your connection)"
            )
            .to_owned(),
            YtdlError::Cookies(browser) => {
                let name = browser.map_or_else(|| tr!("瀏覽器", "the browser").to_owned(), |b| b.label().to_owned());
                tf!("讀不到 {name} 的 Cookie", "Couldn't read {name}'s cookies")
            }
            YtdlError::NoResponse => tr!("yt-dlp 沒有回應", "yt-dlp didn't respond").to_owned(),
            YtdlError::Blocked(_) => tr!(
                "Windows 擋下了 yt-dlp（可能是誤判）",
                "Windows blocked yt-dlp (possibly a false positive)"
            )
            .to_owned(),
            YtdlError::Spawn(kind) => tf!("無法執行 yt-dlp（{kind}）", "Couldn't run yt-dlp ({kind})"),
            YtdlError::NotJson => tr!(
                "yt-dlp 的輸出不是 JSON（可能是 yt-dlp 設定檔裡的選項造成的）",
                "yt-dlp's output isn't JSON (an option in a yt-dlp config file may cause this)"
            )
            .to_owned(),
            YtdlError::NoPlayable => tr!("沒有可以播放的格式", "No playable format").to_owned(),
            YtdlError::UnsupportedProtocol(p) => tf!(
                "播放引擎不支援這種串流（{p}）",
                "The playback engine doesn't support this kind of stream ({p})"
            ),
            YtdlError::EmptyPlaylist => tr!("播放清單是空的", "The playlist is empty").to_owned(),
            YtdlError::FormatGone => tr!(
                "選的畫質已經沒有了，請重新選擇",
                "The chosen quality is no longer available; pick another one"
            )
            .to_owned(),
            YtdlError::Cancelled => tr!("已取消", "Cancelled").to_owned(),
            YtdlError::Other(raw) => tf!("yt-dlp：{raw}", "yt-dlp: {raw}"),
        }
    }

    /// 錯誤附帶的建議（沒有時 None）
    pub fn remedy(&self) -> Option<Remedy> {
        match self {
            YtdlError::Missing => Some(Remedy::GetYtdl),
            YtdlError::Disabled => Some(Remedy::EnableYtdl),
            YtdlError::AgeRestricted | YtdlError::NotABot | YtdlError::MembersOnly => Some(Remedy::UseCookies),
            YtdlError::Forbidden | YtdlError::Outdated => Some(Remedy::UpdateYtdl),
            YtdlError::NeedsJsRuntime => Some(Remedy::GetDeno),
            YtdlError::Cookies(b) if *b != Some(Browser::Firefox) => Some(Remedy::TryFirefox),
            _ => None,
        }
    }
}

/// 解析失敗：原因，加上 yt-dlp 警告裡看得出的提醒。
/// 失敗時警告也要留著：例如沒有 deno 時 YouTube 的錯誤只說「Requested format is not available」（看起來像 yt-dlp
/// 太舊），真正的原因（缺 JavaScript 執行環境）只寫在前面的警告裡
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error: YtdlError,
    pub hints: Vec<Hint>,
}

impl From<YtdlError> for Failure {
    fn from(error: YtdlError) -> Self {
        Failure {
            error,
            hints: Vec::new(),
        }
    }
}

impl Failure {
    /// 建議的處理方式：「取不到影片」而警告說缺 deno 時先取得 deno（更新 yt-dlp 沒有用）
    pub fn remedy(&self) -> Option<Remedy> {
        if self.error == YtdlError::Outdated && self.hints.contains(&Hint::NeedsJsRuntime) {
            return Some(Remedy::GetDeno);
        }
        self.error.remedy()
    }
}

/// yt-dlp 成功了、但有警告（`WARNING:`）時的提醒：例如沒有 deno 時 YouTube 只剩低畫質，
/// 不提醒的話使用者只會覺得畫質變差。不加 `--no-warnings` 就是為了這個
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// 沒有 JavaScript 執行環境（deno），YouTube 只有部分畫質
    NeedsJsRuntime,
    /// yt-dlp 可能需要更新（解不開網站的驗證）
    Outdated,
    /// 有些畫質可能拿不到（其他原因）
    FormatsMissing,
}

impl Hint {
    pub fn message(self) -> &'static str {
        use crate::tr;
        match self {
            Hint::NeedsJsRuntime => tr!(
                "YouTube 需要 deno 才能取得全部畫質",
                "YouTube needs deno for all qualities"
            ),
            Hint::Outdated => tr!("yt-dlp 可能需要更新", "yt-dlp may need an update"),
            Hint::FormatsMissing => tr!("有些畫質可能拿不到", "Some qualities may be missing"),
        }
    }

    pub fn remedy(self) -> Option<Remedy> {
        match self {
            Hint::NeedsJsRuntime => Some(Remedy::GetDeno),
            Hint::Outdated => Some(Remedy::UpdateYtdl),
            Hint::FormatsMissing => None,
        }
    }
}

/// 小寫後比對（yt-dlp 的訊息大小寫不一定）
fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

/// 一個完整的字（前後不是英文字母、數字）：「deno」算，網址裡的「denormal」之類不算
fn has_word(text: &str, word: &str) -> bool {
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    text.match_indices(word)
        .any(|(i, _)| !is_word(text[..i].chars().next_back()) && !is_word(text[i + word.len()..].chars().next()))
}

/// 一行 `ERROR:` / `WARNING:` 的內容：去掉標籤與前面的 `[網站] 代號: `
fn strip_prefix_tag(line: &str) -> &str {
    let mut rest = line.trim();
    if let Some(after) = rest.strip_prefix('[')
        && let Some(end) = after.find(']')
    {
        rest = after[end + 1..].trim_start();
        // 「代號: 」：沒有空白的一段後面接冒號
        if let Some((id, msg)) = rest.split_once(": ")
            && !id.is_empty()
            && !id.contains(char::is_whitespace)
        {
            rest = msg;
        }
    }
    rest.trim()
}

/// yt-dlp 結束時的錯誤（`stderr` 全文、結束代碼）→ 原因。`browser` 是設定裡「從瀏覽器讀 Cookie」選的瀏覽器
pub fn describe(stderr: &str, exit: Option<i32>, browser: Option<Browser>) -> YtdlError {
    let last_error = stderr
        .lines()
        .rev()
        .find_map(|l| l.trim_start().strip_prefix("ERROR:"))
        .map(strip_prefix_tag);
    let Some(raw) = last_error else {
        // 沒有 ERROR 行：被系統結束（沒有結束代碼）或什麼都沒說
        let text = stderr.trim();
        return if text.is_empty() || exit.is_none() {
            YtdlError::NoResponse
        } else {
            YtdlError::Other(truncate(last_line(text)))
        };
    };
    classify_error(raw, browser)
}

fn last_line(text: &str) -> &str {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(text).trim()
}

fn truncate(s: &str) -> String {
    let mut out: String = s.chars().take(MAX_RAW_CHARS).collect();
    if s.chars().count() > MAX_RAW_CHARS {
        out.push('…');
    }
    out
}

/// 一則錯誤訊息 → 原因（依序比對，第一個符合的算數）
fn classify_error(raw: &str, browser: Option<Browser>) -> YtdlError {
    let l = raw.to_lowercase();
    // Cookie 放最前面：訊息裡的路徑、網址可能含有下面其他關鍵字。
    // Chrome、Edge 的 DPAPI 解密失敗沒有「cookie」這個字（「Failed to decrypt with DPAPI. See …/issues/10927 …」）
    if (l.contains("cookie") && has_any(&l, &["could not find", "could not copy", "unable to read", "database"]))
        || has_any(&l, &["failed to decrypt", "dpapi"])
    {
        return YtdlError::Cookies(browser);
    }
    if l.contains("unsupported url") {
        return YtdlError::Unsupported;
    }
    if has_any(
        &l,
        &[
            "confirm your age",
            "age-restricted",
            "age restricted",
            "inappropriate for some users",
        ],
    ) {
        return YtdlError::AgeRestricted;
    }
    if has_any(&l, &["not a bot", "sign in to confirm"]) {
        return YtdlError::NotABot;
    }
    if has_any(&l, &["members-only", "members only", "join this channel"]) {
        return YtdlError::MembersOnly;
    }
    if has_any(
        &l,
        &[
            "video unavailable",
            "private video",
            "this video is private",
            "has been removed",
            "no longer available",
        ],
    ) {
        return YtdlError::Unavailable;
    }
    if has_any(
        &l,
        &[
            "will begin in",
            "premieres in",
            "is_upcoming",
            "this live event will begin",
        ],
    ) {
        return YtdlError::NotStarted;
    }
    if has_any(&l, &["http error 429", "too many requests"]) {
        return YtdlError::RateLimited;
    }
    if l.contains("http error 403") {
        return YtdlError::Forbidden;
    }
    if has_any(&l, &["javascript runtime", "js runtime", "js-runtimes"]) || has_word(&l, "deno") {
        return YtdlError::NeedsJsRuntime;
    }
    if has_any(
        &l,
        &[
            "nsig",
            "n challenge",
            "signature extraction failed",
            "some formats may be missing",
            "requested format is not available",
        ],
    ) {
        return YtdlError::Outdated;
    }
    // DRM 照原本的大小寫比對（小寫的「drm」可能是別的字的一部分）
    if raw.contains("DRM") {
        return YtdlError::Drm;
    }
    if has_any(
        &l,
        &[
            "unable to download webpage",
            "getaddrinfo",
            "name or service not known",
            "failed to resolve",
            "temporary failure in name resolution",
            "nodename nor servname",
            "network is unreachable",
        ],
    ) {
        return YtdlError::Offline;
    }
    if has_any(&l, &["timed out", "read timed out"]) {
        return YtdlError::NoResponse;
    }
    YtdlError::Other(truncate(raw))
}

/// yt-dlp 的警告（`WARNING:` 行）→ 給使用者的提醒（不重複，照出現的順序）
pub fn hints(stderr: &str) -> Vec<Hint> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let Some(w) = line.trim_start().strip_prefix("WARNING:") else {
            continue;
        };
        let l = w.to_lowercase();
        let hint = if l.contains("no supported javascript runtime")
            || (has_any(&l, &["javascript runtime", "js runtime"]) && !l.contains("challenge"))
        {
            Hint::NeedsJsRuntime
        } else if has_any(
            &l,
            &[
                "nsig",
                "n challenge",
                "signature extraction failed",
                "sig extraction failed",
                "outdated",
            ],
        ) {
            Hint::Outdated
        } else if l.contains("some formats may be missing") {
            Hint::FormatsMissing
        } else {
            continue;
        };
        if !out.contains(&hint) {
            out.push(hint);
        }
    }
    // 已經說了原因（缺 deno、要更新），就不再另外說「有些畫質拿不到」
    if out.len() > 1 {
        out.retain(|h| *h != Hint::FormatsMissing);
    }
    out
}

/// 啟動 yt-dlp 失敗（`spawn` 的系統錯誤）→ 原因。
/// Windows：225 = Defender 判定有毒、226 = 已被刪除，4551 = Smart App Control／WDAC 擋下，1260 = 群組原則擋下
pub fn spawn_error(e: &std::io::Error) -> YtdlError {
    match e.raw_os_error() {
        Some(code @ (225 | 226 | 4551 | 1260)) if cfg!(windows) => YtdlError::Blocked(code),
        _ if e.kind() == std::io::ErrorKind::NotFound => YtdlError::Missing,
        _ => YtdlError::Spawn(e.kind()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(line: &str) -> YtdlError {
        describe(&format!("[youtube] abc: Downloading webpage\n{line}\n"), Some(1), None)
    }

    #[test]
    fn errors_map_to_reasons() {
        let table = [
            ("ERROR: Unsupported URL: https://example.com/", YtdlError::Unsupported),
            ("ERROR: [youtube] abc: Video unavailable", YtdlError::Unavailable),
            (
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
                YtdlError::Unavailable,
            ),
            (
                "ERROR: [youtube] abc: This video has been removed by the uploader",
                YtdlError::Unavailable,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users.",
                YtdlError::AgeRestricted,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm you’re not a bot.",
                YtdlError::NotABot,
            ),
            (
                "ERROR: [youtube] abc: Join this channel to get access to members-only content",
                YtdlError::MembersOnly,
            ),
            ("ERROR: [youtube] abc: Premieres in 3 hours", YtdlError::NotStarted),
            (
                "ERROR: [youtube] abc: This live event will begin in 5 minutes.",
                YtdlError::NotStarted,
            ),
            (
                "ERROR: unable to download video data: HTTP Error 429: Too Many Requests",
                YtdlError::RateLimited,
            ),
            (
                "ERROR: unable to download video data: HTTP Error 403: Forbidden",
                YtdlError::Forbidden,
            ),
            (
                "ERROR: [youtube] abc: Requested format is not available.",
                YtdlError::Outdated,
            ),
            (
                "ERROR: [youtube] abc: Signature extraction failed: Some formats may be missing",
                YtdlError::Outdated,
            ),
            (
                "ERROR: [youtube] abc: No supported JavaScript runtime could be found",
                YtdlError::NeedsJsRuntime,
            ),
            ("ERROR: [Niconico] sm9: This video is DRM protected", YtdlError::Drm),
            (
                "ERROR: [generic] Unable to download webpage: <urlopen error [Errno 11001] getaddrinfo failed>",
                YtdlError::Offline,
            ),
            (
                "ERROR: Could not copy Chrome cookie database. See https://github.com/yt-dlp/yt-dlp/issues/7271",
                YtdlError::Cookies(None),
            ),
            (
                "ERROR: could not find firefox cookies database in /home/me/.mozilla",
                YtdlError::Cookies(None),
            ),
            // yt-dlp 的原文（cookies.py）：沒有「cookie」這個字
            (
                "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info",
                YtdlError::Cookies(None),
            ),
            (
                "ERROR: [youtube] abc: [jsc:deno] deno 2.1.0 is older than the minimum supported version 2.3.0",
                YtdlError::NeedsJsRuntime,
            ),
            // 「deno」要是完整的字
            (
                "ERROR: [generic] denormalized id",
                YtdlError::Other("denormalized id".into()),
            ),
        ];
        for (line, want) in table {
            assert_eq!(err(line), want, "{line}");
        }
    }

    #[test]
    fn the_last_error_line_counts_and_unknown_text_is_kept() {
        let stderr = "ERROR: Unsupported URL: x\nERROR: [vimeo] 123: Something odd happened\n";
        assert_eq!(
            describe(stderr, Some(1), None),
            YtdlError::Other("Something odd happened".into())
        );
        // 太長的截掉
        let long = format!("ERROR: {}", "x".repeat(500));
        let YtdlError::Other(raw) = describe(&long, Some(1), None) else {
            panic!("應該是 Other");
        };
        assert_eq!(raw.chars().count(), MAX_RAW_CHARS + 1);
    }

    #[test]
    fn no_output_means_no_response() {
        assert_eq!(describe("", Some(1), None), YtdlError::NoResponse);
        // 被系統結束（沒有結束代碼）
        assert_eq!(describe("something\n", None, None), YtdlError::NoResponse);
        // 沒有 ERROR 行但有說話：最後一行原文
        assert_eq!(
            describe("Traceback\nKeyError: 'x'\n", Some(1), None),
            YtdlError::Other("KeyError: 'x'".into())
        );
    }

    #[test]
    fn cookie_errors_name_the_browser_and_suggest_firefox() {
        let e = describe(
            "ERROR: Could not copy Chrome cookie database.",
            Some(1),
            Some(Browser::Chrome),
        );
        assert_eq!(e, YtdlError::Cookies(Some(Browser::Chrome)));
        assert_eq!(e.remedy(), Some(Remedy::TryFirefox));
        assert_eq!(YtdlError::Cookies(Some(Browser::Firefox)).remedy(), None);
        let e = describe(
            "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info\n",
            Some(1),
            Some(Browser::Edge),
        );
        assert_eq!(e, YtdlError::Cookies(Some(Browser::Edge)));
        assert_eq!(e.remedy(), Some(Remedy::TryFirefox));
    }

    #[test]
    fn a_failure_keeps_the_warnings() {
        // 沒有 deno：錯誤看起來像「yt-dlp 太舊」，警告才說出真正的原因 → 建議取得 deno，不是更新 yt-dlp
        let f = Failure {
            error: YtdlError::Outdated,
            hints: vec![Hint::NeedsJsRuntime],
        };
        assert_eq!(f.remedy(), Some(Remedy::GetDeno));
        assert_eq!(Failure::from(YtdlError::Outdated).remedy(), Some(Remedy::UpdateYtdl));
        let f = Failure {
            error: YtdlError::NotABot,
            hints: vec![Hint::NeedsJsRuntime],
        };
        assert_eq!(f.remedy(), Some(Remedy::UseCookies));
    }

    #[test]
    fn warnings_become_hints() {
        // 沒有 --no-warnings：沒有 deno 時 YouTube 照樣成功、只剩低畫質，靠警告才知道原因
        let stderr = "WARNING: [youtube] No supported JavaScript runtime could be found. Only deno is enabled by \
                      default; YouTube extraction without a JS runtime has been deprecated, and some formats may be \
                      missing.\nWARNING: [youtube] abc: Some formats may be missing\n";
        assert_eq!(hints(stderr), vec![Hint::NeedsJsRuntime]);
        let stderr = "WARNING: [youtube] abc: n challenge solving failed: Some formats may be missing. Ensure you \
                      have a supported JavaScript runtime\n";
        assert_eq!(hints(stderr), vec![Hint::Outdated]);
        assert_eq!(
            hints("WARNING: [youtube] abc: Some formats may be missing\n"),
            vec![Hint::FormatsMissing]
        );
        assert_eq!(
            hints("WARNING: [generic] Falling back on generic information extractor\n"),
            vec![]
        );
        // ERROR 行不是提醒
        assert_eq!(hints("ERROR: No supported JavaScript runtime\n"), vec![]);
    }

    #[test]
    fn spawn_errors() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(spawn_error(&e), YtdlError::Missing);
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(spawn_error(&e), YtdlError::Spawn(std::io::ErrorKind::PermissionDenied));
        if cfg!(windows) {
            for code in [225, 226, 4551, 1260] {
                assert_eq!(
                    spawn_error(&std::io::Error::from_raw_os_error(code)),
                    YtdlError::Blocked(code)
                );
            }
        }
    }

    #[test]
    fn settings_remedies_point_to_the_network_page() {
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        // 關掉了 yt-dlp：跟沒有 yt-dlp 一樣不加「無法播放網站影片：」，建議打開
        assert_eq!(
            YtdlError::Disabled.message(),
            "網站影片需要 yt-dlp（已在「設定 → 網路」關閉）"
        );
        assert_eq!(YtdlError::Disabled.remedy(), Some(Remedy::EnableYtdl));
        assert!(Remedy::EnableYtdl.advice().contains("設定 → 網路"));
        // Cookie 的建議改成設定頁（不是 yt-dlp 的設定檔）
        assert!(Remedy::UseCookies.advice().contains("設定 → 網路"));
        assert!(!Remedy::UseCookies.advice().contains("--cookies-from-browser"));
        for r in [
            Remedy::GetYtdl,
            Remedy::EnableYtdl,
            Remedy::UseCookies,
            Remedy::TryFirefox,
        ] {
            assert!(r.in_settings(), "{r:?}");
        }
        for r in [Remedy::UpdateYtdl, Remedy::GetDeno] {
            assert!(!r.in_settings(), "{r:?}");
        }
    }

    #[test]
    fn messages_follow_the_ui_language() {
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            YtdlError::Unsupported.message(),
            "Can't play the website video: yt-dlp doesn't support this site"
        );
        assert_eq!(YtdlError::Missing.message(), "Website videos need yt-dlp");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(
            YtdlError::Unsupported.message(),
            "無法播放網站影片：yt-dlp 不支援這個網站"
        );
        assert_eq!(Hint::NeedsJsRuntime.message(), "YouTube 需要 deno 才能取得全部畫質");
        assert!(Remedy::GetYtdl.advice().contains("安裝 yt-dlp"));
        // 只有 Windows 說放在影戲的資料夾（macOS 在 .app 裡面、AppImage 是唯讀的）
        use crate::paths::Os;
        assert!(Remedy::GetYtdl.advice_on(Os::Windows).contains("放在影戲的資料夾"));
        for os in [Os::Macos, Os::Linux] {
            let a = Remedy::GetYtdl.advice_on(os);
            assert!(a.starts_with("請安裝 yt-dlp") && !a.contains("影戲的資料夾"), "{a}");
        }
        assert!(Remedy::GetYtdl.advice_on(Os::Macos).contains("brew install yt-dlp"));
        assert!(Remedy::GetYtdl.advice_on(Os::Linux).contains("~/.local/bin"));
        crate::i18n::set_lang(crate::i18n::Lang::En);
        for os in [Os::Windows, Os::Macos, Os::Linux] {
            assert!(Remedy::GetYtdl.advice_on(os).starts_with("Install yt-dlp"));
        }
        assert!(Remedy::GetYtdl.advice().starts_with("Install yt-dlp"));
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }
}
