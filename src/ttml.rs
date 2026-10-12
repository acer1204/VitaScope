//! TTML／DFXP 字幕轉成 SRT。
//!
//! FFmpeg 只有 TTML 的輸出（muxer、encoder），沒有讀 TTML 的分離器：直接交給 mpv 會載入失敗（-12）。
//! 影戲自己轉成 SRT（斜體、粗體、底線、顏色變成 SRT 的標記；在畫面上半部的區域變成 `{\an8}`），
//! 再走一般外掛字幕的流程（`subs::load_as` → 暫存檔 → mpv）。
//!
//! - 認得：TTML1／TTML2、IMSC 的文字字幕、舊版 DFXP（2006 年的命名空間）
//! - 時間：時鐘（`00:01:02.5`）、影格（`00:01:02:12`、`00:01:02:12.1`）、偏移（`1.5s`、`500ms`、`12f`、`10000000t`、`2m`、`1h`），
//!   `ttp:frameRate`、`ttp:frameRateMultiplier`、`ttp:subFrameRate`、`ttp:tickRate`
//! - 注音（`tts:ruby`）：SRT 畫不了，讀音放在括號裡接在字的後面（`漢字(かんじ)`）
//! - 時間容器：子元素的時間從父元素的開始算（`par`）；`timeContainer="seq"` 接在前一個後面
//! - 不支援：圖片字幕（IMSC image）、`ttp:timeBase="smpte"`／`"clock"`（回傳 [`TtmlError::Unsupported`]）
//! - 安全：quick-xml 不展開 DTD 與外部實體；元素深度、字幕句數有上限（檔案大小的上限在 `subs`）

use std::collections::HashMap;
use std::fmt;

use quick_xml::NsReader;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};

/// TTML 的命名空間：現行的，與舊版 DFXP 的兩個
const TT_NAMESPACES: &[&str] = &[
    "http://www.w3.org/ns/ttml",
    "http://www.w3.org/2006/10/ttaf1",
    "http://www.w3.org/2006/04/ttaf1",
];
const XML_NAMESPACE: &[u8] = b"http://www.w3.org/XML/1998/namespace";
/// 判斷內容是不是 TTML 時只看開頭這麼多位元組
pub const SNIFF_LEN: usize = 2048;
/// 元素最多幾層（正常的字幕不到 10 層；太深的多半是壞掉或惡意的檔案）
const MAX_DEPTH: usize = 64;
/// 最多幾句字幕
pub const MAX_CUES: usize = 50_000;
/// 樣式參照樣式，最多追幾層（也擋住互相參照的迴圈）
const MAX_STYLE_CHAIN: usize = 8;
/// 沒寫結束時間的一句：顯示到下一句開始，最多這麼多秒
const OPEN_END: f64 = 5.0;

/// 轉換失敗的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtmlError {
    /// 不是 TTML：根元素不是 TTML 命名空間的 `tt`
    NotTtml,
    /// XML 格式錯誤（`position` = 出錯的位元組位置）
    Xml { position: u64, message: String },
    /// 認得，但不支援
    Unsupported(Unsupported),
    /// 沒有任何一句字幕
    Empty,
    /// 元素一層包一層太深
    TooDeep,
    /// 字幕太多句
    TooManyCues,
}

/// 不支援的 TTML
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// 圖片字幕（IMSC image profile、`smpte:image`）
    Images,
    /// `ttp:timeBase` 不是 `media`（`smpte`、`clock`）
    TimeBase(String),
}

impl fmt::Display for TtmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            TtmlError::NotTtml => crate::tr!("不是 TTML 字幕", "not a TTML subtitle").to_owned(),
            TtmlError::Xml { position, message } => crate::tf!(
                "XML 格式錯誤（第 {position} 個位元組）：{message}",
                "malformed XML (byte {position}): {message}"
            ),
            TtmlError::Unsupported(Unsupported::Images) => crate::tr!(
                "圖片字幕（IMSC image）不支援",
                "image subtitles (IMSC image profile) are not supported"
            )
            .to_owned(),
            TtmlError::Unsupported(Unsupported::TimeBase(base)) => crate::tf!(
                "不支援的時間基準 ttp:timeBase=\"{base}\"",
                "unsupported time base ttp:timeBase=\"{base}\""
            ),
            TtmlError::Empty => crate::tr!("沒有任何一句字幕", "no subtitle cues").to_owned(),
            TtmlError::TooDeep => crate::tf!(
                "元素超過 {MAX_DEPTH} 層",
                "elements nested more than {MAX_DEPTH} levels deep"
            ),
            TtmlError::TooManyCues => crate::tf!("超過 {MAX_CUES} 句字幕", "more than {MAX_CUES} subtitle cues"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for TtmlError {}

/// 轉換的結果
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    /// SRT 的內容
    pub srt: String,
    /// 根元素的 `xml:lang`（例如 `en`、`zh-TW`）；沒寫是 None
    pub lang: Option<String>,
    /// 幾句
    pub cues: usize,
}

/// 內容看起來是 TTML：開頭 [`SNIFF_LEN`] 位元組裡，第一個元素是 TTML 命名空間的 `tt`。
/// 給副檔名看不出格式的檔案（`.xml`）用；只看開頭，不檢查整份檔案
pub fn looks_like_ttml(text: &str) -> bool {
    let mut end = text.len().min(SNIFF_LEN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = text[..end].trim_start_matches('\u{feff}');
    let mut reader = NsReader::from_str(head);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let (ns, local) = reader.resolver().resolve_element(e.name());
                return local.as_ref() == b"tt" && matches!(ns, ResolveResult::Bound(n) if is_tt_namespace(n.as_ref()));
            }
            // 根元素之前有文字：不是 XML（例如 SRT 開頭的編號）
            Ok(Event::Text(t)) if !t.iter().all(u8::is_ascii_whitespace) => return false,
            Ok(Event::Eof) => return false,
            Ok(_) => {}
            // 根元素的開始標籤超過開頭這一段（被截斷）：改用文字比對
            Err(_) => return first_tag_is_tt(head) && TT_NAMESPACES.iter().any(|ns| head.contains(ns)),
        }
    }
}

/// 第一個開始標籤（跳過 `<?…?>`、`<!…>`）的名稱是 `tt` 或 `前綴:tt`
fn first_tag_is_tt(head: &str) -> bool {
    let mut rest = head;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        if rest.starts_with(['?', '!']) {
            continue;
        }
        let name: &str = rest
            .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .next()
            .unwrap_or_default();
        return name.rsplit(':').next() == Some("tt");
    }
    false
}

fn is_tt_namespace(ns: &[u8]) -> bool {
    TT_NAMESPACES.iter().any(|t| t.as_bytes() == ns)
}

/// TTML 轉成 SRT
pub fn to_srt(text: &str) -> Result<String, TtmlError> {
    convert(text).map(|c| c.srt)
}

/// TTML 轉成 SRT，連同語言、句數
pub fn convert(text: &str) -> Result<Converted, TtmlError> {
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = NsReader::from_str(text);
    let mut doc = Doc::default();
    loop {
        let event = reader.read_event().map_err(|e| xml_error(&reader, e))?;
        match event {
            Event::Start(e) => doc.start(&reader, &e)?,
            Event::Empty(e) => {
                doc.start(&reader, &e)?;
                doc.end()?;
            }
            Event::End(_) => doc.end()?,
            Event::Text(t) => {
                let s = t.xml10_content().map_err(|e| xml_error(&reader, e))?;
                doc.text(&s);
            }
            Event::CData(c) => {
                let s = c.decode().map_err(|e| xml_error(&reader, e))?;
                doc.text(&s);
            }
            Event::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref().map_err(|e| xml_error(&reader, e))? {
                    doc.text(c.encode_utf8(&mut [0; 4]));
                } else {
                    let name = r.decode().map_err(|e| xml_error(&reader, e))?;
                    match resolve_predefined_entity(&name) {
                        Some(s) => doc.text(s),
                        // 沒有定義的實體（DTD 裡的不展開）：照原樣留著
                        None => doc.text(&format!("&{name};")),
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !doc.root_seen {
        return Err(TtmlError::NotTtml);
    }
    if !doc.stack.is_empty() {
        return Err(TtmlError::Xml {
            position: reader.buffer_position(),
            message: crate::tr!("檔案在元素結束之前就結束了", "the file ends inside an element").to_owned(),
        });
    }
    doc.finish()
}

fn xml_error(reader: &NsReader<&[u8]>, e: impl fmt::Display) -> TtmlError {
    TtmlError::Xml {
        position: reader.error_position(),
        message: e.to_string(),
    }
}

// ───────────── 時間 ─────────────

/// 時間的參數（根元素的 `ttp:*`）
#[derive(Debug, Clone, Copy)]
struct Params {
    /// 實際的格率（`frameRate × frameRateMultiplier`）
    frame_rate: f64,
    sub_frame_rate: f64,
    tick_rate: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            frame_rate: 30.0,
            sub_frame_rate: 1.0,
            tick_rate: 1.0,
        }
    }
}

/// 正整數（`ttp:frameRate` 之類）
fn positive_int(s: &str) -> Option<f64> {
    let s = s.trim();
    let v: u32 = s.parse().ok()?;
    (v > 0).then_some(f64::from(v))
}

/// 只有數字的整數部分（時間裡的時、分、秒、格）
fn digits(s: &str) -> Option<f64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse::<f64>().ok())
        .flatten()
}

/// `12`、`12.5`（有的檔案用逗號當小數點）
fn decimal(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let ok = (!int.is_empty() || !frac.is_empty())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| s.parse::<f64>().ok()).flatten()
}

/// TTML 的時間（秒）；看不懂回傳 None（當成沒寫）
fn parse_time(s: &str, p: &Params) -> Option<f64> {
    let s = s.trim();
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        return match parts.as_slice() {
            [h, m, sec] => Some(digits(h)? * 3600.0 + digits(m)? * 60.0 + decimal(sec)?),
            [h, m, sec, frames] => {
                let (f, sub) = frames.split_once('.').unwrap_or((frames, "0"));
                let frames = digits(f)? + digits(sub)? / p.sub_frame_rate;
                Some(digits(h)? * 3600.0 + digits(m)? * 60.0 + digits(sec)? + frames / p.frame_rate)
            }
            _ => None,
        };
    }
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, metric) = s.split_at(split);
    let v = decimal(num)?;
    let scale = match metric {
        "h" => 3600.0,
        "m" => 60.0,
        // 沒有單位不合規格，有的檔案這樣寫：當成秒
        "s" | "" => 1.0,
        "ms" => 0.001,
        "f" => 1.0 / p.frame_rate,
        "t" => 1.0 / p.tick_rate,
        _ => return None,
    };
    Some(v * scale)
}

// ───────────── 樣式、區域 ─────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Length {
    Percent(f64),
    Px(f64),
    /// 格（`ttp:cellResolution`）
    Cell(f64),
}

fn parse_length(s: &str) -> Option<Length> {
    let s = s.trim();
    let num = |n: &str| n.trim().parse::<f64>().ok().filter(|v| v.is_finite());
    if let Some(n) = s.strip_suffix('%') {
        num(n).map(Length::Percent)
    } else if let Some(n) = s.strip_suffix("px") {
        num(n).map(Length::Px)
    } else if let Some(n) = s.strip_suffix('c') {
        num(n).map(Length::Cell)
    } else {
        None
    }
}

/// 兩個長度（`tts:origin="10% 80%"`、`tts:extent`）
fn parse_pair(s: &str) -> Option<(Length, Length)> {
    let mut it = s.split_whitespace();
    let pair = (parse_length(it.next()?)?, parse_length(it.next()?)?);
    it.next().is_none().then_some(pair)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Before,
    Center,
    After,
}

/// `tts:ruby`（TTML2、IMSC 1.1 的注音，日文字幕常用）：SRT 畫不了注音，讀音放在括號裡接在後面
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ruby {
    /// `text`、`textContainer`：讀音
    Text,
    /// `delimiter`：不顯示注音時才用的括號（影戲自己加括號，這個就不顯示）
    Delimiter,
    /// `container`、`base`、`baseContainer`、`none`：照一般的文字
    Other,
}

/// 用得到的樣式。每一項 None = 沒指定
#[derive(Debug, Clone, Default, PartialEq)]
struct Style {
    italic: Option<bool>,
    bold: Option<bool>,
    underline: Option<bool>,
    /// `Some(None)` = 預設的顏色（白色、透明）
    color: Option<Option<[u8; 3]>>,
    origin: Option<(Length, Length)>,
    extent: Option<(Length, Length)>,
    display_align: Option<Align>,
    /// 不繼承（只看元素自己的、參照的樣式）
    ruby: Option<Ruby>,
}

impl Style {
    /// `over` 有指定的項目蓋過自己的
    fn apply(&mut self, over: &Style) {
        fn set<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
            if src.is_some() {
                dst.clone_from(src);
            }
        }
        set(&mut self.italic, &over.italic);
        set(&mut self.bold, &over.bold);
        set(&mut self.underline, &over.underline);
        set(&mut self.color, &over.color);
        set(&mut self.origin, &over.origin);
        set(&mut self.extent, &over.extent);
        set(&mut self.display_align, &over.display_align);
        set(&mut self.ruby, &over.ruby);
    }

    /// 會繼承給子元素的（文字的樣式；位置、大小是區域的，不繼承）
    fn inheritable(&self) -> Style {
        Style {
            italic: self.italic,
            bold: self.bold,
            underline: self.underline,
            color: self.color,
            ..Style::default()
        }
    }

    /// 一個 `tts:*` 屬性
    fn set_attr(&mut self, name: &[u8], value: &str) {
        let v = value.trim();
        match name {
            b"fontStyle" => self.italic = Some(v == "italic" || v == "oblique"),
            b"fontWeight" => self.bold = Some(v == "bold"),
            b"textDecoration" => {
                for token in v.split_whitespace() {
                    match token {
                        "underline" => self.underline = Some(true),
                        "noUnderline" | "none" => self.underline = Some(false),
                        _ => {}
                    }
                }
            }
            b"color" => {
                if let Some(c) = parse_color(v) {
                    self.color = Some(c);
                }
            }
            b"origin" => self.origin = parse_pair(v),
            b"extent" => self.extent = parse_pair(v),
            b"ruby" => {
                self.ruby = Some(match v {
                    "text" | "textContainer" => Ruby::Text,
                    "delimiter" => Ruby::Delimiter,
                    _ => Ruby::Other,
                })
            }
            b"displayAlign" => {
                self.display_align = match v {
                    "before" => Some(Align::Before),
                    "center" => Some(Align::Center),
                    "after" => Some(Align::After),
                    _ => None,
                }
            }
            _ => {}
        }
    }
}

/// `tts:color`：`#rrggbb`、`#rrggbbaa`、`rgb(r,g,b)`、`rgba(r,g,b,a)`、具名的顏色。
/// 透明（alpha 0、`transparent`）回傳 `Some(None)`；看不懂回傳 None
fn parse_color(s: &str) -> Option<Option<[u8; 3]>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let rgb = [byte(0)?, byte(2)?, byte(4)?];
        let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
        return Some((alpha > 0).then_some(rgb));
    }
    let lower = s.to_ascii_lowercase();
    for (prefix, n) in [("rgba(", 4), ("rgb(", 3)] {
        if let Some(args) = lower.strip_prefix(prefix).and_then(|r| r.strip_suffix(')')) {
            let v: Vec<u8> = args.split(',').filter_map(|a| a.trim().parse().ok()).collect();
            if v.len() != n {
                return None;
            }
            let alpha = if n == 4 { v[3] } else { 255 };
            return Some((alpha > 0).then_some([v[0], v[1], v[2]]));
        }
    }
    let rgb = match lower.as_str() {
        "transparent" => return Some(None),
        "black" => [0, 0, 0],
        "silver" => [0xc0, 0xc0, 0xc0],
        "gray" | "grey" => [0x80, 0x80, 0x80],
        "white" => [0xff, 0xff, 0xff],
        "maroon" => [0x80, 0, 0],
        "red" => [0xff, 0, 0],
        "purple" => [0x80, 0, 0x80],
        "fuchsia" | "magenta" => [0xff, 0, 0xff],
        "green" => [0, 0x80, 0],
        "lime" => [0, 0xff, 0],
        "olive" => [0x80, 0x80, 0],
        "yellow" => [0xff, 0xff, 0],
        "navy" => [0, 0, 0x80],
        "blue" => [0, 0, 0xff],
        "teal" => [0, 0x80, 0x80],
        "aqua" | "cyan" => [0, 0xff, 0xff],
        _ => return None,
    };
    Some(Some(rgb))
}

/// `<style>`、`<region>` 的定義：自己的屬性，加上參照的樣式（`style="s1 s2"`）
#[derive(Debug, Clone, Default)]
struct StyleDef {
    own: Style,
    refs: Vec<String>,
}

// ───────────── 解析 ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum El {
    Tt,
    Head,
    Styling,
    Style,
    Layout,
    Region,
    Body,
    Div,
    P,
    Span,
    Br,
    /// 其他（metadata、set、別的命名空間的元素）：裡面的文字不顯示
    Other,
}

/// 開著的元素
#[derive(Debug, Clone)]
struct Frame {
    el: El,
    /// 絕對時間（秒）
    begin: f64,
    end: Option<f64>,
    /// 繼承下來的文字樣式
    style: Style,
    region: Option<String>,
    preserve: bool,
    /// `timeContainer="seq"`：子元素一個接一個
    seq: bool,
    /// seq 容器：下一個子元素的開始時間
    cursor: f64,
    /// 時間在父元素的範圍內（範圍外的整個不顯示）
    active: bool,
    /// 裡面的文字是字幕（在 `p`、`span` 裡）
    text_ok: bool,
    /// 這個 `p` 開了一句字幕
    opens_cue: bool,
    /// 在注音的讀音裡（`tts:ruby="text"` 的 span 裡面）
    ruby_text: bool,
    /// 這個 span 是讀音：前後加括號
    ruby_paren: bool,
    /// `<style>`、`<region>` 的 `xml:id`
    id: Option<String>,
}

/// 一段文字或換行，帶著自己的時間範圍（`span` 可以有自己的時間）
#[derive(Debug, Clone)]
enum Run {
    Text {
        text: String,
        style: Style,
        preserve: bool,
        begin: f64,
        end: Option<f64>,
    },
    Break {
        begin: f64,
        end: Option<f64>,
    },
}

impl Run {
    fn span(&self) -> (f64, Option<f64>) {
        match self {
            Run::Text { begin, end, .. } | Run::Break { begin, end } => (*begin, *end),
        }
    }
}

/// 一個 `p`
#[derive(Debug, Clone)]
struct Cue {
    begin: f64,
    end: Option<f64>,
    top: bool,
    runs: Vec<Run>,
}

/// 元素的屬性（只留用得到的）
#[derive(Debug, Default)]
struct Attrs {
    begin: Option<String>,
    end: Option<String>,
    dur: Option<String>,
    style_refs: Vec<String>,
    region: Option<String>,
    seq: bool,
    id: Option<String>,
    space: Option<String>,
    lang: Option<String>,
    inline: Style,
    params: HashMap<Vec<u8>, String>,
}

#[derive(Default)]
struct Doc {
    root_seen: bool,
    params: Params,
    lang: Option<String>,
    /// 根元素的 `tts:extent` 是像素時（換算 `px` 的位置）
    root_px: Option<(f64, f64)>,
    /// `ttp:cellResolution` 的列數（換算 `c` 的位置）
    cell_rows: f64,
    styles: HashMap<String, StyleDef>,
    regions: HashMap<String, StyleDef>,
    /// 已經算好的樣式（每個 id 只算一次：一個樣式參照很多個、層層參照時不會越算越多）
    resolved: HashMap<String, Style>,
    stack: Vec<Frame>,
    cue: Option<Cue>,
    cues: Vec<Cue>,
    images: bool,
    /// 讀音的左括號還沒加（見 [`Doc::text`]）
    paren_owed: bool,
}

/// 屬性屬於哪一類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrKind {
    /// 沒有前綴（`begin`、`style`、`region`…）
    Plain,
    Xml,
    Param,
    Styling,
    Other,
}

fn attr_kind(reader: &NsReader<&[u8]>, key: QName<'_>) -> AttrKind {
    let (ns, _) = reader.resolver().resolve_attribute(key);
    match ns {
        ResolveResult::Unbound => AttrKind::Plain,
        ResolveResult::Bound(n) => {
            let n = n.as_ref();
            if n == XML_NAMESPACE {
                AttrKind::Xml
            } else if n.ends_with(b"#parameter") {
                AttrKind::Param
            } else if n.ends_with(b"#styling") || n.ends_with(b"#style") {
                AttrKind::Styling
            } else {
                AttrKind::Other
            }
        }
        // 前綴沒有宣告（有的檔案直接寫 tts:color 沒有 xmlns:tts）：照慣用的前綴猜
        ResolveResult::Unknown(prefix) => match prefix.as_slice() {
            b"ttp" => AttrKind::Param,
            b"tts" => AttrKind::Styling,
            _ => AttrKind::Other,
        },
    }
}

impl Doc {
    fn read_attrs(&mut self, reader: &NsReader<&[u8]>, e: &BytesStart<'_>) -> Result<Attrs, TtmlError> {
        let mut a = Attrs::default();
        for attr in e.attributes() {
            let attr = attr.map_err(|err| xml_error(reader, err))?;
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|err| xml_error(reader, err))?
                .into_owned();
            let local = attr.key.local_name();
            let local = local.as_ref();
            if local == b"backgroundImage" {
                self.images = true;
            }
            match (attr_kind(reader, attr.key), local) {
                (AttrKind::Plain, b"begin") => a.begin = Some(value),
                (AttrKind::Plain, b"end") => a.end = Some(value),
                (AttrKind::Plain, b"dur") => a.dur = Some(value),
                (AttrKind::Plain, b"style") => a.style_refs = value.split_whitespace().map(str::to_owned).collect(),
                (AttrKind::Plain, b"region") => a.region = Some(value.trim().to_owned()),
                (AttrKind::Plain, b"timeContainer") => a.seq = value.trim() == "seq",
                // 有的舊檔案不加 xml: 前綴
                (AttrKind::Xml | AttrKind::Plain, b"id") => a.id = Some(value.trim().to_owned()),
                (AttrKind::Xml, b"space") => a.space = Some(value),
                (AttrKind::Xml, b"lang") => a.lang = Some(value),
                (AttrKind::Param, name) => {
                    a.params.insert(name.to_vec(), value);
                }
                (AttrKind::Styling, name) => a.inline.set_attr(name, &value),
                _ => {}
            }
        }
        Ok(a)
    }

    fn start(&mut self, reader: &NsReader<&[u8]>, e: &BytesStart<'_>) -> Result<(), TtmlError> {
        if self.stack.len() >= MAX_DEPTH {
            return Err(TtmlError::TooDeep);
        }
        let (ns, local) = reader.resolver().resolve_element(e.name());
        // 沒有命名空間的也當成 TTML（有的檔案漏寫 xmlns）
        let tt_ns = match ns {
            ResolveResult::Bound(n) => is_tt_namespace(n.as_ref()),
            ResolveResult::Unbound => true,
            ResolveResult::Unknown(_) => false,
        };
        let local = local.as_ref();
        if local == b"image" {
            self.images = true;
        }
        let a = self.read_attrs(reader, e)?;
        let Some(parent) = self.stack.last().cloned() else {
            if self.root_seen {
                return Err(TtmlError::Xml {
                    position: reader.buffer_position(),
                    message: crate::tr!("有兩個根元素", "more than one root element").to_owned(),
                });
            }
            if !(tt_ns && local == b"tt") {
                return Err(TtmlError::NotTtml);
            }
            self.root_seen = true;
            return self.start_root(a);
        };
        let el = if tt_ns {
            match local {
                b"head" => El::Head,
                b"styling" => El::Styling,
                b"style" => El::Style,
                b"layout" => El::Layout,
                b"region" => El::Region,
                b"body" => El::Body,
                b"div" => El::Div,
                b"p" => El::P,
                b"span" => El::Span,
                b"br" => El::Br,
                _ => El::Other,
            }
        } else {
            El::Other
        };
        let preserve = match a.space.as_deref().map(str::trim) {
            Some("preserve") => true,
            Some("default") => false,
            _ => parent.preserve,
        };
        let mut frame = Frame {
            el,
            begin: parent.begin,
            end: parent.end,
            style: parent.style.clone(),
            region: parent.region.clone(),
            preserve,
            seq: a.seq,
            cursor: 0.0,
            active: parent.active,
            text_ok: false,
            opens_cue: false,
            ruby_text: parent.ruby_text,
            ruby_paren: false,
            id: a.id.clone(),
        };
        match el {
            El::Style => {
                let def = StyleDef {
                    own: a.inline,
                    refs: a.style_refs,
                };
                match (&a.id, parent.el) {
                    (Some(id), _) => {
                        self.styles.insert(id.clone(), def);
                    }
                    // 區域裡沒有 id 的 <style>：區域自己的樣式
                    (None, El::Region) => {
                        if let Some(region) = parent.id.as_ref().and_then(|id| self.regions.get_mut(id)) {
                            region.own.apply(&def.own);
                            region.refs.extend(def.refs);
                        }
                    }
                    _ => {}
                }
            }
            El::Region => {
                if let Some(id) = &a.id {
                    self.regions.insert(
                        id.clone(),
                        StyleDef {
                            own: a.inline,
                            refs: a.style_refs,
                        },
                    );
                }
            }
            El::Body | El::Div | El::P | El::Span => {
                self.timing(&mut frame, &parent, &a);
                // 樣式：區域 < 父元素繼承的 < 參照的樣式 < 自己的屬性
                let mut style = Style::default();
                if let Some(region) = &a.region {
                    style.apply(&self.region_style(region).inheritable());
                    frame.region = Some(region.clone());
                }
                style.apply(&parent.style);
                let mut ruby = None;
                for id in &a.style_refs {
                    let referenced = self.resolve_style(id, 0);
                    ruby = referenced.ruby.or(ruby);
                    style.apply(&referenced.inheritable());
                }
                let ruby = a.inline.ruby.or(ruby);
                style.apply(&a.inline.inheritable());
                frame.style = style;
                if el == El::P && self.cue.is_none() {
                    frame.text_ok = true;
                    if frame.active {
                        frame.opens_cue = true;
                        let top = self.region_is_top(frame.region.as_deref());
                        self.cue = Some(Cue {
                            begin: frame.begin,
                            end: frame.end,
                            top,
                            runs: Vec::new(),
                        });
                    }
                } else {
                    // span（或 p 裡面又一個 p）：在字幕裡才算
                    frame.text_ok = parent.text_ok;
                }
                if el == El::Span {
                    match ruby {
                        Some(Ruby::Text) if !parent.ruby_text => {
                            frame.ruby_text = true;
                            frame.ruby_paren = true;
                        }
                        Some(Ruby::Delimiter) => frame.text_ok = false,
                        _ => {}
                    }
                }
            }
            El::Br => {
                if parent.text_ok
                    && parent.active
                    && let Some(cue) = &mut self.cue
                {
                    cue.runs.push(Run::Break {
                        begin: parent.begin,
                        end: parent.end,
                    });
                }
            }
            _ => {}
        }
        // 讀音的左括號等到真的有文字才加（空的讀音不留下一對括號）
        self.paren_owed |= frame.ruby_paren;
        self.stack.push(frame);
        Ok(())
    }

    fn start_root(&mut self, a: Attrs) -> Result<(), TtmlError> {
        let param = |name: &[u8]| a.params.get(name).map(String::as_str);
        if let Some(base) = param(b"timeBase").map(str::trim)
            && base != "media"
        {
            return Err(TtmlError::Unsupported(Unsupported::TimeBase(base.to_owned())));
        }
        if param(b"profile").is_some_and(|p| p.contains("image")) {
            self.images = true;
        }
        let rate = param(b"frameRate").and_then(positive_int);
        let multiplier = param(b"frameRateMultiplier")
            .and_then(|m| {
                let mut it = m.split_whitespace().map(positive_int);
                Some(it.next()?? / it.next()??)
            })
            .unwrap_or(1.0);
        let sub_frame_rate = param(b"subFrameRate").and_then(positive_int).unwrap_or(1.0);
        let frame_rate = rate.unwrap_or(30.0) * multiplier;
        // 規格：沒寫 tickRate 時，有寫格率就是「格率 × 子格率」，否則每秒 1 個
        let tick_rate = param(b"tickRate").and_then(positive_int).unwrap_or(if rate.is_some() {
            frame_rate * sub_frame_rate
        } else {
            1.0
        });
        self.params = Params {
            frame_rate,
            sub_frame_rate,
            tick_rate,
        };
        self.cell_rows = param(b"cellResolution")
            .and_then(|c| c.split_whitespace().nth(1).and_then(positive_int))
            .unwrap_or(15.0);
        if let Some((Length::Px(w), Length::Px(h))) = a.inline.extent
            && w > 0.0
            && h > 0.0
        {
            self.root_px = Some((w, h));
        }
        self.lang = a.lang.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
        self.stack.push(Frame {
            el: El::Tt,
            begin: 0.0,
            end: None,
            style: Style::default(),
            region: None,
            preserve: a.space.as_deref().map(str::trim) == Some("preserve"),
            seq: false,
            cursor: 0.0,
            active: true,
            text_ok: false,
            opens_cue: false,
            ruby_text: false,
            ruby_paren: false,
            id: None,
        });
        Ok(())
    }

    /// 元素的絕對時間：`begin`、`end` 從父元素的開始算（seq 容器從前一個結束算），`dur` 從自己的開始算；
    /// 不超過父元素的結束
    fn timing(&self, frame: &mut Frame, parent: &Frame, a: &Attrs) {
        let p = &self.params;
        let base = if parent.seq { parent.cursor } else { parent.begin };
        let begin = base + a.begin.as_deref().and_then(|s| parse_time(s, p)).unwrap_or(0.0);
        let mut end = a.end.as_deref().and_then(|s| parse_time(s, p)).map(|e| base + e);
        if let Some(dur) = a.dur.as_deref().and_then(|s| parse_time(s, p)) {
            end = Some(end.map_or(begin + dur, |e| e.min(begin + dur)));
        }
        if let Some(parent_end) = parent.end {
            end = Some(end.map_or(parent_end, |e| e.min(parent_end)));
        }
        frame.begin = begin;
        frame.end = end;
        frame.cursor = begin;
        frame.active = parent.active && end.is_none_or(|e| e > begin);
    }

    fn end(&mut self) -> Result<(), TtmlError> {
        if self.stack.last().is_some_and(|f| f.ruby_paren) {
            if self.paren_owed {
                self.paren_owed = false;
            } else {
                self.text(")");
            }
        }
        let Some(frame) = self.stack.pop() else {
            return Ok(());
        };
        if frame.opens_cue
            && let Some(cue) = self.cue.take()
            && !cue.runs.is_empty()
        {
            if self.cues.len() >= MAX_CUES {
                return Err(TtmlError::TooManyCues);
            }
            self.cues.push(cue);
        }
        if let Some(parent) = self.stack.last_mut()
            && parent.seq
            && matches!(frame.el, El::Body | El::Div | El::P | El::Span)
        {
            parent.cursor = frame.end.unwrap_or(frame.begin);
        }
        Ok(())
    }

    fn text(&mut self, s: &str) {
        let Some(frame) = self.stack.last() else { return };
        if !frame.text_ok || !frame.active {
            return;
        }
        let Some(cue) = &mut self.cue else { return };
        let owed;
        let s = if std::mem::take(&mut self.paren_owed) {
            owed = format!("({s}");
            owed.as_str()
        } else {
            s
        };
        let style = frame.style.inheritable();
        // 跟上一段的樣式、時間都一樣：接在一起（實體、字元參照會把一段文字切成好幾個事件）
        if let Some(Run::Text {
            text,
            style: last_style,
            preserve,
            begin,
            end,
        }) = cue.runs.last_mut()
            && *last_style == style
            && *preserve == frame.preserve
            && *begin == frame.begin
            && *end == frame.end
        {
            text.push_str(s);
            return;
        }
        cue.runs.push(Run::Text {
            text: s.to_owned(),
            style,
            preserve: frame.preserve,
            begin: frame.begin,
            end: frame.end,
        });
    }

    /// 樣式 `id` 算好的屬性（參照的樣式先套，再套自己的）
    fn resolve_style(&mut self, id: &str, depth: usize) -> Style {
        if let Some(style) = self.resolved.get(id) {
            return style.clone();
        }
        let Some(def) = self.styles.get(id).cloned() else {
            return Style::default();
        };
        let mut style = Style::default();
        if depth < MAX_STYLE_CHAIN {
            for r in &def.refs {
                style.apply(&self.resolve_style(r, depth + 1));
            }
        }
        style.apply(&def.own);
        self.resolved.insert(id.to_owned(), style.clone());
        style
    }

    fn region_style(&mut self, id: &str) -> Style {
        let Some(def) = self.regions.get(id).cloned() else {
            return Style::default();
        };
        let mut style = Style::default();
        for r in &def.refs {
            style.apply(&self.resolve_style(r, 0));
        }
        style.apply(&def.own);
        style
    }

    /// 位置的百分比（垂直方向）
    fn percent_y(&self, len: Length) -> Option<f64> {
        match len {
            Length::Percent(v) => Some(v),
            Length::Px(v) => self.root_px.map(|(_, h)| v / h * 100.0),
            Length::Cell(v) => Some(v / self.cell_rows * 100.0),
        }
    }

    /// 字幕在畫面的上半部：文字的位置（區域的上緣、中間或下緣，看 `displayAlign`）在 50% 以上。
    /// 沒寫位置、明寫 `displayAlign="before"` 的區域也算上面；沒有區域的在下面（一般字幕的位置）
    fn region_is_top(&mut self, region: Option<&str>) -> bool {
        let Some(id) = region else { return false };
        if !self.regions.contains_key(id) {
            return false;
        }
        let style = self.region_style(id);
        let align = style.display_align.unwrap_or(Align::Before);
        let Some((_, y)) = style.origin else {
            return style.display_align == Some(Align::Before);
        };
        let Some(y) = self.percent_y(y) else { return false };
        let height = style.extent.and_then(|(_, h)| self.percent_y(h)).unwrap_or(0.0);
        let anchor = match align {
            Align::Before => y,
            Align::Center => y + height / 2.0,
            Align::After => y + height,
        };
        anchor < 50.0
    }

    fn finish(self) -> Result<Converted, TtmlError> {
        let mut cues = self.cues;
        // 沒有結束時間的：顯示到下一句開始，最多 OPEN_END 秒
        let mut begins: Vec<f64> = cues.iter().map(|c| c.begin).collect();
        begins.sort_by(f64::total_cmp);
        for cue in &mut cues {
            if cue.end.is_none() {
                let next = begins.partition_point(|b| *b <= cue.begin);
                let limit = cue.begin + OPEN_END;
                cue.end = Some(begins.get(next).map_or(limit, |n| n.min(limit)));
            }
        }
        let mut out: Vec<(f64, f64, String)> = Vec::new();
        for cue in &cues {
            segments(cue, &mut out);
        }
        // 依開始時間排（一樣的照檔案裡的順序）；空的已經拿掉
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        if out.len() > MAX_CUES {
            return Err(TtmlError::TooManyCues);
        }
        let mut srt = String::new();
        let mut n = 0;
        for (begin, end, text) in &out {
            let (b, e) = (millis(*begin), millis(*end));
            if e <= b {
                continue;
            }
            n += 1;
            srt.push_str(&format!("{n}\n{} --> {}\n{text}\n\n", srt_time(b), srt_time(e)));
        }
        if n == 0 {
            return Err(if self.images {
                TtmlError::Unsupported(Unsupported::Images)
            } else {
                TtmlError::Empty
            });
        }
        Ok(Converted {
            srt,
            lang: self.lang,
            cues: n,
        })
    }
}

fn millis(t: f64) -> u64 {
    (t.max(0.0) * 1000.0).round() as u64
}

/// `HH:MM:SS,mmm`
fn srt_time(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// 一個 `p` 切成幾段：`span` 有自己的時間時，每個時間點之間顯示的文字不一樣
fn segments(cue: &Cue, out: &mut Vec<(f64, f64, String)>) {
    const EPS: f64 = 1e-9;
    let (pb, pe) = (cue.begin, cue.end.unwrap_or(cue.begin));
    let clamp = |t: f64| t.clamp(pb, pe);
    let mut points = vec![pb, pe];
    for run in &cue.runs {
        let (b, e) = run.span();
        points.push(clamp(b));
        points.push(clamp(e.unwrap_or(pe)));
    }
    points.sort_by(f64::total_cmp);
    points.dedup_by(|a, b| (*a - *b).abs() < EPS);
    let first = out.len();
    for w in points.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let active: Vec<&Run> = cue
            .runs
            .iter()
            .filter(|r| {
                let (b, e) = r.span();
                b <= t0 + EPS && e.unwrap_or(pe).min(pe) >= t1 - EPS
            })
            .collect();
        let text = render(&active, cue.top);
        if text.is_empty() {
            continue;
        }
        // 跟前一段（同一個 p）一樣、接在一起：合成一句
        if out.len() > first
            && let Some(last) = out.last_mut()
            && last.2 == text
            && (last.1 - t0).abs() < EPS
        {
            last.1 = t1;
            continue;
        }
        out.push((t0, t1, text));
    }
}

/// 一段文字的 SRT 標記
fn wrap(text: &str, style: &Style) -> String {
    let mut s = text.to_owned();
    if style.underline == Some(true) {
        s = format!("<u>{s}</u>");
    }
    if style.italic == Some(true) {
        s = format!("<i>{s}</i>");
    }
    if style.bold == Some(true) {
        s = format!("<b>{s}</b>");
    }
    // 白色是字幕本來的顏色：不寫（寫了會蓋過使用者在 mpv 設定的字幕顏色）
    if let Some(Some(rgb)) = style.color
        && rgb != [0xff, 0xff, 0xff]
    {
        s = format!("<font color=\"#{:02X}{:02X}{:02X}\">{s}</font>", rgb[0], rgb[1], rgb[2]);
    }
    s
}

/// 沒有 `xml:space="preserve"` 的空白：連續的空白、換行都變成一個空格
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// 一段時間裡顯示的文字（SRT 的格式）。空行拿掉：SRT 的空行是一句的結束
fn render(runs: &[&Run], top: bool) -> String {
    // 一行一組：（文字, 樣式, 保留空白）
    let mut lines: Vec<Vec<(String, &Style, bool)>> = vec![Vec::new()];
    for run in runs {
        match run {
            Run::Break { .. } => lines.push(Vec::new()),
            Run::Text {
                text, style, preserve, ..
            } => {
                if *preserve {
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    for (i, part) in text.split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Vec::new());
                        }
                        if let Some(line) = lines.last_mut() {
                            line.push((part.to_owned(), style, true));
                        }
                    }
                } else if let Some(line) = lines.last_mut() {
                    line.push((collapse(text), style, false));
                }
            }
        }
    }
    let mut out_lines = Vec::new();
    for line in lines {
        // 行首、行尾的空白，和兩段之間重複的空白拿掉（保留空白的那幾段不動）
        let mut parts: Vec<(String, &Style, bool)> = Vec::new();
        let mut after_space = true;
        for (text, style, preserve) in line {
            let text = if !preserve && after_space {
                text.trim_start_matches(' ').to_owned()
            } else {
                text
            };
            if text.is_empty() {
                continue;
            }
            after_space = text.ends_with(' ');
            parts.push((text, style, preserve));
        }
        while let Some(last) = parts.last_mut() {
            if !last.2 {
                let trimmed = last.0.trim_end_matches(' ').len();
                last.0.truncate(trimmed);
            }
            if last.0.is_empty() {
                parts.pop();
            } else {
                break;
            }
        }
        let rendered: String = parts.iter().map(|(t, s, _)| wrap(t, s)).collect();
        if !rendered.trim().is_empty() {
            out_lines.push(rendered);
        }
    }
    let text = out_lines.join("\n");
    if top && !text.is_empty() {
        format!("{{\\an8}}{text}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = r#"xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:ttp="http://www.w3.org/ns/ttml#parameter""#;

    /// 一份 TTML：`root` 是根元素的其他屬性，`head`、`body` 是內容
    fn doc(root: &str, head: &str, body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tt {NS} xml:lang=\"en\" {root}>\n<head>{head}</head>\n<body>{body}</body>\n</tt>\n"
        )
    }

    fn srt(root: &str, head: &str, body: &str) -> String {
        to_srt(&doc(root, head, body)).unwrap()
    }

    /// SRT 拆成（開始, 結束, 文字）
    fn cues(srt: &str) -> Vec<(String, String, String)> {
        srt.split("\n\n")
            .filter(|b| !b.trim().is_empty())
            .map(|block| {
                let mut lines = block.lines();
                lines.next().unwrap();
                let (a, b) = lines.next().unwrap().split_once(" --> ").unwrap();
                (a.to_owned(), b.to_owned(), lines.collect::<Vec<_>>().join("\n"))
            })
            .collect()
    }

    fn one(body: &str) -> (String, String, String) {
        let all = cues(&srt("", "", body));
        assert_eq!(all.len(), 1, "{all:?}");
        all.into_iter().next().unwrap()
    }

    fn c(a: &str, b: &str, t: &str) -> (String, String, String) {
        (a.to_owned(), b.to_owned(), t.to_owned())
    }

    #[test]
    fn clock_times_and_srt_layout() {
        let out = srt(
            "",
            "",
            r#"<div><p begin="00:00:01.5" end="00:00:02.250">Hello</p><p begin="01:02:03.004" end="01:02:04">World</p></div>"#,
        );
        assert_eq!(
            out,
            "1\n00:00:01,500 --> 00:00:02,250\nHello\n\n2\n01:02:03,004 --> 01:02:04,000\nWorld\n\n"
        );
    }

    #[test]
    fn frame_times_with_a_ntsc_multiplier() {
        // 24 × 1000/1001 = 23.976 格：第 23 格 = 0.959375 秒
        let root = r#"ttp:frameRate="24" ttp:frameRateMultiplier="1000 1001""#;
        let all = cues(&srt(root, "", r#"<p begin="00:00:02:23" end="00:00:04:00">x</p>"#));
        assert_eq!(all, [c("00:00:02,959", "00:00:04,000", "x")]);
        // 子格：12.1 格（subFrameRate 2 → 12.5 格）
        let root = r#"ttp:frameRate="25" ttp:subFrameRate="2""#;
        let all = cues(&srt(root, "", r#"<p begin="00:00:00:12.1" end="00:00:01:00">x</p>"#));
        assert_eq!(all, [c("00:00:00,500", "00:00:01,000", "x")]);
        // 沒寫格率：30
        let all = cues(&srt("", "", r#"<p begin="00:00:00:15" end="1s">x</p>"#));
        assert_eq!(all, [c("00:00:00,500", "00:00:01,000", "x")]);
    }

    #[test]
    fn tick_and_offset_times() {
        let root = r#"ttp:tickRate="10000000""#;
        let all = cues(&srt(root, "", r#"<p begin="15000000t" end="25000000t">x</p>"#));
        assert_eq!(all, [c("00:00:01,500", "00:00:02,500", "x")]);
        // 沒寫 tickRate、有寫格率：格率 × 子格率
        let root = r#"ttp:frameRate="25""#;
        let all = cues(&srt(root, "", r#"<p begin="25t" end="50t">x</p>"#));
        assert_eq!(all, [c("00:00:01,000", "00:00:02,000", "x")]);
        // 都沒寫：每秒 1 個
        let all = cues(&srt("", "", r#"<p begin="2t" end="3t">x</p>"#));
        assert_eq!(all, [c("00:00:02,000", "00:00:03,000", "x")]);
        // 其他單位
        let all = cues(&srt(
            r#"ttp:frameRate="50""#,
            "",
            r#"<p begin="500ms" end="1.5s">a</p><p begin="100f" end="0.05m">b</p><p begin="0.001h" end="3.7s">c</p>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:00,500", "00:00:01,500", "a"),
                c("00:00:02,000", "00:00:03,000", "b"),
                c("00:00:03,600", "00:00:03,700", "c"),
            ]
        );
    }

    #[test]
    fn times_are_relative_to_the_parent() {
        // div 從 10 秒開始：p 的時間從 div 算；dur 從自己的開始算；不超過 div 的結束
        let all = cues(&srt(
            "",
            "",
            r#"<div begin="10s" end="20s"><p begin="1s" end="2s">a</p><p begin="3s" dur="1.5s">b</p>
               <div begin="5s"><p begin="1s" end="30s">c</p></div></div>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:11,000", "00:00:12,000", "a"),
                c("00:00:13,000", "00:00:14,500", "b"),
                c("00:00:16,000", "00:00:20,000", "c"),
            ]
        );
        // body 也可以有時間；整個在父元素結束之後的不顯示
        let all = cues(&srt(
            "",
            "",
            r#"<div begin="1s" end="3s"><p begin="1s" end="2s">a</p><p begin="5s" end="6s">late</p></div>"#,
        ));
        assert_eq!(all, [c("00:00:02,000", "00:00:03,000", "a")]);
    }

    #[test]
    fn seq_container_runs_children_one_after_another() {
        let all = cues(&srt(
            "",
            "",
            r#"<div timeContainer="seq"><p dur="1s">a</p><p begin="0.5s" dur="1s">b</p></div>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:00,000", "00:00:01,000", "a"),
                c("00:00:01,500", "00:00:02,500", "b"),
            ]
        );
    }

    #[test]
    fn missing_end_runs_to_the_next_cue_at_most_five_seconds() {
        let all = cues(&srt(
            "",
            "",
            r#"<p begin="1s">a</p><p begin="3s">b</p><p begin="20s">c</p>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:01,000", "00:00:03,000", "a"),
                c("00:00:03,000", "00:00:08,000", "b"),
                c("00:00:20,000", "00:00:25,000", "c"),
            ]
        );
    }

    #[test]
    fn line_breaks_and_whitespace() {
        // 縮排、換行都變成一個空格，行首行尾的拿掉；<br/> 換行
        let (_, _, t) = one("<p begin=\"1s\" end=\"2s\">\n    First   line<br/>\n    second\tline\n  </p>");
        assert_eq!(t, "First line\nsecond line");
        // xml:space="preserve"：空白照留，換行也是換行
        let (_, _, t) = one("<p begin=\"1s\" end=\"2s\" xml:space=\"preserve\">  two  spaces\nnext</p>");
        assert_eq!(t, "  two  spaces\nnext");
        // 兩個 <br/> 不會變成空行（SRT 的空行是一句的結束）
        let (_, _, t) = one(r#"<p begin="1s" end="2s">a<br/><br/>b</p>"#);
        assert_eq!(t, "a\nb");
        // span 之間的空白只留一個
        let (_, _, t) = one(r#"<p begin="1s" end="2s"><span>Hello </span> <span> world</span></p>"#);
        assert_eq!(t, "Hello world");
    }

    #[test]
    fn styles_by_id_chains_and_inline() {
        let head = r##"<styling>
            <style xml:id="base" tts:color="yellow"/>
            <style xml:id="it" style="base" tts:fontStyle="italic"/>
            <style xml:id="loop1" style="loop2" tts:fontWeight="bold"/>
            <style xml:id="loop2" style="loop1"/>
        </styling>"##;
        let all = cues(&srt(
            "",
            head,
            r##"<p begin="1s" end="2s" style="it">a</p>
                <p begin="2s" end="3s"><span tts:fontWeight="bold">b</span> <span tts:textDecoration="underline" tts:color="#00ff00">c</span></p>
                <p begin="3s" end="4s" style="loop1">d</p>
                <p begin="4s" end="5s" style="it"><span tts:fontStyle="normal">e</span></p>"##,
        ));
        assert_eq!(
            all,
            [
                c(
                    "00:00:01,000",
                    "00:00:02,000",
                    "<font color=\"#FFFF00\"><i>a</i></font>"
                ),
                c(
                    "00:00:02,000",
                    "00:00:03,000",
                    "<b>b</b> <font color=\"#00FF00\"><u>c</u></font>"
                ),
                c("00:00:03,000", "00:00:04,000", "<b>d</b>"),
                c("00:00:04,000", "00:00:05,000", "<font color=\"#FFFF00\">e</font>"),
            ]
        );
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff8000"), Some(Some([0xff, 0x80, 0])));
        assert_eq!(parse_color("#FF800080"), Some(Some([0xff, 0x80, 0])));
        assert_eq!(parse_color("#ff800000"), Some(None), "完全透明");
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Some([1, 2, 3])));
        assert_eq!(parse_color("rgba(1,2,3,255)"), Some(Some([1, 2, 3])));
        assert_eq!(parse_color("rgba(1,2,3,0)"), Some(None));
        assert_eq!(parse_color("cyan"), Some(Some([0, 0xff, 0xff])));
        assert_eq!(parse_color("transparent"), Some(None));
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(parse_color("rgb(1,2)"), None);
        assert_eq!(parse_color("bogus"), None);
        // 白色是預設的顏色：不寫進 SRT
        let (_, _, t) = one(r#"<p begin="1s" end="2s" tts:color="white">a</p>"#);
        assert_eq!(t, "a");
    }

    #[test]
    fn top_regions_get_an8() {
        let head = r#"<layout>
            <region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%"/>
            <region xml:id="bottom" tts:origin="10% 80%" tts:extent="80% 15%"/>
            <region xml:id="before" tts:displayAlign="before"/>
            <region xml:id="full-after" tts:origin="0% 0%" tts:extent="100% 100%" tts:displayAlign="after"/>
            <region xml:id="px" tts:origin="0px 100px" tts:extent="1920px 200px"/>
            <region xml:id="styled" style="s-top"/>
        </layout>
        <styling><style xml:id="s-top" tts:origin="0% 10%"/></styling>"#;
        let all = cues(&srt(
            r#"tts:extent="1920px 1080px""#,
            head,
            r#"<p begin="1s" end="2s" region="top">a</p>
               <p begin="2s" end="3s" region="bottom">b</p>
               <p begin="3s" end="4s" region="before">c</p>
               <p begin="4s" end="5s" region="full-after">d</p>
               <p begin="5s" end="6s" region="px">e</p>
               <p begin="6s" end="7s">f</p>
               <div region="top"><p begin="7s" end="8s">g</p></div>
               <p begin="8s" end="9s" region="styled">h</p>"#,
        ));
        let texts: Vec<&str> = all.iter().map(|c| c.2.as_str()).collect();
        assert_eq!(
            texts,
            [
                "{\\an8}a", "b", "{\\an8}c", "d", "{\\an8}e", "f", "{\\an8}g", "{\\an8}h"
            ]
        );
    }

    #[test]
    fn region_styles_are_inherited() {
        let head = r#"<layout><region xml:id="r" tts:fontStyle="italic"/></layout>"#;
        let (_, _, t) = cues(&srt("", head, r#"<p begin="1s" end="2s" region="r">a</p>"#))
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(t, "<i>a</i>");
    }

    #[test]
    fn timed_spans_split_the_cue() {
        // 卡拉 OK 式的逐字出現：每個時間點之間顯示的文字不一樣
        let all = cues(&srt(
            "",
            "",
            r#"<p begin="1s" end="4s"><span>one</span> <span begin="1s">two</span> <span begin="2s" end="2.5s">three</span></p>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:01,000", "00:00:02,000", "one"),
                c("00:00:02,000", "00:00:03,000", "one two"),
                c("00:00:03,000", "00:00:03,500", "one two three"),
                c("00:00:03,500", "00:00:04,000", "one two"),
            ]
        );
    }

    #[test]
    fn legacy_dfxp_namespaces_entities_and_char_refs() {
        let dfxp = r#"<?xml version="1.0" encoding="utf-8"?>
<tt xmlns="http://www.w3.org/2006/10/ttaf1" xmlns:tts="http://www.w3.org/2006/10/ttaf1#styling" xmlns:ttp="http://www.w3.org/2006/10/ttaf1#parameter" ttp:tickRate="10000000" xml:lang="zh-TW">
  <head><styling><style id="s1" tts:fontStyle="italic"/></styling></head>
  <body><div>
    <p begin="10000000t" end="20000000t">Tom &amp; Jerry &lt;3 &#x4F60;&#22909; &quot;hi&quot;</p>
    <p begin="20000000t" end="30000000t" style="s1">&undefined; x</p>
  </div></body>
</tt>"#;
        assert!(looks_like_ttml(dfxp));
        let conv = convert(dfxp).unwrap();
        assert_eq!(conv.lang.as_deref(), Some("zh-TW"));
        assert_eq!(conv.cues, 2);
        assert_eq!(
            cues(&conv.srt),
            [
                c("00:00:01,000", "00:00:02,000", "Tom & Jerry <3 你好 \"hi\""),
                c("00:00:02,000", "00:00:03,000", "<i>&undefined; x</i>"),
            ]
        );
        // 2006/04 的命名空間、前綴的元素名稱
        let old = r#"<tt:tt xmlns:tt="http://www.w3.org/2006/04/ttaf1"><tt:body><tt:p begin="1s" end="2s">old</tt:p></tt:body></tt:tt>"#;
        assert!(looks_like_ttml(old));
        assert_eq!(cues(&to_srt(old).unwrap()), [c("00:00:01,000", "00:00:02,000", "old")]);
    }

    #[test]
    fn errors() {
        // XML 壞掉
        let broken = doc("", "", r#"<p begin="1s" end="2s">a</span>"#);
        assert!(
            matches!(to_srt(&broken), Err(TtmlError::Xml { .. })),
            "{:?}",
            to_srt(&broken)
        );
        let full = doc("", "", r#"<p begin="1s" end="2s">a</p>"#);
        let truncated = &full[..full.len() - 20];
        assert!(
            matches!(to_srt(truncated), Err(TtmlError::Xml { .. })),
            "{:?}",
            to_srt(truncated)
        );
        let two_roots = format!(
            "{}<tt xmlns=\"http://www.w3.org/ns/ttml\"/>",
            doc("", "", "<p end=\"1s\">a</p>")
        );
        assert!(
            matches!(to_srt(&two_roots), Err(TtmlError::Xml { .. })),
            "{:?}",
            to_srt(&two_roots)
        );
        // 不是 TTML
        assert_eq!(to_srt("<html><body><p>a</p></body></html>"), Err(TtmlError::NotTtml));
        assert_eq!(
            to_srt("1\n00:00:01,000 --> 00:00:02,000\nhi\n"),
            Err(TtmlError::NotTtml)
        );
        assert_eq!(to_srt(""), Err(TtmlError::NotTtml));
        assert_eq!(
            to_srt(r#"<tt xmlns="urn:other"><body><p begin="1s" end="2s">a</p></body></tt>"#),
            Err(TtmlError::NotTtml)
        );
        // 沒有字幕
        assert_eq!(to_srt(&doc("", "", "")), Err(TtmlError::Empty));
        assert_eq!(
            to_srt(&doc("", "", "<div><p begin=\"1s\" end=\"2s\">  </p></div>")),
            Err(TtmlError::Empty)
        );
        // 圖片字幕（IMSC image）
        let image = doc(
            r#"xmlns:smpte="http://www.smpte-ra.org/schemas/2052-1/2010/smpte" ttp:profile="http://www.w3.org/ns/ttml/profile/imsc1/image""#,
            r#"<metadata><smpte:image imagetype="PNG" encoding="Base64" xml:id="img0">iVBORw0KGgo=</smpte:image></metadata>"#,
            r##"<div begin="1s" end="2s" smpte:backgroundImage="#img0"/>"##,
        );
        assert_eq!(to_srt(&image), Err(TtmlError::Unsupported(Unsupported::Images)));
        // SMPTE、clock 的時間基準
        for base in ["smpte", "clock"] {
            let d = doc(
                &format!("ttp:timeBase=\"{base}\""),
                "",
                r#"<p begin="1s" end="2s">a</p>"#,
            );
            assert_eq!(
                to_srt(&d),
                Err(TtmlError::Unsupported(Unsupported::TimeBase(base.to_owned())))
            );
        }
        // 說明文字有中英文
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(TtmlError::Empty.to_string(), "no subtitle cues");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(TtmlError::Empty.to_string(), "沒有任何一句字幕");
    }

    #[test]
    fn limits() {
        // 太深
        let deep = format!(
            "<tt xmlns=\"http://www.w3.org/ns/ttml\"><body>{}</body></tt>",
            "<div>".repeat(70) + &"</div>".repeat(70)
        );
        assert_eq!(to_srt(&deep), Err(TtmlError::TooDeep));
        // 太多句
        let many: String = (0..=MAX_CUES)
            .map(|i| format!("<p begin=\"{i}s\" end=\"{}s\">x</p>", i + 1))
            .collect();
        assert_eq!(to_srt(&doc("", "", &many)), Err(TtmlError::TooManyCues));
        // 不展開 DTD 裡定義的實體（billion laughs 之類）
        let dtd = r#"<?xml version="1.0"?><!DOCTYPE tt [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]>
<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="1s" end="2s">&lol2;</p></body></tt>"#;
        assert_eq!(
            cues(&to_srt(dtd).unwrap()),
            [c("00:00:01,000", "00:00:02,000", "&lol2;")]
        );
    }

    #[test]
    fn cues_are_sorted_and_empty_ones_dropped() {
        let all = cues(&srt(
            "",
            "",
            r#"<p begin="5s" end="6s">late</p><p begin="1s" end="2s">early</p><p begin="2s" end="3s"></p><p begin="3s" end="3s">zero</p>"#,
        ));
        assert_eq!(
            all,
            [
                c("00:00:01,000", "00:00:02,000", "early"),
                c("00:00:05,000", "00:00:06,000", "late"),
            ]
        );
    }

    #[test]
    fn ruby_readings_go_in_parentheses() {
        // IMSC 1.1 的注音：讀音放在括號裡（不是黏在字後面）；文件自己的括號（delimiter）不重複；
        // 用樣式指定的也算；空的讀音不留括號
        let head =
            r##"<styling><style xml:id="rt" tts:ruby="text"/><style xml:id="rc" tts:ruby="container"/></styling>"##;
        let all = cues(&srt(
            "",
            head,
            r##"<p begin="1s" end="2s"><span tts:ruby="container"><span tts:ruby="base">漢字</span><span tts:ruby="text">かんじ</span></span>です</p>
                <p begin="2s" end="3s"><span style="rc"><span tts:ruby="base">字</span><span tts:ruby="delimiter">（</span><span style="rt">じ</span><span tts:ruby="delimiter">）</span></span></p>
                <p begin="3s" end="4s"><span tts:ruby="container"><span tts:ruby="baseContainer"><span tts:ruby="base">今日</span></span><span tts:ruby="textContainer" tts:fontStyle="italic"><span tts:ruby="text">きょう</span></span></span></p>
                <p begin="4s" end="5s"><span tts:ruby="container"><span tts:ruby="base">空</span><span tts:ruby="text"></span></span></p>"##,
        ));
        assert_eq!(
            all,
            [
                c("00:00:01,000", "00:00:02,000", "漢字(かんじ)です"),
                c("00:00:02,000", "00:00:03,000", "字(じ)"),
                c("00:00:03,000", "00:00:04,000", "今日<i>(きょう)</i>"),
                c("00:00:04,000", "00:00:05,000", "空"),
            ]
        );
    }

    #[test]
    fn metadata_text_is_not_shown() {
        let all = cues(&srt(
            r#"xmlns:ttm="http://www.w3.org/ns/ttml#metadata""#,
            "<metadata><ttm:title>Title</ttm:title></metadata>",
            r#"<p begin="1s" end="2s">a<metadata><ttm:desc>hidden</ttm:desc></metadata>b</p>"#,
        ));
        assert_eq!(all, [c("00:00:01,000", "00:00:02,000", "ab")]);
    }

    #[test]
    fn sniffing() {
        let ttml = doc("", "", r#"<p begin="1s" end="2s">a</p>"#);
        assert!(looks_like_ttml(&ttml));
        assert!(looks_like_ttml(&format!("\u{feff}{ttml}")), "開頭有 BOM");
        assert!(looks_like_ttml(&format!("<!-- <x> --><?pi x?>{ttml}")));
        // 根元素的開始標籤很長（被開頭的 2 KiB 截斷）
        let long = format!(
            "<tt xmlns=\"http://www.w3.org/ns/ttml\" {}><body/></tt>",
            "xmlns:a=\"urn:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" ".repeat(60)
        );
        assert!(long.find('>').unwrap() > SNIFF_LEN);
        assert!(looks_like_ttml(&long));
        // 不是 TTML
        assert!(!looks_like_ttml("<?xml version=\"1.0\"?><rss><channel/></rss>"));
        assert!(!looks_like_ttml("<tt><body/></tt>"), ".xml 要有 TTML 的命名空間");
        assert!(!looks_like_ttml("1\n00:00:01,000 --> 00:00:02,000\n<i>hi</i>\n"));
        assert!(!looks_like_ttml("[Script Info]\nTitle: x\n"));
        assert!(!looks_like_ttml(""));
        // TTML 的命名空間要在開頭的 2 KiB 裡（在很後面的不算）
        let late = format!("<x>{}</x><tt xmlns=\"http://www.w3.org/ns/ttml\"/>", " ".repeat(3000));
        assert!(!looks_like_ttml(&late));
    }

    #[test]
    fn times() {
        let p = Params {
            frame_rate: 25.0,
            sub_frame_rate: 2.0,
            tick_rate: 1000.0,
        };
        let t = |s| parse_time(s, &p);
        assert_eq!(t("00:00:01"), Some(1.0));
        assert_eq!(t("1:02:03.5"), Some(3723.5));
        assert_eq!(t("00:00:01,25"), Some(1.25), "有的檔案用逗號");
        assert_eq!(t("00:00:01:05"), Some(1.2));
        assert_eq!(t("00:00:00:05.1"), Some(0.22));
        assert_eq!(t("2500t"), Some(2.5));
        assert_eq!(t("50f"), Some(2.0));
        assert_eq!(t(" 3s "), Some(3.0));
        assert_eq!(t("3"), Some(3.0));
        for bad in [
            "",
            "-1s",
            "1x",
            "a:b:c",
            "1:2",
            "1:2:3:4:5",
            "1..2s",
            "s",
            "1e3s",
            "NaNs",
        ] {
            assert_eq!(t(bad), None, "{bad:?}");
        }
    }
}
