# 影戲 VitaScope — 開發規劃

目標：跨平台（Windows / macOS / Linux）、功能看齊 PotPlayer 的影片播放器。
先做出能用的基本版，再依等級逐步補完；每完成一項就測一項。

**目前進度（2026-10-06）**：L1 完成。Windows 本機、macOS / Linux（GitHub Actions）自動測試通過；
另用實際影片庫（約 1.4 萬部）抽樣 626 個檔案實測，全部能播放。

---

## 1. 技術選型

### 結論：Rust + egui + libmpv

| 層 | 選用 | 說明 |
|---|---|---|
| 語言 | Rust | 沿用 Zoetrope 的經驗與工具鏈 |
| 介面 | egui / eframe（**glow / OpenGL 後端**） | 與 Zoetrope 同一套 UI 寫法；改用 glow 後端是為了和 mpv 的 OpenGL 渲染介面接起來 |
| 播放引擎 | libmpv（`libmpv2` crate） | 解碼、硬體解碼、音畫同步、字幕渲染、HDR、串流全部由 mpv 處理 |
| 畫面輸出 | mpv render API → OpenGL FBO → egui 貼圖 | 影片畫面是 egui 裡的一張貼圖，控制列、OSD 直接疊在上面 |

### 為什麼看圖軟體和播放器的做法不同

看圖軟體的核心是「解一張圖然後顯示」，用純 Rust 自己解碼可行，Zoetrope 就是這樣做的。
播放器的核心難題不在介面，而在引擎：解碼、音畫同步、精準跳轉、硬體解碼、ASS 特效字幕、HDR 色調映射。
自己用 FFmpeg 從頭寫，要花好幾個月才追得上 mpv 的基本盤。

mpv 是 IINA（macOS）、mpv.net（Windows）、Celluloid / Haruna（Linux）、Flutter media_kit 等播放器共用的引擎，
已經在三個平台上驗證過。我們專心做介面和功能，引擎交給 mpv。

### 其他方案比較

| 方案 | 引擎 | 優點 | 缺點 |
|---|---|---|---|
| **Rust + egui + libmpv（採用）** | mpv | 沿用既有經驗；格式、硬解、字幕一次到位 | 要打包 libmpv；OpenGL 整合要寫一些銜接程式碼 |
| Rust + FFmpeg 自己寫 | FFmpeg | 完全掌控，學習價值高 | 音畫同步、跳轉、硬解、字幕、HDR 全部要自己寫，進度最慢 |
| Flutter + media_kit | mpv | 介面開發快，也能延伸到手機 | 要學 Dart、本機沒有 Flutter SDK；細部控制受套件限制 |
| C++ Qt/QML + libmpv | mpv | Haruna 等成熟專案在用 | C++ 建置複雜，Qt 打包與授權麻煩 |
| Electron / Tauri + HTML5 video | 瀏覽器 | 介面最快 | MKV、AVI、HEVC、ASS 字幕都有問題，不適合 |

### 程式架構

L1 完成時的實際結構見 [README](README.md#程式架構)。之後預計新增：

```
src/
├─ library/         播放清單、播放紀錄（續播）、最近開啟、書籤（L2）
├─ keymap.rs        自訂快捷鍵（L3）
└─ platform/        檔案關聯、單一執行個體、系統媒體鍵（L2–L3）
```

### 各平台的 libmpv 來源

| 平台 | 開發時 | 發佈時 |
|---|---|---|
| Windows | 下載 libmpv 開發包（`libmpv-2.dll`） | dll 跟著執行檔一起放 |
| macOS | `brew install mpv` | 把 dylib 及相依函式庫打包進 `.app` |
| Linux | 套件管理員安裝 `libmpv-dev` | 依賴系統 libmpv，或打包成 AppImage / Flatpak |

授權：libmpv 是 LGPL（預設建置為 GPL），和 Zoetrope 採用的 AGPL 相容。

---

## 2. 格式支援分級

> **重點**：用了 mpv 之後，罕見格式幾乎也都能「直接播」，不需要額外寫程式。
> 所以分級決定的不是「要不要寫」，而是**保證到什麼程度**：

| 等級 | 保證程度 | 測試方式 | 硬體解碼 | 檔案關聯 |
|---|---|---|---|---|
| **常見** | 必須完美 | 自動測試全覆蓋，失敗就擋下發佈；三平台都要手動驗收 | 三平台都要驗證 | 預設關聯 |
| **通用** | 必須能播 | 自動測試覆蓋，失敗就擋下發佈 | 能用就用，軟解保證能播 | 可選擇關聯 |
| **罕見** | 盡力支援 | 有樣本就測，只回報、不擋發佈 | 不要求 | 不關聯 |

### 2.1 容器（檔案格式）

| 等級 | 格式 | 副檔名 | 常見來源 |
|---|---|---|---|
| 常見 | MP4 | `.mp4` `.m4v` | 手機、網路下載、串流平台 |
| 常見 | Matroska | `.mkv` | 動畫字幕組、BD / WEB 重製 |
| 常見 | WebM | `.webm` | YouTube、網頁 |
| 常見 | QuickTime | `.mov` | iPhone、相機、剪輯軟體 |
| 常見 | AVI | `.avi` | 舊下載、監視器、早期數位相機 |
| 通用 | MPEG-TS / BDAV | `.ts` `.m2ts` `.mts` | 數位電視錄影、AVCHD 攝影機、藍光 |
| 通用 | MPEG-PS | `.mpg` `.mpeg` `.vob` | DVD、早期影片 |
| 通用 | ASF / WMV | `.wmv` `.asf` | 舊 Windows 影片 |
| 通用 | Flash Video | `.flv` `.f4v` | 早期網路影片、直播錄影 |
| 通用 | 3GPP | `.3gp` `.3g2` | 舊手機 |
| 通用 | Ogg | `.ogv` `.ogg` | 開源專案 |
| 罕見 | RealMedia | `.rm` `.rmvb` | 2000 年代下載 |
| 罕見 | MXF | `.mxf` | 廣播、專業攝影機 |
| 罕見 | DV | `.dv` | MiniDV 攝影機 |
| 罕見 | 其他 | `.ogm` `.nut` `.ivf` `.y4m` `.bik` `.smk` `.amv` `.wtv` `.dvr-ms` | 特殊工具、遊戲、錄影系統 |

### 2.2 影像編碼

| 等級 | 編碼 | 備註 |
|---|---|---|
| 常見 | H.264 / AVC | 最普遍。包含 Hi10P（舊動畫常見，GPU 不支援，只能軟解） |
| 常見 | H.265 / HEVC | 8-bit 與 10-bit（Main10） |
| 常見 | AV1 | YouTube、Netflix、新動畫壓制常用 |
| 常見 | VP9 | YouTube、WebM，含 10-bit |
| 通用 | MPEG-4 Part 2 | DivX / Xvid |
| 通用 | MPEG-2 / MPEG-1 | DVD、數位電視、VCD |
| 通用 | VC-1 / WMV 1–3 | 舊 Windows 影片、部分藍光 |
| 通用 | VP8 | 舊版 WebM |
| 通用 | ProRes | 剪輯、iPhone ProRes 錄影 |
| 通用 | MJPEG | 網路攝影機、舊數位相機 |
| 通用 | H.263 / Theora | 舊手機、Ogg |
| 罕見 | H.266 / VVC | 新標準，目前很少見 |
| 罕見 | RealVideo（RV30 / RV40） | `.rmvb` |
| 罕見 | DNxHD / DNxHR、CineForm | 專業剪輯中介檔 |
| 罕見 | 無損編碼：FFV1、HuffYUV、Lagarith、UtVideo | 錄影、保存 |
| 罕見 | 舊式：Sorenson、VP6、Cinepak、Indeo、MS Video 1 | 1990–2000 年代 |
| 罕見 | AVS2 / AVS3、Bink | 中國標準、遊戲影片 |

### 2.3 音訊編碼

| 等級 | 編碼 | 備註 |
|---|---|---|
| 常見 | AAC（LC / HE） | MP4 標配 |
| 常見 | MP3 | |
| 常見 | Opus | WebM / YouTube |
| 常見 | AC-3（Dolby Digital） | 電影、DVD |
| 常見 | FLAC | MKV 無損音軌 |
| 通用 | E-AC-3（Dolby Digital Plus） | 串流平台下載 |
| 通用 | DTS（核心） | 電影重製 |
| 通用 | Vorbis、ALAC、PCM / WAV、WMA、MP2 | |
| 罕見 | TrueHD / Dolby Atmos、DTS-HD MA / DTS:X | 會解成 PCM 播放；直通到擴大機屬於 L3 功能 |
| 罕見 | APE、WavPack、TTA、Musepack、DSD（DSF / DFF） | 發燒友音樂格式 |
| 罕見 | RealAudio（cook）、AMR、Speex、ATRAC | |

### 2.4 字幕

| 等級 | 格式 | 備註 |
|---|---|---|
| 常見 | SRT | |
| 常見 | ASS / SSA | 含特效、MKV 內嵌字型（動畫必備） |
| 常見 | MKV / MP4 內嵌文字字幕 | |
| 常見 | **字幕編碼自動偵測** | UTF-8 / UTF-16 / **Big5** / GBK，舊的中文 SRT 很多是 Big5 |
| 通用 | PGS（`.sup`） | 藍光圖形字幕 |
| 通用 | VobSub（`.idx` + `.sub`） | DVD 圖形字幕 |
| 通用 | WebVTT（`.vtt`） | 網頁、串流 |
| 通用 | SAMI（`.smi`） | 韓國、舊台灣下載常見 |
| 罕見 | MicroDVD（影格制 `.sub`）、SubViewer、MPL2、TTML / DFXP、LRC | |
| 罕見 | DVB 字幕、Teletext、EIA-608 / 708 隱藏字幕 | 電視錄影、美國串流內嵌 |

### 2.5 畫面特性

| 等級 | 項目 | 備註 |
|---|---|---|
| 常見 | 8-bit / 10-bit SDR，720p–4K，24–60 fps | |
| 常見 | 可變幀率（VFR） | 手機錄影幾乎都是 |
| 常見 | 旋轉中繼資料 | 手機直拍，要自動轉正 |
| 通用 | HDR10 / HLG → SDR 色調映射 | 在一般螢幕上看 HDR 影片 |
| 通用 | 隔行掃描（去交錯） | 電視錄影、DVD |
| 通用 | 非方形像素 | DVD 變形寬螢幕 |
| 通用 | 高幀率 120 fps 以上 | |
| 罕見 | Dolby Vision（Profile 5 / 8） | ⚠ 見風險 R3 |
| 罕見 | HDR 直通到 HDR 螢幕 | ⚠ 見風險 R3 |
| 罕見 | 8K、12-bit、4:4:4 | |
| 罕見 | 3D（左右 / 上下）、360° 全景 | |

---

## 3. 功能分級

用核取方塊追蹤進度，完成並測試通過後打勾。括號裡註明是怎麼驗證的：
「自動」= `cargo test` 涵蓋；「截圖」= `--shot` 自動截圖檢查；「手動」= 還需要人實際操作確認。

### L1 基本播放（MVP）

**目標：可以當作簡單的播放器每天使用。**

- [x] 開啟檔案：命令列參數（截圖）、拖放影片（自動）、檔案對話框（**待手動**）
- [x] 播放 / 暫停 / 停止（自動：空白鍵、按鈕）
- [x] 進度條：點擊跳轉、顯示目前時間 / 總長度（自動、截圖）；拖曳（**待手動**）
- [x] 鍵盤快退快進（← → 5 秒、Ctrl+← → 30 秒）、音量（↑ ↓）（自動）
- [x] 音量調整、靜音（自動）
- [x] 全螢幕：F / Enter（自動）、全螢幕版面（截圖）；雙擊畫面（**待手動**）
- [x] 視窗依影片比例調整，高 DPI 下 1:1，直拍影片轉正、補黑邊（截圖）
- [x] 硬體解碼：自動啟用（GUI 為 nvdec），Hi10P 自動退回軟解（自動 `hwdec`、截圖記錄）
- [x] 音軌、字幕軌切換（自動：選單操作）
- [x] 自動載入同檔名的外掛字幕（自動、截圖）
- [x] 外掛字幕自己判斷編碼（UTF-8 / UTF-16 / Big5 / GBK / Shift_JIS…），修好缺標頭的 ASS（自動、真實檔案比對）
- [x] 繁中字幕優先：看懂字幕組的各種標法，檔名沒標的從內容判斷繁簡（自動、真實影片庫 73 個多字幕檔案）
- [x] 有字幕軌就自動顯示（比照 PotPlayer；mpv 預設不顯示沒有語言標籤的字幕）（自動）
- [x] 全螢幕時控制列自動隱藏，移動滑鼠時再出現；控制列出現時字幕自動上移（截圖）
- [x] 無法開啟時顯示中文錯誤原因（自動、截圖）
- [x] 記住視窗大小、位置、音量；不記全螢幕狀態（設定檔確認）
- [x] 「常見」格式測試矩陣三平台全數通過 — Windows（本機）、macOS、Linux（GitHub Actions）

**L1 期間一併完成的 L2 / L3 項目**：OSD 提示、拖放字幕檔載入外掛字幕、單擊畫面暫停、「關於」與檢查更新、
三平台安裝檔的自動發佈流程。

**手動驗收結果（Windows）**：拖曳進度條、單擊暫停、雙擊全螢幕、檔案對話框、字幕選單、音量條、靜音、
全螢幕按鈕都正常；過程中發現並修正 3 個問題（暫停中開新檔不會播放、換檔時視窗沿用舊尺寸、Esc 關不掉選單）。
**待確認**：實體鍵盤按 Esc 離開全螢幕（模擬按鍵只收到「放開」，可能是輸入法攔截）、實際聲音輸出、多螢幕。

### L2 日常主力

**目標：日常使用可以取代 PotPlayer。**

播放控制
- [ ] 播放清單：同資料夾自動接續；上一個 / 下一個（PgUp / PgDn）
- [ ] 播放清單面板：排序、刪除、拖曳排序；儲存 / 讀取 `.m3u`
- [ ] 續播：記住每個檔案的播放位置
- [ ] 最近開啟的檔案
- [ ] 變速 0.25× – 4×（保持音調）
- [ ] 逐格前進 / 後退
- [ ] A-B 段落重播
- [ ] 章節：進度條上的標記、跳到上 / 下一章
- [ ] 進度條預覽縮圖（滑鼠停在進度條上就顯示該時間點的畫面）

字幕與音訊
- [ ] 字幕外觀：字型、大小、顏色、邊框、位置
- [ ] 字幕時間軸偏移（同步）、手動指定編碼
- [ ] 手動載入外部字幕（拖放字幕檔已完成；選單「載入字幕…」未做）
- [ ] 雙字幕（主字幕 + 副字幕同時顯示）
- [ ] 音訊延遲調整
- [ ] 自動載入同名的外掛音軌（`.mka`，字幕組常附評論音軌或 5.1 音軌；實際影片庫有 37 個）
- [ ] 純音訊檔播放（顯示專輯封面）

畫面
- [ ] 長寬比：自動 / 4:3 / 16:9 / 2.35:1 / 自訂
- [ ] 裁切、縮放、平移、旋轉、翻轉
- [ ] 視窗置頂

操作體驗
- [ ] 滑鼠滾輪調音量、右鍵選單
- [x] OSD 提示（音量、跳轉、暫停、切換軌道時在畫面上顯示）— L1 期間完成
- [ ] 截圖：原始解析度 / 含字幕，存檔或複製到剪貼簿
- [ ] 媒體資訊面板：編碼、解析度、位元率、硬解狀態、掉格數
- [ ] 設定視窗，設定自動儲存
- [ ] 介面語言：繁體中文 / English

系統整合與發佈
- [ ] 檔案關聯（Windows 登錄檔、macOS Info.plist、Linux `.desktop`）
- [ ] 單一執行個體：雙擊另一個檔案時送到已開啟的視窗（可在設定關閉）
- [ ] 打包發佈：Windows zip / 安裝檔、macOS `.app` / `.dmg`、Linux AppImage
- [ ] ✅ 「通用」格式測試矩陣通過

### L3 進階調校

**目標：PotPlayer 進階使用者常用的功能。**

影像
- [ ] 影像調整：亮度、對比、飽和度、色相、Gamma
- [ ] 去交錯（自動 / 強制）、去色帶（deband）
- [ ] 縮放演算法選擇（bilinear、spline36、ewa_lanczos…）
- [ ] GLSL 著色器載入與切換（Anime4K、FSRCNNX、銳化）
- [ ] HDR → SDR 色調映射選項（演算法、目標峰值亮度）

音訊
- [ ] 選擇音訊輸出裝置
- [ ] 等化器
- [ ] 音量正規化、動態範圍壓縮（夜間模式）
- [ ] 聲道混音（5.1 → 2.0）、音量放大超過 100%
- [ ] 音訊直通（AC-3 / DTS / TrueHD 經 HDMI、S/PDIF 送到擴大機）

功能
- [ ] 自訂快捷鍵（提供 PotPlayer 風格的預設組）
- [ ] 書籤：在檔案中標記時間點並命名
- [ ] 開啟網址：HTTP、HLS（`.m3u8`）、DASH（`.mpd`）
- [ ] 網站影片（透過 yt-dlp），可選畫質
- [ ] 線上搜尋字幕（OpenSubtitles API）
- [ ] 片段輸出：把 A-B 段落存成檔案（不重新編碼）、轉成 GIF
- [ ] 縮圖總覽圖匯出（thumbnail sheet）
- [ ] 播放清單：隨機、單一 / 全部重複、播完後動作（關閉、休眠、關機）
- [ ] 系統媒體整合：媒體鍵、Windows SMTC、macOS「正在播放」、Linux MPRIS
- [ ] 外觀：深色 / 淺色主題、迷你播放器、子母畫面（PiP）
- [ ] ✅ 「罕見」格式盡力支援，有樣本的納入測試

### L4 擴充與專業

**目標：外掛生態與專業用途。**

- [ ] 腳本：支援 mpv 的 Lua / JavaScript 腳本（直接使用 mpv 生態系的現成腳本）
- [ ] 自家外掛 API
- [ ] 補幀：mpv interpolation（平滑運動）、VapourSynth（SVP、RIFE）
- [ ] 串流錄製（邊看邊存，不重新編碼）
- [ ] DVD / 藍光：ISO 或資料夾，依標題播放（不含光碟選單）
- [ ] 擷取裝置：視訊鏡頭、擷取卡
- [ ] 網路來源：SMB、FTP、WebDAV 瀏覽，DLNA
- [ ] 遠端控制（手機網頁遙控）
- [ ] HDR 直通、Dolby Vision（⚠ 見風險 R3）
- [ ] 3D、360° 影片
- [ ] 自動更新

---

## 4. 測試策略

### 4.1 格式測試矩陣（自動）

1. **產生樣本**：`python scripts/gen_samples.py` 用本機 FFmpeg 產生每個等級的測試檔（3 秒彩條 + 測試音，每種容器 × 編碼的組合各一個）。
   已確認本機 FFmpeg 8.1 具備 x264、x265、VP8/9、AV1、MPEG-1/2/4、WMV2、ProRes、MJPEG、Theora、FLV、RV20、FFV1、DNxHD、
   AAC、MP3、Opus、AC-3、E-AC-3、DTS、Vorbis、FLAC、ALAC、WMA、TrueHD、Speex，以及 SRT / ASS / WebVTT / VobSub / mov_text 編碼器。
   無法用 FFmpeg 產生的（VC-1、RV40、PGS、Dolby Vision、Atmos 等）改用公開樣本，放在 `samples/external/`（不進版控）。
2. **自動測試**：`cargo test --test formats` 以 headless 模式執行 libmpv（`vo=null`、`ao=null`），每個樣本檢查：
   - 能開啟，偵測到的編碼與預期一致
   - 能播到 1 秒、跳到中間、播到結尾，過程中沒有錯誤事件
   - 字幕軌數量與內容正確（包含 Big5 編碼的 SRT）
3. **硬體解碼**：用 `hwdec=auto-copy` 播放，讀取 `hwdec-current` 確認真的走了 GPU（CI 主機沒有 GPU，所以在本機測）。
4. **依等級判定**：常見、通用失敗 → 測試失敗；罕見失敗 → 只列在報告中。

### 4.2 功能測試

- **播放器核心**（`src/player.rs`）：用 headless libmpv 測屬性與指令，例如設定 2× 速度後讀回、A-B 重播是否回到 A 點、字幕延遲是否生效。
- **介面**（`tests/ui.rs`）：用 `egui_kittest` 模擬點擊、按鍵與拖放，檢查控制列、選單、快捷鍵的行為。
  播放器以 headless 模式執行，不需要 GPU。
- **影片畫面**：`vitascope 影片 --shot 截圖.png [--fullscreen]` 播放後自動截下整個視窗並關閉，
  驗證 mpv → FBO → 視窗的渲染路徑、版面、字幕位置。
- **手動驗收清單**：每完成一個等級，三平台各跑一次（視窗、全螢幕、高 DPI、多螢幕、硬體解碼、拖放）。

### 4.4 目前的測試數量

| 測試 | 數量 | 平台 |
|---|---|---|
| 格式矩陣 `tests/formats.rs` | 92 個樣本（常見 31、通用 36、罕見 25） | Windows、macOS、Linux |
| 介面 `tests/ui.rs` | 19 | Windows、macOS、Linux |
| 播放核心 `tests/smoke.rs` + 單元測試 | 3 + 11 | Windows、macOS、Linux |
| 硬體解碼 `tests/hwdec.rs`（需要 GPU） | 8 種編碼 | RTX 3090 通過 |

### 4.5 真實影片庫普查（`examples/media_survey.rs`）

用一個約 1.4 萬部動畫的影片庫（NAS），每種副檔名抽樣、每個資料夾挑一個，共 626 個檔案，
逐一開檔 → 解出第一格 → 跳到中間 → 播放 1 秒。結果 **626 / 626 能播放**。

實際遇到、但自己產生不了的格式：RealVideo 4 + Cook（RMVB）、VC-1、DivX 3 / MS-MPEG4v2、PCM Blu-ray、
藍光 PGS 字幕、各家字幕組的外掛 ASS / SSA。普查也找到這些問題，都已修正並做成樣本：

| 問題 | 數量 | 修正 |
|---|---|---|
| 外掛字幕編碼猜錯，整份變亂碼（GBK 被當成 BIG5） | 6 個檔案，其中 4 個看得到亂碼 | 自己判斷編碼（chardetng），轉成 UTF-8 再載入 |
| 有繁中卻選到簡中（`Zh-TW..ass` 雙點、`big5.ass` 認不出來） | 73 個多字幕檔中 5 個 | 自己判斷繁簡（檔名標記 + 內容） |
| ASS 少了 `[Script Info]` 標頭，mpv 打不開 | 1 個 | 自動補上標頭 |

### 4.3 CI

GitHub Actions 在 Windows / macOS / Ubuntu 三平台建置，並跑格式測試（軟解）與單元測試。

---

## 5. L1 開發順序

每一步做完都要有可執行的結果，並寫好對應的測試。

1. 建立 Cargo 專案，eframe（glow 後端）開出空白視窗
2. 取得 libmpv，寫第一個 headless 測試：開檔並讀出長度與編碼
3. 樣本產生腳本 + 「常見」格式測試矩陣（headless 先跑通）
4. render API 接上 egui，畫面出現在視窗裡
5. 控制列：播放 / 暫停、進度條、音量、時間顯示
6. 快捷鍵、全螢幕、控制列自動隱藏
7. 開檔方式（對話框、拖放、命令列）
8. 音軌 / 字幕選單、外掛字幕自動載入
9. 設定保存、錯誤處理 → L1 驗收

---

## 6. 風險與待驗證事項

| # | 風險 | 對策 |
|---|---|---|
| R1 | macOS 打包 libmpv 及其相依 dylib 最麻煩 | 開發期先用 brew，L2 打包時再處理 |
| R2 | macOS 已將 OpenGL 標為棄用 | 目前仍可正常使用（IINA 也是用 OpenGL 接 mpv），持續觀察 |
| R3 | 部分 HDR / Dolby Vision 功能只有 mpv 的 gpu-next 渲染器支援，嵌入式 render API 是否支援要看 mpv 版本；egui 也還沒有 HDR 輸出 | L3 前先驗證；必要時 HDR 直通模式改用 mpv 原生視窗 |
| R4 | Wayland 不支援把 mpv 嵌進子視窗（`wid`） | 本來就採用 render API，不受影響 |
| R5 | 上次的視窗位置在已拔掉的螢幕上時，視窗可能開在看不到的地方 | L2 加上「位置超出所有螢幕就置中」的檢查 |
| R6 | 全螢幕時字幕上移只對文字字幕（SRT 等）有效，ASS 字幕有自己的版面 | 觀察實際使用情況再決定是否處理 |

## 7. 開發中學到的事

- **mpv 的記錄訊息比一般事件晚送達**：`mpv_wait_event` 會先把一般事件取完才送記錄訊息，
  所以開檔失敗時，詳細原因會比 `END_FILE` 晚到，要在收到後補進錯誤說明。
- **`dwidth` / `dheight` 是旋轉前的尺寸**（GPU 負責旋轉時），要用 `video-out-params` 的 `rotate` 自己交換寬高。
- **視窗顯示前送出的大小 / 全螢幕指令會被忽略**，要等視窗出現後（第 2 幀起）再送。
- **mingw 的 ld 打不開非 ASCII 路徑**：連結用的檔案都放在純英文的建置目錄。
- **MicroDVD 偵測要求前三行都是 `{n}{n}` 格式**，只有兩條字幕的檔案會偵測失敗；實際檔案通常有 `{1}{1}幀率` 標頭。
- **mpv 的軌道清單通知比 FileLoaded / PlaybackRestart 晚到**：開檔當下要自己讀一次 `track-list`，否則會讀到「還沒選軌」的舊清單。
- **自己產生的測試樣本不一定重現真實問題**：短的 GBK 字幕 mpv 猜得對，真實的長字幕卻猜錯。
  真實影片庫的普查找到的問題，樣本找不到。
