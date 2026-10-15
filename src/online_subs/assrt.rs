//! 射手網（偽）assrt.net 的 API v1：搜尋（`sub/search`）、展開一筆字幕的檔案（`sub/detail`）、下載檔案。
//!
//! - 使用者自己的 API Token（32 個英數字），放在 `Authorization: Bearer` 標頭，**不放進網址**。
//!   照使用者輸入的樣子保存，用的時候才檢查格式（R-C7）；伺服器說無效（1、20001）也是 [`NetError::InvalidToken`]。
//! - 回應一律是 JSON，`status` 是錯誤碼（HTTP 200、4xx、5xx 都可能帶著）：0 = 成功，101 = 搜尋文字太短
//!   （用戶端不自己數字數：中文兩個字的片名也送出去，R-C6），20900 = 找不到，30900 = 太頻繁，30000～30300 = 網站的問題。
//! - 連不上 `api.assrt.net` 時改試一次文件寫的另一個網域。
//! - 下載網址：文件裡是 `http://file*.assrt.net/…`，改成 https（file0 驗證過）；**不退回 http**（R-C5）。
//!   一筆字幕裡的每個檔案都有自己的網址（壓縮檔裡的也是：`/onthefly/…`），影戲不用解壓縮。
//!   只有壓縮檔、VobSub 的字幕要到網頁下載。
//! - 規定要顯示「字幕服務由 assrt.net 提供」（G6 的視窗）。

use super::http::{Kind, Method, Request, Transport};
use super::{AssrtFile, NetError, Provider, ResultId, SearchJob, SearchPage, Site, SubResult, VideoRef};
use crate::subs::SubLang;

/// API 位址
pub const ROOT: &str = "https://api.assrt.net/v1";
/// 連不上時再試的另一個網域（網站文件寫的）
pub const ALT_ROOT: &str = "https://api.makedie.me/v1";
/// 一頁幾個
pub const PAGE_SIZE: u32 = 15;
/// API 回應最多讀多少
const API_LIMIT: u64 = 4 * 1024 * 1024;

/// 檢查 Token 的格式（32 個英數字，前後的空白不算）；空的是 [`NetError::NoToken`]
pub fn check_token(token: &str) -> Result<String, NetError> {
    let t = token.trim();
    if t.is_empty() {
        return Err(NetError::NoToken);
    }
    if t.len() == 32 && t.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(t.to_owned())
    } else {
        Err(NetError::InvalidToken)
    }
}

/// 字幕的網頁（只有壓縮檔時請使用者到這裡下載）
pub fn page_url(id: u64) -> String {
    format!("https://assrt.net/xml/sub/{}/{id}.xml", id / 1000)
}

/// `http://file*.assrt.net/…` 改成 https；其他網址照舊（不是 https 的會被連線拒絕）
pub fn https_url(url: &str) -> String {
    let url = url.trim();
    if let Ok(u) = url::Url::parse(url)
        && u.scheme() == "http"
        && u.host_str()
            .is_some_and(|h| h.starts_with("file") && h.ends_with(".assrt.net"))
    {
        return format!("https{}", &url[4..]);
    }
    url.to_owned()
}

/// 檔名裡的語言標記（`Movie.2019.chs&eng.srt` → `chs&eng`）；認不出來時空的
pub fn name_tag(file_name: &str) -> String {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let Some((stem, _)) = base.rsplit_once('.') else {
        return String::new();
    };
    let last = stem.rsplit('.').next().unwrap_or_default();
    if last != stem && crate::subs::classify_label(last).is_some() {
        super::sanitize_tag(last)
    } else {
        String::new()
    }
}

/// `status` 錯誤碼 → [`NetError`]
pub fn status_error(code: i64) -> NetError {
    match code {
        1 | 20001 => NetError::InvalidToken,
        101 => NetError::QueryTooShort,
        20900 => NetError::NotFound(Site::Assrt),
        30900 => NetError::RateLimited(Site::Assrt),
        _ => NetError::Server {
            site: Site::Assrt,
            code: u32::try_from(code.max(0)).unwrap_or(u32::MAX),
        },
    }
}

/// GET 一個 API（`path` 含查詢字串），回傳 `status` 是 0 的 JSON
fn get(env: &super::OnlineEnv, token: &str, path: &str) -> Result<serde_json::Value, NetError> {
    let roots = std::iter::once(env.assrt_root.as_str()).chain(env.assrt_alt_root.as_deref());
    let mut last = NetError::Offline;
    for root in roots {
        let url = format!("{}{path}", root.trim_end_matches('/'));
        let req =
            Request::new(Method::Get, url, Kind::Api, API_LIMIT).header("Authorization", format!("Bearer {token}"));
        let reply = match env.http.send(&req) {
            Ok(r) => r,
            // 連不上：試另一個網域
            Err(e @ Transport::Connect(_)) => {
                last = NetError::transport(Site::Assrt, e);
                continue;
            }
            Err(e) => return Err(NetError::transport(Site::Assrt, e)),
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&reply.body) else {
            return Err(if reply.status >= 500 {
                NetError::Server {
                    site: Site::Assrt,
                    code: u32::from(reply.status),
                }
            } else {
                NetError::BadResponse(Site::Assrt)
            });
        };
        return match v.get("status").and_then(serde_json::Value::as_i64) {
            Some(0) if reply.status < 400 => Ok(v),
            Some(0) | None => Err(match reply.status {
                429 => NetError::RateLimited(Site::Assrt),
                s if s >= 400 => NetError::Server {
                    site: Site::Assrt,
                    code: u32::from(s),
                },
                _ => NetError::BadResponse(Site::Assrt),
            }),
            Some(code) => Err(status_error(code)),
        };
    }
    Err(last)
}

/// `langlist`（`langcht`、`langchs`、`langeng`、`langdou`（雙語）…）與說明（`简 繁 英`）→ 語言
fn lang_of(lang: Option<&serde_json::Value>) -> (SubLang, String) {
    let desc = lang
        .and_then(|l| l.get("desc"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let list = lang
        .and_then(|l| l.get("langlist"))
        .and_then(serde_json::Value::as_object);
    // 沒有 langlist 時看說明的字（網站用一個字代表一種語言：简 繁 英 日 韩、双语）
    let in_desc = |k: &str| match k {
        "langcht" => desc.contains('繁'),
        "langchs" => desc.contains('简') || desc.contains('簡'),
        "langdou" => desc.contains("双语") || desc.contains("雙語"),
        "langeng" => desc.contains('英'),
        "langjap" => desc.contains('日'),
        _ => false,
    };
    let has = |k: &str| match list {
        Some(m) => m.get(k).is_some_and(|v| v.as_bool().unwrap_or(true)),
        None => in_desc(k),
    };
    let found = match (has("langcht"), has("langchs")) {
        (true, false) => SubLang::Traditional,
        (false, true) => SubLang::Simplified,
        (true, true) => SubLang::Chinese,
        _ if has("langdou") => SubLang::Chinese,
        _ if has("langeng") => SubLang::English,
        _ if has("langjap") || has("langjpn") => SubLang::Japanese,
        _ => crate::subs::classify_label(&desc).unwrap_or(SubLang::Unknown),
    };
    (found, desc)
}

fn text(v: &serde_json::Value, k: &str) -> String {
    v.get(k)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// 搜尋
pub(super) fn search(env: &super::OnlineEnv, job: &SearchJob, video: &VideoRef) -> Result<SearchPage, NetError> {
    let token = check_token(&job.assrt_token)?;
    let q = job.query.trim();
    if q.is_empty() {
        return Err(NetError::QueryTooShort);
    }
    let page = job.page.max(1);
    let path = format!(
        "/sub/search?q={}&cnt={PAGE_SIZE}&pos={}",
        super::encode_query(q, false),
        (page - 1) * PAGE_SIZE
    );
    let results = match get(env, &token, &path) {
        Ok(v) => parse_search(&v),
        // 找不到 = 沒有結果
        Err(NetError::NotFound(_)) => Vec::new(),
        Err(e) => return Err(e),
    };
    let full = results.len() as u32 >= PAGE_SIZE;
    Ok(SearchPage {
        provider: Provider::Assrt,
        video: video.clone(),
        query: job.query.clone(),
        page,
        total_pages: if full { page + 1 } else { page },
        total: None,
        results,
    })
}

/// `sub/search` 的回應 → 每筆字幕一列（還沒展開：要下載先 [`detail`]）
pub fn parse_search(v: &serde_json::Value) -> Vec<SubResult> {
    let Some(subs) = v
        .get("sub")
        .and_then(|s| s.get("subs"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    subs.iter()
        .filter_map(|s| {
            let sub_id = s.get("id").and_then(serde_json::Value::as_u64)?;
            let mut r = SubResult::new(Provider::Assrt, ResultId::Assrt { sub_id, file: None });
            (r.lang, r.lang_code) = lang_of(s.get("lang"));
            r.name = text(s, "native_name");
            let video_name = text(s, "videoname");
            if r.name.is_empty() {
                r.name = video_name;
            } else if !video_name.is_empty() && video_name != r.name {
                r.name = format!("{} / {video_name}", r.name);
            }
            r.format = text(s, "subtype");
            r.rating = s.get("vote_score").and_then(serde_json::Value::as_f64);
            r.release_site = text(s, "release_site");
            r.upload_date = text(s, "upload_time");
            r.archive_only = r.format.to_ascii_lowercase().contains("vobsub");
            r.page_url = Some(page_url(sub_id));
            Some(r)
        })
        .collect()
}

/// 展開一筆字幕：裡面每個字幕檔一列
pub(super) fn detail(env: &super::OnlineEnv, token: &str, sub_id: u64) -> Result<Vec<SubResult>, NetError> {
    let token = check_token(token)?;
    let v = get(env, &token, &format!("/sub/detail?id={sub_id}"))?;
    parse_detail(&v, sub_id)
}

/// `sub/detail` 的回應 → 檔案的列；沒有可以直接下載的檔案（只有壓縮檔、VobSub）時 [`NetError::Archive`]
pub fn parse_detail(v: &serde_json::Value, sub_id: u64) -> Result<Vec<SubResult>, NetError> {
    let page = page_url(sub_id);
    let archive = || NetError::Archive {
        page: Some(page.clone()),
    };
    let sub = v
        .get("sub")
        .and_then(|s| s.get("subs"))
        .and_then(serde_json::Value::as_array)
        .and_then(|a| a.first())
        .ok_or(NetError::NotFound(Site::Assrt))?;
    let (lang, lang_code) = lang_of(sub.get("lang"));
    let row = |name: &str, url: &str, size: String| {
        let ext = super::file_ext(name).filter(|e| super::allowed_ext(e))?;
        let file = AssrtFile {
            name: name.to_owned(),
            url: https_url(url),
            size,
        };
        let mut r = SubResult::new(
            Provider::Assrt,
            ResultId::Assrt {
                sub_id,
                file: Some(file),
            },
        );
        r.lang = crate::subs::classify_label(name).unwrap_or(lang);
        r.lang_code = lang_code.clone();
        r.name = name.to_owned();
        r.format = ext.to_ascii_uppercase();
        r.release_site = text(sub, "release_site");
        r.page_url = Some(page.clone());
        Some(r)
    };
    if text(sub, "subtype").to_ascii_lowercase().contains("vobsub") {
        return Err(archive());
    }
    let mut rows: Vec<SubResult> = sub
        .get("filelist")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let url = text(f, "url");
            (!url.is_empty())
                .then(|| row(&text(f, "f"), &url, text(f, "s")))
                .flatten()
        })
        .collect();
    if rows.is_empty() {
        // 單一檔案的字幕：`url` + `filename`（壓縮檔就請使用者到網頁下載）
        let (name, url) = (text(sub, "filename"), text(sub, "url"));
        if !url.is_empty()
            && let Some(r) = row(&name, &url, String::new())
        {
            rows.push(r);
        }
    }
    if rows.is_empty() {
        return Err(archive());
    }
    // 繁體的排前面
    rows.sort_by_key(|r| std::cmp::Reverse(r.lang));
    Ok(rows)
}

/// 下載一個檔案（網址會過期，不存起來）
pub(super) fn fetch_file(env: &super::OnlineEnv, url: &str) -> Result<Vec<u8>, NetError> {
    let req = Request::new(Method::Get, https_url(url), Kind::Download, super::MAX_SUBTITLE_SIZE);
    let reply = env.http.send(&req).map_err(|e| NetError::transport(Site::Assrt, e))?;
    match reply.status {
        200..=299 => Ok(reply.body),
        404 | 410 => Err(NetError::LinkExpired),
        429 => Err(NetError::RateLimited(Site::Assrt)),
        code => Err(NetError::Server {
            site: Site::Assrt,
            code: u32::from(code),
        }),
    }
}
