//! 檔案關聯（Windows）：把影戲加到影片、音訊檔的「開啟檔案」選單，並列在「設定 → 預設應用程式」裡。
//!
//! - 只寫目前使用者的登錄檔（HKCU），不需要系統管理員
//! - Windows 10 / 11 不允許程式自己設成預設程式（`UserChoice` 有雜湊保護），只能請使用者到
//!   「預設應用程式」選；這裡提供開啟那個設定頁的按鈕
//! - 不動副檔名本身的預設值；移除時只刪自己寫的部分
//! - 安裝程式（Inno Setup）寫的是同樣的 ProgID，誰都可以移除誰寫的
//!
//! macOS 由 .app 的 Info.plist 宣告，Linux 由 .desktop 檔宣告，不需要這個模組。

use crate::formats;

/// 關聯的副檔名：常見與通用的影片、音訊（罕見的不關聯，見 ROADMAP 第 2 節）
pub fn extensions() -> Vec<(&'static str, bool)> {
    formats::VIDEO
        .iter()
        .map(|e| (*e, true))
        .chain(formats::AUDIO.iter().map(|e| (*e, false)))
        .collect()
}

#[cfg(windows)]
pub use win::*;

#[cfg(test)]
mod tests {
    use crate::formats;

    /// 安裝程式（packaging/windows/vitascope.iss）的副檔名清單要跟程式的一樣，兩邊才能互相移除
    #[test]
    fn installer_lists_the_same_extensions() {
        let iss =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/windows/vitascope.iss")).unwrap();
        let list = |name: &str| -> Vec<String> {
            let line = iss
                .lines()
                .find(|l| l.starts_with(&format!("#dim {name}[")))
                .unwrap_or_else(|| panic!("找不到 {name}"));
            let (head, body) = line.split_once('{').unwrap();
            let items: Vec<String> = body
                .trim_end_matches('}')
                .split(',')
                .map(|e| e.trim().trim_matches('"').to_owned())
                .collect();
            let size: usize = head.split(['[', ']']).nth(1).unwrap().parse().unwrap();
            assert_eq!(size, items.len(), "{name} 宣告的大小");
            items
        };
        assert_eq!(list("VideoExts"), formats::VIDEO);
        assert_eq!(list("AudioExts"), formats::AUDIO);
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// 寫在登錄檔哪裡（測試時換成別的位置，不會動到真的關聯）
    #[derive(Debug, Clone)]
    pub struct Places {
        pub classes: String,
        pub app: String,
        pub registered: String,
    }

    impl Default for Places {
        fn default() -> Self {
            Self {
                classes: r"Software\Classes".to_owned(),
                app: r"Software\VitaScope".to_owned(),
                registered: r"Software\RegisteredApplications".to_owned(),
            }
        }
    }

    /// 「預設應用程式」裡的名稱（也是設定頁的參數）
    const REGISTERED_NAME: &str = "VitaScope";
    const EXE_NAME: &str = "vitascope.exe";
    pub const PROGID_VIDEO: &str = "VitaScope.Video";
    pub const PROGID_AUDIO: &str = "VitaScope.Audio";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(code: u32) -> io::Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }

    /// HKCU\subkey 的字串值（name = None 是預設值），機碼不存在就建立
    fn set(subkey: &str, name: Option<&str>, value: &str) -> io::Result<()> {
        let mut key: HKEY = null_mut();
        let name_w = name.map(wide);
        let data = wide(value);
        // SAFETY: 標準的登錄檔呼叫；字串都以 0 結尾
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                null(),
                &mut key,
                null_mut(),
            ))?;
            let r = RegSetValueExW(
                key,
                name_w.as_ref().map_or(null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            );
            RegCloseKey(key);
            check(r)
        }
    }

    fn get(subkey: &str, name: Option<&str>) -> Option<String> {
        let name_w = name.map(wide);
        let mut buf = vec![0u16; 2048];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: 緩衝區大小以位元組傳入
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                name_w.as_ref().map_or(null(), |n| n.as_ptr()),
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        (r == ERROR_SUCCESS).then(|| {
            buf.truncate((bytes as usize / 2).saturating_sub(1));
            String::from_utf16_lossy(&buf)
        })
    }

    fn delete_tree(subkey: &str) {
        let w = wide(subkey);
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, w.as_ptr());
            windows_sys::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, w.as_ptr());
        }
    }

    fn delete_value(subkey: &str, name: &str) {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(subkey).as_ptr(), wide(name).as_ptr()) };
    }

    /// 通知檔案總管關聯改了（不然要登出才會更新圖示、選單）
    fn notify_shell() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    }

    fn open_command(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    /// 加到「開啟檔案」選單、列在預設應用程式裡。可以重複呼叫（免安裝版搬到別的資料夾後再呼叫一次就更新路徑）
    pub fn register(exe: &Path, places: &Places) -> io::Result<()> {
        let command = open_command(exe);
        let icon = format!("{},0", exe.display());
        let classes = &places.classes;
        let app = format!(r"{classes}\Applications\{EXE_NAME}");
        set(&app, Some("FriendlyAppName"), "影戲 VitaScope")?;
        set(&format!(r"{app}\DefaultIcon"), None, &icon)?;
        set(&format!(r"{app}\shell\open\command"), None, &command)?;
        for (progid, name) in [(PROGID_VIDEO, "影片檔"), (PROGID_AUDIO, "音訊檔")] {
            let key = format!(r"{classes}\{progid}");
            set(&key, None, name)?;
            set(&format!(r"{key}\DefaultIcon"), None, &icon)?;
            // 檔案總管選超過 15 個檔案時「開啟」會消失；Player 放寬到 100
            set(&format!(r"{key}\shell\open"), Some("MultiSelectModel"), "Player")?;
            set(&format!(r"{key}\shell\open\command"), None, &command)?;
        }
        let caps = format!(r"{}\Capabilities", places.app);
        for (ext, video) in super::extensions() {
            let progid = if video { PROGID_VIDEO } else { PROGID_AUDIO };
            set(&format!(r"{classes}\.{ext}\OpenWithProgids"), Some(progid), "")?;
            set(&format!(r"{app}\SupportedTypes"), Some(&format!(".{ext}")), "")?;
            set(&format!(r"{caps}\FileAssociations"), Some(&format!(".{ext}")), progid)?;
        }
        set(&caps, Some("ApplicationName"), "影戲 VitaScope")?;
        set(&caps, Some("ApplicationDescription"), "以 libmpv 為引擎的影片播放器")?;
        set(&caps, Some("ApplicationIcon"), &icon)?;
        set(&places.registered, Some(REGISTERED_NAME), &caps)?;
        notify_shell();
        Ok(())
    }

    /// 只刪自己寫的；副檔名本身的預設值、使用者選的預設程式都不動（指到已刪掉的 ProgID 時 Windows 會忽略）
    pub fn unregister(places: &Places) {
        let classes = &places.classes;
        for (ext, video) in super::extensions() {
            let progid = if video { PROGID_VIDEO } else { PROGID_AUDIO };
            delete_value(&format!(r"{classes}\.{ext}\OpenWithProgids"), progid);
        }
        delete_tree(&format!(r"{classes}\{PROGID_VIDEO}"));
        delete_tree(&format!(r"{classes}\{PROGID_AUDIO}"));
        delete_tree(&format!(r"{classes}\Applications\{EXE_NAME}"));
        delete_value(&places.registered, REGISTERED_NAME);
        delete_tree(&places.app);
        notify_shell();
    }

    /// 已經登錄、而且指向這個執行檔
    pub fn is_registered(exe: &Path, places: &Places) -> bool {
        let key = format!(r"{}\{PROGID_VIDEO}\shell\open\command", places.classes);
        get(&key, None).as_deref() == Some(open_command(exe).as_str())
    }

    /// 開啟「設定 → 預設應用程式」裡影戲的那一頁（Windows 11 2023 年 4 月之後；舊版開的是預設應用程式頁）
    pub fn open_default_apps_settings() {
        let uri = wide(&format!("ms-settings:defaultapps?registeredAppUser={REGISTERED_NAME}"));
        let verb = wide("open");
        unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), uri.as_ptr(), null(), null(), SW_SHOWNORMAL) };
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 寫到測試用的位置（HKCU\Software\VitaScopeTest-<pid>），測完刪掉
        fn test_places() -> (Places, String) {
            let root = format!(r"Software\VitaScopeTest-{}", std::process::id());
            let places = Places {
                classes: format!(r"{root}\Classes"),
                app: format!(r"{root}\App"),
                registered: format!(r"{root}\RegisteredApplications"),
            };
            (places, root)
        }

        #[test]
        fn register_and_unregister() {
            let (places, root) = test_places();
            let exe = Path::new(r"C:\Programs\影戲\vitascope.exe");
            register(exe, &places).unwrap();
            assert!(is_registered(exe, &places));
            assert!(
                !is_registered(Path::new(r"D:\別處\vitascope.exe"), &places),
                "搬家後要重新登錄"
            );
            let mp4 = get(&format!(r"{}\.mp4\OpenWithProgids", places.classes), Some(PROGID_VIDEO));
            assert_eq!(mp4.as_deref(), Some(""));
            let flac = get(
                &format!(r"{}\.flac\OpenWithProgids", places.classes),
                Some(PROGID_AUDIO),
            );
            assert_eq!(flac.as_deref(), Some(""));
            assert_eq!(
                get(&format!(r"{}\{PROGID_VIDEO}\shell\open\command", places.classes), None).as_deref(),
                Some("\"C:\\Programs\\影戲\\vitascope.exe\" \"%1\"")
            );
            unregister(&places);
            assert!(!is_registered(exe, &places));
            assert!(get(&format!(r"{}\.mp4\OpenWithProgids", places.classes), Some(PROGID_VIDEO)).is_none());
            delete_tree(&root);
        }
    }
}
