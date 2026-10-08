//! 音效選項（輸出裝置、獨佔模式、轉成立體聲、音訊直通）對應到 mpv：介面上每個值 mpv 都接受、
//! 預設設定跟 mpv 原本的值一樣、套用時只送有變的、音訊直通真的會開始（ao=null 也接受 spdif）。
//! headless（不出畫面、不出聲音），三個平台的 CI 都跑；CI 的電腦沒有音訊裝置，不依賴真的裝置。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use vitascope::player::{AsyncKey, Options, Player, PlayerEvent, TrackKind, async_key};
use vitascope::sound::{self, AUTO_DEVICE, AudioSettings, MANAGED, Passthrough, SPDIF_CODECS};

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
    std::fs::write(&conf, "audio-channels=stereo\naudio-spdif=ac3\n").unwrap();
    let p = player_with(&[("include", conf.to_str().unwrap())]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(p.get_string("audio-channels").unwrap(), "stereo");
    for name in ["audio-channels", "audio-spdif"] {
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
