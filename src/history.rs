//! 播放紀錄：最近開啟的檔案、每個檔案上次看到哪裡（續播）。
//! 跟設定分開存（`history.json`，和 settings.json 放在同一個資料夾），設定檔才不會越來越大。

use crate::playlist::same_file;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 最近開啟的檔案最多記幾個
const MAX_RECENT: usize = 20;
/// 續播位置最多記幾個檔案（超過就丟掉最舊的）
const MAX_POSITIONS: usize = 300;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// 最近開啟的檔案，最新的在前面
    pub recent: Vec<String>,
    /// 上次看到的位置，最新的在前面
    pub positions: Vec<Position>,
    /// 存檔位置；None = 只放在記憶體（自動測試用）
    #[serde(skip)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub path: String,
    /// 秒
    pub time: f64,
}

impl History {
    /// 讀取紀錄（設定資料夾裡的 history.json）
    pub fn load() -> Self {
        match crate::settings::config_dir() {
            Some(dir) => Self::load_from(dir.join("history.json")),
            None => Self::default(),
        }
    }

    /// 讀取指定位置的紀錄；檔案不存在就從空的開始。
    /// 檔案壞了（例如寫到一半斷電）就改名成 history.json.bad 留著，不要被下一次存檔蓋掉
    pub fn load_from(path: PathBuf) -> Self {
        let mut history = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("[vitascope] 播放紀錄格式錯誤，改名成 .bad 後重新開始：{e}");
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        history.path = Some(path);
        history
    }

    /// 修改紀錄並存檔。先讀回磁碟上的版本再套用這次的修改：
    /// 同時開著好幾個播放器時，才不會互相蓋掉對方的紀錄
    pub fn update(&mut self, change: impl FnOnce(&mut History)) -> std::io::Result<()> {
        if let Some(path) = &self.path
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(disk) = serde_json::from_str::<History>(&text)
        {
            self.recent = disk.recent;
            self.positions = disk.positions;
        }
        change(self);
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 先寫暫存檔（每個程序用自己的檔名）再改名，中途當掉或同時存檔都不會留下寫一半的檔案
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(
                serde_json::to_string_pretty(self)
                    .map_err(std::io::Error::other)?
                    .as_bytes(),
            )?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn add_recent(&mut self, path: &str) {
        self.remove_recent(path);
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn remove_recent(&mut self, path: &str) {
        self.recent.retain(|p| !same_path(p, path));
    }

    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }

    /// 忘掉某個檔案的續播位置
    pub fn forget(&mut self, path: &str) {
        self.positions.retain(|p| !same_path(&p.path, path));
    }

    /// 記下 `path` 看到哪裡。剛開始看、快看完、或很短的檔案不記（並清掉舊的紀錄），
    /// 下次開啟才不會跳到片尾，或為了幾秒鐘跳來跳去
    pub fn remember(&mut self, path: &str, time: f64, duration: f64) {
        self.forget(path);
        if worth_resuming(time, duration) {
            self.positions.insert(
                0,
                Position {
                    path: path.to_owned(),
                    time,
                },
            );
            self.positions.truncate(MAX_POSITIONS);
        }
    }

    /// 上次看到的位置（秒）
    pub fn resume_point(&self, path: &str) -> Option<f64> {
        self.positions.iter().find(|p| same_path(&p.path, path)).map(|p| p.time)
    }
}

fn same_path(a: &str, b: &str) -> bool {
    same_file(Path::new(a), Path::new(b))
}

/// 一分鐘以上的檔案，看了 10 秒以上、離結尾還有 15 秒以上才記
pub fn worth_resuming(time: f64, duration: f64) -> bool {
    duration >= 60.0 && time >= 10.0 && time <= duration - 15.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_are_unique_and_capped() {
        let mut h = History::default();
        for i in 0..30 {
            h.add_recent(&format!("{i}.mp4"));
        }
        h.add_recent("5.mp4");
        assert_eq!(h.recent.len(), MAX_RECENT);
        assert_eq!(h.recent[0], "5.mp4");
        assert_eq!(h.recent.iter().filter(|p| *p == "5.mp4").count(), 1);
        h.remove_recent("5.mp4");
        assert!(!h.recent.contains(&"5.mp4".to_owned()));
    }

    #[test]
    fn remembers_only_meaningful_positions() {
        let mut h = History::default();
        h.remember("movie.mkv", 1234.5, 5400.0);
        assert_eq!(h.resume_point("movie.mkv"), Some(1234.5));
        // 看完了：清掉紀錄，下次從頭開始
        h.remember("movie.mkv", 5395.0, 5400.0);
        assert_eq!(h.resume_point("movie.mkv"), None);
        // 才看幾秒、或是很短的檔案：不記
        h.remember("movie.mkv", 3.0, 5400.0);
        h.remember("clip.mp4", 20.0, 30.0);
        assert!(h.positions.is_empty());
    }

    #[test]
    fn positions_are_capped() {
        let mut h = History::default();
        for i in 0..(MAX_POSITIONS + 10) {
            h.remember(&format!("{i}.mkv"), 100.0, 1000.0);
        }
        assert_eq!(h.positions.len(), MAX_POSITIONS);
        assert_eq!(
            h.positions[0].path,
            format!("{}.mkv", MAX_POSITIONS + 9),
            "最新的在前面"
        );
        assert_eq!(h.resume_point("0.mkv"), None, "最舊的被丟掉");
    }

    #[test]
    fn in_memory_history_does_not_touch_disk() {
        let mut h = History::default();
        assert!(h.path.is_none());
        h.update(|h| h.add_recent("a.mp4")).unwrap();
        assert_eq!(h.recent, ["a.mp4"]);
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vitascope-history-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.json")
    }

    #[test]
    fn saves_and_loads_back() {
        let path = temp_file("roundtrip");
        let mut h = History::load_from(path.clone());
        h.update(|h| {
            h.add_recent("movie.mkv");
            h.remember("movie.mkv", 600.0, 5400.0);
        })
        .unwrap();
        let again = History::load_from(path.clone());
        assert_eq!(again.recent, ["movie.mkv"]);
        assert_eq!(again.resume_point("movie.mkv"), Some(600.0));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "暫存檔要改名掉：{leftovers:?}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn two_players_do_not_overwrite_each_other() {
        let path = temp_file("two-players");
        let mut a = History::load_from(path.clone());
        let mut b = History::load_from(path.clone());
        a.update(|h| h.add_recent("a.mkv")).unwrap();
        b.update(|h| h.add_recent("b.mkv")).unwrap();
        a.update(|h| h.remember("a.mkv", 100.0, 1000.0)).unwrap();
        let disk = History::load_from(path.clone());
        assert_eq!(disk.recent, ["b.mkv", "a.mkv"]);
        assert_eq!(disk.resume_point("a.mkv"), Some(100.0));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_file_is_kept_aside() {
        let path = temp_file("corrupt");
        std::fs::write(&path, "{ 壞掉的").unwrap();
        let mut h = History::load_from(path.clone());
        assert!(h.recent.is_empty());
        assert!(path.with_extension("json.bad").exists(), "壞掉的檔案改名留著");
        h.update(|h| h.add_recent("a.mkv")).unwrap();
        assert_eq!(History::load_from(path.clone()).recent, ["a.mkv"]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn windows_paths_ignore_case() {
        let mut h = History::default();
        h.remember(r"Q:\動畫\第1集.MKV", 100.0, 1000.0);
        h.add_recent(r"Q:\動畫\第1集.MKV");
        h.add_recent(r"q:\動畫\第1集.mkv");
        if cfg!(windows) {
            assert_eq!(h.resume_point(r"q:\動畫\第1集.mkv"), Some(100.0));
            assert_eq!(h.recent.len(), 1);
        } else {
            assert_eq!(h.recent.len(), 2);
        }
    }

    #[test]
    fn old_or_partial_files_still_load() {
        let h: History = serde_json::from_str(r#"{ "recent": ["a.mp4"] }"#).unwrap();
        assert_eq!(h.recent, ["a.mp4"]);
        assert!(h.positions.is_empty());
    }
}
