//! 連結 libmpv。
//!
//! - Windows：使用 vendor/libmpv/windows-x64/（pwsh scripts/fetch-libmpv.ps1 下載），
//!   並把 libmpv-2.dll 放到執行檔旁邊，`cargo run` 和 `cargo test` 才找得到。
//! - macOS（Apple Silicon）：vendor/libmpv/macos-arm64/（bash scripts/fetch-libmpv.sh 下載）；Intel Mac 用 Homebrew 的 mpv。
//!   `VITASCOPE_LIBMPV=system` 改用 Homebrew 的 mpv（只供本機實驗；發佈版一定用 vendor 的）。
//! - Linux：系統套件（`libmpv-dev` / `mpv-libs-devel`），tar.gz 用的就是這個。
//!   `VITASCOPE_LIBMPV=vendor` 改用 vendor/libmpv/linux-x64/（AppImage 內含的那一份，bash scripts/fetch-libmpv.sh 下載）。
//!
//! vendor/ 的 libmpv 都是本專案從原始碼建置的（.github/workflows/libmpv-*.yml）。用它們時，
//! `VITASCOPE_LIBMPV_MANIFEST` 指向它的 components.json（tests/engine_build.rs 用來核對版本）。

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VITASCOPE_LIBMPV");
    let choice = env::var("VITASCOPE_LIBMPV").unwrap_or_default();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    match target_os.as_str() {
        "windows" => windows(),
        // 本專案只建置 Apple Silicon 版的 libmpv；Intel Mac 用 Homebrew 的 mpv
        "macos" if choice == "system" || target_arch != "aarch64" => homebrew(),
        "macos" => vendored("macos-arm64", "libmpv.2.dylib", "libmpv.dylib"),
        "linux" if choice == "vendor" => vendored("linux-x64", "lib/libmpv.so.2", "libmpv.so"),
        _ => {} // Linux：libmpv 在系統預設的函式庫路徑
    }
}

/// Homebrew 安裝的 mpv（只供本機實驗）
fn homebrew() {
    for prefix in ["/opt/homebrew/lib", "/usr/local/lib"] {
        if Path::new(prefix).join("libmpv.dylib").exists() {
            println!("cargo:rustc-link-search=native={prefix}");
            // /opt/homebrew/lib 不在 dyld 的預設搜尋路徑，開發版執行檔要記住位置
            println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}");
        }
    }
}

/// 本專案建置的 libmpv（macOS、Linux AppImage 用的那一份）：放到 OUT_DIR，以 -lmpv 連結，執行時從 rpath 找。
/// 打包時再換成 .app / AppImage 裡的位置
fn vendored(platform: &str, file: &str, link_name: &str) {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("vendor/libmpv").join(platform);
    println!("cargo:rerun-if-changed={}", vendor.display());
    let lib = vendor.join(file);
    if !lib.exists() {
        panic!("找不到 {}\n請先執行：bash scripts/fetch-libmpv.sh", lib.display());
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    place(&lib, &out_dir.join(lib.file_name().unwrap())); // 執行時 dyld / ld.so 找的名字（install name / SONAME）
    place(&lib, &out_dir.join(link_name)); // -lmpv 找的名字
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", out_dir.display());
    println!(
        "cargo:rustc-env=VITASCOPE_LIBMPV_MANIFEST={}",
        vendor.join("components.json").display()
    );
}

fn windows() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("vendor/libmpv/windows-x64");
    println!("cargo:rerun-if-changed={}", vendor.display());

    let dll = vendor.join("libmpv-2.dll");
    let components = vendor.join("components.json");
    // 沒有 components.json 的是之前用的別人建置的 DLL（含與 GPL-3.0 不相容的元件），不能再用
    if !dll.exists() || !components.exists() {
        panic!(
            "{} 裡沒有本專案建置的 libmpv（還沒下載，或是舊版）\n請執行：pwsh scripts/fetch-libmpv.ps1",
            vendor.display()
        );
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
    println!("cargo:rustc-env=VITASCOPE_LIBMPV_MANIFEST={}", components.display());
    resources(&manifest, &out_dir);
}

/// 執行檔的圖示與版本資訊（packaging/windows/vitascope.rc）。用 MinGW 的 windres 編譯；
/// 找不到 windres 時只發出警告（開發用的建置照樣能跑，只是執行檔沒有圖示），發佈流程會另外檢查
fn resources(manifest: &Path, out_dir: &Path) {
    let rc_dir = manifest.join("packaging/windows");
    let ico = manifest.join("packaging/icons/vitascope.ico");
    println!("cargo:rerun-if-changed={}", rc_dir.join("vitascope.rc").display());
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-env-changed=RC");
    // 複製到 OUT_DIR（純 ASCII 路徑）再編譯：MinGW 的工具打不開非 ASCII 路徑
    let copied = std::fs::copy(rc_dir.join("vitascope.rc"), out_dir.join("vitascope.rc"))
        .and_then(|_| std::fs::copy(&ico, out_dir.join("vitascope.ico")));
    if let Err(e) = copied {
        println!("cargo:warning=無法準備圖示資源：{e}");
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let obj = out_dir.join("vitascope-res.o");
    let status = std::process::Command::new(env::var("RC").unwrap_or_else(|_| "windres".into()))
        .current_dir(out_dir)
        .args(["--target", "pe-x86-64", "-c", "65001", "-O", "coff"])
        .arg(format!("-DVER_MAJOR={}", parts[0]))
        .arg(format!("-DVER_MINOR={}", parts.get(1).unwrap_or(&"0")))
        .arg(format!("-DVER_PATCH={}", parts.get(2).unwrap_or(&"0")))
        .arg(format!("-DVER_STR=\\\"{version}\\\""))
        .args(["-i", "vitascope.rc", "-o"])
        .arg(&obj)
        .status();
    match status {
        // 直接把目的檔交給連結器（放進 .a 的話，沒有符號被引用會被丟掉）
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        Ok(s) => println!("cargo:warning=windres 失敗（{s}），執行檔不會有圖示"),
        Err(e) => println!("cargo:warning=找不到 windres（{e}），執行檔不會有圖示"),
    }
}

/// 優先建立硬連結（libmpv 有幾十 MB，複製太慢），跨磁碟時才複製。
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
