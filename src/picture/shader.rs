//! 像素著色器（使用者自己的 .glsl，例如 Anime4K、FSRCNNX）：檢查檔案、組出 mpv 的 glsl-shaders 清單、
//! 套用後畫不出來時還原。都是純函式與狀態機，實際送給 mpv 的在 `Player`（`set_user_shaders`）。
//!
//! glsl-shaders 由影戲管理：清單 = 使用中組合的檔案，後面接翻轉用的著色器（`geometry::flip_shader_path`）。
//! 翻轉是每個檔案各自的，換檔時拿掉；組合整個程式共用，換檔照舊。

use std::path::Path;
use std::time::{Duration, Instant};

/// mpv 路徑清單（glsl-shaders）的分隔字元：Windows 是「;」，其他是「:」（mpv 的 OPTION_PATH_SEPARATOR）
pub const LIST_SEP: char = if cfg!(windows) { ';' } else { ':' };

/// 著色器檔案最大多少（2 MiB）：再大就不是著色器了（Anime4K 最大的檔案約 300 KB）
pub const MAX_SIZE: u64 = 2 * 1024 * 1024;

/// 加入檔案時檢查到的內容
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderInfo {
    /// 第一個 `//!DESC` 的說明
    pub desc: Option<String>,
    /// 有 `//!COMPUTE`（運算著色器；macOS 的 OpenGL 4.1 不支援）
    pub compute: bool,
}

/// 著色器檔案不能用的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderProblem {
    /// 檔案不在了（移走、刪掉、網路磁碟沒接上）
    Missing,
    /// 讀不到（權限之類；內容是系統的錯誤訊息）
    Unreadable(String),
    /// 超過 `MAX_SIZE`
    TooLarge,
    /// 不是文字檔（有 NUL 字元）
    NotText,
    /// 不是 mpv 格式（沒有 //!HOOK 也沒有 //!TEXTURE）：PotPlayer、MPC 的 .hlsl / .fx 之類
    NotMpvFormat,
    /// macOS 不支援的運算著色器（//!COMPUTE）
    ComputeUnsupported,
    /// 路徑含有清單的分隔字元（mpv 會把它拆成兩個檔案）
    PathSeparator,
    /// 路徑不是 UTF-8（mpv 只收 UTF-8，設定檔也存不了）
    NotUtf8Path,
}

impl ShaderProblem {
    /// 給使用者看的說明
    pub fn message(&self) -> String {
        match self {
            ShaderProblem::Missing => crate::tr!("找不到檔案", "File not found").to_owned(),
            ShaderProblem::Unreadable(e) => crate::tf!("無法讀取：{e}", "Can't read the file: {e}"),
            ShaderProblem::TooLarge => {
                crate::tr!("檔案太大（超過 2 MB）", "The file is too large (over 2 MB)").to_owned()
            }
            ShaderProblem::NotText => crate::tr!("不是文字檔", "Not a text file").to_owned(),
            ShaderProblem::NotMpvFormat => crate::tr!(
                "這不是 mpv 格式的 GLSL 著色器（需要 //!HOOK）",
                "This isn't an mpv GLSL shader (it needs //!HOOK)"
            )
            .to_owned(),
            ShaderProblem::ComputeUnsupported => {
                crate::tr!("macOS 不支援 COMPUTE 著色器", "macOS doesn't support COMPUTE shaders").to_owned()
            }
            ShaderProblem::PathSeparator => crate::tf!(
                "路徑裡有「{LIST_SEP}」，mpv 無法載入",
                "The path contains \"{LIST_SEP}\", which mpv can't load"
            ),
            ShaderProblem::NotUtf8Path => crate::tr!(
                "路徑裡有無法辨識的字元（mpv 只接受 UTF-8）",
                "The path has characters mpv can't read (it needs UTF-8)"
            )
            .to_owned(),
        }
    }
}

/// 檢查著色器的內容（加入檔案時）。看的是 mpv 使用者著色器的指令（每行開頭的 `//!`）
pub fn inspect(bytes: &[u8], macos: bool) -> Result<ShaderInfo, ShaderProblem> {
    if bytes.len() as u64 > MAX_SIZE {
        return Err(ShaderProblem::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(ShaderProblem::NotText);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut desc = None;
    let (mut hook, mut compute) = (false, false);
    for line in text.lines() {
        let Some(rest) = line.trim_start_matches('\u{feff}').trim_start().strip_prefix("//!") else {
            continue;
        };
        let (command, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        match command {
            "HOOK" | "TEXTURE" => hook = true,
            "COMPUTE" => compute = true,
            "DESC" if desc.is_none() => desc = Some(arg.trim().to_owned()).filter(|d| !d.is_empty()),
            _ => {}
        }
    }
    if !hook {
        return Err(ShaderProblem::NotMpvFormat);
    }
    if compute && macos {
        return Err(ShaderProblem::ComputeUnsupported);
    }
    Ok(ShaderInfo { desc, compute })
}

/// 檢查路徑能不能放進 glsl-shaders；可以的話回傳存進設定的字串
pub fn check_path(path: &Path) -> Result<String, ShaderProblem> {
    let s = path.to_str().ok_or(ShaderProblem::NotUtf8Path)?;
    if s.contains(LIST_SEP) {
        return Err(ShaderProblem::PathSeparator);
    }
    Ok(s.to_owned())
}

/// 檢查一個著色器檔案（路徑、大小、內容）
pub fn inspect_file(path: &Path, macos: bool) -> Result<ShaderInfo, ShaderProblem> {
    check_path(path)?;
    let unreadable = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::NotFound => ShaderProblem::Missing,
        _ => ShaderProblem::Unreadable(e.to_string()),
    };
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if !meta.is_file() {
        return Err(ShaderProblem::NotText);
    }
    // 先看大小：不要為了檢查把一個大檔案整個讀進來
    if meta.len() > MAX_SIZE {
        return Err(ShaderProblem::TooLarge);
    }
    inspect(&std::fs::read(path).map_err(unreadable)?, macos)
}

/// 送給 mpv 的清單：使用者的組合，後面接翻轉用的著色器（先左右、再上下）
pub fn compose(user: &[String], hflip: Option<&Path>, vflip: Option<&Path>) -> Vec<String> {
    user.iter()
        .cloned()
        .chain(
            [hflip, vflip]
                .into_iter()
                .flatten()
                .map(|p| p.to_string_lossy().into_owned()),
        )
        .collect()
}

/// 清單 → `change-list glsl-shaders set` 的值（用平台的路徑分隔字元串起來）。
/// 路徑裡有分隔字元的話 mpv 會拆成兩個檔案，所以不接受
pub fn list_value(paths: &[String]) -> Result<String, ShaderProblem> {
    if paths.iter().any(|p| p.contains(LIST_SEP)) {
        return Err(ShaderProblem::PathSeparator);
    }
    Ok(paths.join(&LIST_SEP.to_string()))
}

/// 檔案名稱（提示用）
pub fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// 換了著色器之後等多久（從第一次畫出影格算起）沒有錯誤就算成功
pub const WATCH_TIME: Duration = Duration::from_secs(3);
/// …或畫了幾格
pub const WATCH_RENDERS: u64 = 30;

/// 換了著色器之後，看它能不能用。畫面輸出要真的畫出影格才會跑著色器（暫停、沒開檔、
/// 只是視窗大小變了而還沒有影格時都不會），所以從換了之後第一次畫出影格才開始計時，
/// 等 3 秒或 30 格；還沒畫之前一直等著。這裡的「重畫次數」是畫出影格的次數
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShaderApply {
    #[default]
    Idle,
    Pending {
        /// 換之前使用中的組合（還原用）
        previous: Option<u32>,
        /// 這次套用的檔案
        files: Vec<String>,
        /// 什麼時候換的（之前的錯誤記錄不算）
        since: Instant,
        /// 換的時候的重畫次數（那時沒有畫面：None，等有畫面時從那時的次數算起）
        base: Option<u64>,
        /// 換了之後第一次重畫（時間、重畫次數）：從這時開始計時
        window: Option<(Instant, u64)>,
    },
}

/// `ShaderApply::tick` 的結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 還在看
    Wait,
    /// 沒問題（或沒有在看）
    Done,
    /// 著色器畫不出來：改回 `previous`
    Revert {
        previous: Option<u32>,
        /// 出問題的檔案（完整路徑；記錄裡沒寫是哪個檔案時是第一個）
        file: String,
        /// 原因（mpv 的錯誤記錄）
        reason: String,
    },
}

impl ShaderApply {
    /// 換了著色器：開始看（`renders` = 目前的重畫次數；沒有畫面時 None）
    pub fn start(previous: Option<u32>, files: Vec<String>, now: Instant, renders: Option<u64>) -> Self {
        ShaderApply::Pending {
            previous,
            files,
            since: now,
            base: renders,
            window: None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ShaderApply::Pending { .. })
    }

    /// 每一幀：`renders` 是目前的重畫次數（沒有畫面時 None，一直等），`errors` 是這段期間畫面輸出的錯誤記錄
    pub fn tick(&mut self, now: Instant, renders: Option<u64>, errors: &[(Instant, String)]) -> Verdict {
        let ShaderApply::Pending {
            previous,
            files,
            since,
            base,
            window,
        } = self
        else {
            return Verdict::Done;
        };
        let fresh: Vec<&str> = errors
            .iter()
            .filter(|(t, _)| *t >= *since)
            .map(|(_, text)| text.as_str())
            .collect();
        if let Some(file) = fresh.iter().find_map(|text| blamed_file(text, files)) {
            let verdict = Verdict::Revert {
                previous: *previous,
                file,
                reason: reason(&fresh),
            };
            *self = ShaderApply::Idle;
            return verdict;
        }
        let Some(r) = renders else {
            return Verdict::Wait;
        };
        // 剛有畫面（換的時候還沒有），或畫面重新建立（計數歸零）：從現在的次數重新算
        let b = base.get_or_insert(r);
        if r < *b {
            *b = r;
            *window = None;
        }
        if window.is_none() && r > *b {
            *window = Some((now, r));
        }
        match *window {
            Some((t0, r0)) if now.duration_since(t0) >= WATCH_TIME || r - r0 >= WATCH_RENDERS => {
                *self = ShaderApply::Idle;
                Verdict::Done
            }
            _ => Verdict::Wait,
        }
    }
}

/// 錯誤記錄跟著色器有關的話，是哪個檔案：寫了檔名就是那個檔案，
/// 只寫了 shader / .glsl / hook 就算第一個檔案；無關的錯誤回傳 None
fn blamed_file(text: &str, files: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    if let Some(f) = files.iter().find(|f| lower.contains(&file_name(f).to_lowercase())) {
        return Some(f.clone());
    }
    let about_shaders = ["shader", ".glsl", "hook"].iter().any(|w| lower.contains(w));
    about_shaders.then(|| files.first().cloned()).flatten()
}

/// 錯誤記錄 → 原因：優先用寫了 error 的那一行（編譯器的訊息），不用著色器原始碼那幾行（「[ 12] …」）
fn reason(errors: &[&str]) -> String {
    // 拿掉記錄前面的「[libmpv_render] 」
    let strip = |t: &str| -> String {
        let t = t.trim();
        match t.strip_prefix('[').and_then(|r| r.split_once("] ")) {
            Some((_, rest)) => rest.trim().to_owned(),
            None => t.to_owned(),
        }
    };
    let lines: Vec<String> = errors.iter().map(|t| strip(t)).collect();
    let is_source = |l: &str| l.starts_with('[') && l[1..].trim_start().starts_with(|c: char| c.is_ascii_digit());
    let pick = lines
        .iter()
        .find(|l| !is_source(l) && l.to_lowercase().contains("error"))
        .or_else(|| lines.iter().find(|l| !is_source(l)))
        .or(lines.first())
        .cloned()
        .unwrap_or_default();
    // 太長的訊息截短（提示一行放得下）
    match pick.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &pick[..i]),
        None => pick,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: &str = "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC 反相測試\n\
                        vec4 hook() { vec4 c = HOOKED_tex(HOOKED_pos); return vec4(1.0 - c.rgb, c.a); }\n";

    #[test]
    fn inspect_accepts_mpv_hooks_and_reads_the_description() {
        let info = inspect(HOOK.as_bytes(), false).unwrap();
        assert_eq!(info.desc.as_deref(), Some("反相測試"));
        assert!(!info.compute);
        // 只有 TEXTURE 的檔案（查表用）、BOM、縮排、沒有 DESC 也可以
        let lut = "\u{feff}  //!TEXTURE lut\n//!SIZE 2 2\n//!FORMAT rgba8\n00ff00ff\n";
        assert_eq!(
            inspect(lut.as_bytes(), true),
            Ok(ShaderInfo {
                desc: None,
                compute: false
            })
        );
        // 第一個 DESC
        let two = "//!HOOK LUMA\n//!DESC 第一段\nvec4 hook(){return HOOKED_tex(HOOKED_pos);}\n\
                   //!HOOK LUMA\n//!DESC 第二段\nvec4 hook(){return HOOKED_tex(HOOKED_pos);}\n";
        assert_eq!(inspect(two.as_bytes(), false).unwrap().desc.as_deref(), Some("第一段"));
    }

    #[test]
    fn inspect_rejects_hlsl_binary_and_huge_files() {
        // PotPlayer / MPC-HC 的 HLSL 像素著色器
        let hlsl = "sampler s0 : register(s0);\nfloat4 main(float2 tex : TEXCOORD0) : COLOR {\n\
                    return 1 - tex2D(s0, tex);\n}\n";
        assert_eq!(inspect(hlsl.as_bytes(), false), Err(ShaderProblem::NotMpvFormat));
        // 註解裡提到 //!HOOK 不算（不在行首）
        assert_eq!(
            inspect(b"// use //!HOOK MAIN here\nvoid main() {}\n", false),
            Err(ShaderProblem::NotMpvFormat)
        );
        assert_eq!(inspect(b"", false), Err(ShaderProblem::NotMpvFormat));
        let mut binary = HOOK.as_bytes().to_vec();
        binary.extend([0, 1, 2, 0xff]);
        assert_eq!(inspect(&binary, false), Err(ShaderProblem::NotText));
        let mut huge = HOOK.as_bytes().to_vec();
        huge.resize(MAX_SIZE as usize + 1, b' ');
        assert_eq!(inspect(&huge, false), Err(ShaderProblem::TooLarge));
        huge.truncate(MAX_SIZE as usize);
        assert!(inspect(&huge, false).is_ok(), "剛好 2 MiB 可以");
    }

    #[test]
    fn compute_shaders_are_rejected_only_on_macos() {
        let compute = "//!HOOK MAIN\n//!BIND HOOKED\n//!COMPUTE 32 8\n//!DESC 運算\nvoid hook() {}\n";
        assert_eq!(
            inspect(compute.as_bytes(), false),
            Ok(ShaderInfo {
                desc: Some("運算".into()),
                compute: true
            })
        );
        assert_eq!(
            inspect(compute.as_bytes(), true),
            Err(ShaderProblem::ComputeUnsupported)
        );
        assert!(inspect(HOOK.as_bytes(), true).is_ok(), "一般的著色器 macOS 也可以");
    }

    #[test]
    fn compose_puts_the_flips_after_the_user_chain() {
        let user = vec!["/s/a.glsl".to_owned(), "/s/b.glsl".to_owned()];
        let (h, v) = (Path::new("/c/hflip.glsl"), Path::new("/c/vflip.glsl"));
        assert_eq!(compose(&user, None, None), user);
        assert_eq!(
            compose(&user, Some(h), Some(v)),
            ["/s/a.glsl", "/s/b.glsl", "/c/hflip.glsl", "/c/vflip.glsl"]
        );
        assert_eq!(
            compose(&user, None, Some(v)),
            ["/s/a.glsl", "/s/b.glsl", "/c/vflip.glsl"]
        );
        assert_eq!(compose(&[], Some(h), None), ["/c/hflip.glsl"]);
        assert!(compose(&[], None, None).is_empty());
    }

    #[test]
    fn list_value_rejects_the_separator() {
        let sep = LIST_SEP;
        let a = "影戲 著色器/放大 A.glsl".to_owned();
        let b = "x/b.glsl".to_owned();
        assert_eq!(list_value(&[a.clone(), b.clone()]), Ok(format!("{a}{sep}{b}")));
        assert_eq!(list_value(&[]), Ok(String::new()));
        let bad = format!("x/a{sep}b.glsl");
        assert_eq!(list_value(&[a, bad.clone()]), Err(ShaderProblem::PathSeparator));
        assert_eq!(check_path(Path::new(&bad)), Err(ShaderProblem::PathSeparator));
        // 另一個平台的分隔字元沒關係（例如 Linux 的檔名裡的「;」）
        let other = if cfg!(windows) { "x/a:b.glsl" } else { "x/a;b.glsl" };
        assert_eq!(check_path(Path::new(other)), Ok(other.to_owned()));
        assert_eq!(list_value(&[other.to_owned()]), Ok(other.to_owned()));
        assert_eq!(check_path(Path::new("x/放大.glsl")), Ok("x/放大.glsl".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_rejected() {
        use std::os::unix::ffi::OsStrExt;
        let p = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.glsl"));
        assert_eq!(check_path(p), Err(ShaderProblem::NotUtf8Path));
        assert_eq!(inspect_file(p, false), Err(ShaderProblem::NotUtf8Path));
    }

    #[test]
    fn inspect_file_checks_existence_and_size_first() {
        let dir = std::env::temp_dir().join(format!("vitascope-shader-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("反相.glsl");
        std::fs::write(&ok, HOOK).unwrap();
        assert_eq!(inspect_file(&ok, false).unwrap().desc.as_deref(), Some("反相測試"));
        assert_eq!(inspect_file(&dir.join("沒有.glsl"), false), Err(ShaderProblem::Missing));
        let big = dir.join("big.glsl");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_SIZE + 1).unwrap();
        assert_eq!(inspect_file(&big, false), Err(ShaderProblem::TooLarge));
        assert_eq!(inspect_file(&dir, false), Err(ShaderProblem::NotText), "資料夾");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn problems_have_both_languages() {
        let all = [
            ShaderProblem::Missing,
            ShaderProblem::Unreadable("x".into()),
            ShaderProblem::TooLarge,
            ShaderProblem::NotText,
            ShaderProblem::NotMpvFormat,
            ShaderProblem::ComputeUnsupported,
            ShaderProblem::PathSeparator,
            ShaderProblem::NotUtf8Path,
        ];
        assert_eq!(
            ShaderProblem::NotMpvFormat.message(),
            "這不是 mpv 格式的 GLSL 著色器（需要 //!HOOK）"
        );
        let zh: Vec<String> = all.iter().map(ShaderProblem::message).collect();
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let en: Vec<String> = all.iter().map(ShaderProblem::message).collect();
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        for (z, e) in zh.iter().zip(&en) {
            assert_ne!(z, e);
            assert!(e.is_ascii(), "{e}");
        }
    }

    // ───────────── 套用後畫不出來時還原 ─────────────

    fn files() -> Vec<String> {
        vec!["/s/Anime4K_Clamp.glsl".into(), "/s/Anime4K_Upscale.glsl".into()]
    }

    fn err(t: Instant, text: &str) -> (Instant, String) {
        (t, text.to_owned())
    }

    #[test]
    fn watch_waits_for_the_first_render_then_times_out() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = ShaderApply::start(Some(7), files(), t0, Some(100));
        // 暫停中（沒有重畫）：多久都一直等
        assert_eq!(w.tick(ms(10_000), Some(100), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(60_000), Some(100), &[]), Verdict::Wait);
        // 第一次重畫：開始計時
        assert_eq!(w.tick(ms(60_100), Some(101), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(62_000), Some(110), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(63_100), Some(111), &[]), Verdict::Done, "3 秒");
        assert_eq!(w, ShaderApply::Idle);
        assert_eq!(w.tick(ms(63_200), Some(200), &[]), Verdict::Done, "沒有在看");
        // 一直重畫：30 次就夠了
        let mut w = ShaderApply::start(None, files(), t0, Some(0));
        assert_eq!(w.tick(ms(10), Some(1), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(200), Some(30), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(250), Some(31), &[]), Verdict::Done);
        // 沒有畫面（自動測試、還沒開檔）：一直等
        let mut w = ShaderApply::start(None, files(), t0, None);
        assert_eq!(w.tick(ms(99_000), None, &[]), Verdict::Wait);
        assert!(w.is_pending());
        // 有畫面了：之前畫的不算，從那時的次數算起（不是一有畫面就開始計時）
        assert_eq!(w.tick(ms(99_100), Some(800), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(120_000), Some(800), &[]), Verdict::Wait, "還沒畫");
        assert_eq!(w.tick(ms(120_010), Some(801), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(123_020), Some(802), &[]), Verdict::Done);
        // 畫面重新建立（次數變小）：重新算
        let mut w = ShaderApply::start(None, files(), t0, Some(500));
        assert_eq!(w.tick(ms(10), Some(3), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(5_000), Some(3), &[]), Verdict::Wait, "還沒重畫");
        assert_eq!(w.tick(ms(5_010), Some(4), &[]), Verdict::Wait);
        assert_eq!(w.tick(ms(8_020), Some(5), &[]), Verdict::Done);
    }

    #[test]
    fn shader_errors_revert_to_the_previous_preset() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = ShaderApply::start(Some(7), files(), t0, Some(100));
        // 換之前的錯誤、跟著色器無關的錯誤：不算
        let unrelated = [
            err(t0 - Duration::from_millis(5), "[libmpv_render] shader compile failed"),
            err(ms(5), "[vo/gpu] Could not create swapchain"),
        ];
        assert_eq!(w.tick(ms(10), Some(101), &unrelated), Verdict::Wait);
        // 編譯失敗（原始碼那幾行之後才是編譯器的訊息）
        let errors = [
            err(ms(20), "[libmpv_render] fragment shader source:"),
            err(ms(20), "[libmpv_render] [ 12] vec4 hook() { return error; }"),
            err(ms(20), "[libmpv_render] fragment shader compile log (status=0):"),
            err(
                ms(20),
                "[libmpv_render] 0(12) : error C1008: undefined variable \"error\"",
            ),
        ];
        assert_eq!(
            w.tick(ms(20), Some(102), &errors),
            Verdict::Revert {
                previous: Some(7),
                file: "/s/Anime4K_Clamp.glsl".into(),
                reason: "0(12) : error C1008: undefined variable \"error\"".into(),
            }
        );
        assert_eq!(w, ShaderApply::Idle);
        // 記錄寫了檔名：是那個檔案（大小寫不分）；還沒重畫也算（錯誤記錄可能比重畫次數早看到）
        let mut w = ShaderApply::start(None, files(), t0, Some(0));
        let named = [err(
            ms(1),
            "[libmpv_render] anime4k_upscale.glsl: Unrecognized command 'HOKO'!",
        )];
        assert_eq!(
            w.tick(ms(1), Some(0), &named),
            Verdict::Revert {
                previous: None,
                file: "/s/Anime4K_Upscale.glsl".into(),
                reason: "anime4k_upscale.glsl: Unrecognized command 'HOKO'!".into(),
            }
        );
        // 只寫了 hook：算第一個檔案
        let mut w = ShaderApply::start(None, files(), t0, None);
        let Verdict::Revert { file, .. } = w.tick(ms(1), None, &[err(ms(1), "[vo/gpu] Failed hooking pass")]) else {
            panic!("要還原");
        };
        assert_eq!(file, "/s/Anime4K_Clamp.glsl");
        // 時間到之後的錯誤不算（已經不在看了）
        let mut w = ShaderApply::start(None, files(), t0, Some(0));
        w.tick(ms(1), Some(1), &[]);
        assert_eq!(w.tick(ms(3_100), Some(2), &[]), Verdict::Done);
        assert_eq!(w.tick(ms(3_200), Some(3), &errors), Verdict::Done);
    }

    #[test]
    fn long_reasons_are_shortened() {
        let long = format!("[libmpv_render] error: {}", "很長".repeat(100));
        let r = reason(&[long.as_str()]);
        assert_eq!(r.chars().count(), 121);
        assert!(r.ends_with('…'));
        assert_eq!(reason(&["[vo/gpu] [  3] only source"]), "[  3] only source");
    }
}
