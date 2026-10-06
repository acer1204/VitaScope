# 影戲 VitaScope

**跨平台影片播放器**：以 [libmpv](https://mpv.io/) 為播放引擎，用 Rust + [egui](https://github.com/emilk/egui) 打造介面。
Windows / macOS / Linux 同一套程式碼，目標是功能看齊 PotPlayer。

> 名稱取自 1896 年第一台在戲院成功商業放映的放映機 Vitascope。
> 「影戲」是清末民初對電影的稱呼。姊妹作是看圖軟體 [Zoetrope 走馬燈](https://github.com/acer1204/Zoetrope)。

目前進度：**L1 基本播放**。完整規劃見 [ROADMAP.md](ROADMAP.md)。

## 下載

到 [Releases](https://github.com/acer1204/VitaScope/releases) 下載對應平台的檔案：

| 平台 | 檔案 | 使用方式 |
|---|---|---|
| Windows 10 / 11（64 位元） | `VitaScope-*-windows-x64.zip` | 解壓縮後執行資料夾裡的 `vitascope.exe` |
| macOS（Apple Silicon，最低版本見各版本的發佈說明） | `VitaScope-*-macos-arm64.zip` | 解壓縮後把資料夾裡的 `VitaScope.app` 拖到「應用程式」 |
| Linux（x64） | `VitaScope-*-linux-x64.tar.gz` | 先安裝 libmpv（Ubuntu / Debian：`sudo apt install libmpv2`；Fedora：`sudo dnf install mpv-libs`），解壓縮後執行 `vitascope` |

- **Windows**：程式沒有數位簽章，第一次執行時可能出現「Windows 已保護您的電腦」，請按「其他資訊」→「仍要執行」。
  需要已安裝顯示卡驅動程式（播放引擎需要 OpenGL 3 與 Vulkan 執行環境）。
- **macOS**：沒有經過 Apple 公證，第一次開啟會被擋下。請到「系統設定」→「隱私權與安全性」，在下方按「仍要打開」；
  也可以在終端機執行 `xattr -dr com.apple.quarantine /Applications/VitaScope.app`。
- **Linux**：需要有 libmpv.so.2 的發行版（例如 Ubuntu 24.04 以後）。中文介面需要中文字型（例如 `fonts-noto-cjk`），
  開檔對話框需要 xdg-desktop-portal 或 zenity。

播放器的「關於」（控制列的 ℹ 或 F1）可以檢查更新，有新版本時會開啟 Releases 頁面。

### 設定檔與移除

程式本身不需要安裝，刪掉資料夾即可移除。另外會建立這些資料：

| 平台 | 設定（音量、視窗位置） | 字幕轉碼暫存 |
|---|---|---|
| Windows | `%APPDATA%\Vitascope\settings.json` | `%LOCALAPPDATA%\VitaScope\subs` |
| macOS | `~/Library/Application Support/Vitascope/settings.json` | `~/Library/Caches/VitaScope/subs` |
| Linux | `~/.config/vitascope/settings.json` | `~/.cache/vitascope/subs` |

## 功能（L1）

- 開檔：檔案對話框、拖放、命令列（`vitascope 影片.mkv`）
- 播放、暫停、停止；進度條可拖曳跳轉，滑鼠停在上面會顯示該位置的時間
- 硬體解碼自動啟用（NVIDIA 為 nvdec），不支援時自動退回軟解
- 音軌、字幕切換；同檔名的外掛字幕自動載入（也會找 `Subs` 之類的子資料夾）
- **外掛字幕自動判斷編碼**：UTF-8、UTF-16、Big5、GBK、Shift_JIS…，舊字幕不會變亂碼
- **繁中字幕優先**：看得懂字幕組的各種標法（`tc`、`cht`、`BIG5`、`zh-Hant`、`繁體`、`jptc`…），
  檔名沒標的也會從內容判斷繁簡
- 有字幕軌就自動顯示（比照 PotPlayer；mpv 預設只顯示有語言標籤的字幕）
- 視窗依影片比例調整，高 DPI 螢幕上 1:1 顯示；手機直拍影片會自動轉正
- 全螢幕時控制列浮在畫面上，2 秒沒動作自動隱藏；控制列出現時字幕自動上移，不會被蓋住
- 開檔失敗時用中文顯示原因
- 記住音量、視窗位置與大小

### 快捷鍵

| 按鍵 | 功能 |
|---|---|
| 空白鍵 | 播放 / 暫停 |
| ← / → | 後退 / 前進 5 秒 |
| Ctrl（macOS：⌘）+ ← / → | 後退 / 前進 30 秒 |
| ↑ / ↓ | 音量 ±5 |
| M | 靜音 |
| F、Enter、雙擊畫面 | 全螢幕 |
| Esc | 離開全螢幕 |
| Ctrl（macOS：⌘）+ O | 開啟檔案 |
| F1 | 關於 / 檢查更新 |
| 單擊畫面 | 播放 / 暫停 |

## 支援格式

格式分為「常見 / 通用 / 罕見」三級，詳見 [ROADMAP.md 第 2 節](ROADMAP.md#2-格式支援分級)。
目前自動測試涵蓋 **92 個樣本，三個等級全數通過**；另外用一個約 1.4 萬部動畫的影片庫抽樣 626 個檔案實測，
**全部能播放**（含 RMVB、VC-1、Hi10P、PGS 字幕等）。例如：

- **常見**：MP4、MKV、WebM、MOV、AVI；H.264（含 Hi10P）、HEVC（含 10-bit、4K）、AV1、VP9；AAC、MP3、Opus、AC-3、FLAC；SRT、ASS、mov_text
- **通用**：TS / M2TS、MPG / VOB、WMV、FLV、3GP、OGV；MPEG-2、Xvid、VC-1、ProRes、MJPEG；E-AC-3、DTS；WebVTT、SAMI；HDR10、HLG、隔行掃描
- **罕見**：RealMedia、MXF、DV、VVC (H.266)、FFV1、DNxHR、TrueHD、WavPack、MicroDVD…

## 建置

需要 [Rust](https://rustup.rs/)（stable）和 libmpv。

### Windows

```powershell
pwsh scripts/fetch-libmpv.ps1   # 下載 libmpv 到 vendor/libmpv/windows-x64/
cargo run --release
```

開發時用的是 `x86_64-pc-windows-gnu` 工具鏈；MSVC 工具鏈也可以建置。
專案路徑有非 ASCII 字元（例如在「桌面」底下）時，mingw 的 ld 會找不到檔案，
請在 `.cargo/config.toml` 把 `target-dir` 設到純英文路徑。

### macOS

```bash
brew install mpv
cargo run --release
```

### Linux

```bash
sudo apt install libmpv-dev      # Fedora：sudo dnf install mpv-libs-devel
cargo run --release
```

## 測試

```bash
python scripts/gen_samples.py              # 用 FFmpeg 產生測試樣本（需要 FFmpeg 7.1 以上的 full build）
cargo test                                 # 格式矩陣、播放核心、介面測試（不需要 GPU）
cargo test --test hwdec -- --ignored       # 硬體解碼測試（需要 GPU）
```

| 測試 | 內容 |
|---|---|
| `tests/formats.rs` | 格式測試矩陣：每個樣本檢查編碼、解碼、字幕文字、旋轉 / HDR 中繼資料、跳轉、播到結尾 |
| `tests/smoke.rs` | libmpv 載入、開檔、錯誤訊息 |
| `tests/ui.rs` | 介面測試（egui_kittest）：快捷鍵、按鈕、選單、拖放、「關於」與檢查更新 |
| `tests/hwdec.rs` | 硬體解碼確實走 GPU，Hi10P 自動退回軟解 |

影片畫面的渲染可以用自動截圖驗證：

```bash
vitascope 影片.mp4 --shot 截圖.png [--shot-delay 秒] [--fullscreen]
```

用自己的影片庫做大量測試（只讀取，不修改檔案）：

```bash
cargo run --release --example media_survey -- 檔案清單.tsv [--per-ext 200] [--hwdec]
```

## 發佈新版本

1. 修改 `Cargo.toml` 的 `version`
2. 推送同名標籤：`git tag v0.2.0 && git push origin v0.2.0`
3. GitHub Actions 會編譯三個平台、打包並建立 Release（[.github/workflows/release.yml](.github/workflows/release.yml)）

標籤必須跟 `Cargo.toml` 的版本一致，否則發佈流程會中止。

## 程式架構

```
src/
├─ main.rs       進入點、命令列參數
├─ app.rs        播放器視窗：控制列、快捷鍵、全螢幕、OSD
├─ video.rs      mpv render API → OpenGL FBO → egui 畫面
├─ player.rs     播放器核心：mpv 屬性與事件 → Rust 狀態（介面和測試共用）
├─ mpv/          libmpv 的安全包裝（client API、render API）
├─ subs.rs       外掛字幕：尋找、編碼偵測、繁簡判斷
├─ settings.rs   設定檔（音量、視窗位置）
├─ update.rs     檢查更新（GitHub Releases）
├─ formats.rs    支援的副檔名
├─ fonts.rs      載入系統中文字型
└─ autoshot.rs   開發用：自動截圖
```

## 授權

[GPL-3.0-or-later](LICENSE)。

使用的 libmpv（[shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake) 建置）
採用 GPL 授權，所以本專案也採用 GPL。
