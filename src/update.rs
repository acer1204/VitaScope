//! 檢查更新：查詢 GitHub 上最新的 Release，跟目前版本比較。
//!
//! 只在使用者按下「檢查更新」時才連網，不會自動連線。

use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const AUTHOR: &str = "acer1204";
pub const AUTHOR_URL: &str = "https://github.com/acer1204";
pub const REPO_URL: &str = "https://github.com/acer1204/VitaScope";
pub const RELEASES_URL: &str = "https://github.com/acer1204/VitaScope/releases";
pub const LICENSE_URL: &str = "https://github.com/acer1204/VitaScope/blob/main/LICENSE";
pub const NOTICES_URL: &str = "https://github.com/acer1204/VitaScope/blob/main/packaging/THIRD-PARTY-NOTICES.md";
const LATEST_API: &str = "https://api.github.com/repos/acer1204/VitaScope/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Checking,
    UpToDate {
        latest: String,
    },
    Available {
        latest: String,
    },
    /// GitHub 上還沒有任何正式發佈的版本
    NoRelease,
    Failed(String),
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 「v1.2.3」「1.2.3-beta.1」→ [1, 2, 3]（預發佈標記不比較）
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

/// `latest` 是否比 `current` 新
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        _ => false,
    }
}

/// 解讀 GitHub API `releases/latest` 的回應
pub fn interpret(json: &str, current: &str) -> UpdateStatus {
    let tag = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("tag_name").and_then(|t| t.as_str()).map(str::to_owned));
    match tag {
        Some(latest) if is_newer(&latest, current) => UpdateStatus::Available { latest },
        Some(latest) => UpdateStatus::UpToDate { latest },
        None => UpdateStatus::Failed(crate::tr!("GitHub 的回應格式不正確", "Unexpected response from GitHub").into()),
    }
}

/// 連到 GitHub 查詢（會等待網路，請在背景執行緒呼叫）
pub fn check_blocking() -> UpdateStatus {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let response = agent
        .get(LATEST_API)
        .header("User-Agent", concat!("VitaScope/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call();
    match response {
        Ok(mut r) => match r.body_mut().read_to_string() {
            Ok(body) => interpret(&body, current_version()),
            Err(e) => UpdateStatus::Failed(crate::tf!("讀取回應失敗：{e}", "Cannot read the response: {e}")),
        },
        Err(ureq::Error::StatusCode(code)) => from_http_status(code),
        Err(e) => UpdateStatus::Failed(crate::tf!("無法連線到 GitHub：{e}", "Cannot connect to GitHub: {e}")),
    }
}

/// GitHub 回應錯誤碼時的說明
fn from_http_status(code: u16) -> UpdateStatus {
    match code {
        // 沒有任何 Release 時，GitHub 回 404
        404 => UpdateStatus::NoRelease,
        // 未登入的查詢每小時限 60 次（以 IP 計算，公司或宿舍網路可能共用額度）
        403 | 429 => UpdateStatus::Failed(
            crate::tr!(
                "GitHub 暫時限制查詢次數，請稍後再試，或直接開啟發佈頁面",
                "GitHub is rate-limiting requests. Try again later, or open the releases page"
            )
            .into(),
        ),
        _ => UpdateStatus::Failed(crate::tf!(
            "GitHub 回應錯誤（HTTP {code}）",
            "GitHub returned an error (HTTP {code})"
        )),
    }
}

/// 在背景檢查，完成後喚醒介面重繪
pub fn check_in_background(on_done: impl Fn() + Send + 'static) -> Arc<Mutex<UpdateStatus>> {
    let status = Arc::new(Mutex::new(UpdateStatus::Checking));
    let slot = status.clone();
    // 介面語言記在執行緒上：背景執行緒產生的訊息也要用同一個語言
    let lang = crate::i18n::lang();
    std::thread::spawn(move || {
        crate::i18n::set_lang(lang);
        let result = check_blocking();
        if let Ok(mut s) = slot.lock() {
            *s = result;
        }
        on_done();
    });
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"), "要用數字比較，不是字串比較");
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"), "預發佈標記不算比較新");
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"), "看不懂的標籤不當成新版");
    }

    /// 實際連到 GitHub（需要網路，預設不跑）：cargo test --lib update -- --ignored --nocapture
    #[test]
    #[ignore = "需要網路"]
    fn checks_github_for_real() {
        let status = check_blocking();
        println!("GitHub 查詢結果：{status:?}");
        assert!(!matches!(status, UpdateStatus::Failed(_)), "{status:?}");
    }

    #[test]
    fn explains_http_errors() {
        assert_eq!(from_http_status(404), UpdateStatus::NoRelease);
        for code in [403, 429] {
            assert!(
                matches!(from_http_status(code), UpdateStatus::Failed(m) if m.contains("限制")),
                "{code}"
            );
        }
        assert!(matches!(from_http_status(500), UpdateStatus::Failed(m) if m.contains("500")));
    }

    #[test]
    fn interprets_github_response() {
        let newer = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/acer1204/VitaScope/releases/tag/v0.2.0"}"#;
        assert_eq!(
            interpret(newer, "0.1.0"),
            UpdateStatus::Available {
                latest: "v0.2.0".into()
            }
        );
        let same = r#"{"tag_name":"v0.1.0"}"#;
        assert_eq!(
            interpret(same, "0.1.0"),
            UpdateStatus::UpToDate {
                latest: "v0.1.0".into()
            }
        );
        assert!(matches!(interpret("not json", "0.1.0"), UpdateStatus::Failed(_)));
    }
}
