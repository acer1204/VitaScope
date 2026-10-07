//! 啟動系統的程式（檔案管理員、fc-list）。
//!
//! Linux AppImage 的啟動腳本（AppRun）在系統沒有 libpulse、libva 時，把替身函式庫的資料夾加進
//! `LD_LIBRARY_PATH`，只給 libmpv 用。啟動別的程式時要還原成原本的值：不然那個程式（例如會試著載入
//! libpulse 的瀏覽器）會拿到替身，呼叫到替身不支援的函式就中止。AppRun 把原本的值記在
//! `VITASCOPE_ORIG_LD_LIBRARY_PATH`；不是從 AppImage 啟動時沒有這個變數，什麼都不改。

use std::ffi::{OsStr, OsString};
use std::process::Command;

const ORIG: &str = "VITASCOPE_ORIG_LD_LIBRARY_PATH";

/// 跟 `Command::new` 一樣，但 `LD_LIBRARY_PATH` 還原成 AppRun 改之前的值
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    restore_env(&mut cmd, std::env::var_os(ORIG));
    cmd
}

fn restore_env(cmd: &mut Command, orig: Option<OsString>) {
    let Some(orig) = orig else { return };
    if orig.is_empty() {
        cmd.env_remove("LD_LIBRARY_PATH");
    } else {
        cmd.env("LD_LIBRARY_PATH", orig);
    }
    cmd.env_remove(ORIG);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(cmd: &Command, key: &str) -> Option<Option<OsString>> {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .map(|(_, v)| v.map(OsStr::to_os_string))
    }

    #[test]
    fn restores_what_apprun_saved() {
        // 原本沒有設定：移除（AppRun 加上的替身路徑不傳下去）
        let mut cmd = Command::new("x");
        restore_env(&mut cmd, Some(OsString::new()));
        assert_eq!(env_of(&cmd, "LD_LIBRARY_PATH"), Some(None));
        assert_eq!(env_of(&cmd, ORIG), Some(None));

        // 原本有值：還原成那個值
        let mut cmd = Command::new("x");
        restore_env(&mut cmd, Some(OsString::from("/opt/lib")));
        assert_eq!(env_of(&cmd, "LD_LIBRARY_PATH"), Some(Some(OsString::from("/opt/lib"))));

        // 不是從 AppImage 啟動：照常繼承
        let mut cmd = Command::new("x");
        restore_env(&mut cmd, None);
        assert_eq!(cmd.get_envs().count(), 0);
    }
}
