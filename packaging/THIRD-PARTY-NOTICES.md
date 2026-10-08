# 第三方元件與授權聲明

影戲 VitaScope 本身以 **GPL-3.0-or-later** 授權（全文見 `LICENSE`）。
安裝包內含以下第三方元件，各自依其授權散布。

## 播放引擎：mpv 與 FFmpeg

Windows、macOS 與 Linux AppImage 內含的播放引擎（libmpv）由本專案從原始碼建置：mpv 以 `-Dgpl=false`、
FFmpeg 不加 `--enable-gpl` / `--enable-version3` 建置，每個元件固定版本並核對雜湊，連同相依函式庫靜態連結成一個函式庫，
整體依 **LGPL-2.1-or-later** 散布。建置流程在本儲存庫的 `.github/workflows/libmpv-*.yml`，
建置結果發佈在 prerelease `libmpv-<平台>-rN`（不是影戲的版本）。

| 平台 | 內含方式 | 元件清單與授權條文 |
|---|---|---|
| Windows | 程式資料夾的 `libmpv-2.dll`（prerelease `libmpv-win64-rN`，確切的 rN 見清單檔） | 同資料夾的 `THIRD-PARTY-WINDOWS.md`、`licenses/` |
| macOS | `VitaScope.app/Contents/Frameworks/libmpv.2.dylib`（prerelease `libmpv-macos-arm64-rN`；只依賴 macOS 內建的函式庫） | `VitaScope.app/Contents/Resources/THIRD-PARTY-MACOS.md`、`Contents/Resources/licenses/` |
| Linux（tar.gz） | 不內含，使用系統安裝的 libmpv | 各發行版的套件 |
| Linux（AppImage） | `usr/lib/libmpv.so.2`（prerelease `libmpv-linux-x64-rN`）；另有 winit / glutin 執行時載入的 Ubuntu 24.04 視窗函式庫（libxkbcommon、libxkbcommon-x11、libxcb-xkb、libXcursor、libXi、libXext、libXfixes、libXrender、libwayland-cursor、libwayland-egl）、系統沒有 PulseAudio / libva 時用的替身函式庫，以及檔案開頭的 AppImage 執行環境 | AppImage 的 `usr/share/doc/vitascope/THIRD-PARTY-LINUX.md`、`licenses/`；Ubuntu 套件的 copyright 檔在 `usr/share/doc/<套件>/`（參照到的授權全文如果有，在 `usr/share/common-licenses/`） |

主要元件的授權：

| 元件 | 授權 | 原始碼 |
|---|---|---|
| mpv | LGPL-2.1-or-later（以 `-Dgpl=false` 建置） | https://github.com/mpv-player/mpv |
| FFmpeg | LGPL-2.1-or-later（不含 GPL 選項）；編譯進去的檔案裡有些另外帶 MIT、BSD 等寬鬆授權聲明，原文在 `licenses/ffmpeg/permissive/`（索引見其中的 `README.txt`） | https://ffmpeg.org/download.html |
| libplacebo | LGPL-2.1-or-later | https://code.videolan.org/videolan/libplacebo |
| libass | ISC | https://github.com/libass/libass |
| FreeType | FreeType License（FTL） | https://freetype.org |
| HarfBuzz | MIT-Modern-Variant AND MIT | https://github.com/harfbuzz/harfbuzz |
| FriBidi | LGPL-2.1-or-later | https://github.com/fribidi/fribidi |
| libunibreak | Zlib | https://github.com/adah1972/libunibreak |
| dav1d | BSD-2-Clause AND ISC | https://code.videolan.org/videolan/dav1d |
| zimg | WTFPL | https://github.com/sekrit-twc/zimg |
| zlib | Zlib | https://zlib.net |
| libxml2（FFmpeg 的 DASH 分離器用） | MIT AND ISC-Veillard | https://gitlab.gnome.org/GNOME/libxml2 |
| nv-codec-headers（Windows、Linux） | MIT | https://github.com/FFmpeg/nv-codec-headers |

各元件的確切版本、下載位置與雜湊列在各平台的清單檔。

可以把內含的 libmpv 換成自己修改、重新建置的版本（相同的 libmpv C API）：

- Windows：替換程式資料夾（安裝版在 `%LOCALAPPDATA%\Programs\VitaScope`）裡的 `libmpv-2.dll`。
- macOS：替換 `VitaScope.app/Contents/Frameworks/libmpv.2.dylib`，再執行 `codesign --force --deep --sign - VitaScope.app` 重新簽章。
- Linux AppImage：用 `--appimage-extract` 解開後替換 `squashfs-root/usr/lib/libmpv.so.2`，或執行時用 `LD_LIBRARY_PATH` 指向自己建置的版本。

## 原始碼取得

- **影戲 VitaScope**：https://github.com/acer1204/VitaScope ，每個版本都有同名的 tag。
- **播放引擎（libmpv）**：完整對應原始碼（各元件原始碼、修正檔、建置腳本與設定）是每個 Release 附的
  `vitascope-libmpv-<平台>-rN-src.tar.xz`，也在對應的 prerelease `libmpv-<平台>-rN`。
- **AppImage 內含的 Ubuntu 套件**：`THIRD-PARTY-LINUX.md` 列出原始碼套件與確切版本（`apt-get source` 或 Launchpad 取得）。
- 如果任何連結失效、無法取得某個版本安裝包所含元件的對應原始碼，請到
  https://github.com/acer1204/VitaScope/issues 提出，我們會提供完整的對應原始碼。
  只要該版本的安裝包還在提供下載，這個承諾就有效（之後也至少再三年）。

## Windows 執行檔的編譯器執行庫

Windows 的 `vitascope.exe` 以 MinGW-w64（GCC）編譯，靜態連結了 mingw-w64 的執行庫（授權條文在 `licenses/mingw-w64/`）
與 GCC 的執行庫（GCC Runtime Library Exception，沒有另外的散布條件）。

## Rust 套件與內建字型

VitaScope 使用的 Rust 套件（含 egui 內建的 Ubuntu Font 與 Noto Emoji 字型）及其完整授權條文，
見同資料夾的 `THIRD-PARTY-RUST.html`（由 [cargo-about](https://github.com/EmbarkStudios/cargo-about) 自動產生）。
