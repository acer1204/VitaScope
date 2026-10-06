//! 連結 libmpv。
//!
//! - Windows：使用 vendor/libmpv/windows-x64/（scripts/fetch-libmpv.ps1 下載），
//!   並把 libmpv-2.dll 放到執行檔旁邊，`cargo run` 和 `cargo test` 才找得到。
//! - macOS：Homebrew 安裝的 mpv（`brew install mpv`）。
//! - Linux：系統套件（`libmpv-dev` / `mpv-libs-devel`）。

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "windows" => windows(),
        "macos" => {
            for prefix in ["/opt/homebrew/lib", "/usr/local/lib"] {
                if Path::new(prefix).join("libmpv.dylib").exists() {
                    println!("cargo:rustc-link-search=native={prefix}");
                    // /opt/homebrew/lib 不在 dyld 的預設搜尋路徑，開發版執行檔要記住位置
                    println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}");
                }
            }
        }
        _ => {} // Linux：libmpv 在系統預設的函式庫路徑
    }
}

fn windows() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("vendor/libmpv/windows-x64");
    println!("cargo:rerun-if-changed={}", vendor.display());

    let dll = vendor.join("libmpv-2.dll");
    if !dll.exists() {
        panic!("找不到 {}\n請先執行：pwsh scripts/fetch-libmpv.ps1", dll.display());
    }
    // 連結和執行都從 OUT_DIR 取用：
    // - mingw 的 ld 打不開非 ASCII 路徑（專案在「桌面」底下），OUT_DIR 在純 ASCII 的 target 目錄
    // - OUT_DIR 在 target 目錄內，cargo run / cargo test 會把它加進 PATH，執行時找得到 dll
    // - target/<profile>/ 讓直接雙擊 exe 也能執行
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    // windows-gnu 找 libmpv.dll.a；MSVC 的 link.exe 找 mpv.lib，而它看得懂 GNU 格式的匯入函式庫
    let import_lib = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        "mpv.lib"
    } else {
        "libmpv.dll.a"
    };
    place(&vendor.join("libmpv.dll.a"), &out_dir.join(import_lib));
    place(&dll, &out_dir.join("libmpv-2.dll"));
    if let Some(profile_dir) = out_dir.ancestors().nth(3) {
        place(&dll, &profile_dir.join("libmpv-2.dll"));
    }
}

/// 優先建立硬連結（dll 約 120 MB，複製太慢），跨磁碟時才複製。
fn place(src: &Path, dst: &Path) {
    if let (Ok(a), Ok(b)) = (src.metadata(), dst.metadata())
        && a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
    {
        return;
    }
    let _ = std::fs::remove_file(dst);
    if std::fs::hard_link(src, dst).is_err() {
        std::fs::copy(src, dst).unwrap_or_else(|e| panic!("無法複製 {} → {}: {e}", src.display(), dst.display()));
    }
}
