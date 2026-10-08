//! 音效選項（輸出裝置、獨佔模式、轉成立體聲、音訊直通）對應到 mpv：介面上每個值 mpv 都接受、
//! 預設設定跟 mpv 原本的值一樣、套用時只送有變的、音訊直通真的會開始（ao=null 也接受 spdif）。
//! headless（不出畫面、不出聲音），三個平台的 CI 都跑；CI 的電腦沒有音訊裝置，不依賴真的裝置。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use vitascope::player::{AfCaps, AsyncKey, Options, Player, PlayerEvent, TrackKind, async_key};
use vitascope::sound::{
    self, AUTO_DEVICE, AudioSettings, EqPreset, Equalizer, Leveling, MANAGED, Passthrough, SPDIF_CODECS, af_chain,
    band_command, boost_level, limit_command,
};

const TIMEOUT: Duration = Duration::from_secs(15);

/// 樣本的完整路徑；沒有產生出來時 None（「罕見」格式有些 FFmpeg 建置產生不了）
fn sample_if_exists(rel: &str) -> Option<String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    p.exists().then(|| p.to_string_lossy().into_owned())
}

fn sample(rel: &str) -> String {
    sample_if_exists(rel).unwrap_or_else(|| panic!("找不到樣本 {rel}，請先執行：python scripts/gen_samples.py"))
}

fn player_with(extra: &[(&str, &str)]) -> Player {
    Player::new(Options {
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗")
}

/// 等一個非同步指令的回覆，回傳（種類, 錯誤）
fn reply(p: &mut Player) -> (Option<AsyncKey>, Option<String>) {
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
        Ok(PlayerEvent::CommandReply { id, error }) => (async_key(id), error),
        other => panic!("等不到回覆：{other:?}"),
    }
}

/// 每一種直通格式的組合（2⁵）
fn every_passthrough() -> Vec<Passthrough> {
    (0..1u32 << SPDIF_CODECS.len())
        .map(|bits| {
            let mut p = Passthrough {
                enabled: true,
                ..Passthrough::default()
            };
            for (i, c) in SPDIF_CODECS.into_iter().enumerate() {
                p.set_codec(c, bits & (1 << i) != 0);
            }
            p
        })
        .collect()
}

#[test]
fn every_ui_value_is_accepted_by_mpv() {
    let p = player_with(&[]);
    let mut values: Vec<(&str, String)> = Vec::new();
    for downmix in [false, true] {
        for normalize in [false, true] {
            for exclusive in [false, true] {
                let a = AudioSettings {
                    downmix,
                    normalize_downmix: normalize,
                    exclusive,
                    ..AudioSettings::default()
                };
                values.extend(sound::mpv_options(&a, AUTO_DEVICE, &HashSet::new()));
            }
        }
    }
    for pt in every_passthrough() {
        values.push(("audio-spdif", sound::spdif_value(&pt)));
    }
    for (name, value) in &values {
        p.mpv()
            .set_property(name, value.as_str())
            .unwrap_or_else(|e| panic!("mpv 不接受 {name}={value}：{e}"));
        let now = p.get_string(name).unwrap();
        assert_eq!(&now, value, "{name}");
    }
}

#[test]
fn default_options_are_the_engine_defaults() {
    // 預設設定送出的值跟 mpv 原本的預設一模一樣（使用者沒改設定時，行為跟以前相同）
    let mut p = player_with(&[]);
    for (name, value) in sound::mpv_options(&AudioSettings::default(), AUTO_DEVICE, &HashSet::new()) {
        let default = p.get_string(&format!("option-info/{name}/default-value")).unwrap();
        assert_eq!(value, default, "{name}");
    }
    assert_eq!(
        p.get_string("option-info/audio-channels/default-value").unwrap(),
        sound::CHANNELS_DEFAULT
    );
    // 套用（同步）之後也一樣
    let opts = sound::mpv_options(&AudioSettings::default(), AUTO_DEVICE, &HashSet::new());
    let sent = p.apply_sound(&opts, true);
    assert_eq!(sent.len(), MANAGED.len());
    assert!(sent.iter().all(|(_, _, r)| matches!(r, Ok(None))), "{sent:?}");
    for name in MANAGED {
        assert_eq!(
            p.get_string(name).unwrap(),
            p.get_string(&format!("option-info/{name}/default-value")).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn apply_sends_only_what_changed() {
    let mut p = player_with(&[("audio-exclusive", "yes")]);
    let overrides = p.user_overrides().clone();
    let opts = |a: &AudioSettings| sound::mpv_options(a, AUTO_DEVICE, &overrides);
    let mut a = AudioSettings::default();
    // 啟動時同步送；使用者指定的 audio-exclusive 不列
    let sent = p.apply_sound(&opts(&a), true);
    let names: Vec<&str> = sent.iter().map(|(n, _, _)| *n).collect();
    assert_eq!(
        names,
        [
            "audio-device",
            "audio-channels",
            "audio-normalize-downmix",
            "audio-spdif"
        ]
    );
    assert_eq!(p.get_string("audio-exclusive").unwrap(), "yes", "使用者指定的不能蓋掉");
    assert!(p.apply_sound(&opts(&a), false).is_empty(), "沒變就不送");
    // 轉成立體聲：聲道與避免破音兩項，種類都是 Downmix
    a.downmix = true;
    let keys: Vec<(&str, AsyncKey)> = p
        .apply_sound(&opts(&a), false)
        .iter()
        .map(|(n, k, _)| (*n, *k))
        .collect();
    assert_eq!(
        keys,
        [
            ("audio-channels", AsyncKey::Downmix),
            ("audio-normalize-downmix", AsyncKey::Downmix)
        ]
    );
    a.passthrough.enabled = true;
    let mut b = a.clone();
    b.exclusive = true; // 使用者指定了，照樣不送
    let keys: Vec<(&str, AsyncKey)> = p
        .apply_sound(&opts(&b), false)
        .iter()
        .map(|(n, k, _)| (*n, *k))
        .collect();
    assert_eq!(keys, [("audio-spdif", AsyncKey::Spdif)]);
    let device = sound::mpv_options(&b, "wasapi/{nope}", &overrides);
    let keys: Vec<(&str, AsyncKey)> = p.apply_sound(&device, false).iter().map(|(n, k, _)| (*n, *k)).collect();
    assert_eq!(keys, [("audio-device", AsyncKey::AudioDevice)]);
    for _ in 0..4 {
        let (k, error) = reply(&mut p);
        assert!(k.is_some());
        assert_eq!(error, None);
    }
    for (name, value) in [
        ("audio-channels", "stereo"),
        ("audio-normalize-downmix", "yes"),
        ("audio-spdif", "ac3,eac3,dts"),
        // ao=null 不管裝置名稱，mpv 照樣接受
        ("audio-device", "wasapi/{nope}"),
    ] {
        assert_eq!(p.get_string(name).unwrap(), value, "{name}");
    }
    // mpv 拒絕的值：忘掉之後下次再送
    let bogus = [("audio-channels", "no-such-layout".to_owned())];
    assert_eq!(p.apply_sound(&bogus, false).len(), 1);
    let (k, error) = reply(&mut p);
    assert_eq!(k, Some(AsyncKey::Downmix));
    assert!(error.is_some());
    assert!(p.apply_sound(&bogus, false).is_empty(), "還記著");
    p.forget_sound("audio-channels");
    let names: Vec<&str> = p.apply_sound(&opts(&a), false).iter().map(|(n, _, _)| *n).collect();
    assert_eq!(
        names,
        ["audio-device", "audio-channels"],
        "忘掉的再送一次正確的值（裝置也改回 auto）"
    );
}

#[test]
fn options_set_by_an_included_file_are_left_alone() {
    // VITASCOPE_MPV_OPTS="include=…"：設定檔間接改到的音效選項也算使用者指定的
    let dir = std::env::temp_dir().join(format!("vitascope-sound-include-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("mine.conf");
    std::fs::write(&conf, "audio-channels=stereo\naudio-spdif=ac3\naf=lavfi=[anull]\n").unwrap();
    let p = player_with(&[("include", conf.to_str().unwrap())]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(p.get_string("audio-channels").unwrap(), "stereo");
    // af（等化器、音量平衡的濾鏡鏈）也是
    for name in ["audio-channels", "audio-spdif", "af"] {
        assert!(p.user_overrides().contains(name), "{name} 由設定檔指定");
    }
    assert!(!p.user_overrides().contains("audio-device"), "沒改到的照常管理");
}

#[test]
fn the_engine_device_list_parses() {
    // 第一項一定是 auto（CI 沒有音訊裝置時只有它）；每一項都有名稱
    let mut p = player_with(&[]);
    let list = p.read_audio_devices().expect("讀不到 audio-device-list").to_vec();
    assert_eq!(list[0].name, AUTO_DEVICE, "{list:?}");
    assert!(list.iter().all(|d| !d.name.is_empty()));
    assert_eq!(p.state.audio_devices.as_deref(), Some(list.as_slice()));
    // 觀察清單：mpv 送來的跟直接讀的一樣
    let mut q = player_with(&[]);
    q.watch_audio_devices();
    q.wait_state(TIMEOUT, |s| s.audio_devices.is_some()).unwrap();
    assert_eq!(q.state.audio_devices.as_deref(), Some(list.as_slice()));
    // 測試用的假清單：之後 mpv 送來的不蓋掉它（先放假清單再開始觀察：mpv 一定會送一次目前的清單過來）
    let fake = sound::parse_devices(r#"[{"name":"auto"},{"name":"coreaudio/x","description":"X"}]"#);
    q.set_fake_audio_devices(fake.clone());
    assert_eq!(q.read_audio_devices(), Some(fake.as_slice()));
    let mut r = player_with(&[]);
    r.set_fake_audio_devices(fake.clone());
    r.watch_audio_devices();
    let _ = r.wait_state(Duration::from_millis(500), |_| false);
    assert_eq!(r.state.audio_devices.as_deref(), Some(fake.as_slice()));
}

#[test]
fn spdif_probe() {
    // ao=null 也接受音訊直通（ao_null 不檢查是不是 PCM）：audio-out-params 的格式是 spdif-ac3
    let mut p = player_with(&[]);
    // 系統的 libmpv 0.40 以前，播放中改 audio-spdif 要到下一個檔案才生效（CI 的 Ubuntu 是 0.37）
    let live = p.probe_caps().spdif_live;
    let file = sample("common/mkv_hevc_ac3.mkv");
    let a = AudioSettings {
        passthrough: Passthrough {
            enabled: true,
            ..Passthrough::default()
        },
        ..AudioSettings::default()
    };
    let opts = sound::mpv_options(&a, AUTO_DEVICE, &HashSet::new());
    p.apply_sound(&opts, true);
    assert_eq!(p.get_string("audio-spdif").unwrap(), "ac3,eac3,dts");
    p.open(&file).unwrap();
    p.wait_state(TIMEOUT, |s| s.loaded && s.audio_spdif.as_deref() == Some("ac3"))
        .unwrap_or_else(|e| panic!("{e}：{:?}", p.state));
    let out: serde_json::Value = serde_json::from_str(&p.get_string("audio-out-params").unwrap()).unwrap();
    assert!(out["format"].as_str().unwrap().starts_with("spdif-"), "{out}");
    let codec = p
        .state
        .selected(TrackKind::Audio)
        .and_then(|t| t.codec.clone())
        .unwrap();
    assert!(sound::predict_spdif(&codec, &a.passthrough), "{codec}");
    // 播放中關掉：mpv 重新開解碼器，改回一般的 PCM（舊的引擎重新開檔才生效）
    let mut off = a.clone();
    off.passthrough.enabled = false;
    let sent = p.apply_sound(&sound::mpv_options(&off, AUTO_DEVICE, &HashSet::new()), false);
    assert_eq!(sent.len(), 1);
    assert_eq!(reply(&mut p), (Some(AsyncKey::Spdif), None));
    if !live {
        eprintln!("這個播放引擎播放中改 audio-spdif 不會馬上生效：重新開檔");
        p.open(&file).unwrap();
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    }
    p.wait_state(TIMEOUT, |s| s.loaded && s.audio_spdif.is_none())
        .unwrap_or_else(|e| panic!("關掉直通：{e}：{:?}", p.state));
    // 再打開
    p.apply_sound(&opts, false);
    assert_eq!(reply(&mut p), (Some(AsyncKey::Spdif), None));
    if !live {
        p.open(&file).unwrap();
    }
    p.wait_state(TIMEOUT, |s| s.loaded && s.audio_spdif.as_deref() == Some("ac3"))
        .unwrap_or_else(|e| panic!("再打開直通：{e}：{:?}", p.state));
}

#[test]
fn predict_spdif_matches_the_engine() {
    // 每一種直通格式的樣本：mpv 回報的音軌 codec 跟 predict_spdif 的對照、實際有沒有直通
    let all = Passthrough {
        enabled: true,
        ac3: true,
        eac3: true,
        dts: true,
        dts_hd: true,
        truehd: true,
    };
    let defaults = Passthrough {
        enabled: true,
        ..Passthrough::default()
    };
    for (file, format) in [
        ("common/mkv_hevc_ac3.mkv", "ac3"),
        ("general/mkv_h264_eac3.mkv", "eac3"),
        ("general/mkv_h264_dts.mkv", "dts"),
        ("rare/mkv_h264_truehd.mkv", "truehd"),
    ] {
        let Some(path) = sample_if_exists(file) else {
            eprintln!("沒有樣本 {file}，略過");
            continue;
        };
        for pt in [all, defaults] {
            let mut p = player_with(&[("audio-spdif", &sound::spdif_value(&pt))]);
            p.open(&path).unwrap();
            p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
                .unwrap_or_else(|e| panic!("{file}：{e}"));
            let codec = p
                .state
                .selected(TrackKind::Audio)
                .and_then(|t| t.codec.clone())
                .unwrap();
            let predicted = sound::predict_spdif(&codec, &pt);
            if predicted {
                p.wait_state(TIMEOUT, |s| s.audio_spdif.is_some())
                    .unwrap_or_else(|e| panic!("{file}（{codec}）預測會直通：{e}"));
                assert_eq!(p.state.audio_spdif.as_deref(), Some(format), "{file}");
            } else {
                let _ = p.wait_state(Duration::from_millis(500), |_| false);
                assert_eq!(p.state.audio_spdif, None, "{file}（{codec}）預測不會直通");
            }
            // TrueHD 預設不勾，其他三種預設會直通
            assert_eq!(predicted, pt == all || format != "truehd", "{file} {pt:?}");
        }
    }
}

// ───────────── 等化器、音量平衡、音量放大（影戲自己的 af 濾鏡鏈） ─────────────

/// 六個濾鏡都有的引擎才跑（本專案建置的引擎、CI 的系統 libmpv 都有；舊的引擎略過）
fn filters_or_skip(p: &mut Player, test: &str) -> Option<AfCaps> {
    let caps = p.probe_caps().af;
    if caps.all() {
        Some(caps)
    } else {
        eprintln!("{test}：播放引擎缺少音訊濾鏡（{caps:?}），略過");
        None
    }
}

/// 測試用的暫存資料夾（名稱各測試不同，平行跑也不會撞到）
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vitascope-sound-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const RATE: u32 = 48_000;

/// 寫一個 32 位元浮點的 WAV：取樣率 `rate`、`channels` 聲道（`mask` = WAVE_FORMAT_EXTENSIBLE 的聲道配置，
/// 例如 5.1 是 0x3F）、`frames` 個取樣，`value(取樣編號, 聲道)` 給值。
/// 測試的聲音都在這裡產生：本專案建置的引擎沒有 FFmpeg 的 sine 來源濾鏡（av://lavfi:sine 開不起來）
fn write_wav(path: &str, rate: u32, channels: u16, mask: u32, frames: usize, value: impl Fn(usize, usize) -> f32) {
    let data_len = frames * usize::from(channels) * 4;
    let mut b: Vec<u8> = Vec::with_capacity(data_len + 80);
    let block = channels * 4;
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((data_len + 60) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&0xFFFEu16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    b.extend_from_slice(&block.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&22u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&mask.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    b.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
    b.extend_from_slice(b"fact");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&(frames as u32).to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        for ch in 0..usize::from(channels) {
            b.extend_from_slice(&value(i, ch).to_le_bytes());
        }
    }
    std::fs::write(path, b).unwrap();
}

/// 單聲道的正弦波（`amp` 是峰值；`amp_at(秒)` 可以讓振幅隨時間變），取樣率 `rate`
fn tone_with(path: &str, rate: u32, freq: f64, seconds: f64, amp_at: impl Fn(f64) -> f64) {
    let frames = (f64::from(rate) * seconds) as usize;
    write_wav(path, rate, 1, 0x4, frames, |i, _| {
        let t = i as f64 / f64::from(rate);
        (amp_at(t) * (std::f64::consts::TAU * freq * t).sin()) as f32
    });
}

/// 48 kHz 單聲道的正弦波，振幅固定
fn tone(path: &str, freq: f64, amp: f64, seconds: f64) {
    tone_with(path, RATE, freq, seconds, |_| amp);
}

/// 讀進來的 WAV：每個聲道的取樣（−1…1）
struct Wav {
    rate: u32,
    channels: Vec<Vec<f32>>,
}

impl Wav {
    fn read(path: &str) -> Wav {
        let b = std::fs::read(path).unwrap_or_else(|e| panic!("讀不到 {path}：{e}"));
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let (mut format, mut channels, mut rate, mut bits) = (0u16, 0usize, 0u32, 0u16);
        let mut pos = 12;
        while pos + 8 <= b.len() {
            let id = &b[pos..pos + 4];
            let len = u32_at(pos + 4) as usize;
            let body = pos + 8;
            if id == b"fmt " {
                format = u16_at(body);
                channels = usize::from(u16_at(body + 2));
                rate = u32_at(body + 4);
                bits = u16_at(body + 14);
                if format == 0xFFFE {
                    format = u16_at(body + 24);
                }
            } else if id == b"data" {
                // mpv 播完才回填長度；沒回填（0x7ffff000）時讀到檔尾
                let end = body.saturating_add(len).min(b.len());
                let bytes = usize::from(bits / 8);
                let frame = bytes * channels;
                let mut out = vec![Vec::new(); channels];
                for f in b[body..end].chunks_exact(frame) {
                    for (ch, s) in f.chunks_exact(bytes).enumerate() {
                        let v = match (format, bits) {
                            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                            (1, 16) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                            other => panic!("不認得的 WAV 格式 {other:?}"),
                        };
                        out[ch].push(v);
                    }
                }
                return Wav { rate, channels: out };
            }
            pos = body + len + (len & 1);
        }
        panic!("{path} 沒有 data");
    }

    fn seconds(&self) -> f64 {
        self.channels[0].len() as f64 / f64::from(self.rate)
    }
}

/// Goertzel：`x` 裡 `freq` Hz 的振幅（峰值）
fn goertzel(x: &[f32], rate: u32, freq: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / f64::from(rate);
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for v in x {
        let s0 = f64::from(*v) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power.max(0.0).sqrt() / x.len() as f64
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn peak(x: &[f32]) -> f64 {
    x.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max)
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// 中間那一段（去掉頭尾：濾鏡剛開始、淡出的部分不算）
fn middle(x: &[f32], from: f64, to: f64) -> &[f32] {
    &x[(x.len() as f64 * from) as usize..(x.len() as f64 * to) as usize]
}

/// 產生的 WAV 一律指定用 wav 讀：正弦波的資料剛好每 192 位元組重複時，FFmpeg 會把它誤認成 MPEG-TS（M2TS）
const FORCE_WAV: (&str, &str) = ("demuxer-lavf-format", "wav");

/// 用 ao=pcm 把 `input` 經過 `af` 播一次（不出聲音，播多快就多快），回傳輸出的 WAV。`extra`：其他 mpv 選項
fn render(dir: &TempDir, name: &str, input: &str, af: &str, extra: &[(&str, &str)]) -> Wav {
    let out = dir.file(&format!("{name}.wav"));
    let mut opts: Vec<(&str, &str)> = vec![("ao", "pcm"), ("ao-pcm-file", &out), ("vid", "no"), FORCE_WAV];
    opts.extend_from_slice(extra);
    let mut p = player_with(&opts);
    p.mpv().set_property("af", af).unwrap();
    p.open(input).unwrap();
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. })) {
        Ok(PlayerEvent::EndFile { error: None, .. }) => {}
        other => panic!("{name}：{other:?}"),
    }
    assert!(
        !p.recent_errors().iter().any(|e| e.contains("Disabling filter")),
        "{name}：有濾鏡被停用 {:?}",
        p.recent_errors()
    );
    // 關掉播放器：mpv 寫完、回填 WAV 的長度
    drop(p);
    Wav::read(&out)
}

/// af-command（同步）；回傳錯誤
fn af_command(p: &Player, args: &[String; 5]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    p.mpv().command(&args).map_err(|e| e.to_string())
}

/// 濾鏡鏈的各種組合：等化器、每種音量平衡、音量放大
fn chain_variants() -> Vec<(&'static str, AudioSettings)> {
    let eq = Equalizer {
        enabled: true,
        preset: EqPreset::Rock,
        ..Equalizer::default()
    };
    let mut v = vec![
        (
            "等化器",
            AudioSettings {
                eq,
                ..AudioSettings::default()
            },
        ),
        (
            "只有放大",
            AudioSettings {
                volume_max: 200,
                ..AudioSettings::default()
            },
        ),
    ];
    for (name, leveling) in [
        ("夜間模式", Leveling::Night),
        ("人聲平衡", Leveling::Dialogue),
        ("音量平均", Leveling::Normalize),
    ] {
        v.push((
            name,
            AudioSettings {
                leveling,
                ..AudioSettings::default()
            },
        ));
    }
    v.push((
        "全部",
        AudioSettings {
            eq,
            leveling: Leveling::Night,
            volume_max: 200,
            ..AudioSettings::default()
        },
    ));
    v
}

/// 播放中：等開始播放、濾鏡收到第一批聲音（af-command 要等濾鏡建好才會成功）
fn wait_filters(p: &mut Player, probe: &[String; 5], what: &str) {
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart)
        .unwrap_or_else(|e| panic!("{what}：{e}"));
    let start = std::time::Instant::now();
    while let Err(e) = af_command(p, probe) {
        assert!(start.elapsed() < TIMEOUT, "{what}：af-command 一直失敗：{e}");
        let _ = p.wait_state(Duration::from_millis(20), |_| false);
    }
}

#[test]
fn eq_chain_and_af_command() {
    let mut p = player_with(&[]);
    let Some(caps) = filters_or_skip(&mut p, "eq_chain_and_af_command") else {
        return;
    };
    drop(p);
    let dir = TempDir::new("eq-chain");
    let file = sample("common/mp4_h264_aac.mp4");
    let mut runs: Vec<(String, AudioSettings, String)> = chain_variants()
        .into_iter()
        .map(|(name, a)| (name.to_owned(), a, file.clone()))
        .collect();
    let variants = runs.len();
    // 低取樣率：16 kHz 那一段在 Nyquist 上（32 kHz）或超過（22.05 kHz），前面的 aformat 先換取樣率
    for rate in [32000, 22050] {
        let a = chain_variants().remove(0).1;
        let wav = dir.file(&format!("sine_{rate}.wav"));
        tone_with(&wav, rate, 440.0, 5.0, |_| 0.5);
        runs.push((format!("等化器 {rate} Hz"), a, wav));
    }
    for (i, (name, a, input)) in runs.into_iter().enumerate() {
        let chain = af_chain(&a, &caps, false, boost_level(f64::from(a.volume_max)));
        let mut p = player_with(if i < variants { &[] } else { &[FORCE_WAV] });
        p.mpv()
            .set_property("af", chain.as_str())
            .unwrap_or_else(|e| panic!("{name}：mpv 不接受 af={chain}：{e}"));
        // mpv 讀回來的寫法不一樣（lavfi=graph=%長度%…），比對有哪幾段
        let back = p.get_string("af").unwrap();
        for label in [sound::EQ_LABEL, sound::LEVEL_LABEL, sound::LIMIT_LABEL] {
            assert_eq!(
                sound::has_stage(&back, label),
                sound::has_stage(&chain, label),
                "{name}：{back}"
            );
        }
        p.open(&input).unwrap();
        let probe = limit_command(1.5);
        wait_filters(&mut p, &probe, &name);
        // 每一段的即時調整
        let mut commands = vec![limit_command(sound::limiter_level(&a, &caps, 2.0))];
        if a.eq.enabled {
            commands.extend((0..10).map(|i| band_command(i, -3.5)));
        }
        commands.extend(
            match a.leveling {
                Leveling::Night => Some(["ratio", "2", "acompressor@c"]),
                Leveling::Dialogue => Some(["e", "4", "speechnorm@s"]),
                Leveling::Normalize => Some(["g", "15", "dynaudnorm@d"]),
                Leveling::Off => None,
            }
            .map(|[param, value, target]| ["af-command", sound::LEVEL_LABEL, param, value, target].map(str::to_owned)),
        );
        for c in &commands {
            af_command(&p, c).unwrap_or_else(|e| panic!("{name}：{c:?} 失敗：{e}"));
        }
        // 濾鏡失敗時 mpv 會停用它並記一筆錯誤（Disabling filter …），播放照樣繼續
        let _ = p.wait_state(Duration::from_millis(400), |_| false);
        assert!(
            !p.recent_errors().iter().any(|e| e.contains("Disabling filter")),
            "{name}：有濾鏡被停用 {:?}",
            p.recent_errors()
        );
        assert!(p.state.loaded, "{name}：播放中斷 {:?}", p.recent_errors());
        // 改了別的段落之後，af 字串裡沒有的段落 af-command 會失敗（app 依這個判斷要不要直接改寫字串）
        if !a.eq.enabled {
            assert!(af_command(&p, &band_command(0, 1.0)).is_err(), "{name}");
        }
    }
}

/// 等化器（自訂，只有 `band` 那一段 `db`）；不自動防止破音（前級 1，量得到等化器本身的增益）
fn eq_only_band(band: usize, db: f32) -> AudioSettings {
    let mut gains = [0.0; 10];
    gains[band] = db;
    AudioSettings {
        eq: Equalizer {
            enabled: true,
            preset: EqPreset::Custom,
            gains,
            auto_preamp: false,
        },
        ..AudioSettings::default()
    }
}

#[test]
fn audio_dsp() {
    // ao=pcm 把濾鏡鏈的輸出寫成 WAV，量測實際的效果（測試用的聲音由這裡產生）
    let mut p = player_with(&[]);
    let Some(caps) = filters_or_skip(&mut p, "audio_dsp") else {
        return;
    };
    drop(p);
    let dir = TempDir::new("dsp");

    // 1. 等化器 1 kHz 那一段（b6）+12 dB：1 kHz 的音量大 12 ± 1.5 dB（−30 dBFS 的正弦波，限幅器不會動作）
    let quiet = dir.file("tone_1k_-30.wav");
    tone(&quiet, 1000.0, 10f64.powf(-30.0 / 20.0), 2.0);
    let flat = render(&dir, "eq_flat", &quiet, "", &[]);
    let eq = af_chain(&eq_only_band(5, 12.0), &caps, false, 1.0);
    let boosted = render(&dir, "eq_b6", &quiet, &eq, &[]);
    let level = |w: &Wav| goertzel(middle(&w.channels[0], 0.2, 0.8), w.rate, 1000.0);
    let gain = db(level(&boosted) / level(&flat));
    eprintln!("等化器 b6 +12 dB：1 kHz 實測 {gain:+.2} dB");
    assert!((gain - 12.0).abs() <= 1.5, "等化器 +12 dB 實測 {gain:+.2} dB");
    // 別的段落（31 Hz）+12 dB 幾乎不影響 1 kHz
    let low_chain = af_chain(&eq_only_band(0, 12.0), &caps, false, 1.0);
    let low = render(&dir, "eq_b1", &quiet, &low_chain, &[]);
    let leak = db(level(&low) / level(&flat));
    eprintln!("等化器 b1 +12 dB：1 kHz 實測 {leak:+.2} dB");
    assert!(leak.abs() < 1.0, "31 Hz 那一段影響到 1 kHz：{leak:+.2} dB");
    // 自動防止破音：最高 +12 dB 時整體先降 12 dB，1 kHz 回到原本的大小
    let mut auto = eq_only_band(5, 12.0);
    auto.eq.auto_preamp = true;
    let pre = render(&dir, "eq_b6_preamp", &quiet, &af_chain(&auto, &caps, false, 1.0), &[]);
    let net = db(level(&pre) / level(&flat));
    eprintln!("等化器 b6 +12 dB、自動防止破音：1 kHz 實測 {net:+.2} dB");
    assert!(net.abs() <= 1.5, "前級沒有作用：{net:+.2} dB");

    // 2. 音量 200%（放大 8 倍）：−3 dBFS 的正弦波經過限幅器，峰值不超過 0.98，RMS 至少大 2 dB
    let loud = dir.file("tone_1k_-3.wav");
    tone(&loud, 1000.0, 10f64.powf(-3.0 / 20.0), 2.0);
    let plain = render(&dir, "boost_off", &loud, "", &[]);
    let max200 = AudioSettings {
        volume_max: 200,
        ..AudioSettings::default()
    };
    let chain = af_chain(&max200, &caps, false, boost_level(200.0));
    let louder = render(&dir, "boost_200", &loud, &chain, &[]);
    let pk = peak(&louder.channels[0]);
    let up = db(rms(middle(&louder.channels[0], 0.2, 0.8)) / rms(middle(&plain.channels[0], 0.2, 0.8)));
    eprintln!("音量 200%：峰值 {pk:.4}，RMS {up:+.2} dB");
    assert!(pk <= 0.98 + 1e-4, "限幅器沒擋住：峰值 {pk}");
    assert!(up >= 2.0, "放大不夠：RMS {up:+.2} dB");

    // 3. 夜間模式：大聲的段落變小、小聲的段落變大（大聲與小聲段落的音量差變小），峰值也不超過 0.98
    let bursts = dir.file("bursts.wav");
    // 0.5 秒大聲（0.9）、1.5 秒小聲（0.02）輪流
    tone_with(&bursts, RATE, 1000.0, 8.0, |t| if t % 2.0 < 0.5 { 0.9 } else { 0.02 });
    let range = |w: &Wav| {
        let x = &w.channels[0];
        let rate = f64::from(w.rate);
        let seg = |from: f64, to: f64| rms(&x[(from * rate) as usize..(to * rate) as usize]);
        // 第三輪（濾鏡已經穩定）：大聲的後半段、小聲的中間
        db(seg(4.25, 4.45) / seg(5.5, 6.0))
    };
    let dry = render(&dir, "night_off", &bursts, "", &[]);
    let night = AudioSettings {
        leveling: Leveling::Night,
        ..AudioSettings::default()
    };
    let wet = render(&dir, "night_on", &bursts, &af_chain(&night, &caps, false, 1.0), &[]);
    let (before, after) = (range(&dry), range(&wet));
    let wet_peak = peak(&wet.channels[0]);
    eprintln!("夜間模式：大聲與小聲段落差 {before:.1} dB → {after:.1} dB，峰值 {wet_peak:.3}");
    assert!(
        after < before - 6.0,
        "夜間模式沒有縮小音量差：{before:.1} → {after:.1} dB"
    );
    assert!(wet_peak <= 0.98 + 1e-4, "峰值 {wet_peak}");

    // 4. 轉成立體聲（audio-channels=stereo）：5.1 的中央聲道平均分到左右
    let surround = dir.file("center_5.1.wav");
    let frames = RATE as usize * 2;
    // 5.1（FL FR FC LFE BL BR）：只有中央聲道有 1 kHz
    write_wav(&surround, RATE, 6, 0x3F, frames, |i, ch| {
        if ch == 2 {
            (0.3 * (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(RATE)).sin()) as f32
        } else {
            0.0
        }
    });
    let as_is = render(&dir, "surround", &surround, "", &[]);
    assert_eq!(as_is.channels.len(), 6, "沒轉的話照樣 6 聲道");
    let stereo = render(&dir, "downmix", &surround, "", &[("audio-channels", "stereo")]);
    assert_eq!(stereo.channels.len(), 2);
    let l = goertzel(middle(&stereo.channels[0], 0.2, 0.8), RATE, 1000.0);
    let r = goertzel(middle(&stereo.channels[1], 0.2, 0.8), RATE, 1000.0);
    eprintln!("5.1 → 立體聲：中央聲道 0.3 → 左 {l:.3}、右 {r:.3}");
    assert!(l > 0.1 && r > 0.1, "中央聲道沒有混進左右：{l} {r}");
    assert!(db(l / r).abs() < 0.5, "左右不平均：{l} {r}");
}

/// 暫停著開 `input`、等濾鏡建好、送 `command`（af-command）；`rewrite` 有值時接著非同步改寫 af；
/// 然後跳到 5 秒、播完。回傳 ao=pcm 寫出的 WAV
fn seek_after_command(
    dir: &TempDir,
    name: &str,
    input: &str,
    af: &str,
    command: &[String; 5],
    rewrite: Option<&str>,
) -> Wav {
    let out = dir.file(&format!("{name}.wav"));
    let mut p = player_with(&[("ao", "pcm"), ("ao-pcm-file", &out), ("vid", "no"), FORCE_WAV]);
    p.mpv().set_property("af", af).unwrap();
    // 暫停著開檔：ao=pcm 還不寫出聲音，濾鏡照樣收到第一批聲音（af-command 才會成功）
    p.set_pause(true).unwrap();
    p.open(input).unwrap();
    wait_filters(&mut p, command, name);
    if let Some(chain) = rewrite {
        let id = p.set_async(AsyncKey::Af, "af", chain).unwrap();
        match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
            Ok(PlayerEvent::CommandReply { id: got, error }) => assert_eq!((got, error), (id, None)),
            other => panic!("{other:?}"),
        }
    }
    p.seek_to(5.0, true).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    p.set_pause(false).unwrap();
    p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
        .unwrap();
    drop(p);
    Wav::read(&out)
}

#[test]
fn af_command_gain_survives_seek() {
    // af-command 只是即時的回饋：跳轉時 mpv 用 af 字串重建濾鏡，即時改的增益就沒了。
    // 所以 app 在調整停下來之後改寫整條 af 字串（mpv 只重建改到的那一段），跳轉之後增益還在
    let mut p = player_with(&[]);
    let Some(caps) = filters_or_skip(&mut p, "af_command_gain_survives_seek") else {
        return;
    };
    drop(p);
    let dir = TempDir::new("seek-survival");
    let input = dir.file("tone.wav");
    tone(&input, 1000.0, 10f64.powf(-30.0 / 20.0), 10.0);
    let flat = af_chain(&eq_only_band(5, 0.0), &caps, false, 1.0);
    let plus12 = af_chain(&eq_only_band(5, 12.0), &caps, false, 1.0);
    let reference = render(&dir, "reference", &input, "", &[]);
    let level = |w: &Wav| goertzel(middle(&w.channels[0], 0.3, 0.9), w.rate, 1000.0);
    let kept = seek_after_command(&dir, "rewritten", &input, &flat, &band_command(5, 12.0), Some(&plus12));
    // 跳到 5 秒之後才開始寫：輸出大約 5 秒
    assert!(kept.seconds() < 7.0, "沒有跳轉：輸出 {:.1} 秒", kept.seconds());
    let gain = db(level(&kept) / level(&reference));
    eprintln!("af-command +12 dB、改寫字串、跳轉之後：{gain:+.2} dB");
    assert!((gain - 12.0).abs() <= 1.5, "跳轉之後增益不見了：{gain:+.2} dB");
    // 只送 af-command、不改寫：跳轉之後增益不見（引擎的行為；這就是要改寫字串的原因）
    let lost = seek_after_command(&dir, "command_only", &input, &flat, &band_command(5, 12.0), None);
    let gain = db(level(&lost) / level(&reference));
    eprintln!("只有 af-command、跳轉之後：{gain:+.2} dB");
    assert!(
        gain.abs() <= 1.5,
        "預期跳轉後 af-command 的增益被字串蓋掉：{gain:+.2} dB"
    );
    // 音量放大（限幅器的 level_in）也一樣
    let max200 = AudioSettings {
        volume_max: 200,
        ..AudioSettings::default()
    };
    let unity = af_chain(&max200, &caps, false, 1.0);
    let doubled = af_chain(&max200, &caps, false, 2.0);
    let kept = seek_after_command(
        &dir,
        "boost_rewritten",
        &input,
        &unity,
        &limit_command(2.0),
        Some(&doubled),
    );
    let gain = db(level(&kept) / level(&reference));
    eprintln!("level_in ×2、改寫字串、跳轉之後：{gain:+.2} dB");
    assert!((gain - 6.02).abs() <= 1.0, "跳轉之後放大不見了：{gain:+.2} dB");
}

#[test]
fn volume_total_boost() {
    let mut p = player_with(&[]);
    let Some(caps) = filters_or_skip(&mut p, "volume_total_boost") else {
        return;
    };
    let a = AudioSettings {
        volume_max: 200,
        ..AudioSettings::default()
    };
    // 先調到 100% 以下（之後等 mpv 的音量變回 100 才有意義）
    p.set_volume_total(50.0, 200.0, true).unwrap();
    p.wait_state(TIMEOUT, |s| s.volume == 50.0).unwrap();
    assert_eq!((p.volume_total(), p.boost_pct()), (50.0, 0.0));
    // 有限幅器：mpv 的音量停在 100，超過的部分（boost_pct）由 app 放進限幅器的 level_in
    //（app 實際送出的 af 在 tests/ui.rs 的 volume_up_past_100_when_max_raised 檢查）
    p.set_volume_total(150.0, 200.0, true).unwrap();
    p.wait_state(TIMEOUT, |s| s.volume == 100.0).unwrap();
    assert_eq!(p.get_f64("volume").unwrap(), 100.0);
    assert_eq!(p.volume_total(), 150.0);
    assert_eq!(p.boost_pct(), 50.0);
    let chain = af_chain(&a, &caps, false, boost_level(p.volume_total()));
    p.mpv().set_property("af", chain.as_str()).unwrap();
    // 播放中即時調整 level_in：濾鏡鏈裡的限幅器接受
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    wait_filters(&mut p, &limit_command(boost_level(170.0)), "放大");
    // 回到 100% 以下：直接是 mpv 的音量
    p.set_volume_total(80.0, 200.0, true).unwrap();
    p.wait_state(TIMEOUT, |s| s.volume == 80.0).unwrap();
    assert_eq!((p.volume_total(), p.boost_pct()), (80.0, 0.0));
    // 超過上限的拉回上限；mpv 的音量照樣是 100
    p.set_volume_total(500.0, 200.0, true).unwrap();
    p.wait_state(TIMEOUT, |s| s.volume == 100.0).unwrap();
    assert_eq!(p.get_f64("volume").unwrap(), 100.0);
    assert_eq!((p.volume_total(), p.boost_pct()), (200.0, 100.0));
    // 沒有限幅器：用 mpv 自己的音量（把 volume-max 提高到上限）
    p.set_volume_total(150.0, 200.0, false).unwrap();
    p.wait_state(TIMEOUT, |s| s.volume == 150.0).unwrap();
    assert_eq!((p.volume_total(), p.boost_pct()), (150.0, 0.0));
    assert_eq!(p.get_f64("volume-max").unwrap(), 200.0);
    assert!(!p.recent_errors().iter().any(|e| e.contains("Disabling filter")));
}

fn disabled_filters(p: &Player) -> Vec<String> {
    p.recent_errors()
        .iter()
        .filter(|e| e.contains("Disabling filter"))
        .cloned()
        .collect()
}

/// 送一個非同步指令、等它的回覆（不能失敗）
fn async_ok(p: &mut Player, args: &[&str]) {
    let id = p.command_async_keyed(AsyncKey::Af, args).unwrap();
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
        Ok(PlayerEvent::CommandReply { id: got, error }) => assert_eq!((got, error), (id, None), "{args:?}"),
        other => panic!("{args:?}：{other:?}"),
    }
}

#[test]
fn spdif_clears_and_revives_the_chain() {
    // 音訊直通的資料不能過濾鏡：af 裡有等化器的話，直通一開始濾鏡就失敗、被 mpv 停用（記一筆錯誤）。
    // app 在直通開始之前先把 af 清空；關掉直通之後送「af ""」再送整條 af（兩個非同步指令照順序執行）
    // 重播同一個檔案：樣本只有 3 秒，後面幾步（重新開音訊、等濾鏡失敗）在 CI 上可能比它還久
    let mut p = player_with(&[("audio-spdif", "ac3"), ("loop-file", "inf")]);
    let Some(caps) = filters_or_skip(&mut p, "spdif_clears_and_revives_the_chain") else {
        return;
    };
    let live = p.probe_caps().spdif_live;
    let a = AudioSettings {
        eq: Equalizer {
            enabled: true,
            preset: EqPreset::Rock,
            ..Equalizer::default()
        },
        ..AudioSettings::default()
    };
    let chain = af_chain(&a, &caps, false, 1.0);
    assert_eq!(af_chain(&a, &caps, true, 1.0), "", "直通中是空的");
    let file = sample("common/mkv_hevc_ac3.mkv");
    // 1. 直通之前先清空：濾鏡不會失敗
    p.mpv().set_property("af", "").unwrap();
    p.open(&file).unwrap();
    p.wait_state(TIMEOUT, |s| s.audio_spdif.as_deref() == Some("ac3"))
        .unwrap_or_else(|e| panic!("{e}：{:?}", p.state));
    let _ = p.wait_state(Duration::from_millis(300), |_| false);
    assert_eq!(disabled_filters(&p), Vec::<String>::new());
    // 2. 關掉直通，然後 af "" + 整條：等化器重新有作用
    async_ok(&mut p, &["set", "audio-spdif", ""]);
    async_ok(&mut p, &["set", "af", ""]);
    async_ok(&mut p, &["set", "af", &chain]);
    if !live {
        // 系統的 libmpv 0.40 以前：播放中改 audio-spdif 不會馬上生效，重新開檔
        p.open(&file).unwrap();
    }
    p.wait_state(TIMEOUT, |s| s.loaded && s.audio_spdif.is_none())
        .unwrap_or_else(|e| panic!("關掉直通：{e}：{:?}", p.state));
    wait_filters_running(&mut p, &band_command(0, 2.0), "關掉直通之後");
    let _ = p.wait_state(Duration::from_millis(300), |_| false);
    assert_eq!(disabled_filters(&p), Vec::<String>::new());
    if !live {
        return;
    }
    // 3. 直通時 af 沒清空（預測漏掉的情況，例如 mpv 自己換了音軌）：濾鏡碰到直通的資料就失敗、被停用。
    //    關掉直通之後同樣送 af "" + 整條，濾鏡重新建立（mpv 0.41 起才能在播放中切換直通）。
    //    註：這個引擎重新開音訊時會清掉濾鏡的「已失敗」（mp_output_chain_reset_harder），設回同樣的字串其實也會恢復；
    //    先清空再設不依賴這一點
    async_ok(&mut p, &["set", "audio-spdif", "ac3"]);
    p.wait_state(TIMEOUT, |s| s.audio_spdif.as_deref() == Some("ac3"))
        .unwrap_or_else(|e| panic!("再打開直通：{e}"));
    let start = std::time::Instant::now();
    while disabled_filters(&p).is_empty() {
        assert!(start.elapsed() < TIMEOUT, "預期等化器碰到直通的資料會失敗");
        let _ = p.wait_state(Duration::from_millis(20), |_| false);
    }
    // 等化器、限幅器各自失敗（各記一筆）：等都記下來再記住內容。之後比對內容（不是筆數）：
    // 錯誤只留最近 8 筆，新的失敗擠掉舊的時筆數可能一樣
    let _ = p.wait_state(Duration::from_millis(300), |_| false);
    let failed = disabled_filters(&p);
    async_ok(&mut p, &["set", "audio-spdif", ""]);
    async_ok(&mut p, &["set", "af", ""]);
    async_ok(&mut p, &["set", "af", &chain]);
    p.wait_state(TIMEOUT, |s| s.audio_spdif.is_none()).unwrap();
    wait_filters_running(&mut p, &band_command(0, 2.0), "先清空再設");
    let _ = p.wait_state(Duration::from_millis(300), |_| false);
    assert_eq!(disabled_filters(&p), failed, "沒有再失敗");
}

/// 播放中（不等 PlaybackRestart）：af-command 成功為止
fn wait_filters_running(p: &mut Player, probe: &[String; 5], what: &str) {
    let start = std::time::Instant::now();
    while let Err(e) = af_command(p, probe) {
        assert!(start.elapsed() < TIMEOUT, "{what}：af-command 一直失敗：{e}");
        let _ = p.wait_state(Duration::from_millis(20), |_| false);
    }
}
