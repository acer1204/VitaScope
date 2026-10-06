//! 播放清單：開一個檔案時，同資料夾裡同類的檔案（影片或音樂）依檔名排序成清單，
//! 播完自動接下一個，PgUp / PgDn 切換上一個 / 下一個（比照 PotPlayer）。
//! 一次拖放多個檔案時，清單就是那幾個檔案。

use crate::formats;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Playlist {
    items: Vec<PathBuf>,
    /// 目前的檔案；目前的檔案被移出清單時，是原本接在它後面的那一項
    index: usize,
    /// 目前播放的檔案已經從清單移除（繼續播，下一個是 `index` 那一項）
    current_removed: bool,
    /// 使用者自己整理的清單（拖放多個檔案、開啟清單檔、在面板上編輯過）：關閉程式時存起來。
    /// 同資料夾掃描出來的清單下次開檔時會再掃，不用存
    manual: bool,
}

impl Playlist {
    /// `path` 所在資料夾裡、跟它同類的檔案。讀不到資料夾時，清單只有這個檔案
    pub fn for_file(path: &Path) -> Self {
        let kind = formats::media_kind(path);
        let mut items: Vec<PathBuf> = path
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|entries| {
                entries
                    .flatten()
                    // 先看副檔名，不是同類的檔案就不用再查檔案類型（網路磁碟上每查一次都慢）
                    .filter(|e| kind.is_some() && formats::media_kind(&e.path()) == kind)
                    .filter(|e| !is_hidden(e))
                    .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        sort_by_name(&mut items);
        let mut list = Self::from_files(items);
        if !list.select(path) {
            // 副檔名不在清單上之類的情況：至少包含目前的檔案
            list = Self::from_files(vec![path.to_path_buf()]);
        }
        list
    }

    /// 指定的檔案（例如一次拖放的多個檔案），維持原本的順序
    pub fn from_files(items: Vec<PathBuf>) -> Self {
        Self {
            items,
            index: 0,
            current_removed: false,
            manual: false,
        }
    }

    /// 標成使用者自己整理的清單
    pub fn manual(mut self) -> Self {
        self.manual = true;
        self
    }

    pub fn is_manual(&self) -> bool {
        self.manual
    }

    /// 上次存下的清單：還沒開始播，「下一個」是上次播的那一項
    pub fn restored(items: Vec<PathBuf>, current: Option<usize>) -> Self {
        Self {
            index: current.unwrap_or(0).min(items.len().saturating_sub(1)),
            items,
            current_removed: true,
            manual: true,
        }
    }

    /// 把 `path` 設成目前的檔案；不在清單上就回傳 false
    pub fn select(&mut self, path: &Path) -> bool {
        // 同一個檔案在清單上出現兩次時，優先用目前那一項（不然雙擊第二個會跳到第一個）
        if !self.current_removed && self.items.get(self.index).is_some_and(|p| same_file(p, path)) {
            return true;
        }
        match self.items.iter().position(|p| same_file(p, path)) {
            Some(i) => {
                self.index = i;
                self.current_removed = false;
                true
            }
            None => false,
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.items.iter().any(|p| same_file(p, path))
    }

    pub fn current(&self) -> Option<&Path> {
        if self.current_removed {
            return None;
        }
        self.items.get(self.index).map(PathBuf::as_path)
    }

    /// 目前的檔案在清單上的位置（從 0 開始）；已經移出清單時是 None
    pub fn current_index(&self) -> Option<usize> {
        (!self.current_removed && self.index < self.items.len()).then_some(self.index)
    }

    pub fn next(&self) -> Option<&Path> {
        let next = if self.current_removed {
            self.index
        } else {
            self.index + 1
        };
        self.items.get(next).map(PathBuf::as_path)
    }

    pub fn prev(&self) -> Option<&Path> {
        self.index
            .checked_sub(1)
            .and_then(|i| self.items.get(i))
            .map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 目前是第幾個（從 1 開始）
    pub fn position(&self) -> usize {
        self.index + 1
    }

    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    /// 移出清單（不動檔案）。移掉的是目前的檔案時，照樣繼續播，下一個是原本接在它後面的
    pub fn remove(&mut self, i: usize) {
        if i >= self.items.len() {
            return;
        }
        self.manual = true;
        self.items.remove(i);
        if i < self.index {
            self.index -= 1;
        } else if i == self.index && !self.current_removed {
            self.current_removed = true;
        }
    }

    /// 把第 `from` 項移到第 `to` 項的位置（拖曳排序）；`to` 是移動之前的位置，可以等於 `len()`（放到最後）
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to > self.items.len() || from == to || from + 1 == to {
            return;
        }
        self.manual = true;
        let current = self.current().map(Path::to_path_buf);
        let item = self.items.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.items.insert(to, item);
        if let Some(current) = current {
            self.select(&current);
        } else if self.current_removed {
            // 目前的檔案不在清單上：「下一個」跟著原本的那一項走
            if from == self.index {
                self.index = to;
            } else {
                if from < self.index {
                    self.index -= 1;
                }
                if to <= self.index {
                    self.index += 1;
                }
            }
        }
    }

    /// 加到清單最後（已經在清單上的不重複加）；回傳加了幾個
    pub fn extend(&mut self, files: impl IntoIterator<Item = PathBuf>) -> usize {
        self.manual = true;
        let before = self.items.len();
        for f in files {
            if !self.contains(&f) {
                self.items.push(f);
            }
        }
        self.items.len() - before
    }

    /// 依檔名自然排序，目前的檔案維持是目前的
    pub fn sort(&mut self) {
        self.manual = true;
        let current = self.current().map(Path::to_path_buf);
        let next = self.next().map(Path::to_path_buf);
        sort_by_name(&mut self.items);
        match (current, next) {
            (Some(c), _) => {
                self.select(&c);
            }
            (None, Some(n)) => {
                if let Some(i) = self.items.iter().position(|p| same_file(p, &n)) {
                    self.index = i;
                }
            }
            (None, None) => {}
        }
    }
}

/// 隱藏檔：「.」開頭（包括 macOS 在外接磁碟留下的「._檔名」），以及 Windows 的隱藏 / 系統檔
fn is_hidden(e: &std::fs::DirEntry) -> bool {
    if e.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
        // Windows 上 DirEntry 的 metadata 來自目錄列表本身，不用另外查
        if e.metadata().is_ok_and(|m| m.file_attributes() & HIDDEN_OR_SYSTEM != 0) {
            return true;
        }
    }
    false
}

/// 是否為同一個檔案。Windows 的檔名不分大小寫（包括中文以外的各種字母）
pub fn same_file(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a == b || a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// 依檔名的自然順序排序（第 2 集在第 10 集前面）
pub fn sort_by_name(items: &mut [PathBuf]) {
    items.sort_by(|a, b| {
        let name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        natural_cmp(&name(a), &name(b))
    });
}

/// 「自然排序」：數字部分照數值比，所以「第2集」排在「第10集」前面；其餘不分大小寫
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        digits.push(c);
                    }
                    digits
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m_trim, n_trim) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                // 位數多的比較大；位數一樣就逐字比（不會溢位，多長的數字都可以）
                let ord = m_trim.len().cmp(&n_trim.len()).then_with(|| m_trim.cmp(n_trim));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_for_episodes() {
        let mut names = vec![
            "第10集.mp4",
            "第2集.mp4",
            "第1集.mp4",
            "EP 03.mkv",
            "ep 1.mkv",
            "a.mp4",
            "A.mp4",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "A.mp4",
                "a.mp4",
                "ep 1.mkv",
                "EP 03.mkv",
                "第1集.mp4",
                "第2集.mp4",
                "第10集.mp4"
            ]
        );
        assert_eq!(
            natural_cmp("x007", "x7"),
            Ordering::Less,
            "數值相同時照原字串排，結果才穩定"
        );
        assert_eq!(natural_cmp("s01e9", "s01e10"), Ordering::Less);
        assert_eq!(
            natural_cmp("99999999999999999999999", "100000000000000000000000"),
            Ordering::Less,
            "很長的數字也不會溢位"
        );
    }

    #[test]
    fn steps_through_files() {
        let mut list = Playlist::from_files(vec!["a.mp4".into(), "b.mp4".into(), "c.mp4".into()]);
        assert_eq!(list.prev(), None);
        assert_eq!(list.next(), Some(Path::new("b.mp4")));
        assert!(list.select(Path::new("c.mp4")));
        assert_eq!(list.position(), 3);
        assert_eq!(list.next(), None);
        assert_eq!(list.prev(), Some(Path::new("b.mp4")));
        assert!(!list.select(Path::new("d.mp4")));
        assert_eq!(list.current(), Some(Path::new("c.mp4")), "選不到時維持原本的位置");
    }

    fn names(list: &Playlist) -> Vec<String> {
        list.items().iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn removing_items_keeps_the_current_file() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        list.select(Path::new("c"));
        list.remove(0);
        assert_eq!(list.current(), Some(Path::new("c")));
        assert_eq!(list.position(), 2);
        // 移掉正在播的：繼續播，下一個是原本接在後面的 d，上一個是 b
        list.remove(1);
        assert_eq!(names(&list), ["b", "d"]);
        assert_eq!(list.current(), None);
        assert_eq!(list.current_index(), None);
        assert_eq!(list.next(), Some(Path::new("d")));
        assert_eq!(list.prev(), Some(Path::new("b")));
        list.remove(9);
        assert_eq!(list.len(), 2, "超出範圍的不理會");
    }

    #[test]
    fn moving_items_follows_the_current_file() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        list.select(Path::new("b"));
        // 把 d 拖到最前面
        list.move_item(3, 0);
        assert_eq!(names(&list), ["d", "a", "b", "c"]);
        assert_eq!(list.current(), Some(Path::new("b")));
        // 把 a 拖到最後（to = len）
        list.move_item(1, 4);
        assert_eq!(names(&list), ["d", "b", "c", "a"]);
        assert_eq!(list.current_index(), Some(1));
        // 放回原位：不動
        list.move_item(2, 3);
        assert_eq!(names(&list), ["d", "b", "c", "a"]);
        // 正在播的已移出清單時，「下一個」跟著原本那一項走
        list.remove(1);
        assert_eq!(list.next(), Some(Path::new("c")));
        list.move_item(2, 0);
        assert_eq!(names(&list), ["a", "d", "c"]);
        assert_eq!(list.next(), Some(Path::new("c")));
        list.move_item(2, 0);
        assert_eq!(names(&list), ["c", "a", "d"]);
        assert_eq!(list.next(), Some(Path::new("c")), "拖的就是下一個");
        assert_eq!(list.prev(), None);
    }

    #[test]
    fn extend_skips_duplicates_and_sort_keeps_current() {
        let mut list = Playlist::from_files(vec!["第10集".into(), "第2集".into()]);
        list.select(Path::new("第2集"));
        assert_eq!(list.extend(["第1集".into(), "第2集".into()]), 1);
        list.sort();
        assert_eq!(names(&list), ["第1集", "第2集", "第10集"]);
        assert_eq!(list.current(), Some(Path::new("第2集")));
        // 正在播的已移出清單：排序後「下一個」還是同一個檔案
        list.remove(1);
        assert_eq!(list.next(), Some(Path::new("第10集")));
        list.extend(["第0集".into()]);
        list.sort();
        assert_eq!(list.next(), Some(Path::new("第10集")));
    }

    #[test]
    fn edits_make_a_list_manual_and_restored_lists_start_before_the_saved_item() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into()]);
        assert!(!list.is_manual());
        list.sort();
        assert!(list.is_manual());
        let restored = Playlist::restored(vec!["a".into(), "b".into(), "c".into()], Some(1));
        assert_eq!(restored.current(), None, "還沒開始播");
        assert_eq!(restored.next(), Some(Path::new("b")), "PgDn 從上次那一項開始");
        assert!(restored.is_manual());
    }

    #[test]
    fn duplicates_prefer_the_current_entry() {
        let mut list = Playlist::from_files(vec!["a".into(), "b".into(), "a".into()]);
        list.index = 2;
        assert!(list.select(Path::new("a")));
        assert_eq!(list.position(), 3, "已經在第二個 a 上，不會跳回第一個");
    }

    #[test]
    fn same_folder_same_kind() {
        let dir = std::env::temp_dir().join(format!("vitascope-playlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "第10集.mkv",
            "第2集.mp4",
            "第1集.mkv",
            "第1集.ass",
            "主題曲.mp3",
            "說明.txt",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let list = Playlist::for_file(&dir.join("第2集.mp4"));
        let names: Vec<String> = list
            .items
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["第1集.mkv", "第2集.mp4", "第10集.mkv"],
            "只收影片，字幕、音樂、文字檔不算"
        );
        assert_eq!(list.position(), 2);

        let music = Playlist::for_file(&dir.join("主題曲.mp3"));
        assert_eq!(music.len(), 1);

        // 不認得的副檔名：清單只有它自己
        let other = Playlist::for_file(&dir.join("說明.txt"));
        assert_eq!(other.len(), 1);
        assert_eq!(other.current(), Some(dir.join("說明.txt").as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
