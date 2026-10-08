# 影戲 VitaScope

**跨平台影片播放器**：以 [libmpv](https://mpv.io/) 為播放引擎，用 Rust + [egui](https://github.com/emilk/egui) 打造介面。
Windows / macOS / Linux 同一套程式碼，目標是功能看齊 PotPlayer。

> 名稱取自 1896 年第一台在戲院成功商業放映的放映機 Vitascope。
> 「影戲」是清末民初對電影的稱呼。姊妹作是看圖軟體 [Zoetrope 走馬燈](https://github.com/acer1204/Zoetrope)。

目前進度：**L1 基本播放、L2 日常主力已完成**（v0.2.0），接下來是 L3 進階調校；做到哪裡見下方的[功能清單](#功能清單)，完整規劃見 [ROADMAP.md](ROADMAP.md)。

## 下載

到 [Releases](https://github.com/acer1204/VitaScope/releases) 下載對應平台的檔案：

| 平台 | 檔案 | 使用方式 |
|---|---|---|
| Windows 10 / 11（64 位元） | `VitaScope-*-windows-x64-setup.exe`（建議） | 執行安裝程式。不需要系統管理員，裝在 `%LOCALAPPDATA%\Programs\VitaScope`；可勾選加入影音檔的「開啟檔案」選單 |
| | `VitaScope-*-windows-x64.zip`（免安裝） | 解壓縮後執行資料夾裡的 `vitascope.exe` |
| macOS 11 以上（Apple Silicon） | `VitaScope-*-macos-arm64.dmg`（建議） | 打開後把 `VitaScope.app` 拖到「應用程式」 |
| | `VitaScope-*-macos-arm64.zip` | 解壓縮後把資料夾裡的 `VitaScope.app` 拖到「應用程式」 |
| Linux（x64） | `VitaScope-*-linux-x86_64.AppImage`（建議） | `chmod +x` 後直接執行，不用另外安裝 libmpv |
| | `VitaScope-*-linux-x64.tar.gz` | 先安裝 libmpv（Ubuntu / Debian：`sudo apt install libmpv2`；Fedora：`sudo dnf install mpv-libs`），解壓縮後執行 `./install.sh` 加入應用程式選單（`./install.sh --default` 同時設成預設播放器），或直接執行 `vitascope` |

- **Windows**：程式沒有數位簽章，第一次執行時可能出現「Windows 已保護您的電腦」，請按「其他資訊」→「仍要執行」
  （開啟「智慧型應用程式控制」的 Windows 11 會直接擋下）。需要已安裝顯示卡驅動程式（播放引擎需要 OpenGL 3）。
  Windows 不允許程式自己設成預設播放器：到「設定 → 應用程式 → 預設應用程式」選影戲，或在影戲的「設定 → 系統」按「選擇預設播放器…」。
- **macOS**：沒有經過 Apple 公證，第一次開啟會被擋下。請到「系統設定」→「隱私權與安全性」，在下方按「仍要打開」；
  也可以在終端機執行 `xattr -dr com.apple.quarantine /Applications/VitaScope.app`。
- **Linux**：需要 glibc 2.39 以上（例如 Ubuntu 24.04、Debian 13 以後、目前的 Fedora）；tar.gz 另外需要 libmpv.so.2 與 libxkbcommon-x11（一般桌面都有；AppImage 已內含）。
  AppImage 需要 FUSE（沒有的話加 `--appimage-extract-and-run` 執行）與 OpenSSL 3（libssl3，桌面系統都有）。中文介面需要中文字型（例如 `fonts-noto-cjk`），
  開檔對話框需要 xdg-desktop-portal 或 zenity。沒有顯示卡加速的環境（例如虛擬機，使用 Mesa 的軟體繪圖）
  會自動改用較簡單的畫面處理，畫質稍差但可以正常播放。

播放器的「關於」（控制列的 ℹ 或 F1）可以檢查更新，有新版本時會開啟 Releases 頁面。各版本的改動見 [CHANGELOG.md](CHANGELOG.md)。

### 設定檔與移除

- **Windows 安裝版**：在「設定 → 應用程式」解除安裝（會一併移除檔案關聯）。
- **Windows 免安裝版**：刪掉資料夾即可。如果在「設定 → 系統」打開過檔案關聯，刪除前先把它關掉。
- **macOS**：把 `VitaScope.app` 丟到垃圾桶。
- **Linux**：AppImage 直接刪除；用 `install.sh` 安裝的，執行 `./install.sh --uninstall`。

程式另外會建立這些資料。設定與播放紀錄在移除程式時保留，不需要可以自己刪；
暫存資料夾在 Windows 安裝版解除安裝時會一併刪除，其他情況也可以自己刪：

| 平台 | 設定、播放紀錄、播放清單 | 暫存（字幕轉碼、翻轉用的著色器等） |
|---|---|---|
| Windows | `%APPDATA%\Vitascope\`（`settings.json`、`history.json`、`playlist.m3u8`） | `%LOCALAPPDATA%\VitaScope\` |
| macOS | `~/Library/Application Support/Vitascope/` | `~/Library/Caches/VitaScope/` |
| Linux | `~/.config/vitascope/` | `~/.cache/vitascope/` |

截圖預設存在系統「圖片」資料夾裡的 `VitaScope`，不會隨程式刪除。

## 功能清單

已完成的項目打勾；分級說明與每一項的驗證方式見 [ROADMAP.md 第 3 節](ROADMAP.md#3-功能分級)。

### L1 基本播放（已完成）

- [x] 開檔：檔案對話框、拖放、命令列（`vitascope 影片.mkv`）
- [x] 播放、暫停、停止；進度條可拖曳跳轉，滑鼠停在上面會顯示該位置的時間
- [x] 鍵盤快退快進、音量、靜音
- [x] 全螢幕：控制列浮在畫面上，2 秒沒動作自動隱藏；控制列出現時字幕自動上移，不會被蓋住
- [x] 視窗依影片比例調整，高 DPI 螢幕上 1:1 顯示；手機直拍影片會自動轉正
- [x] 硬體解碼自動啟用（NVIDIA 為 nvdec），不支援時自動退回軟解
- [x] 音軌、字幕切換；同檔名的外掛字幕自動載入（也會找 `Subs` 之類的子資料夾）
- [x] **外掛字幕自動判斷編碼**：UTF-8、UTF-16、Big5、GBK、Shift_JIS…，舊字幕不會變亂碼
- [x] **繁中字幕優先**：看得懂字幕組的各種標法（`tc`、`cht`、`BIG5`、`zh-Hant`、`繁體`、`jptc`…），
  檔名沒標的也會從內容判斷繁簡
- [x] 有字幕軌就自動顯示（比照 PotPlayer；mpv 預設只顯示有語言標籤的字幕）
- [x] 開檔失敗時用中文顯示原因
- [x] 記住音量、視窗位置與大小
- [x] 「常見」格式三平台全數通過自動測試

### L2 日常主力（已完成）

播放控制
- [x] 播放清單：同資料夾的影片依檔名排序（第 2 集在第 10 集前面），播完自動接下一個；PgUp / PgDn 切換
- [x] 播放清單面板（F6）：雙擊播放、拖曳排序、Delete 移除、依檔名排序、加入檔案 / 資料夾；
  開啟 / 儲存 `.m3u` / `.m3u8`；自己整理的清單下次開啟時還在
- [x] 續播：再次開啟時從上次看到的地方繼續
- [x] 最近開啟的檔案（起始畫面、右鍵選單）
- [x] 變速 0.25×–4×（保持音調）
- [x] 逐格前進 / 後退
- [x] A-B 段落重播
- [x] 章節：進度條上的標記與名稱、跳到上 / 下一章
- [x] 進度條預覽縮圖（滑鼠停在進度條上顯示那個時間的畫面）

字幕與音訊
- [x] 字幕外觀：字型、大小、顏色、邊框、陰影、位置、粗體（字幕選單 →「字幕外觀…」）
- [x] 字幕時間軸偏移（同步）、手動指定字幕編碼（自動判斷猜錯、顯示亂碼時）
- [x] 載入字幕檔（選單或拖放；可以跟影片一起拖放）、音軌檔（選單）
- [x] 雙字幕（主字幕 + 第二字幕同時顯示，第二字幕在畫面上方）
- [x] 音訊延遲調整
- [x] 自動載入同名的外掛音軌（`.mka`），預設仍用影片內建的音軌
- [x] 純音訊檔播放（顯示專輯封面與歌名、演出者、專輯）

畫面
- [x] 長寬比：原始 / 16:9 / 4:3 / 16:10 / 1.85:1 / 2.35:1（自訂比例未做）
- [x] 裁切（裁成指定比例、填滿視窗）、縮放、移動、旋轉、左右 / 上下翻轉；換檔時自動還原
- [x] 視窗置頂（右鍵選單，設定會記住）

操作體驗
- [x] 滑鼠滾輪調音量、右鍵選單
- [x] OSD 提示（音量、跳轉、暫停、切換軌道時在畫面上顯示）
- [x] 拖放字幕檔載入外掛字幕、單擊畫面暫停
- [x] 「關於」與檢查更新
- [x] 截圖：原始解析度、可選含不含字幕，存檔（預設在「圖片」資料夾的 VitaScope）或複製到剪貼簿
- [x] 媒體資訊面板：編碼、解析度、位元率、HDR、硬解狀態、掉格數；可以複製成文字
- [x] 設定視窗（F5）：介面語言、硬體解碼、跳轉秒數、截圖資料夾、快捷鍵一覽…；改了馬上生效、自動儲存
- [x] 介面語言：繁體中文 / English

系統整合與發佈
- [x] 檔案關聯：Windows（安裝程式或「設定 → 系統」加入「開啟檔案」選單）、macOS（Finder 的「打開檔案的應用程式」）、
  Linux（`.desktop` 檔；`install.sh --default` 設成預設）
- [x] 單一執行個體：雙擊另一個檔案時送到已開啟的視窗（一次選好幾個檔案會變成一個播放清單；可在設定關閉，`--new-window` 只對這次開新視窗）
- [x] 應用程式圖示：視窗、工作列、Windows 執行檔、macOS `.app`、Linux 選單
- [x] 三平台安裝包自動發佈（Windows zip、macOS `.app`、Linux tar.gz）
- [x] 安裝程式：Windows 安裝檔（不需要系統管理員）、macOS `.dmg`、Linux AppImage
- [x] 「通用」格式三平台全數通過自動測試

### L3 進階調校（進行中）

- [x] **流暢播放**：依視窗所在螢幕的實際更新率（例如 119.88 Hz）微調播放速度，每格影像顯示的次數固定，
  平移畫面不會忽快忽慢；使用電池時自動暫停。在「設定 → 播放」或右鍵選單「畫質」打開（目前預設關閉）
- [ ] 影像調整：亮度、對比、飽和度、色相、Gamma
- [ ] 去交錯、去色帶
- [ ] 縮放演算法選擇
- [ ] GLSL 著色器（Anime4K、FSRCNNX、銳化）
- [ ] HDR → SDR 色調映射選項
- [ ] 選擇音訊輸出裝置、等化器、音量正規化 / 夜間模式、聲道混音、音量超過 100%、音訊直通
- [ ] 自訂快捷鍵
- [ ] 書籤
- [ ] 開啟網址（HTTP、HLS、DASH）、網站影片（yt-dlp）
- [ ] 線上搜尋字幕
- [ ] 片段輸出、轉成 GIF、縮圖總覽圖
- [ ] 播放清單：隨機、重複、播完後動作
- [ ] 系統媒體整合：媒體鍵、Windows SMTC、macOS「正在播放」、Linux MPRIS
- [ ] 深色 / 淺色主題、迷你播放器、子母畫面
- [ ] 「罕見」格式盡力支援（Windows / macOS 的自動測試全數通過；Linux CI 的 FFmpeg 6.1 做不出 MKV 旋轉樣本，只列在報告裡）

### L4 擴充與專業（未開始）

- [ ] mpv 的 Lua / JavaScript 腳本
- [ ] 外掛 API
- [ ] 補幀（mpv interpolation、VapourSynth）
- [ ] 串流錄製
- [ ] DVD / 藍光（ISO 或資料夾）
- [ ] 擷取裝置（視訊鏡頭、擷取卡）
- [ ] 網路來源：SMB、FTP、WebDAV、DLNA
- [ ] 遠端控制（手機網頁遙控）
- [ ] HDR 直通、Dolby Vision
- [ ] Windows HDR 輸出，支援 NVIDIA RTX 視訊增強（把一般影片轉成 HDR、超解析度）
- [ ] 3D、360° 影片
- [ ] 自動更新

### 快捷鍵

| 按鍵 | 功能 |
|---|---|
| 空白鍵 | 播放 / 暫停 |
| ← / → | 後退 / 前進 5 秒（設定裡可以改） |
| Ctrl（macOS：⌘）+ ← / → | 後退 / 前進 30 秒（設定裡可以改） |
| ↑ / ↓ | 音量 ±5 |
| M | 靜音 |
| F、Enter、雙擊畫面 | 全螢幕 |
| Esc | 離開全螢幕 |
| Ctrl（macOS：⌘）+ O | 開啟檔案 |
| F1 | 關於 / 檢查更新 |
| F5 | 設定（介面語言、硬體解碼、跳轉秒數、截圖資料夾…） |
| 單擊畫面 | 播放 / 暫停 |
| PgUp / PgDn | 上一個 / 下一個檔案（同資料夾的影片，依檔名排序；播完自動接下一個） |
| Ctrl（macOS：⌘）+ PgUp / PgDn | 上一章 / 下一章 |
| C / X / Z | 加快 / 減慢 0.1 倍、恢復正常速度（0.25×–4×，保持音調） |
| . / , | 逐格前進 / 後退 |
| [ / ] | 字幕提早 / 延後 0.1 秒（換檔時歸零） |
| - / =（或 +） | 聲音提早 / 延後 0.1 秒 |
| A（或 Ctrl（macOS：⌘）+ F6） | 畫面比例：原始 → 16:9 → 4:3 → 16:10 → 1.85:1 → 2.35:1 |
| Ctrl+Q（macOS 也是 Control） | 裁切：不裁 → 16:9 → 4:3 → 1.85:1 → 2.35:1 |
| 9 / 1 / 5 | 放大 / 縮小 / 恢復 100%（Ctrl 或 ⌘ + 滾輪、觸控板捏合也可以縮放） |
| Alt（macOS：Option）+ 方向鍵 | 移動畫面；Ctrl（⌘）+ 5 置中 |
| Alt（Option）+ K | 順時針旋轉 90° |
| Ctrl（⌘）+ Z / P | 左右翻轉 / 上下翻轉 |
| Alt（Option）+ Backspace | 畫面調整全部還原 |
| Ctrl（⌘）+ T | 視窗置頂 |
| F6 | 播放清單（清單開著時 Delete 移除選取的項目；macOS 也可以用 Backspace） |
| Ctrl + F1（macOS：⌘ + I），或 Ctrl + I | 媒體資訊 |
| Ctrl（⌘）+ E | 擷取畫面（存到截圖資料夾） |
| Ctrl（⌘）+ C | 擷取畫面（複製到剪貼簿） |
| L | A-B 重播：設定起點 → 設定終點 → 取消 |
| Home | 從頭播放（再次開啟的檔案會從上次看到的地方繼續） |
| 在畫面上捲動滑鼠滾輪 | 音量 ±5 |
| 在畫面上按右鍵 | 選單：最近開啟的檔案、播放速度、章節、音軌、字幕… |

字幕選單另有「第二字幕」「字幕延遲」「字幕編碼」「載入字幕檔…」「字幕外觀…」；音軌選單有「音訊延遲」「載入音軌檔…」。
右鍵選單的「畫面」有畫面比例、裁切、縮放、移動、旋轉、翻轉與重設；「擷取畫面」可以另存新檔、選擇含不含字幕、開啟或變更截圖資料夾。
播放清單開著時，拖放進來的檔案會加到清單最後；清單上可以按右鍵複製路徑。

## 支援格式

格式分為「常見 / 通用 / 罕見」三級，詳見 [ROADMAP.md 第 2 節](ROADMAP.md#2-格式支援分級)。
目前自動測試涵蓋 **98 個樣本**（常見、通用全數通過；罕見只在 Linux 有一項因 FFmpeg 版本舊做不出樣本）；另外用一個約 1.4 萬部動畫的影片庫抽樣 626 個檔案實測，
**全部能播放**（含 RMVB、VC-1、Hi10P、PGS 字幕等）。例如：

- **常見**：MP4、MKV、WebM、MOV、AVI；H.264（含 Hi10P）、HEVC（含 10-bit、4K）、AV1、VP9；AAC、MP3、Opus、AC-3、FLAC；SRT、ASS、mov_text
- **通用**：TS / M2TS、MPG / VOB、WMV、FLV、3GP、OGV；MPEG-2、Xvid、VC-1、ProRes、MJPEG；E-AC-3、DTS；WebVTT、SAMI；HDR10、HLG、隔行掃描
- **罕見**：RealMedia、MXF、DV、VVC (H.266)、FFV1、DNxHR、TrueHD、WavPack、MicroDVD…

## 建置

需要 [Rust](https://rustup.rs/)（stable）和 libmpv。Windows 與 macOS 用本專案從原始碼建置的 libmpv
（`.github/workflows/libmpv-*.yml`，發佈在 prerelease `libmpv-<平台>-rN`），用下面的腳本下載並核對雜湊。

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
bash scripts/fetch-libmpv.sh    # 下載 libmpv 到 vendor/libmpv/macos-arm64/
cargo run --release
```

只想在本機試 Homebrew 的 mpv：`brew install mpv` 後加 `VITASCOPE_LIBMPV=system`（發佈版一定用上面那一份）。

### Linux

```bash
sudo apt install libmpv-dev      # Fedora：sudo dnf install mpv-libs-devel
cargo run --release
```

AppImage 用的是本專案建置的 libmpv；要用它測試：`bash scripts/fetch-libmpv.sh && VITASCOPE_LIBMPV=vendor cargo test`。

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
| `tests/ui.rs` | 介面測試（egui_kittest）：快捷鍵、按鈕、選單、拖放、播放清單面板、截圖、設定視窗、「關於」與檢查更新 |
| `tests/instance.rs` | 單一執行個體：同時啟動好幾個程式，檔案都送到同一個視窗；第一個關掉後由下一個接手 |
| `tests/mediainfo.rs`、`tests/thumbs.rs` | 媒體資訊、進度條預覽縮圖 |
| `tests/hwdec.rs` | 硬體解碼確實走 GPU，Hi10P 自動退回軟解 |

影片畫面的渲染可以用自動截圖驗證：

```bash
vitascope 影片.mp4 --shot 截圖.png [--shot-delay 秒] [--fullscreen]
```

排查顯示或播放問題時可以用這些環境變數：

| 環境變數 | 作用 |
|---|---|
| `VITASCOPE_DEBUG=1` | 印出 mpv 的警告與錯誤、影片畫面的像素取樣；也可以直接指定 mpv 的記錄等級，例如 `VITASCOPE_DEBUG=v`。`VITASCOPE_DEBUG=pacing` 另外印出流暢播放的決定（螢幕更新率、電源、套用的設定） |
| `VITASCOPE_MPV_OPTS="名稱=值 名稱=值"` | 額外指定 mpv 選項（以空白分隔），例如 `VITASCOPE_MPV_OPTS="gpu-dumb-mode=yes"`；指定了 `video-sync` 或 `display-fps-override` 時流暢播放不會去改它們 |
| `VITASCOPE_PACING=off` | 流暢播放完全不動作（跟沒有這個功能時一樣） |

流暢播放的實機量測（全螢幕播 1080p 平移影片，從 mpv 的記錄算每格顯示幾次螢幕更新；會在螢幕上開視窗約 15 秒）：

```bash
python scripts/gen_samples.py --tier pacing
cargo test --test pacing_window -- --ignored --nocapture --test-threads=1
python scripts/pacing_stats.py <暫存資料夾>/vitascope-pacing-window/display
```

Windows 的發佈版沒有主控台視窗，要把輸出存到檔案才看得到。請在「命令提示字元」（cmd）執行：

```bat
set VITASCOPE_DEBUG=1
vitascope.exe 影片.mp4 2> log.txt
```

（PowerShell 不會把這類視窗程式的輸出導到檔案，記錄會是空的。）

用自己的影片庫做大量測試（只讀取，不修改檔案）：

```bash
cargo run --release --example media_survey -- 檔案清單.tsv [--per-ext 200] [--hwdec]
```

## 發佈新版本

1. 修改 `Cargo.toml` 的 `version`；把 [CHANGELOG.md](CHANGELOG.md) 的「下一版（尚未發佈）」改成這個版本的標題
   （格式：`## v0.2.0 — 日期`，發佈說明的「更新內容」就是取這一段）
2. 推送同名標籤：`git tag v0.2.0 && git push origin v0.2.0`
3. GitHub Actions 會編譯三個平台、打包並建立 Release（[.github/workflows/release.yml](.github/workflows/release.yml)），
   發佈說明的「更新內容」取自 CHANGELOG.md 的對應段落

標籤必須跟 `Cargo.toml` 的版本一致，否則發佈流程會中止。

## 程式架構

```
src/
├─ main.rs       進入點、命令列參數（`vitascope [檔案…] [--fullscreen] [--new-window] [--version]`；
│                另有開發用的 `--shot`、解除安裝程式用的 `--unregister-associations`）
├─ app.rs        播放器視窗：控制列、快捷鍵、全螢幕、OSD
├─ app/          視窗的各個部分：播放清單面板、設定視窗、媒體資訊、擷取畫面、進度條預覽縮圖
├─ video.rs      mpv render API → OpenGL FBO → egui 畫面
├─ player.rs     播放器核心：mpv 屬性與事件 → Rust 狀態（介面和測試共用）
├─ mpv/          libmpv 的安全包裝（client API、render API：OpenGL 與軟體繪圖）
├─ geometry.rs   畫面調整：長寬比、裁切、縮放、旋轉、翻轉
├─ subs.rs       外掛字幕：尋找、編碼偵測、繁簡判斷
├─ playlist.rs   播放清單：同資料夾的檔案、自然排序、編輯
├─ m3u.rs        播放清單檔（.m3u / .m3u8）的讀寫、下次開啟時還原
├─ history.rs    播放紀錄：最近開啟的檔案、續播位置
├─ mediainfo.rs  媒體資訊：讀 mpv 的格式資訊、整理成文字
├─ screenshot.rs 擷取畫面：檔名、轉正、PNG、截圖資料夾
├─ thumbs.rs     進度條預覽縮圖（另一個 mpv，在背景執行緒用軟體繪圖）
├─ icon.rs       視窗圖示（圖檔在 packaging/icons/）
├─ i18n.rs       介面語言（繁體中文 / English）
├─ settings.rs   設定檔（音量、視窗位置、播放選項）
├─ instance.rs   單一執行個體：鎖定檔決定誰是主視窗，其他程式用具名管道（Unix socket）把檔案送過去
├─ assoc.rs      Windows 檔案關聯（目前使用者的登錄檔；安裝程式寫的是同一組）
├─ macos_open.rs macOS：接收 Finder 開檔的 Apple Event
├─ update.rs     檢查更新（GitHub Releases）
├─ formats.rs    支援的副檔名
├─ fonts.rs      載入系統中文字型
└─ autoshot.rs   開發用：自動截圖

packaging/
├─ icons/        各尺寸圖示、.ico、.icns（`cargo run --example make_icon` 產生）
├─ libmpv/       三個平台共用的 libmpv 建置腳本（原始碼包、元件清單）
├─ windows/      執行檔的圖示與版本資訊（.rc）、Inno Setup 安裝程式（.iss）、libmpv/（Windows 版 libmpv 的版本與建置腳本）
├─ macos/        .app 的 Info.plist（檔案關聯、圖示）、libmpv/（macOS 版）
└─ linux/        .desktop 檔、AppStream 中繼資料、install.sh、AppImage 的 AppRun 與組裝腳本、libmpv/（AppImage 用）
```

## 授權

[GPL-3.0-or-later](LICENSE)。

安裝包內含的 libmpv（mpv 與 FFmpeg 等）由本專案從原始碼建置，依 LGPL-2.1-or-later 散布，不會限制影戲本身的授權；
各元件的版本、授權與對應原始碼見 [packaging/THIRD-PARTY-NOTICES.md](packaging/THIRD-PARTY-NOTICES.md)
（安裝包裡是程式旁邊的 `THIRD-PARTY-NOTICES.md`）。
