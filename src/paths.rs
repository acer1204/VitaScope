//! 影戲自己的資料夾：快取（暫存）與外部工具（之後下載的 yt-dlp、deno）。
//!
//! 設定與播放紀錄在 `settings::config_dir()`（會漫遊、要保留）；這裡的都放在本機、可以整個刪掉：
//! Windows 的解除安裝程式會刪掉整個 `%LOCALAPPDATA%\VitaScope`（packaging/windows/vitascope.iss 的 `[UninstallDelete]`）。

use std::ffi::OsString;
use std::path::PathBuf;

/// 這個資料夾規則是哪個系統的（測試可以在任何系統上檢查每個系統的規則）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// 快取的根資料夾（轉碼後的字幕在它的 `subs`、翻轉用的著色器在 `shaders`）：
/// - Windows：`%LOCALAPPDATA%\VitaScope`
/// - macOS：`~/Library/Caches/VitaScope`
/// - Linux：`$XDG_CACHE_HOME/vitascope`（預設 `~/.cache/vitascope`）
///
/// 放在使用者自己的資料夾，不放共用的 /tmp：多使用者的電腦上，別人無法搶先建立同名資料夾、也無法放入假檔案。
/// 找不到使用者資料夾時才退回系統暫存資料夾，仍放在自己的子資料夾裡
pub fn cache_root() -> PathBuf {
    cache_root_for(Os::current(), &|k| std::env::var_os(k)).unwrap_or_else(|| std::env::temp_dir().join("vitascope"))
}

/// 外部工具的資料夾（影戲下載的 yt-dlp、deno 放這裡；自己安裝的另外找）：
/// - Windows：`%LOCALAPPDATA%\VitaScope\tools`（解除安裝時一起刪掉）
/// - macOS：`~/Library/Application Support/Vitascope/tools`
/// - Linux：`${XDG_DATA_HOME:-~/.local/share}/vitascope/tools`
///
/// 找不到使用者資料夾時 None：不退回共用的暫存資料夾（要執行的程式不能放在別人也能寫的地方）
pub fn tools_dir() -> Option<PathBuf> {
    tools_dir_for(Os::current(), &|k| std::env::var_os(k))
}

/// 環境變數是完整路徑（空字串、相對路徑都不算：XDG 的規定也是忽略相對路徑。相對路徑會跟著目前資料夾變，
/// 之後會從工具資料夾執行程式，不能放在那裡）。`os` 是這條規則的系統，不是正在執行的系統（測試在任何系統上檢查）
fn var(os: Os, env: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    env(key)
        .filter(|v| absolute(os, &v.to_string_lossy()))
        .map(PathBuf::from)
}

/// `p` 在 `os` 上是不是完整路徑：Windows `C:\…`、`C:/…`、`\\server\…`；其他系統 `/…`
pub(crate) fn absolute(os: Os, p: &str) -> bool {
    match os {
        Os::Windows => {
            let b = p.as_bytes();
            (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
                || p.starts_with(r"\\")
        }
        Os::Macos | Os::Linux => p.starts_with('/'),
    }
}

fn cache_root_for(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match os {
        Os::Windows => var(os, env, "LOCALAPPDATA").map(|d| d.join("VitaScope")),
        Os::Macos => var(os, env, "HOME").map(|h| h.join("Library/Caches/VitaScope")),
        Os::Linux => var(os, env, "XDG_CACHE_HOME")
            .or_else(|| var(os, env, "HOME").map(|h| h.join(".cache")))
            .map(|d| d.join("vitascope")),
    }
}

fn tools_dir_for(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let base = match os {
        Os::Windows => var(os, env, "LOCALAPPDATA").map(|d| d.join("VitaScope")),
        Os::Macos => var(os, env, "HOME").map(|h| h.join("Library/Application Support/Vitascope")),
        Os::Linux => var(os, env, "XDG_DATA_HOME")
            .or_else(|| var(os, env, "HOME").map(|h| h.join(".local/share")))
            .map(|d| d.join("vitascope")),
    };
    base.map(|b| b.join("tools"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn each_system_has_its_own_folders() {
        let win = env_of(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local"), ("HOME", "/ignored")]);
        let base = PathBuf::from(r"C:\Users\me\AppData\Local").join("VitaScope");
        assert_eq!(cache_root_for(Os::Windows, &win), Some(base.clone()));
        assert_eq!(tools_dir_for(Os::Windows, &win), Some(base.join("tools")));

        let mac = env_of(&[("HOME", "/Users/me")]);
        assert_eq!(
            cache_root_for(Os::Macos, &mac),
            Some(PathBuf::from("/Users/me").join("Library/Caches/VitaScope"))
        );
        assert_eq!(
            tools_dir_for(Os::Macos, &mac),
            Some(
                PathBuf::from("/Users/me")
                    .join("Library/Application Support/Vitascope")
                    .join("tools")
            )
        );

        // Linux：XDG 變數優先，沒有（或是空字串）時用家目錄底下的預設位置
        let linux = env_of(&[("HOME", "/home/me")]);
        assert_eq!(
            cache_root_for(Os::Linux, &linux),
            Some(PathBuf::from("/home/me").join(".cache").join("vitascope"))
        );
        assert_eq!(
            tools_dir_for(Os::Linux, &linux),
            Some(
                PathBuf::from("/home/me")
                    .join(".local/share")
                    .join("vitascope")
                    .join("tools")
            )
        );
        let xdg = env_of(&[("HOME", "/home/me"), ("XDG_CACHE_HOME", "/c"), ("XDG_DATA_HOME", "/d")]);
        assert_eq!(
            cache_root_for(Os::Linux, &xdg),
            Some(PathBuf::from("/c").join("vitascope"))
        );
        assert_eq!(
            tools_dir_for(Os::Linux, &xdg),
            Some(PathBuf::from("/d").join("vitascope").join("tools"))
        );
        let empty_xdg = env_of(&[("HOME", "/home/me"), ("XDG_DATA_HOME", "")]);
        assert_eq!(
            tools_dir_for(Os::Linux, &empty_xdg),
            Some(
                PathBuf::from("/home/me")
                    .join(".local/share")
                    .join("vitascope")
                    .join("tools")
            )
        );
    }

    #[test]
    fn relative_values_are_ignored() {
        // 相對路徑會跟著目前資料夾變（XDG 也規定要忽略）：XDG 變數退回家目錄，家目錄也是相對的就沒有工具資料夾
        let rel_xdg = env_of(&[("HOME", "/home/me"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "c")]);
        assert_eq!(
            tools_dir_for(Os::Linux, &rel_xdg),
            Some(
                PathBuf::from("/home/me")
                    .join(".local/share")
                    .join("vitascope")
                    .join("tools")
            )
        );
        assert_eq!(
            cache_root_for(Os::Linux, &rel_xdg),
            Some(PathBuf::from("/home/me").join(".cache").join("vitascope"))
        );
        let rel_home = env_of(&[("HOME", "me"), ("XDG_DATA_HOME", "data")]);
        assert_eq!(tools_dir_for(Os::Linux, &rel_home), None);
        assert_eq!(tools_dir_for(Os::Macos, &rel_home), None);
        for bad in ["AppData", r"C:AppData", "/no-drive"] {
            let env = move |k: &str| (k == "LOCALAPPDATA").then(|| OsString::from(bad));
            assert_eq!(tools_dir_for(Os::Windows, &env), None, "{bad}");
        }
        let unc = env_of(&[("LOCALAPPDATA", r"\\server\share\me")]);
        assert_eq!(
            tools_dir_for(Os::Windows, &unc),
            Some(PathBuf::from(r"\\server\share\me").join("VitaScope").join("tools"))
        );
        let slash = env_of(&[("LOCALAPPDATA", "C:/Users/me/AppData/Local")]);
        assert!(tools_dir_for(Os::Windows, &slash).is_some());
    }

    #[test]
    fn no_home_means_no_tools_folder() {
        // 要執行的程式不放到共用的暫存資料夾
        let none = env_of(&[]);
        for os in [Os::Windows, Os::Macos, Os::Linux] {
            assert_eq!(tools_dir_for(os, &none), None);
            assert_eq!(cache_root_for(os, &none), None);
        }
    }

    #[test]
    fn subtitle_cache_path_is_unchanged() {
        // 轉碼後字幕的位置跟以前一樣：這裡是改用 cache_root() 之前 subs::cache_dir() 自己寫的規則
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let old_base = if cfg!(windows) {
            var("LOCALAPPDATA").map(|d| d.join("VitaScope"))
        } else if cfg!(target_os = "macos") {
            var("HOME").map(|h| h.join("Library/Caches/VitaScope"))
        } else {
            var("XDG_CACHE_HOME")
                .or_else(|| var("HOME").map(|h| h.join(".cache")))
                .map(|d| d.join("vitascope"))
        };
        // 環境變數是相對路徑時新的規則故意不同（不用它），這種環境不比
        if let Some(base) = old_base.filter(|b| b.is_absolute()) {
            assert_eq!(crate::subs::cache_dir(), base.join("subs"));
        }
    }
}
