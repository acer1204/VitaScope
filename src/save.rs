//! 存新檔：先寫暫存檔，寫完再換成正式的名稱，絕不覆蓋已經有的檔案。
//!
//! 匯出（片段、GIF、縮圖總覽圖）、之後下載的字幕都用這裡：
//! - 暫存檔放在目的地的資料夾（同一個磁碟，換名稱不用複製），名稱是 `<名稱>.<行程編號>-<流水號>.vitascope-part.<副檔名>`：
//!   保留真正的副檔名，mpv 寫檔時（`av_guess_format`）才選得到對的格式；行程編號 + 流水號讓兩個影戲、
//!   同一個影戲的兩個工作同時匯出同一部影片時不會寫到同一個暫存檔。
//! - 寫完時（[`finish_new`]）用「硬連結」換成正式名稱：目的地已經有同名的檔案時硬連結會失敗，
//!   不會像改名一樣蓋掉（選好名稱到換名稱之間，別的程式可能剛好建立了同名的檔案）。
//!   FAT、exFAT 之類不支援硬連結的磁碟：Windows 用不取代的搬移，其他系統先確認不存在再改名。
//! - 當掉、被強制結束時留下的暫存檔，下次啟動時由 [`sweep`] 刪掉（只刪影戲自己的、夠舊的）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 暫存檔名稱裡的標記（[`sweep`] 只刪有這個標記的檔案）
pub const PART_MARK: &str = ".vitascope-part.";

/// 同一個行程裡的流水號（每個暫存檔一個）
static SEQ: AtomicU64 = AtomicU64::new(1);

/// Windows：防毒軟體、OneDrive 剛好開著新檔案時，最多重試這麼久
const BUSY_RETRY: Duration = Duration::from_secs(2);

/// 名稱一直被別人搶先用掉時最多換幾次名稱（不會發生；避免無限迴圈）
const MAX_NAME_TRIES: usize = 1000;

/// 暫存檔的完整路徑：`<dir>/<stem>.<pid>-<seq>.vitascope-part.<ext>`。不建立檔案。
/// 跳過已經存在的名稱：流水號每次啟動都從 1 開始、行程編號會重複使用，之前換不成正式名稱而留下的
/// 完整檔案（`export::keep_file`）可能剛好同名，寫檔程式開檔時會把它清空。會查資料夾：在背景執行緒呼叫
pub fn temp_in(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let stem = crate::screenshot::sanitize_stem(stem);
    let mut tries = 0;
    loop {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{stem}.{}-{seq}{PART_MARK}{ext}", std::process::id()));
        tries += 1;
        if std::fs::symlink_metadata(&path).is_err() || tries >= MAX_NAME_TRIES {
            return path;
        }
    }
}

/// 是不是影戲的暫存檔（`temp_in` 產生的名稱）
pub fn is_part(name: &str) -> bool {
    name.contains(PART_MARK)
}

/// 把寫好的暫存檔換成正式的名稱 `wanted`；已經有這個名稱時改用「名稱 (2).副檔名」…，絕不覆蓋。
/// 回傳實際的路徑。暫存檔在成功時刪掉；失敗時留著（內容是完整的，由呼叫的人決定怎麼處理）
pub fn finish_new(temp: &Path, wanted: &Path) -> io::Result<PathBuf> {
    finish_with(temp, wanted, true, &mut |_| {})
}

/// `finish_new` 本體：`hard_links` = 先試硬連結（測試用 false 模擬 FAT）；
/// `before` 在選好名稱、還沒換名稱之前呼叫（測試用來模擬別的程式剛好建立了同名的檔案）
fn finish_with(temp: &Path, wanted: &Path, hard_links: bool, before: &mut dyn FnMut(&Path)) -> io::Result<PathBuf> {
    let dir = wanted.parent().unwrap_or(Path::new("."));
    let name = wanted
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let deadline = Instant::now() + BUSY_RETRY;
    let mut tries = 0;
    loop {
        let target = crate::screenshot::unique_path(dir, &name);
        before(&target);
        match place(temp, &target, hard_links) {
            Ok(()) => return Ok(target),
            // 選好名稱之後有人建立了同名的檔案：換下一個名稱
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < MAX_NAME_TRIES => tries += 1,
            Err(e) if busy(&e) && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e),
        }
    }
}

/// 暫存檔放到 `target`（`target` 已經存在時回傳 AlreadyExists，不覆蓋）
fn place(temp: &Path, target: &Path, hard_links: bool) -> io::Result<()> {
    if hard_links {
        match std::fs::hard_link(temp, target) {
            Ok(()) => {
                remove_soon(temp);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists || busy(&e) => return Err(e),
            // 檔案系統不支援硬連結（FAT、exFAT、部分網路磁碟）：改用不取代的搬移
            Err(_) => {}
        }
    }
    move_no_replace(temp, target)
}

/// 刪掉已經連結到正式名稱的暫存檔；防毒軟體剛好開著時重試一下，還是刪不掉就留給下次啟動的 `sweep`
fn remove_soon(temp: &Path) {
    let deadline = Instant::now() + BUSY_RETRY;
    loop {
        match std::fs::remove_file(temp) {
            Err(e) if busy(&e) && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => return,
        }
    }
}

/// 暫時的錯誤：Windows 上別的程式（防毒軟體、OneDrive）剛好開著檔案
fn busy(e: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED、ERROR_SHARING_VIOLATION、ERROR_LOCK_VIOLATION
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// 搬移，但不取代已經有的檔案
#[cfg(windows)]
fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(std::iter::once(0)).collect() };
    let (from, to) = (wide(from), wide(to));
    // 不給 MOVEFILE_REPLACE_EXISTING：目的地已經有檔案時失敗（ERROR_ALREADY_EXISTS），一步完成、沒有空檔
    // SAFETY: 兩個都是以 0 結尾的 UTF-16 字串，呼叫期間有效
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// 搬移，但不取代已經有的檔案（先確認不存在再改名；只在不支援硬連結的檔案系統上用到）
#[cfg(not(windows))]
fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    std::fs::rename(from, to)
}

/// 刪掉 `dir` 裡影戲留下的暫存檔（名稱有 `.vitascope-part.`、最後修改超過 `older_than`）。回傳刪了幾個
pub fn sweep(dir: &Path, older_than: Duration) -> usize {
    sweep_matching(dir, is_part, older_than)
}

/// 刪掉 `dir` 裡名稱符合 `matches`、最後修改超過 `older_than` 的檔案（不進子資料夾）。回傳刪了幾個。
/// 還在寫的檔案修改時間一直更新，不會被刪；Windows 上別人開著的檔案刪不掉，留到下次
pub fn sweep_matching(dir: &Path, matches: impl Fn(&str) -> bool, older_than: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut removed = 0;
    for e in entries.flatten() {
        if !matches(&e.file_name().to_string_lossy()) {
            continue;
        }
        let old = e
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > older_than);
        if old && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// 這個測試專用的暫存資料夾（每次重建）
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vitascope-save-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parts_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| is_part(n))
            .collect()
    }

    #[test]
    fn temp_name_keeps_extension_and_is_unique() {
        let dir = Path::new("/x");
        let a = temp_in(dir, "第1集 00.12.03-00.12.45", "mkv");
        let b = temp_in(dir, "第1集 00.12.03-00.12.45", "mkv");
        assert_ne!(a, b, "同一個名稱的兩個工作不能用同一個暫存檔");
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        // 真正的副檔名放最後：mpv 依副檔名選寫檔的格式
        assert!(name.ends_with(".mkv"), "{name}");
        assert!(name.starts_with("第1集 00.12.03-00.12.45."), "{name}");
        assert!(name.contains(&format!(".{}-", std::process::id())), "{name}");
        assert!(is_part(&name));
        assert_eq!(a.parent(), Some(dir));
        // 名稱裡不能用的字元換掉（不會變成子資料夾）
        let c = temp_in(dir, "a/b:c", "gif");
        assert_eq!(c.parent(), Some(dir));
        assert!(c.to_string_lossy().ends_with(".gif"));
        assert!(!is_part("影片.mkv") && !is_part("vitascope-part.mkv"));
    }

    #[test]
    fn temp_name_skips_files_that_already_exist() {
        // 之前留下的完整檔案（行程編號重複使用、流水號從 1 開始）剛好同名：不能拿來當暫存檔（開檔會清空它）
        let dir = scratch("taken");
        let probe = temp_in(&dir, "片段", "mkv");
        let name = probe.file_name().unwrap().to_string_lossy().into_owned();
        let seq: u64 = name
            .split_once(&format!(".{}-", std::process::id()))
            .and_then(|(_, rest)| rest.split_once(PART_MARK))
            .and_then(|(n, _)| n.parse().ok())
            .unwrap();
        // 接下來的 50 個名稱都已經有檔案（別的測試同時拿走的流水號也沒關係：拿到的一定是不存在的）
        let taken: Vec<PathBuf> = (seq + 1..=seq + 50)
            .map(|n| dir.join(format!("片段.{}-{n}{PART_MARK}mkv", std::process::id())))
            .collect();
        for p in &taken {
            std::fs::write(p, b"kept").unwrap();
        }
        let fresh = temp_in(&dir, "片段", "mkv");
        assert!(!fresh.exists(), "{}", fresh.display());
        assert!(!taken.contains(&fresh));
        for p in &taken {
            assert_eq!(std::fs::read(p).unwrap(), b"kept");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finish_never_overwrites() {
        for hard_links in [true, false] {
            let dir = scratch(&format!("finish-{hard_links}"));
            let wanted = dir.join("片段.mkv");
            std::fs::write(&wanted, b"old").unwrap();
            let temp = temp_in(&dir, "片段", "mkv");
            std::fs::write(&temp, b"new").unwrap();
            let got = finish_with(&temp, &wanted, hard_links, &mut |_| {}).unwrap();
            assert_eq!(got, dir.join("片段 (2).mkv"));
            assert_eq!(std::fs::read(&wanted).unwrap(), b"old", "已經有的檔案不能被蓋掉");
            assert_eq!(std::fs::read(&got).unwrap(), b"new");
            assert!(parts_in(&dir).is_empty(), "暫存檔要刪掉：{:?}", parts_in(&dir));
            // 名稱沒人用：就用這個名稱
            let temp = temp_in(&dir, "b", "gif");
            std::fs::write(&temp, b"gif").unwrap();
            assert_eq!(
                finish_with(&temp, &dir.join("b.gif"), hard_links, &mut |_| {}).unwrap(),
                dir.join("b.gif")
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn a_name_taken_after_choosing_it_is_not_overwritten() {
        // 選好名稱之後、換名稱之前，別的程式建立了同名的檔案（例如另一個影戲剛好也存好了）
        for hard_links in [true, false] {
            let dir = scratch(&format!("race-{hard_links}"));
            let wanted = dir.join("a.mkv");
            let temp = temp_in(&dir, "a", "mkv");
            std::fs::write(&temp, b"mine").unwrap();
            let mut first = true;
            let got = finish_with(&temp, &wanted, hard_links, &mut |target| {
                if std::mem::take(&mut first) {
                    std::fs::write(target, b"theirs").unwrap();
                }
            })
            .unwrap();
            assert_eq!(got, dir.join("a (2).mkv"));
            assert_eq!(std::fs::read(&wanted).unwrap(), b"theirs", "別人的檔案不能被蓋掉");
            assert_eq!(std::fs::read(&got).unwrap(), b"mine");
            assert!(parts_in(&dir).is_empty());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn a_missing_temp_file_is_an_error() {
        let dir = scratch("missing");
        let temp = temp_in(&dir, "a", "mkv");
        assert!(finish_new(&temp, &dir.join("a.mkv")).is_err());
        assert!(!dir.join("a.mkv").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sweep_removes_only_old_partial_files() {
        let dir = scratch("sweep");
        let hour_ago = SystemTime::now() - Duration::from_secs(2 * 3600);
        let old_part = temp_in(&dir, "old", "mkv");
        let new_part = temp_in(&dir, "new", "mkv");
        let old_video = dir.join("old.mkv");
        for p in [&old_part, &new_part, &old_video] {
            std::fs::write(p, b"x").unwrap();
        }
        for p in [&old_part, &old_video] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(hour_ago)
                .unwrap();
        }
        assert_eq!(sweep(&dir, Duration::from_secs(3600)), 1);
        assert!(!old_part.exists(), "超過一小時的暫存檔要刪");
        assert!(new_part.exists(), "還在寫的（剛修改過的）不能刪");
        assert!(old_video.exists(), "使用者的檔案不能刪");
        assert_eq!(sweep(&dir.join("沒有這個資料夾"), Duration::ZERO), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
