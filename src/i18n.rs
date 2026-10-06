//! 介面語言：繁體中文（預設）、English。
//!
//! 每個顯示給使用者的文字在原地同時寫中文和英文，依目前的語言選一個：
//! - `tr!("播放", "Play")` → `&'static str`
//! - `tf!("音量 {v:.0}%", "Volume {v:.0}%")` → `String`（兩邊都是 `format!` 的格式字串，編譯時就會檢查參數）
//!
//! 不用「中文 → 英文」的對照表：少一個要同步的檔案，翻譯也不會漏掉（漏寫英文編譯不過）。
//! 語言記在目前的執行緒（介面都在主執行緒上畫；自動測試每個測試一個執行緒，不會互相影響）。
//! 開發用的記錄訊息（eprintln）維持中文。

use serde::{Deserialize, Serialize};
use std::cell::Cell;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    #[serde(rename = "zh-TW")]
    ZhTw,
    #[serde(rename = "en")]
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::ZhTw, Lang::En];

    /// 選單上顯示的名稱（用那個語言自己的寫法）
    pub fn name(self) -> &'static str {
        match self {
            Lang::ZhTw => "繁體中文",
            Lang::En => "English",
        }
    }
}

thread_local! {
    static LANG: Cell<Lang> = const { Cell::new(Lang::ZhTw) };
}

/// 設定目前執行緒的介面語言
pub fn set_lang(lang: Lang) {
    LANG.with(|l| l.set(lang));
}

pub fn lang() -> Lang {
    LANG.with(Cell::get)
}

pub fn is_en() -> bool {
    lang() == Lang::En
}

/// 依介面語言選一個字串：`tr!("中文", "English")`
#[macro_export]
macro_rules! tr {
    ($zh:expr, $en:expr $(,)?) => {
        if $crate::i18n::is_en() { $en } else { $zh }
    };
}

/// 依介面語言選一個格式字串：`tf!("中文 {x}", "English {x}")`、`tf!("{} 個", "{} items", n)`
#[macro_export]
macro_rules! tf {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_en() { format!($en $(, $arg)*) } else { format!($zh $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_is_per_thread() {
        set_lang(Lang::En);
        assert_eq!(tr!("播放", "Play"), "Play");
        let n = 3;
        assert_eq!(tf!("{n} 個檔案", "{n} files"), "3 files");
        // 別的執行緒（例如另一個測試）還是預設的中文
        std::thread::spawn(|| assert_eq!(tr!("播放", "Play"), "播放"))
            .join()
            .unwrap();
        set_lang(Lang::ZhTw);
        assert_eq!(tf!("{} 個檔案", "{} files", n), "3 個檔案");
    }

    #[test]
    fn serialized_names() {
        assert_eq!(serde_json::to_string(&Lang::En).unwrap(), "\"en\"");
        assert_eq!(serde_json::from_str::<Lang>("\"zh-TW\"").unwrap(), Lang::ZhTw);
    }
}
