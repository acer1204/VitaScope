//! 畫質選項（去交錯、去色帶、銳化、縮放演算法、HDR 色調映射）對應到 mpv：介面上每個值 mpv 都接受、
//! 預設設定跟 mpv 原本的值一樣、套用時只送有變的、去交錯「自動」只處理交錯的影片。
//! headless（不出畫面、不出聲音），三個平台的 CI 都跑。
//!
//! 本專案建置的播放引擎要全部接受；Linux tar.gz 用的系統 libmpv（CI 是 Ubuntu 的 0.37）沒有
//! deinterlace=auto、deinterlace-active，這兩項略過。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use vitascope::picture::{
    self, ChromaScaler, Deinterlace, Downscaler, Gamut, MANAGED, PictureDefaults, Quality, Strength, ToneCurve,
    ToneSettings, Upscaler, VideoSettings,
};
use vitascope::player::{AsyncKey, EngineCaps, Options, Player, PlayerEvent, async_key};

const TIMEOUT: Duration = Duration::from_secs(15);

/// 樣本的完整路徑；沒有產生出來時 None（有些 FFmpeg 建置產生不了交錯的 MPEG-2）
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

/// 本專案建置的播放引擎（有 components.json；系統的 libmpv 沒有）
fn vendored_engine() -> bool {
    option_env!("VITASCOPE_LIBMPV_MANIFEST").is_some()
}

/// 含 L3 元件的播放引擎（components.json 列有 libxml2；跟 tests/engine_build.rs 的條件一樣）
fn l3_engine() -> bool {
    let Some(path) = option_env!("VITASCOPE_LIBMPV_MANIFEST") else {
        return false;
    };
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("讀不到 {path}：{e}"));
    let m: serde_json::Value = serde_json::from_str(&text).unwrap();
    m["components"]
        .as_array()
        .is_some_and(|c| c.iter().any(|c| c["name"] == "libxml2"))
}

/// 介面上每個選項會送給 mpv 的值（`picture::mpv_options` 的所有可能）
fn every_ui_value() -> Vec<(&'static str, String)> {
    let mut all: Vec<(&'static str, String)> = Vec::new();
    let mut add = |name: &'static str, value: &str| all.push((name, value.to_owned()));
    for d in Deinterlace::ALL {
        add("deinterlace", d.mpv());
    }
    for s in Strength::ALL {
        add("sharpen", s.sharpen());
        add("deband", if s.deband().is_some() { "yes" } else { "no" });
        let d = s.deband().unwrap_or(picture::Deband::MPV_DEFAULT);
        add("deband-iterations", &d.iterations.to_string());
        add("deband-threshold", &d.threshold.to_string());
        add("deband-range", &d.range.to_string());
        add("deband-grain", &d.grain.to_string());
    }
    for s in Upscaler::ALL {
        add("scale", s.mpv());
    }
    for s in Downscaler::ALL {
        add("dscale", s.mpv());
    }
    for s in ChromaScaler::ALL {
        add("cscale", s.mpv());
    }
    // 三種畫質用到的值（「標準」= 引擎的預設值，空的 cscale = 跟放大一樣）
    let defaults = PictureDefaults::default();
    for q in Quality::ALL {
        let v = VideoSettings {
            quality: q,
            ..VideoSettings::default()
        };
        let caps = EngineCaps {
            deint_auto: true,
            ..EngineCaps::default()
        };
        for (name, value) in picture::mpv_options(&v, &caps, &defaults, &HashSet::new()) {
            if ["scale", "dscale", "cscale", "scale-antiring"].contains(&name) {
                add(name, &value);
            }
        }
    }
    for c in ToneCurve::ALL {
        add("tone-mapping", c.mpv());
    }
    for g in Gamut::ALL {
        add("gamut-mapping-mode", g.mpv());
    }
    add("target-peak", "auto");
    for p in ToneSettings::PEAK_PRESETS
        .into_iter()
        .chain([ToneSettings::MIN_PEAK, ToneSettings::MAX_PEAK, 650])
    {
        add("target-peak", &p.to_string());
    }
    add("hdr-compute-peak", "auto");
    add("hdr-compute-peak", "no");
    all
}

#[test]
fn every_ui_value_is_accepted_by_mpv() {
    let p = player_with(&[]);
    let values = every_ui_value();
    // 每個管理的選項都測到了
    for name in MANAGED {
        assert!(values.iter().any(|(k, _)| *k == name), "{name} 沒有測到");
    }
    let strict = vendored_engine();
    let mut tolerated = Vec::new();
    for (name, value) in &values {
        let result = p.mpv().command(&["set", name, value]);
        match result {
            Ok(()) => {}
            // 系統的 libmpv（0.37）：deinterlace 只有 yes / no，播放器偵測到後把「自動」當成關閉
            Err(_) if *name == "deinterlace" && value == "auto" && !l3_engine() => {
                tolerated.push(format!("{name}={value}"));
            }
            // 系統的 libmpv 沒有的選項（較舊的 mpv）
            Err(_) if !strict && p.get_string(&format!("option-info/{name}/name")).is_err() => {
                tolerated.push(format!("{name}（沒有這個選項）"));
            }
            Err(e) => panic!("mpv 不接受 {name}={value:?}：{e}"),
        }
    }
    if !tolerated.is_empty() {
        eprintln!("這個播放引擎不支援（已略過）：{tolerated:?}");
    }
    if l3_engine() {
        assert!(tolerated.is_empty(), "{tolerated:?}");
    }
    // 不是介面上的值：mpv 要拒絕（確認上面真的有檢查）
    assert!(p.mpv().command(&["set", "scale", "no_such_scaler"]).is_err());
    assert!(p.mpv().command(&["set", "tone-mapping", "st2094-99"]).is_err());
}

#[test]
fn default_settings_match_engine_defaults() {
    // 預設的畫質設定套用後，mpv 的值跟它自己的預設值一樣（只有去交錯是「自動」）
    let mut p = player_with(&[]);
    let caps = p.probe_caps();
    let defaults = p.picture_defaults();
    let opts = picture::mpv_options(&VideoSettings::default(), &caps, &defaults, &HashSet::new());
    let sent = p.apply_picture(&opts, true);
    for (name, _, result) in &sent {
        assert!(result.is_ok(), "{name}：{result:?}");
    }
    assert_eq!(sent.len(), MANAGED.len(), "第一次全部都送");
    for name in MANAGED {
        let value = p.get_string(name).unwrap();
        if name == "deinterlace" {
            let expected = if caps.deint_auto { "auto" } else { "no" };
            assert_eq!(value, expected);
            continue;
        }
        let default = p.get_string(&format!("option-info/{name}/default-value")).unwrap();
        assert_eq!(value, default, "{name}");
    }
}

#[test]
fn apply_picture_sends_only_what_changed() {
    let mut p = player_with(&[("scale", "bilinear")]);
    let caps = p.probe_caps();
    let defaults = p.picture_defaults();
    let overrides = p.user_overrides().clone();
    let opts = |v: &VideoSettings| picture::mpv_options(v, &caps, &defaults, &overrides);
    let mut v = VideoSettings::default();
    let first = p.apply_picture(&opts(&v), true);
    assert_eq!(first.len(), MANAGED.len() - 1, "使用者指定的 scale 不送");
    assert!(p.apply_picture(&opts(&v), true).is_empty(), "沒有變就不送");
    // 去色帶開到中等：參數就是 mpv 的預設值，只有 deband 本身變了
    v.deband = Strength::Medium;
    let sent = p.apply_picture(&opts(&v), false);
    let names: Vec<(&str, AsyncKey)> = sent.iter().map(|(n, k, _)| (*n, *k)).collect();
    assert_eq!(names, [("deband", AsyncKey::Deband)]);
    let Ok(Some(id)) = sent[0].2 else {
        panic!("非同步送出要有指令編號：{sent:?}")
    };
    assert_eq!(async_key(id), Some(AsyncKey::Deband));
    // 強：iterations、threshold、grain 變了（range 一樣）
    v.deband = Strength::Strong;
    let names: Vec<&str> = p.apply_picture(&opts(&v), false).iter().map(|(n, _, _)| *n).collect();
    assert_eq!(names, ["deband-iterations", "deband-threshold", "deband-grain"]);
    // 高品質：使用者指定的 scale 不動，其他的照送
    v.quality = Quality::High;
    let sent = p.apply_picture(&opts(&v), false);
    let names: Vec<(&str, AsyncKey)> = sent.iter().map(|(n, k, _)| (*n, *k)).collect();
    assert_eq!(names, [("scale-antiring", AsyncKey::Scaler)]);
    v.tone.curve = ToneCurve::Hable;
    v.deinterlace = Deinterlace::On;
    let names: Vec<(&str, AsyncKey)> = p
        .apply_picture(&opts(&v), false)
        .iter()
        .map(|(n, k, _)| (*n, *k))
        .collect();
    assert_eq!(
        names,
        [("deinterlace", AsyncKey::Deinterlace), ("tone-mapping", AsyncKey::Tone)]
    );
    // 非同步指令照順序執行：等最後一個的回覆，前面的都生效了
    let mut replies = 0;
    while replies < 7 {
        match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
            Ok(PlayerEvent::CommandReply { error, .. }) => {
                assert_eq!(error, None);
                replies += 1;
            }
            other => panic!("等不到回覆：{other:?}"),
        }
    }
    for (name, value) in [
        ("deband", "yes"),
        ("deband-iterations", "2"),
        ("scale", "bilinear"),
        ("scale-antiring", "0.600000"),
        ("tone-mapping", "hable"),
        ("deinterlace", "yes"),
    ] {
        assert_eq!(p.get_string(name).unwrap(), value, "{name}");
    }
    // mpv 拒絕的值：回覆是錯誤；忘掉記下的值之後下次會再送
    let bogus = [("tone-mapping", "no-such-curve".to_owned())];
    let sent = p.apply_picture(&bogus, false);
    assert_eq!(sent.len(), 1);
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
        Ok(PlayerEvent::CommandReply { id, error }) => {
            assert_eq!(async_key(id), Some(AsyncKey::Tone));
            assert!(error.is_some());
        }
        other => panic!("等不到回覆：{other:?}"),
    }
    assert!(p.apply_picture(&bogus, false).is_empty(), "還記著（還沒忘掉）");
    p.forget_picture("tone-mapping");
    // 忘掉之後同一個值也會再送（使用者再選一次同樣的值時要真的送給 mpv）
    let sent = p.apply_picture(&bogus, false);
    assert_eq!(sent.len(), 1, "忘掉之後要再送：{sent:?}");
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
        Ok(PlayerEvent::CommandReply { id, error }) => {
            assert_eq!(async_key(id), Some(AsyncKey::Tone));
            assert!(error.is_some());
        }
        other => panic!("等不到回覆：{other:?}"),
    }
    p.forget_picture("tone-mapping");
    let names: Vec<&str> = p.apply_picture(&opts(&v), false).iter().map(|(n, _, _)| *n).collect();
    assert_eq!(names, ["tone-mapping"], "忘掉之後再送一次正確的值");
}

#[test]
fn options_set_by_a_profile_are_left_alone() {
    // VITASCOPE_MPV_OPTS="profile=high-quality"：設定檔間接改到的畫質選項也算使用者指定的，啟動時不能蓋掉
    let names = player_with(&[]).get_string("profile-list").unwrap_or_default();
    let Some(profile) = ["high-quality", "gpu-hq"]
        .into_iter()
        .find(|p| names.contains(&format!("\"name\":\"{p}\"")))
    else {
        panic!("引擎沒有 high-quality / gpu-hq 設定檔：{names}");
    };
    let mut p = player_with(&[("profile", profile)]);
    let changed: Vec<(&str, String)> = MANAGED
        .into_iter()
        .filter_map(|name| {
            let now = p.get_string(name).ok()?;
            let default = p.get_string(&format!("option-info/{name}/default-value")).ok()?;
            (now != default).then_some((name, now))
        })
        .collect();
    assert!(
        changed.iter().any(|(name, _)| *name == "scale"),
        "{profile} 設定檔應該改了放大的演算法：{changed:?}"
    );
    for (name, _) in &changed {
        assert!(p.user_overrides().contains(*name), "{name} 由設定檔指定");
    }
    assert!(!p.user_overrides().contains("deinterlace"), "沒改到的照常管理");
    let caps = p.probe_caps();
    let defaults = p.picture_defaults();
    let overrides = p.user_overrides().clone();
    let opts = picture::mpv_options(&VideoSettings::default(), &caps, &defaults, &overrides);
    p.apply_picture(&opts, true);
    for (name, value) in &changed {
        assert_eq!(&p.get_string(name).unwrap(), value, "{name} 不能被啟動時的設定蓋掉");
    }
    if caps.deint_auto {
        assert_eq!(p.get_string("deinterlace").unwrap(), "auto");
    }
}

#[test]
fn deinterlace_auto_engages_on_interlaced() {
    let mut p = player_with(&[]);
    let caps = p.probe_caps();
    if !caps.deint_auto || !caps.deint_status {
        assert!(
            !l3_engine(),
            "含 L3 元件的引擎要有 deinterlace=auto 與 deinterlace-active：{caps:?}"
        );
        eprintln!("略過：這個播放引擎沒有 deinterlace=auto 或 deinterlace-active（{caps:?}）");
        return;
    }
    let Some(interlaced) = sample_if_exists("general/ts_mpeg2_interlaced.ts") else {
        eprintln!("略過：沒有交錯的樣本（這個 FFmpeg 產生不了 general/ts_mpeg2_interlaced.ts）");
        return;
    };
    let defaults = p.picture_defaults();
    let opts = picture::mpv_options(&VideoSettings::default(), &caps, &defaults, &HashSet::new());
    p.apply_picture(&opts, true);
    assert_eq!(p.get_string("deinterlace").unwrap(), "auto");
    p.open(&interlaced).unwrap();
    p.wait_state(TIMEOUT, |s| s.loaded && s.deinterlace_active)
        .unwrap_or_else(|e| panic!("交錯的影片要去交錯：{e}：{:?}", p.state));
    // 逐行的影片：自動不去交錯
    p.open(&sample("common/mp4_h264_aac.mp4")).unwrap();
    // 先等這個檔案的 StartFile：上一個檔案的 PlaybackRestart 可能還在佇列裡
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::StartFile).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    p.wait_state(TIMEOUT, |s| {
        s.loaded && s.path.as_deref().is_some_and(|path| path.ends_with("mp4_h264_aac.mp4"))
    })
    .unwrap();
    // deinterlace-active 是非同步送達的：多等一下再確認它沒有變成 true
    let _ = p.wait_state(Duration::from_millis(500), |s| s.deinterlace_active);
    assert!(!p.state.deinterlace_active, "逐行的影片不去交錯");
    assert_eq!(p.get_string("deinterlace").unwrap(), "auto", "設定不會被換檔還原");
}

#[test]
fn video_hdr_follows_the_file() {
    let mut p = player_with(&[]);
    for (rel, hdr) in [
        ("general/mkv_hevc10_hdr10.mkv", true),
        ("common/mp4_h264_aac.mp4", false),
        ("general/mkv_hevc10_hlg.mkv", true),
    ] {
        p.open(&sample(rel)).unwrap();
        // 先等這個檔案的 StartFile：上一個檔案的事件可能還在佇列裡
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::StartFile).unwrap();
        p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
        p.wait_state(TIMEOUT, |s| s.loaded && s.video_hdr == hdr)
            .unwrap_or_else(|e| panic!("{rel} 的 HDR 應該是 {hdr}：{e}"));
    }
    p.stop().unwrap();
    p.wait_state(TIMEOUT, |s| !s.loaded && !s.video_hdr)
        .unwrap_or_else(|e| panic!("關檔後歸零：{e}"));
}
