//! 外掛字幕：自己找檔案、判斷編碼和語言，轉成 UTF-8 再交給 mpv。
//!
//! 不用 mpv 內建 sub-auto 的原因（都來自實際影片庫的普查）：
//! - 編碼猜錯：GBK 字幕被猜成 BIG5、UTF-16 的 SSA 被猜成 Shift_JIS，結果整份字幕變亂碼
//! - 語言認不出來：字幕組的標法五花八門（`tc`、`cht`、`BIG5`、`Zh-TW..ass` 雙點、`jptc` 雙語…），
//!   mpv 只認 2～3 個字母的語言碼，認不出來就照字母順序選，常常選到簡體
//!
//! 這裡的做法：檔名標記 + 內容判斷（數「這／这」「們／们」之類的繁簡字）決定語言，
//! 文字字幕轉成 UTF-8 用 `memory://` 交給 mpv；圖形字幕（PGS、VobSub）照原路徑載入。

use std::path::{Path, PathBuf};

/// 文字字幕：讀進來轉碼
pub const TEXT_EXTS: &[&str] = &["srt", "ass", "ssa", "vtt", "smi", "sami", "sub", "txt"];
/// 圖形字幕或二進位容器：照原路徑交給 mpv
pub const BINARY_EXTS: &[&str] = &["idx", "sup", "mks"];
/// 字幕常放的子資料夾
const SUB_DIRS: &[&str] = &["subs", "sub", "subtitles", "字幕"];
/// 超過這個大小的「字幕」不讀（多半不是字幕）
const MAX_TEXT_SIZE: u64 = 32 * 1024 * 1024;

/// 字幕語言（排序 = 偏好順序，越後面越優先）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubLang {
    Unknown,
    English,
    Japanese,
    Simplified,
    /// 中文，但分不出繁簡（例如 `chi`、`中文`，或繁簡混合）
    Chinese,
    Traditional,
}

impl SubLang {
    /// 傳給 mpv 的語言碼
    pub fn code(self) -> Option<&'static str> {
        match self {
            SubLang::Traditional => Some("zh-Hant"),
            SubLang::Simplified => Some("zh-Hans"),
            SubLang::Chinese => Some("zh"),
            SubLang::Japanese => Some("ja"),
            SubLang::English => Some("en"),
            SubLang::Unknown => None,
        }
    }

    pub fn is_chinese(self) -> bool {
        matches!(self, SubLang::Traditional | SubLang::Simplified | SubLang::Chinese)
    }
}

/// 從語言碼、標題或檔名後綴判斷語言，例如 `tc.ass`、`Zh-TW..ass`、`big5`、`繁體中文`、`jptc`。
/// 判斷不出來回傳 None。
pub fn classify_label(label: &str) -> Option<SubLang> {
    let lower = label.to_lowercase();
    // 中文關鍵字（沒有詞邊界，直接找）
    let has = |s: &str| lower.contains(s);
    let cjk_trad = has("繁") || has("正體") || has("正体");
    // 「簡體」用繁體字寫也是簡體；「簡繁」則是雙語（兩個都有 → 不分繁簡）
    let cjk_simp = has("简") || has("簡");
    if cjk_trad && !cjk_simp {
        return Some(SubLang::Traditional);
    }
    if cjk_simp && !cjk_trad {
        return Some(SubLang::Simplified);
    }

    let mut found: Option<SubLang> = None;
    let mut bump = |l: SubLang| found = Some(found.map_or(l, |f| f.max(l)));
    let tokens = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty());
    let mut trad = false;
    let mut simp = false;
    for t in tokens {
        match t {
            "tc" | "cht" | "zht" | "big5" | "hant" | "tw" | "hk" | "mo" | "traditional" | "trad" | "jptc" | "tcjp"
            | "chtjp" | "jpcht" | "chtja" | "jacht" => trad = true,
            "sc" | "chs" | "zhs" | "gb" | "gbk" | "gb2312" | "gb18030" | "hans" | "cn" | "sg" | "simplified"
            | "jpsc" | "scjp" | "chsjp" | "jpchs" | "chsja" | "jachs" => simp = true,
            "chi" | "zho" | "zh" | "chinese" => bump(SubLang::Chinese),
            "ja" | "jp" | "jpn" | "japanese" => bump(SubLang::Japanese),
            "en" | "eng" | "english" => bump(SubLang::English),
            _ => {}
        }
    }
    match (trad, simp) {
        (true, false) => Some(SubLang::Traditional),
        (false, true) => Some(SubLang::Simplified),
        (true, true) => Some(SubLang::Chinese),
        _ if has("中文") || has("中字") || has("華語") => Some(SubLang::Chinese),
        _ if has("日本語") || has("日文") || has("日语") => Some(SubLang::Japanese),
        _ if has("英文") || has("英語") || has("英语") => Some(SubLang::English),
        _ => found,
    }
}

/// 只有繁體或只有簡體才會用的常見字（對話裡常出現）。
/// 避開「體／体」這類常出現在字型名稱的字，也避開「里、后」這類繁體裡也常用的字
const TRAD_MARKERS: &str =
    "這們個說會對來為時沒麼嗎讓過還裡從見現點話問間開關頭樣學國謝歡東兒給認識該應聽覺邊媽滅戰鬥愛隊與當經號無";
const SIMP_MARKERS: &str =
    "这们个说会对来为时没么吗让过还从见现点话问间开关头样学国谢欢东儿给认识该应听觉边妈灭战斗爱队与当经号无";

/// 從字幕內容判斷語言；內容太少或判斷不出來回傳 None
pub fn classify_content(text: &str) -> Option<SubLang> {
    let is_ass = text.contains("[Events]") || text.contains("[events]");
    let (mut trad, mut simp, mut kana, mut han, mut latin) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for line in text.lines() {
        // ASS / SSA 只看對白，樣式行裡的字型名稱（例如「微軟正黑體」）會干擾判斷
        if is_ass && !line.starts_with("Dialogue:") {
            continue;
        }
        for c in line.chars() {
            if TRAD_MARKERS.contains(c) {
                trad += 1;
            } else if SIMP_MARKERS.contains(c) {
                simp += 1;
            }
            match c {
                '\u{3040}'..='\u{30ff}' => kana += 1,
                '\u{4e00}'..='\u{9fff}' => han += 1,
                'a'..='z' | 'A'..='Z' => latin += 1,
                _ => {}
            }
        }
    }
    if trad + simp >= 8 {
        return Some(if trad >= simp * 3 {
            SubLang::Traditional
        } else if simp >= trad * 3 {
            SubLang::Simplified
        } else {
            SubLang::Chinese
        });
    }
    if kana >= 20 && kana * 2 >= han {
        return Some(SubLang::Japanese);
    }
    if han >= 50 {
        return Some(SubLang::Chinese);
    }
    if latin >= 200 && han + kana < 10 {
        return Some(SubLang::English);
    }
    None
}

/// 把字幕檔的位元組轉成文字，回傳（內容, 判斷出的編碼名稱）
pub fn decode(bytes: &[u8]) -> (String, &'static str) {
    // 1. BOM：UTF-8 / UTF-16LE / UTF-16BE
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        return (text.into_owned(), enc.name());
    }
    // 2. 合法的 UTF-8
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_owned(), "UTF-8");
    }
    // 3. 沒有 BOM 的 UTF-16：ASCII 字元的另一半是 0
    if bytes.len() >= 64 {
        let sample = &bytes[..bytes.len().min(4096) & !1];
        let zeros_odd = sample.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        let zeros_even = sample.iter().step_by(2).filter(|b| **b == 0).count();
        let half = sample.len() / 2;
        if zeros_odd * 3 > half && zeros_even * 10 < half {
            let (text, _) = encoding_rs::UTF_16LE.decode_without_bom_handling(bytes);
            return (text.into_owned(), "UTF-16LE");
        }
        if zeros_even * 3 > half && zeros_odd * 10 < half {
            let (text, _) = encoding_rs::UTF_16BE.decode_without_bom_handling(bytes);
            return (text.into_owned(), "UTF-16BE");
        }
    }
    // 4. 舊式編碼（Big5、GBK、Shift_JIS…）交給 chardetng 判斷
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let enc = detector.guess(None, chardetng::Utf8Detection::Allow);
    let (text, _) = enc.decode_without_bom_handling(bytes);
    (text.into_owned(), enc.name())
}

/// 一個外掛字幕檔
#[derive(Debug, Clone)]
pub struct ExternalSub {
    pub path: PathBuf,
    /// 顯示用的標題：檔名去掉影片名稱的部分，例如 `tc.ass`
    pub title: String,
    pub lang: SubLang,
    /// 文字字幕轉成 UTF-8 的內容；圖形字幕是 None（照路徑載入）
    pub text: Option<String>,
    /// 原始編碼（文字字幕）
    pub encoding: Option<&'static str>,
}

impl ExternalSub {
    /// 交給 mpv 載入的路徑：文字字幕轉成 UTF-8 寫到暫存資料夾（不動原檔），圖形字幕用原路徑。
    ///
    /// 不用 `memory://`：那樣整份字幕內容會變成 track-list 裡的檔名，每次軌道變化都要複製好幾 MB。
    pub fn load_path(&self) -> std::io::Result<PathBuf> {
        let Some(text) = &self.text else {
            return Ok(self.path.clone());
        };
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.path.hash(&mut h);
        text.hash(&mut h);
        let ext = self
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let dir = cache_dir();
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{:016x}.{ext}", h.finish()));
        if !out.exists() {
            // 加上 UTF-8 BOM，mpv 就不會再自己猜編碼
            let mut data = vec![0xef, 0xbb, 0xbf];
            data.extend_from_slice(text.as_bytes());
            std::fs::write(&out, data)?;
        }
        Ok(out)
    }
}

/// 轉碼後字幕的暫存資料夾
pub fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("vitascope-subs")
}

/// 清掉超過一天的暫存字幕
pub fn clean_cache() {
    let Ok(entries) = std::fs::read_dir(cache_dir()) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 找出影片的外掛字幕：同資料夾（以及 Subs 之類的子資料夾）裡，檔名以影片名稱開頭的字幕檔
pub fn find_external(video: &Path) -> Vec<PathBuf> {
    let Some(stem) = video.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
        return Vec::new();
    };
    let Some(dir) = video.parent() else { return Vec::new() };
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if SUB_DIRS.contains(&name.as_str()) && e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }

    let mut found = Vec::new();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        for p in &files {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !name.starts_with(&stem) || p.as_path() == video {
                continue;
            }
            // VobSub 是 .idx + .sub 一組：只載入 .idx，對應的 .sub 不當成文字字幕
            if ext == "sub"
                && files
                    .iter()
                    .any(|q| q.with_extension("") == p.with_extension("") && has_ext(q, "idx"))
            {
                continue;
            }
            if TEXT_EXTS.contains(&ext.as_str()) || BINARY_EXTS.contains(&ext.as_str()) {
                found.push(p.clone());
            }
        }
    }
    found.sort();
    found
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// 修正常見的格式問題。
/// 實際遇過：ASS 少了開頭的 `[Script Info]`，直接從 `Title:` 開始，FFmpeg 認不出格式、mpv 打不開
fn repair(ext: &str, text: String) -> String {
    let is_ass = ext == "ass" || ext == "ssa";
    if is_ass && !text.trim_start().starts_with('[') && text.contains("[Events]") {
        return format!("[Script Info]\n{text}");
    }
    text
}

/// 讀取並分析一個外掛字幕檔
pub fn load(path: &Path, video: &Path) -> std::io::Result<ExternalSub> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem_len = video.file_stem().map_or(0, |s| s.to_string_lossy().chars().count());
    // 標題 = 檔名去掉影片名稱，例如「影片.tc.ass」→「tc.ass」
    let suffix: String = file_name.chars().skip(stem_len).collect();
    let title = suffix.trim_start_matches(['.', ' ', '_', '-']).to_owned();
    let title = if title.is_empty() { file_name.clone() } else { title };

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let label_lang = classify_label(&suffix);
    if !TEXT_EXTS.contains(&ext.as_str()) {
        return Ok(ExternalSub {
            path: path.to_path_buf(),
            title,
            lang: label_lang.unwrap_or(SubLang::Unknown),
            text: None,
            encoding: None,
        });
    }

    if std::fs::metadata(path)?.len() > MAX_TEXT_SIZE {
        return Err(std::io::Error::other("檔案太大，不像字幕"));
    }
    let (text, encoding) = decode(&std::fs::read(path)?);
    let text = repair(&ext, text);
    // 內容判斷優先：實際影片庫裡有「檔名標 zh-TW、內容是簡體」的字幕
    let lang = classify_content(&text).or(label_lang).unwrap_or(SubLang::Unknown);
    Ok(ExternalSub {
        path: path.to_path_buf(),
        title,
        lang,
        text: Some(text),
        encoding: Some(encoding),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_seen_in_real_libraries() {
        use SubLang::*;
        let cases = [
            ("tc.ass", Some(Traditional)),
            ("TC.ass", Some(Traditional)),
            ("sc.ass", Some(Simplified)),
            ("zh-TW.ass", Some(Traditional)),
            ("Zh-TW..ass", Some(Traditional)),
            ("Zh-CN..ass", Some(Simplified)),
            ("ZH-CN.ass", Some(Simplified)),
            ("cht.ass", Some(Traditional)),
            ("chs.ass", Some(Simplified)),
            ("zh-Hant.ass", Some(Traditional)),
            ("zh-Hans.ass", Some(Simplified)),
            ("big5.ass", Some(Traditional)),
            ("S01E04.BIG5.ass", Some(Traditional)),
            ("gb.ass", Some(Simplified)),
            ("jptc.ass", Some(Traditional)),
            ("[CHT_JP].ass", Some(Traditional)),
            ("chi.ocr.srt", Some(Chinese)),
            ("ja.ass", Some(Japanese)),
            ("eng", Some(English)),
            ("繁體中文", Some(Traditional)),
            ("简体中文", Some(Simplified)),
            ("簡繁中文", Some(Chinese)),
            ("Japanese", Some(Japanese)),
            ("ass", None),
            (".ass", None),
        ];
        for (label, want) in cases {
            assert_eq!(classify_label(label), want, "{label}");
        }
    }

    #[test]
    fn content_detection() {
        let trad =
            "1\n00:00:01,000 --> 00:00:02,000\n這是我們的世界，你說對不對？\n\n2\n...\n他們會來這裡嗎？為什麼還沒到\n";
        let simp =
            "1\n00:00:01,000 --> 00:00:02,000\n这是我们的世界，你说对不对？\n\n2\n...\n他们会来这里吗？为什么还没到\n";
        assert_eq!(classify_content(trad), Some(SubLang::Traditional));
        assert_eq!(classify_content(simp), Some(SubLang::Simplified));
        let ja = "1\n00:00:01,000 --> 00:00:02,000\nこれはテストです。ありがとうございます。よろしくお願いします。\n";
        assert_eq!(classify_content(ja), Some(SubLang::Japanese));
        let en = "Hello there, this is a subtitle line. ".repeat(10);
        assert_eq!(classify_content(&en), Some(SubLang::English));
        assert_eq!(classify_content("1\n00:00:01,000 --> 00:00:02,000\n♪\n"), None);
    }

    #[test]
    fn ass_style_font_names_do_not_fool_detection() {
        // 樣式用了繁體字型名稱，但對白是簡體
        let ass = "[V4+ Styles]\nStyle: Default,微軟正黑體 這們個說會對來為時,20\n\n[Events]\n\
                   Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,这是我们的世界\n\
                   Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,他们会来这里吗为什么\n";
        assert_eq!(classify_content(ass), Some(SubLang::Simplified));
    }

    #[test]
    fn decodes_encodings_seen_in_real_libraries() {
        let text = "1\r\n00:00:01,000 --> 00:00:02,000\r\n這是我們的世界，影戲播放器測試\r\n\r\n".repeat(20);
        // UTF-16LE 有 BOM（mpv 會誤判成 Shift_JIS 的那種）
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode(&utf16), (text.clone(), "UTF-16LE"));
        // UTF-16LE 沒有 BOM
        let no_bom: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode(&no_bom).0, text);
        // UTF-8 有 BOM / 沒有 BOM
        let mut utf8 = vec![0xef, 0xbb, 0xbf];
        utf8.extend(text.as_bytes());
        assert_eq!(decode(&utf8).0, text);
        assert_eq!(decode(text.as_bytes()).0, text);
        // Big5、GBK（mpv 會把 GBK 誤判成 BIG5 的那種）
        for enc in [encoding_rs::BIG5, encoding_rs::GBK] {
            let (bytes, _, _) = enc.encode(&text);
            let (decoded, name) = decode(&bytes);
            assert_eq!(decoded, text, "{} 被判斷成 {name}", enc.name());
        }
        // 簡體內容的 GBK
        let simp = "这是我们的世界，他们会来这里吗？为什么还没到\r\n".repeat(20);
        let (bytes, _, _) = encoding_rs::GBK.encode(&simp);
        assert_eq!(decode(&bytes).0, simp);
        // 日文 Shift_JIS
        let ja = "これはテストです。字幕の文字コードを判定します。\r\n".repeat(20);
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&ja);
        assert_eq!(decode(&bytes).0, ja);
    }

    #[test]
    fn repairs_ass_without_script_info_header() {
        let broken = "Title: \nScriptType: v4.00+\n\n[V4+ Styles]\nStyle: Default,Arial,20\n\n[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,你好\n";
        assert!(repair("ass", broken.to_owned()).starts_with("[Script Info]\nTitle:"));
        // 正常的檔案、SRT 都不動
        let ok = format!("[Script Info]\n{broken}");
        assert_eq!(repair("ass", ok.clone()), ok);
        assert_eq!(
            repair("srt", "1\n00:00:01,000 --> 00:00:02,000\n[Events]\n".into()),
            "1\n00:00:01,000 --> 00:00:02,000\n[Events]\n"
        );
    }

    #[test]
    fn finds_external_subtitles() {
        let dir = std::env::temp_dir().join(format!("vitascope-subs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Subs")).unwrap();
        for f in [
            "[Group] Show - 01.mkv",
            "[Group] Show - 01.tc.ass",
            "[Group] Show - 01.sc.ass",
            "[Group] Show - 01.idx",
            "[Group] Show - 01.sub",
            "[Group] Show - 02.tc.ass",
            "[Group] Show - 01.nfo",
            "Subs/[Group] Show - 01.eng.srt",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let video = dir.join("[Group] Show - 01.mkv");
        let names: Vec<String> = find_external(&video)
            .iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            names,
            [
                "Subs/[Group] Show - 01.eng.srt",
                "[Group] Show - 01.idx",
                "[Group] Show - 01.sc.ass",
                "[Group] Show - 01.tc.ass"
            ]
        );
        let sub = load(&dir.join("[Group] Show - 01.tc.ass"), &video).unwrap();
        assert_eq!(sub.title, "tc.ass");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
