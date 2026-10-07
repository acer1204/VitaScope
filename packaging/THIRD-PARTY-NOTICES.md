# 第三方元件與授權聲明

影戲 VitaScope 本身以 **GPL-3.0-or-later** 授權（全文見 `LICENSE`）。
安裝包內含以下第三方元件，各自依其授權散布。

## 播放引擎：mpv 與 FFmpeg

| 平台 | 內含方式 | 來源與版本 |
|---|---|---|
| Windows | `libmpv-2.dll`：mpv 與其相依函式庫（FFmpeg、libass、libplacebo、FreeType、HarfBuzz…）靜態連結成一個 DLL | 由 [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake) 建置（tag `20261006`、建置腳本 commit `05a60b3cfd04e3e3b89918f4a27f3dde2935dff2`）。每個元件的原始碼與確切版本逐一列在同資料夾的 `THIRD-PARTY-WINDOWS.md`，各自的授權條文在 `licenses/` |
| macOS | `VitaScope.app/Contents/Frameworks/*.dylib` | Homebrew 套件，逐一列在 `VitaScope.app/Contents/Resources/THIRD-PARTY-MACOS.md`，各自的授權檔在 `Contents/Resources/licenses/` |
| Linux（tar.gz） | 不內含，使用系統安裝的 libmpv | 各發行版的套件 |
| Linux（AppImage） | AppImage 的 `usr/lib`：Ubuntu 24.04 的 libmpv、FFmpeg 及其相依函式庫；檔案開頭的 AppImage 執行環境 | 逐一列在 AppImage 的 `usr/share/doc/vitascope/THIRD-PARTY-LINUX.md`（二進位套件、原始碼套件與確切版本，以及執行環境 type2-runtime 的版本），各自的 copyright 檔在 `usr/share/doc/<套件>/`，參照的授權全文在 `usr/share/common-licenses/` |

Windows 的 `libmpv-2.dll` 以 GPL 選項建置（FFmpeg 使用 `--enable-gpl --enable-version3`，並含 x264、x265 等 GPL 元件），
整體依 **GPL-3.0-or-later** 散布。建置腳本大多不固定各元件的版本（建置時取各分支的最新版），所以每個元件在那次建置時的
確切 commit 另外整理在 `THIRD-PARTY-WINDOWS.md`；建置方式與修正檔在建置腳本的
[`05a60b3`](https://github.com/shinchiro/mpv-winbuild-cmake/tree/05a60b3cfd04e3e3b89918f4a27f3dde2935dff2) 版。

主要元件的授權：

| 元件 | 授權 | 原始碼 |
|---|---|---|
| mpv | GPL-2.0-or-later（以 GPL 選項建置） | https://github.com/mpv-player/mpv |
| FFmpeg | GPL-3.0-or-later（以 GPL 選項建置） | https://ffmpeg.org/download.html |
| libass | ISC | https://github.com/libass/libass |
| libplacebo | LGPL-2.1-or-later | https://code.videolan.org/videolan/libplacebo |
| FreeType | FreeType License（FTL） | https://freetype.org |
| HarfBuzz | MIT | https://github.com/harfbuzz/harfbuzz |
| FriBidi | LGPL-2.1-or-later | https://github.com/fribidi/fribidi |
| x264 | GPL-2.0-or-later | https://code.videolan.org/videolan/x264 |
| x265 | GPL-2.0-or-later | https://bitbucket.org/multicoreware/x265_git |
| dav1d | BSD-2-Clause | https://code.videolan.org/videolan/dav1d |
| uchardet | MPL-1.1 / GPL-2.0 / LGPL-2.1 | https://gitlab.freedesktop.org/uchardet/uchardet |

完整清單見各平台的清單檔：Windows 的 `THIRD-PARTY-WINDOWS.md`、macOS 的 `THIRD-PARTY-MACOS.md`、AppImage 的 `THIRD-PARTY-LINUX.md`。

## 原始碼取得

- **影戲 VitaScope**：https://github.com/acer1204/VitaScope ，每個版本都有同名的 tag。
- **第三方元件**：各平台清單檔裡列出的原始碼位置與確切版本（Windows：上游儲存庫的 commit 與建置腳本；
  macOS：上游原始碼與 Homebrew 的建置配方；AppImage：Ubuntu 的原始碼套件與版本）。
- 如果任何連結失效、無法取得某個版本安裝包所含元件的對應原始碼，請到
  https://github.com/acer1204/VitaScope/issues 提出，我們會提供完整的對應原始碼。
  只要該版本的安裝包還在提供下載，這個承諾就有效（之後也至少再三年）。

## Rust 套件與內建字型

VitaScope 使用的 Rust 套件（含 egui 內建的 Ubuntu Font 與 Noto Emoji 字型）及其完整授權條文，
見同資料夾的 `THIRD-PARTY-RUST.html`（由 [cargo-about](https://github.com/EmbarkStudios/cargo-about) 自動產生）。
