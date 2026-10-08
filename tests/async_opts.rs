//! 非同步設定 mpv 選項、引擎功能偵測、L3 加的播放狀態（headless：不出畫面、不出聲音，三個平台的 CI 都跑）

use std::path::PathBuf;
use std::time::Duration;
use vitascope::player::{AsyncKey, Options, Player, PlayerEvent, async_key};

const TIMEOUT: Duration = Duration::from_secs(15);

fn sample(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

fn player_with(extra: &[(&str, &str)]) -> Player {
    Player::new(Options {
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗")
}

/// 下一個非同步指令的回覆（編號, 錯誤）
fn next_reply(p: &mut Player) -> (u64, Option<String>) {
    match p.wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::CommandReply { .. })) {
        Ok(PlayerEvent::CommandReply { id, error }) => (id, error),
        other => panic!("等不到非同步指令的回覆：{other:?}"),
    }
}

/// mpv 的路徑清單（glsl-shaders）讀成字串時的分隔字元。0.37 的 glsl-shaders 還是一般的字串清單，
/// 用逗號分隔（測試的路徑裡沒有逗號）
const LIST_SEP: [char; 2] = [if cfg!(windows) { ';' } else { ':' }, ','];

fn shader_list(p: &Player) -> Vec<String> {
    let s = p.get_string("glsl-shaders").unwrap();
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(LIST_SEP).map(str::to_owned).collect()
    }
}

/// 檔名有中文和空白的著色器路徑（只用在清單裡，檔案不用存在）
fn shader_paths(tag: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join("影戲 著色器 測試");
    (0..3)
        .map(|i| {
            dir.join(format!("{tag} 第 {i} 個 放大.glsl"))
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// 本專案建置、含 L3 元件的播放引擎（components.json 列有 libxml2；跟 tests/engine_build.rs 的條件一樣）
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

#[test]
fn async_set_errors_are_routed() {
    let mut p = player_with(&[]);
    let bad = p.set_async(AsyncKey::Deband, "deband", "bogus").unwrap();
    let good = p.set_async(AsyncKey::Deband, "deband", "yes").unwrap();
    assert_ne!(bad, good);
    let (id, error) = next_reply(&mut p);
    assert_eq!(id, bad);
    assert_eq!(async_key(id), Some(AsyncKey::Deband));
    assert!(error.is_some(), "不合法的值要回報錯誤");
    let (id, error) = next_reply(&mut p);
    assert_eq!(id, good);
    assert_eq!(async_key(id), Some(AsyncKey::Deband));
    assert_eq!(error, None);
    assert_eq!(p.get_string("deband").unwrap(), "yes");
    // 其他種類、指令形式也一樣
    let id = p
        .command_async_keyed(AsyncKey::Af, &["af", "add", "@vs-test:no_such_filter_here"])
        .unwrap();
    let (got, error) = next_reply(&mut p);
    assert_eq!((got, async_key(got)), (id, Some(AsyncKey::Af)));
    assert!(error.is_some());
}

#[test]
fn async_requests_keep_fifo_order() {
    let mut p = player_with(&[]);
    let paths = shader_paths("fifo");
    let ids: Vec<u64> = paths
        .iter()
        .map(|path| {
            p.command_async_keyed(AsyncKey::Shaders, &["change-list", "glsl-shaders", "append", path])
                .unwrap()
        })
        .collect();
    // 回覆照送出的順序
    for want in &ids {
        let (id, error) = next_reply(&mut p);
        assert_eq!(id, *want);
        assert_eq!(async_key(id), Some(AsyncKey::Shaders));
        assert_eq!(error, None);
    }
    assert_eq!(shader_list(&p), paths, "非同步指令要照送出的順序執行");
}

/// 同步呼叫可能插隊到還沒執行的非同步指令前面（mpv 的同步呼叫直接鎖住核心，非同步的是排隊）。
/// 插在哪裡不一定，這裡只確認：同步的那一下落在非同步指令之間的某個位置，
/// 非同步指令彼此之間的順序不會亂
#[test]
fn sync_call_after_async_is_not_reordered_wrongly() {
    let mut p = player_with(&[]);
    let paths = shader_paths("sync");
    let sync_path = std::env::temp_dir()
        .join("影戲 著色器 測試")
        .join("同步 設定.glsl")
        .to_string_lossy()
        .into_owned();
    let mut overtaken = [0usize; 4];
    for _ in 0..10 {
        // 上一輪留下的清單不用清：同步設定會整個換掉
        for path in &paths {
            p.command_async_keyed(AsyncKey::Shaders, &["change-list", "glsl-shaders", "append", path])
                .unwrap();
        }
        // 設成只有一個檔案（不能設空字串：mpv 會把它當成一個空的項目）
        p.mpv().set_property("glsl-shaders", sync_path.as_str()).unwrap();
        for _ in &paths {
            assert_eq!(next_reply(&mut p).1, None);
        }
        let list = shader_list(&p);
        assert_eq!(list.first(), Some(&sync_path), "同步設定一定在：{list:?}");
        // 同步設定之後的，必須是非同步指令的最後幾個、而且順序不變
        assert!(paths.ends_with(&list[1..]), "非同步指令的順序亂了：{list:?}");
        overtaken[list.len() - 1] += 1;
    }
    // 記下實際情況（插隊到 0…3 個非同步指令前面的次數）
    eprintln!("同步設定插隊到 [0, 1, 2, 3] 個非同步指令前面的次數：{overtaken:?}");
}

#[test]
fn apply_sync_skips_user_overrides() {
    let p = player_with(&[("deband", "yes")]);
    assert!(p.user_overrides().contains("deband"));
    let errors = p.apply_sync(&[
        ("deband", "no".to_owned()),
        ("sharpen", "0.5".to_owned()),
        ("deinterlace", "bogus".to_owned()),
    ]);
    assert_eq!(p.get_string("deband").unwrap(), "yes", "使用者指定的選項不能被蓋掉");
    assert_eq!(p.get_f64("sharpen").unwrap(), 0.5);
    let failed: Vec<&str> = errors.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(failed, ["deinterlace"]);
}

#[test]
fn probe_caps_on_vendored_engine() {
    let mut p = player_with(&[]);
    let caps = p.probe_caps();
    eprintln!("{caps:?}");
    // 偵測完 af、deinterlace 都恢復原狀
    assert_eq!(p.get_string("af").unwrap(), "");
    assert_eq!(p.get_string("deinterlace").unwrap(), "no");
    assert!(!caps.dumb, "headless 沒有開軟體繪圖的簡化流程");
    assert_eq!(caps.macos, cfg!(target_os = "macos"));
    if l3_engine() {
        assert!(caps.af.all(), "含 L3 元件的引擎六個音訊濾鏡都要有：{:?}", caps.af);
        assert!(caps.deint_auto, "含 L3 元件的引擎有 deinterlace=auto");
    } else {
        eprintln!("不是含 L3 元件的播放引擎：只確認偵測不會出錯");
    }
    let d = p.picture_defaults();
    assert!(!d.scale.is_empty() && !d.dscale.is_empty(), "{d:?}");
    if l3_engine() {
        assert_eq!(
            (d.scale.as_str(), d.dscale.as_str(), d.cscale.as_str()),
            ("lanczos", "hermite", "")
        );
    }
    // 偵測失敗的記錄（mpv 記成錯誤）不能變成開檔失敗時給使用者看的原因
    assert!(!p.probe_af("vitascope_no_such_filter"));
    assert_eq!(p.get_string("af").unwrap(), "", "偵測失敗也不能在 af 留下東西");
    let _ = p.wait_state(Duration::from_millis(500), |_| false);
    assert!(
        !p.recent_errors()
            .iter()
            .any(|e| e.contains("vitascope_no_such_filter") || e.contains("deinterlace")),
        "偵測的記錄跑進 recent_errors：{:?}",
        p.recent_errors()
    );
}

#[test]
fn probe_keeps_the_users_own_af() {
    let user_af = "@mine:lavfi=[volume=0.5]";
    let mut p = player_with(&[("af", user_af)]);
    let before = p.get_string("af").unwrap();
    assert!(before.contains("@mine"), "{before}");
    p.probe_caps();
    assert_eq!(p.get_string("af").unwrap(), before);
    // 使用者的濾鏡剛好用了偵測的標籤：「af remove @vs-probe」會連它一起移除，要整個設回去
    let user_af = "@vs-probe:lavfi=[volume=0.5]";
    let mut p = player_with(&[("af", user_af)]);
    let before = p.get_string("af").unwrap();
    assert!(before.contains("volume"), "{before}");
    p.probe_caps();
    assert_eq!(p.get_string("af").unwrap(), before);
}

#[test]
fn observed_state_tracks_spdif_and_deinterlace() {
    // ao=null 也接受音訊直通，所以 headless 也看得到 spdif-ac3
    let mut p = player_with(&[("audio-spdif", "ac3"), ("deinterlace", "yes")]);
    assert!(p.user_overrides().contains("audio-spdif") && p.user_overrides().contains("deinterlace"));
    assert!(p.state.audio_spdif.is_none() && !p.state.deinterlace_active);
    // deinterlace-active 是較新的 mpv 才有的屬性（CI 用的 Ubuntu 系統 libmpv 0.37 沒有）：沒有就只看直通
    let has_deint_active = p
        .get_string("property-list")
        .unwrap()
        .split(',')
        .any(|name| name == "deinterlace-active");
    if !has_deint_active {
        eprintln!("這個播放引擎沒有 deinterlace-active：略過去交錯的部分");
    }
    p.open(&sample("common/mkv_hevc_ac3.mkv")).unwrap();
    p.wait_state(TIMEOUT, |s| {
        s.loaded && s.audio_spdif.as_deref() == Some("ac3") && (s.deinterlace_active || !has_deint_active)
    })
    .unwrap_or_else(|e| panic!("{e}：{:?}", p.state));
    p.stop().unwrap();
    p.wait_state(TIMEOUT, |s| {
        !s.loaded && s.audio_spdif.is_none() && !s.deinterlace_active
    })
    .unwrap_or_else(|e| panic!("關檔後要歸零：{e}：{:?}", p.state));
    // 一般的 PCM 輸出不是直通
    let mut p = player_with(&[]);
    p.open(&sample("common/mkv_hevc_ac3.mkv")).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    let _ = p.wait_state(Duration::from_millis(300), |_| false);
    assert_eq!(p.state.audio_spdif, None);
    assert!(!p.state.deinterlace_active, "預設不去交錯（mpv 的 deinterlace=no）");
}

#[test]
fn observed_state_tracks_display_sync() {
    // vo-null-fps 讓 vo=null 假裝有 60 Hz 的螢幕
    let mut p = player_with(&[("vo-null-fps", "60"), ("video-sync", "display-resample")]);
    p.open("av://lavfi:testsrc2=size=160x90:rate=30:duration=8").unwrap();
    p.wait_state(TIMEOUT, |s| s.loaded && s.display_sync_active)
        .unwrap_or_else(|e| panic!("{e}：{:?}", p.state));
    p.stop().unwrap();
    p.wait_state(TIMEOUT, |s| !s.loaded && !s.display_sync_active)
        .unwrap_or_else(|e| panic!("關檔後要歸零：{e}：{:?}", p.state));
    // 預設（video-sync=audio）不會同步螢幕
    let mut p = player_with(&[]);
    p.open("av://lavfi:testsrc2=size=160x90:rate=30:duration=8").unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    let _ = p.wait_state(Duration::from_millis(500), |_| false);
    assert!(!p.state.display_sync_active);
}
