//! 真實影片普查：對一批實際的影片檔做 headless 播放測試，統計格式分佈和失敗原因。
//!
//! 用法：
//!   cargo run --release --example media_survey -- <檔案清單.tsv> [選項]
//!
//! 檔案清單：每行「副檔名<TAB>大小<TAB>完整路徑」（例如用 PowerShell 掃描磁碟產生）。
//!
//! 選項：
//!   --per-ext N    每種副檔名最多測 N 個；檔案很多時每個資料夾只挑一個，盡量涵蓋不同作品（預設 200）
//!   --ext mkv,mp4  只測這些副檔名
//!   --hwdec        用 GPU 解碼（auto-copy）並記錄實際使用的解碼器
//!   --jobs N       同時測幾個檔案（預設 4；網路磁碟不要開太多）
//!   --out 檔案     每個檔案的結果寫成 JSON Lines
//!
//! 只讀取檔案，不做任何修改。每個檔案的檢查：開檔 → 解出第一格 → 跳到中間 → 實際播放 1 秒。

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vitascope::player::{Options, Player, PlayerEvent};

const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const PLAY_TIMEOUT: Duration = Duration::from_secs(20);

const MEDIA_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "ts", "m2ts", "mts", "rmvb", "rm", "wmv", "asf", "mov", "flv", "webm", "mpg", "mpeg",
    "vob", "ogm", "ogv", "3gp", "mka", "mp3", "flac", "m4a",
];

#[derive(Clone)]
struct Entry {
    ext: String,
    size: u64,
    path: String,
}

#[derive(Serialize, Default)]
struct Report {
    path: String,
    ext: String,
    size: u64,
    ok: bool,
    /// 失敗在哪個步驟
    stage: Option<String>,
    error: Option<String>,
    open_ms: u128,
    first_frame_ms: u128,
    seek_ms: u128,
    format: Option<String>,
    duration: Option<f64>,
    video: Option<Value>,
    audio: Vec<Value>,
    subs: Vec<Value>,
    hwdec: Option<String>,
    /// 測試過程中 mpv 的錯誤記錄（不一定代表失敗，例如部分損毀的影格）
    mpv_errors: Vec<String>,
}

struct Args {
    list: String,
    per_ext: usize,
    exts: Option<Vec<String>>,
    hwdec: bool,
    jobs: usize,
    out: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        list: String::new(),
        per_ext: 200,
        exts: None,
        hwdec: false,
        jobs: 4,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--per-ext" => a.per_ext = it.next().and_then(|v| v.parse().ok()).expect("--per-ext 需要數字"),
            "--ext" => {
                a.exts = it
                    .next()
                    .map(|v| v.split(',').map(|s| s.trim().to_lowercase()).collect())
            }
            "--hwdec" => a.hwdec = true,
            "--jobs" => a.jobs = it.next().and_then(|v| v.parse().ok()).expect("--jobs 需要數字"),
            "--out" => a.out = it.next(),
            _ => a.list = arg,
        }
    }
    assert!(
        !a.list.is_empty(),
        "用法：media_survey <檔案清單.tsv> [--per-ext N] [--ext mkv,mp4] [--hwdec] [--jobs N] [--out 結果.jsonl]"
    );
    a
}

/// 固定種子的簡單亂數（xorshift），讓抽樣結果每次相同、可重現
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

/// 每種副檔名最多 `per_ext` 個；檔案太多時每個資料夾挑一個，涵蓋不同作品、不同壓制組
fn sample(entries: Vec<Entry>, per_ext: usize) -> Vec<Entry> {
    let mut by_ext: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for e in entries {
        by_ext.entry(e.ext.clone()).or_default().push(e);
    }
    let mut rng = Rng(0x5eed_1896);
    let mut out = Vec::new();
    for (_, files) in by_ext {
        if files.len() <= per_ext {
            out.extend(files);
            continue;
        }
        let mut by_dir: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for f in files {
            let dir = Path::new(&f.path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            by_dir.entry(dir).or_default().push(f);
        }
        let mut dirs: Vec<Vec<Entry>> = by_dir.into_values().collect();
        rng.shuffle(&mut dirs);
        for mut d in dirs.into_iter().take(per_ext) {
            let i = (rng.next() % d.len() as u64) as usize;
            out.push(d.swap_remove(i));
        }
    }
    out
}

fn tracks(p: &Player) -> Vec<Value> {
    p.get_string("track-list")
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// 從 track-list 挑出要記錄的欄位
fn pick(t: &Value, keys: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for k in keys {
        if let Some(v) = t.get(*k).filter(|v| !v.is_null()) {
            m.insert((*k).to_owned(), v.clone());
        }
    }
    Value::Object(m)
}

fn survey(e: &Entry, hwdec: bool) -> Report {
    let mut r = Report {
        path: e.path.clone(),
        ext: e.ext.clone(),
        size: e.size,
        ..Default::default()
    };
    if let Err((stage, err)) = run(e, hwdec, &mut r) {
        r.stage = Some(stage.to_owned());
        r.error = Some(err);
    } else {
        r.ok = true;
    }
    r
}

fn run(e: &Entry, hwdec: bool, r: &mut Report) -> Result<(), (&'static str, String)> {
    let opts = Options {
        hwdec: if hwdec { "auto-copy".into() } else { "no".into() },
        ..Options::headless()
    };
    let mut p = Player::new(opts).map_err(|err| ("建立 mpv", err.to_string()))?;
    p.set_pause(true).map_err(|err| ("暫停", err.to_string()))?;

    let t0 = Instant::now();
    p.open(&e.path).map_err(|err| ("開檔", err.to_string()))?;
    p.wait_for(OPEN_TIMEOUT, |ev| *ev == PlayerEvent::FileLoaded)
        .map_err(|err| ("開檔", err))?;
    r.open_ms = t0.elapsed().as_millis();
    p.wait_for(OPEN_TIMEOUT, |ev| *ev == PlayerEvent::PlaybackRestart)
        .map_err(|err| ("解出第一格", err))?;
    r.first_frame_ms = t0.elapsed().as_millis();

    r.format = p.get_string("file-format").ok();
    r.duration = p.get_f64("duration").ok();
    let video_params: Value = p
        .get_string("video-params")
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or(Value::Null);
    for t in tracks(&p) {
        match t.get("type").and_then(Value::as_str) {
            Some("video") if r.video.is_none() && t.get("selected") == Some(&Value::Bool(true)) => {
                let mut v = pick(
                    &t,
                    &[
                        "codec",
                        "codec-profile",
                        "demux-w",
                        "demux-h",
                        "demux-fps",
                        "albumart",
                        "dolby-vision-profile",
                    ],
                );
                if let Value::Object(m) = &mut v {
                    for k in ["pixelformat", "gamma", "primaries", "colormatrix"] {
                        if let Some(x) = video_params.get(k) {
                            m.insert(k.to_owned(), x.clone());
                        }
                    }
                }
                r.video = Some(v);
            }
            Some("audio") => r.audio.push(pick(
                &t,
                &[
                    "codec",
                    "codec-profile",
                    "demux-channels",
                    "demux-samplerate",
                    "lang",
                    "title",
                    "external",
                    "selected",
                ],
            )),
            Some("sub") => r.subs.push(pick(
                &t,
                &["codec", "lang", "title", "external", "forced", "default", "selected"],
            )),
            _ => {}
        }
    }

    // 跳到中間：檢查索引和跳轉
    if let Some(d) = r.duration.filter(|d| *d > 4.0) {
        let t1 = Instant::now();
        p.seek_to(d * 0.5, false).map_err(|err| ("跳到中間", err.to_string()))?;
        p.wait_for(OPEN_TIMEOUT, |ev| *ev == PlayerEvent::PlaybackRestart)
            .map_err(|err| ("跳到中間", err))?;
        r.seek_ms = t1.elapsed().as_millis();
    }

    // 實際播放 1 秒：解碼要能持續進行
    p.poll();
    let start = p.state.time_pos;
    p.set_pause(false).map_err(|err| ("播放", err.to_string()))?;
    p.wait_state(PLAY_TIMEOUT, |s| s.time_pos >= start + 1.0 || s.eof)
        .map_err(|err| ("播放 1 秒", err))?;

    r.hwdec = p.get_string("hwdec-current").ok();
    r.mpv_errors = p.recent_errors().to_vec();
    Ok(())
}

fn describe(r: &Report) -> String {
    let v = r.video.as_ref();
    let s = |v: Option<&Value>, k: &str| {
        v.and_then(|x| x.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut parts = vec![r.format.clone().unwrap_or_default()];
    if v.is_some() {
        parts.push(format!("{} {}", s(v, "codec"), s(v, "pixelformat")));
    }
    if let Some(a) = r.audio.first() {
        parts.push(s(Some(a), "codec"));
    }
    if !r.subs.is_empty() {
        parts.push(format!("字幕×{}", r.subs.len()));
    }
    parts.join(" | ")
}

fn count(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

fn print_table(title: &str, map: &BTreeMap<String, usize>) {
    println!("\n{title}");
    let mut v: Vec<_> = map.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (k, n) in v {
        println!("  {n:>5}  {k}");
    }
}

fn main() {
    let args = parse_args();
    let text = std::fs::read_to_string(&args.list).expect("讀不到檔案清單");
    let entries: Vec<Entry> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let ext = it.next()?.trim_start_matches('.').to_lowercase();
            let size = it.next()?.parse().ok()?;
            let path = it.next()?.to_owned();
            Some(Entry { ext, size, path })
        })
        .filter(|e| MEDIA_EXTS.contains(&e.ext.as_str()))
        .filter(|e| args.exts.as_ref().is_none_or(|x| x.contains(&e.ext)))
        .collect();
    let total = entries.len();
    let todo = sample(entries, args.per_ext);
    println!(
        "清單中有 {total} 個影音檔，抽樣測試 {} 個（{} 個同時進行）\n",
        todo.len(),
        args.jobs
    );

    let queue = Mutex::new(todo.clone());
    let results = Mutex::new(Vec::<Report>::new());
    let out = args
        .out
        .as_ref()
        .map(|p| Mutex::new(std::fs::File::create(p).expect("無法建立輸出檔")));
    let n = todo.len();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| {
                loop {
                    let Some(e) = queue.lock().unwrap().pop() else { break };
                    let r = survey(&e, args.hwdec);
                    if let Some(f) = &out {
                        let _ = writeln!(f.lock().unwrap(), "{}", serde_json::to_string(&r).unwrap());
                    }
                    let mut res = results.lock().unwrap();
                    res.push(r);
                    let r = res.last().unwrap();
                    let i = res.len();
                    if r.ok {
                        let warn = if r.mpv_errors.is_empty() {
                            ""
                        } else {
                            "  ⚠ 有錯誤記錄"
                        };
                        println!(
                            "[{i:>4}/{n}] ✔ {:<5} {}  ({:.1}s){warn}",
                            r.ext,
                            describe(r),
                            r.first_frame_ms as f64 / 1000.0
                        );
                    } else {
                        println!(
                            "[{i:>4}/{n}] ✘ {:<5} {}：{}\n             {}",
                            r.ext,
                            r.stage.as_deref().unwrap_or(""),
                            r.error.as_deref().unwrap_or(""),
                            r.path
                        );
                    }
                }
            });
        }
    });

    let results = results.into_inner().unwrap();
    let ok = results.iter().filter(|r| r.ok).count();
    println!(
        "\n══════ 結果：{ok}/{} 通過，耗時 {:.0} 秒 ══════",
        results.len(),
        started.elapsed().as_secs_f64()
    );

    let mut by_ext: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut containers, mut vcodecs, mut acodecs, mut subs, mut hdr, mut hw) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    for r in &results {
        let e = by_ext.entry(r.ext.clone()).or_default();
        e.0 += 1;
        e.1 += usize::from(r.ok);
        if let Some(f) = &r.format {
            count(&mut containers, f.clone());
        }
        if let Some(v) = &r.video {
            let profile = s(v, "codec-profile").map(|p| format!(" ({p})")).unwrap_or_default();
            count(
                &mut vcodecs,
                format!(
                    "{} {}{profile}",
                    s(v, "codec").unwrap_or_default(),
                    s(v, "pixelformat").unwrap_or_default()
                ),
            );
            let gamma = s(v, "gamma").unwrap_or_default();
            if gamma == "pq" || gamma == "hlg" || v.get("dolby-vision-profile").is_some() {
                let dv = v
                    .get("dolby-vision-profile")
                    .map(|p| format!(" Dolby Vision P{p}"))
                    .unwrap_or_default();
                count(&mut hdr, format!("{gamma}{dv}"));
            }
        }
        for a in &r.audio {
            let profile = s(a, "codec-profile").map(|p| format!(" ({p})")).unwrap_or_default();
            let ch = s(a, "demux-channels").map(|c| format!(" {c}")).unwrap_or_default();
            let ext = if a.get("external") == Some(&Value::Bool(true)) {
                " [外掛]"
            } else {
                ""
            };
            count(
                &mut acodecs,
                format!("{}{profile}{ch}{ext}", s(a, "codec").unwrap_or_default()),
            );
        }
        for t in &r.subs {
            let ext = if t.get("external") == Some(&Value::Bool(true)) {
                " [外掛]"
            } else {
                ""
            };
            count(&mut subs, format!("{}{ext}", s(t, "codec").unwrap_or_default()));
        }
        if let Some(h) = &r.hwdec {
            count(
                &mut hw,
                if r.video.is_some() {
                    h.clone()
                } else {
                    "（無影像）".into()
                },
            );
        }
    }

    println!("\n副檔名（通過 / 測試）");
    for (ext, (all, ok)) in &by_ext {
        println!("  {ext:<6} {ok:>4} / {all}");
    }
    print_table("容器（mpv 的 file-format）", &containers);
    print_table("影像編碼 + 像素格式", &vcodecs);
    print_table("HDR", &hdr);
    print_table("音訊（每條音軌）", &acodecs);
    print_table("字幕（每條字幕軌）", &subs);
    if args.hwdec {
        print_table("解碼器", &hw);
    }

    // 能播放但有錯誤記錄的：依訊息類型統計（數字換成 #，同類訊息合併），每類舉一個例子
    let mut kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for r in results.iter().filter(|r| r.ok) {
        let mut seen = std::collections::BTreeSet::new();
        for m in &r.mpv_errors {
            let norm: String = m.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
            if seen.insert(norm.clone()) {
                let e = kinds.entry(norm).or_insert((0, r.path.clone()));
                e.0 += 1;
            }
        }
    }
    if !kinds.is_empty() {
        println!("\n能播放、但過程中有錯誤記錄（依類型，數字＝檔案數）");
        let mut v: Vec<_> = kinds.into_iter().collect();
        v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
        for (msg, (n, example)) in v.iter().take(20) {
            println!("  {n:>5}  {msg}\n         例：{example}");
        }
    }
    let failed: Vec<_> = results.iter().filter(|r| !r.ok).collect();
    if !failed.is_empty() {
        println!("\n失敗（{} 個）", failed.len());
        for r in &failed {
            println!(
                "  [{}] {}\n    {}",
                r.stage.as_deref().unwrap_or(""),
                r.path,
                r.error.as_deref().unwrap_or("")
            );
        }
    }
}
