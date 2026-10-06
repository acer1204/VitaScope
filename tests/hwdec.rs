//! 硬體解碼測試（ROADMAP 4.1 第 3 點）
//!
//! 需要 GPU，CI 主機沒有，所以預設不跑：
//!   cargo test --test hwdec -- --ignored --nocapture
//!
//! 用 `hwdec=auto-copy` 播放「常見」的主要編碼，讀 `hwdec-current` 確認真的走了 GPU；
//! Hi10P 沒有 GPU 支援，要確認會自動退回軟解、而且照樣能播。

use std::path::PathBuf;
use std::time::Duration;
use vitascope::player::{Options, Player, PlayerEvent};

/// (樣本, 是否預期硬解)
const CASES: &[(&str, bool)] = &[
    ("common/mp4_h264_aac.mp4", true),
    ("common/mp4_hevc_aac.mp4", true),
    ("common/mp4_hevc10_aac.mp4", true),
    ("common/mp4_hevc10_4k.mp4", true),
    ("common/webm_vp9_opus.webm", true),
    ("common/webm_vp9p2_opus.webm", true),
    ("common/mp4_av1_aac.mp4", true),
    ("common/mkv_h264hi10p_aac.mkv", false),
];

fn decoder_used(rel: &str) -> Result<String, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    let mut p = Player::new(Options {
        hwdec: "auto-copy".into(),
        ..Options::headless()
    })
    .map_err(|e| e.to_string())?;
    p.open(path.to_str().unwrap()).map_err(|e| e.to_string())?;
    p.wait_for(Duration::from_secs(15), |e| *e == PlayerEvent::PlaybackRestart)?;
    // hwdec-current 在解碼器初始化後才更新；軟解時是 "no"
    p.wait_state(Duration::from_secs(5), |s| {
        s.hwdec.as_deref().is_some_and(|h| !h.is_empty())
    })?;
    // 播一小段，確認解碼持續正常（硬解失敗時 mpv 會中途退回軟解）
    std::thread::sleep(Duration::from_millis(500));
    p.poll();
    Ok(p.state.hwdec.clone().unwrap_or_default())
}

#[test]
#[ignore = "需要 GPU：cargo test --test hwdec -- --ignored"]
fn common_codecs_use_gpu() {
    let mut failures = Vec::new();
    println!();
    for (rel, expect_hw) in CASES {
        match decoder_used(rel) {
            Ok(hw) => {
                let is_hw = hw != "no";
                let ok = is_hw == *expect_hw;
                let label = if is_hw { format!("硬解 {hw}") } else { "軟解".into() };
                println!("{} {rel:<34} {label}", if ok { "✔" } else { "✘" });
                if !ok {
                    failures.push(*rel);
                }
            }
            Err(e) => {
                println!("✘ {rel:<34} 播放失敗：{e}");
                failures.push(*rel);
            }
        }
    }
    assert!(failures.is_empty(), "硬解結果不符預期：{failures:?}");
}
