# 影戲 VitaScope — 開發規劃

目標：跨平台（Windows / macOS / Linux）、功能看齊 PotPlayer 的影片播放器。
先做出能用的基本版，再依等級逐步補完；每完成一項就測一項。

**目前進度（2026-10-09）**：L1、L2 完成（v0.2.0）。Windows 本機、macOS / Linux（GitHub Actions）自動測試通過；
另用實際影片庫（約 1.4 萬部）抽樣 626 個檔案實測，全部能播放。L3 進階調校進行中：第一批（流暢播放、畫面輸出不卡住介面、
影像與音訊的調校）已完成並在 v0.3.0 發佈，還需要人實際確認的項目列在各項底下的「手動確認」。

---

## 1. 技術選型

### 結論：Rust + egui + libmpv

| 層 | 選用 | 說明 |
|---|---|---|
| 語言 | Rust | 沿用 Zoetrope 的經驗與工具鏈 |
| 介面 | egui / eframe（**glow / OpenGL 後端**） | 與 Zoetrope 同一套 UI 寫法；改用 glow 後端是為了和 mpv 的 OpenGL 渲染介面接起來 |
| 播放引擎 | libmpv（`libmpv2-sys` 綁定 + 自己寫的安全包裝 `src/mpv/`） | 解碼、硬體解碼、音畫同步、字幕渲染、HDR、串流全部由 mpv 處理 |
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

目前的實際結構見 [README](README.md#程式架構)（L2 的播放清單、播放紀錄是 `playlist.rs`、`m3u.rs`、`history.rs`，
檔案關聯、單一執行個體是 `assoc.rs`、`instance.rs`、`macos_open.rs`）。L3 預計新增：

```
src/
├─ bookmarks.rs     書籤（L3）
├─ keymap.rs        自訂快捷鍵（L3）
└─ media_keys.rs    系統媒體鍵、SMTC / 正在播放 / MPRIS（L3）
```

### 各平台的 libmpv 來源

| 平台 | 開發時 | 發佈時 |
|---|---|---|
| Windows | `pwsh scripts/fetch-libmpv.ps1`（本專案建置的 `libmpv-2.dll`） | dll 跟著執行檔一起放 |
| macOS | `bash scripts/fetch-libmpv.sh`（本專案建置的 `libmpv.2.dylib`） | 放進 `.app` 的 `Contents/Frameworks`（只依賴 macOS 內建的函式庫） |
| Linux | 套件管理員安裝 `libmpv-dev` | tar.gz 依賴系統 libmpv；AppImage 內含本專案建置的 `libmpv.so.2` |

三個平台的 libmpv 都由本專案從原始碼建置（`.github/workflows/libmpv-*.yml`）：每個元件固定版本並核對雜湊，
建置兩次確認逐位元相同，發佈成 prerelease `libmpv-<平台>-rN`，連同完整對應原始碼。版本與建置腳本在
`packaging/<平台>/libmpv/`，三個平台共同的元件版本必須一致（`packaging/libmpv/check-pins.sh`）。
自建的 libmpv 只開影戲用得到的解碼器、濾鏡、封裝格式、協定。L3 需要的元件在 2026-10 一次加齊（Windows r3、macOS r4、
Linux r3）：libxml2（DASH）、片段輸出與轉 GIF 的封裝格式與編碼器、等化器與音量正規化等音訊濾鏡、輸出時 HDR 轉 SDR 用的 zimg。
之後還需要更多時，把元件或選項加進 pins.json / build.sh，build_id 加一，重新建置三個平台。

授權：影戲 VitaScope 採用 GPL-3.0-or-later。內含的 libmpv 以 LGPL 選項建置（mpv `-Dgpl=false`、FFmpeg 不加 GPL 選項），
整體是 LGPL-2.1-or-later，不含與 GPL-3.0 不相容的元件，也不會限制影戲本身的授權。原則：別人建置的函式庫有授權或來源問題時，
自己從固定版本的原始碼建置，優先選 LGPL / 寬鬆授權的元件。第三方元件的授權見 `packaging/THIRD-PARTY-NOTICES.md`。

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
| 罕見 | AVS2 / AVS3、Bink | 中國標準、遊戲影片（AVS2 / AVS3 目前不支援：要 davs2 / uavs3d，自建的 libmpv 不含） |

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
| 罕見 | DVB 字幕、Teletext、EIA-608 / 708 隱藏字幕 | 電視錄影、美國串流內嵌（Teletext 不支援：需要的 libzvbi 含 GPL-2.0-only 的程式碼，跟 GPL-3.0 不相容，自建的 libmpv 不含） |

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
README 的「功能清單」是給使用者看的精簡版，打勾時兩邊一起更新。

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
**待確認**：實體鍵盤按 Esc 離開全螢幕（模擬按鍵只收到「放開」，可能是輸入法攔截；
實體鍵盤按 Esc 關閉「字幕外觀」視窗已確認正常）、實際聲音輸出、多螢幕。

### L2 日常主力

**目標：日常使用可以取代 PotPlayer。**

播放控制
- [x] 播放清單：同資料夾自動接續；上一個 / 下一個（PgUp / PgDn、控制列按鈕）；檔名依數字排序（第 2 集在第 10 集前面）；
  一次拖放多個檔案時清單就是那幾個檔案；資料夾在背景掃描（網路磁碟上的大資料夾不會卡住畫面）（自動）
- [x] 播放清單面板（F6、控制列 ☰、右鍵選單）：雙擊播放、拖曳排序（放在列與列之間也算、拖到邊緣自動捲動）、
  Delete 移除（正在播的照樣播完）、依檔名排序、加入檔案 / 資料夾、右鍵複製路徑；打開時捲到正在播的那一項；
  清單開著時拖放的檔案加到最後。開啟 / 儲存 `.m3u` / `.m3u8`（自己解析：Big5 等舊編碼、相對路徑、`file://`、
  IPTV 屬性；HLS 串流的 `.m3u8` 整個交給 mpv）；自己整理的清單關閉時存起來，下次沒指定檔案時還原（自動）
- [x] 續播：記住每個檔案的播放位置（一分鐘以上的檔案；快看完的不記；播放中每 30 秒存一次），Home 從頭播放；
  同時開好幾個播放器也不會互相蓋掉紀錄（自動）
- [x] 最近開啟的檔案：起始畫面、右鍵選單（自動）
- [x] 變速 0.25× – 4×（保持音調）：C / X / Z、右鍵選單（自動）
- [x] 逐格前進 / 後退：. / ,（自動）
- [x] A-B 段落重播：L，進度條上標出區段；開新檔時取消（自動）
- [x] 章節：進度條上的標記與章節名稱（截圖）、Ctrl+PgUp / PgDn 與右鍵選單跳章節（自動）
- [x] 進度條預覽縮圖：另一個 mpv 用軟體繪圖在背景做（只跳關鍵影格、一次一張、最新的要求優先；
  手機直拍會轉正；串流、純音訊不做）（自動、手動目視）

字幕與音訊
- [x] 字幕外觀：字型、大小、顏色、邊框、陰影、位置、粗體、是否套用到 ASS；改了馬上看到，設定會保存（自動）
- [x] 字幕時間軸偏移（[ / ]、選單；換檔時歸零，主字幕和第二字幕一起調）、手動指定外掛字幕的編碼（自動）
- [x] 手動載入外部字幕：拖放、選單「載入字幕檔…」（拖放：自動；檔案對話框：待手動）；跟影片一起拖放也可以
- [x] 雙字幕：字幕選單「第二字幕」，顯示在畫面上方；換檔時關閉（自動）
- [x] 音訊延遲調整：- / =（或 +）、選單（自動）
- [x] 自動載入同名的外掛音軌（`.mka`，字幕組常附評論音軌或 5.1 音軌；實際影片庫有 37 個，全部同名）。
  加入但不自動選：影片庫裡多數是另一種語言的配音，預設仍用影片內建的音軌（自動）
- [x] 純音訊檔播放：顯示專輯封面與歌名、演出者、專輯；沒有封面時顯示在畫面中間（自動、截圖）

畫面
- [x] 長寬比：原始 / 16:9 / 4:3 / 16:10 / 1.85:1 / 2.35:1（A、Ctrl（macOS：⌘）+ F6、右鍵選單；自訂比例未做）（自動）
- [x] 裁切（裁成指定比例、填滿視窗）、縮放、平移、旋轉 90°、左右 / 上下翻轉；換檔時由 mpv 的 reset-on-next-file 還原。
  翻轉用 GLSL 著色器（硬體解碼也能用、字幕不會鏡像），軟體繪圖時改用濾鏡（自動、截圖）
- [x] 視窗置頂：右鍵選單，設定會記住（自動）

操作體驗
- [x] 滑鼠滾輪調音量、右鍵選單（自動）
- [x] OSD 提示（音量、跳轉、暫停、切換軌道時在畫面上顯示）— L1 期間完成
- [x] 截圖：Ctrl+E 存 PNG（原始解析度、可選含不含字幕、預設在「圖片」的 VitaScope，不覆蓋舊檔）、
  Ctrl+C 複製到剪貼簿、另存新檔、變更 / 開啟截圖資料夾。mpv 的軟體截圖不含畫面輸出做的旋轉和翻轉著色器，
  在背景轉正後再存（自動；視窗模式的旋轉、翻轉手動確認過）
- [x] 媒體資訊面板（Ctrl+F1 / Ctrl+I，macOS ⌘+I）：容器、編碼、解析度、位元率、像素格式與位元深度、HDR10 / HLG /
  Dolby Vision、硬體解碼、掉格、影音同步；面板打開時每秒讀一次；可以複製成文字（自動）
- [x] 設定視窗（F5、右鍵選單）：一般（介面語言、置頂）、播放（自動下一個、續播、硬體解碼、跳轉秒數）、
  字幕（字幕外觀）、截圖（資料夾、含字幕）、系統（只開一個視窗、檔案關聯）、快捷鍵一覽；
  改了馬上生效（只開一個視窗是下次開啟時）、馬上存檔（自動）
- [x] 介面語言：繁體中文 / English，切換馬上生效。每個文字在原地同時寫中英文（`tr!` / `tf!`），
  漏寫英文編譯不過；語言記在介面執行緒上，背景執行緒產生的訊息帶著同一個語言（自動）

系統整合與發佈
- [x] 檔案關聯：Windows 寫目前使用者的登錄檔（ProgID、OpenWithProgids、Capabilities；安裝程式和「設定 → 系統」寫同一組，
  解除安裝時程式自己全部移除；免安裝版搬家後啟動時自動更新路徑）。Windows 10 / 11 不讓程式自己設成預設，
  提供開啟「預設應用程式」設定頁的按鈕。macOS 由 Info.plist 宣告類型，Finder 開檔的 Apple Event 自己接（winit 沒有）；
  Linux 由 `.desktop` 的 MimeType 宣告，`install.sh --default` 設成預設（自動：登錄檔讀寫、安裝 / 解除安裝、
  `.desktop` 與 AppStream 驗證；**Finder / 檔案總管雙擊待手動**）
- [x] 單一執行個體：雙擊另一個檔案時送到已開啟的視窗並帶到前面；檔案總管一次開好幾個檔案（每個檔案各啟動一次）
  合併成一個播放清單；可在設定關閉，`--new-window` 只對這次開新視窗。鎖定檔決定主視窗，Windows 用只限同一位使用者的
  具名管道、Unix 用權限 0700 資料夾裡的 socket；主視窗關掉後由下一個接手（自動：多個程式同時啟動、接手、端對端）
- [x] 應用程式圖示：視窗、工作列、Windows 執行檔（含版本資訊）、macOS `.app`、Linux `.desktop`、安裝程式
  （圖檔由 `examples/make_icon.rs` 產生；發佈流程檢查執行檔有版本資訊）
- [x] 打包發佈：Windows zip / 安裝檔（Inno Setup，每位使用者安裝、不需要系統管理員）、macOS `.app` / `.dmg`、
  Linux tar.gz（附 `install.sh`）/ AppImage（包含 libmpv；在沒有 libmpv 的 Debian 13 容器實際播放）（自動：發佈流程實際安裝、執行、解除安裝）
- [x] ✅ 「通用」格式測試矩陣通過（自動：三平台 CI，`tests/formats.rs` 要求常見與通用全數通過）

### L3 進階調校

**目標：PotPlayer 進階使用者常用的功能。**

影像
- [x] 流暢播放：依螢幕的實際更新率同步影像（mpv display-resample + 精確更新率；使用電池、視窗縮小、
  swap 沒等垂直同步、畫面跟不上時自動改回一般播放；電源只在「使用電池時暫停」時查，Linux 播放中在背景執行緒讀 sysfs、啟動時直接讀一次；
  介面停頓 0.3 秒以上之後確認影像跟上聲音，0.5 秒後還差 50 ms 以上就暫時改用一般播放追上，`VITASCOPE_PACING=resync` 一律這樣做）
  （自動；實機 120 Hz 電視 240/240 格都是 5 次更新；介面停 8 秒之後 1 秒內 avsync 在 10 ms 內、之後每格 5 次更新。
  目前預設關閉，在 60 Hz 螢幕上量過之後再改成預設打開）
  - [ ] 手動確認：60 Hz 螢幕的實機節奏（量過之後才改成預設打開）
  - [ ] 手動確認：G-SYNC / FreeSync（VRR）、顯示卡驅動強制關閉垂直同步時改回一般播放並提示
  - [ ] 手動確認：拖曳、改大小視窗不會誤判「跟不上」「沒等垂直同步」
  - [ ] 手動確認：瀏覽器最大化蓋住播放器 30 秒不會誤判「沒等垂直同步」（Windows 不回報被蓋住）
  - [ ] 手動確認：多螢幕（不同更新率）之間拖曳、在系統設定改更新率
  - [ ] 手動確認：縮到最小（含 Win+D）播 1 分鐘再還原，聲音照常、畫面接得上；筆電拔掉、插回電源時暫停與恢復
  - [ ] 手動確認：macOS 的 ProMotion 螢幕與外接螢幕、X11 多螢幕、Wayland 上的狀態顯示（偵測不到更新率）
  - [ ] 手動確認：播放中打開每一種檔案對話框（開啟影片、載入字幕／音軌、播放清單的加入檔案／加入資料夾／開啟／儲存、
    變更截圖資料夾、像素著色器的加入檔案），Windows、Linux 上影片照樣播放、macOS 暫停；Windows 上主視窗按不到；
    Linux 的對話框不一定擋住主視窗（沒有 portal 時用 zenity，可能躲在主視窗後面），再按一次開檔不會開第二個、提示「檔案對話框已經開著」；
    開著時影片播完換到下一個，選好的字幕／音軌不會加到新的影片上；選好檔案之後字幕跟對白同步（流暢播放開、關都試）
- [x] 畫面輸出不再卡住介面：一般播放時離影格的預定時間大約一次螢幕更新（多 2 ms 的餘裕）才取影格（再讓 mpv 等到預定時間），
  介面的執行緒每格只等這麼久（自動；實機 120 Hz 電視上每格的等待從約 39 ms 降到平均約 3.5～6 ms、最久約 11 ms，
  介面一直重畫時每秒畫 120 次（以前 24 次）；交出影格的時間跟以前差不到 1 ms，
  顯示卡畫完影格的時間（GL 的 timestamp query）也跟以前差不多：介面一直重畫又播 4K 10-bit 軟體解碼時本來會晚好幾毫秒，
  量到來不及就自動提早一點取（最多 10 ms），
  每格 5 次更新的比例跟以前差不多（介面閒著、一直重畫、一直重畫又播 4K 10-bit 三種情況，各量三次比平均；
  10 秒的量測每次差好幾個百分點，三次平均的差也在 ±4 個百分點之內上下）。
  `VITASCOPE_PACING=block` 可以改回以前的做法）
  - [ ] 手動確認：一般播放時滑鼠移過進度條、開選單，介面跟著螢幕更新率反應（不再只有每秒 24 次）
- [x] 影像調整：亮度、對比、飽和度、色相、Gamma（mpv 的畫面輸出選項，不用 FFmpeg 的 eq 濾鏡；W/E、R/T、Y/U、I/O、Q 還原，
  Alt+G 控制面板、右鍵選單「畫質」、「設定 → 畫質」；換檔案時沿用，勾「下次開啟時沿用這些調整」才存檔）
  （自動 kittest：按鍵、滑桿、選單、跨檔案沿用與存檔；截圖亮度檢查 `tests/picture_shot.rs`：亮度 +50 時畫面中央的平均亮度
  一般流程 125.0 → 197.0、軟體繪圖的簡化流程 125.9 → 198.2；Linux 的 llvmpipe（自動改用簡化流程）129.3 → 196.0，
  Linux 的 CI（系統的 libmpv 與本專案建置的）每次都跑）
  - [ ] 手動確認：各項調整在實際影片上的效果（特別是色相、Gamma）、按住按鍵連續調整的反應
- [x] 去交錯（自動 / 開啟 / 關閉，預設自動）、去色帶（deband）、銳化：右鍵選單「畫質」、控制面板、「設定 → 畫質」，整個程式共用、存檔；
  選單與提示顯示目前的狀態（mpv 的 deinterlace-active：已去交錯 / 逐行影片）；引擎沒有 deinterlace=auto 時（系統的 libmpv 0.37）
  不列「自動」、當成關閉
  （自動：`tests/picture.rs` 每個值 mpv 都接受、預設設定只有 deinterlace 跟 mpv 不同、交錯的 TS 自動去交錯而逐行的不會；
  kittest 選單、控制面板、設定頁、軟體繪圖時停用、VITASCOPE_MPV_OPTS 指定的不改；
  實機（RTX 3090）：nvdec 解交錯的 MPEG-2 時 bwdif_cuda 建不起來，mpv 改用 hwdownload + bwdif，照樣去交錯，記錄不當成錯誤）
  - [ ] 手動確認：1080i 電視錄影去交錯的效果、去色帶在有色帶的影片上的效果
- [x] 縮放演算法選擇：快速（bilinear）/ 標準（引擎的預設值）/ 高品質（ewa_lanczossharp、抗振鈴 0.6），放大、縮小、色度可以個別指定
  （自動：每個演算法 mpv 都接受、三種畫質的完整選項與個別指定優先的單元測試、kittest 選單與設定頁）
  - [ ] 手動確認：各演算法放大 480p 影片的銳利度
- [x] GLSL 著色器載入與切換：使用者自己的 .glsl 組成「組合」（不附任何著色器），右鍵選單「畫質 ▸ 像素著色器」切換、
  「設定 → 畫質」新增 / 刪除 / 改名、檔案排序（↑ ↓ ✕）；整個程式共用、存檔，換檔照舊。glsl-shaders 由影戲管理：
  清單 = 組合 + 翻轉的著色器，一次換掉整個清單（change-list set）；翻轉在開新檔之前同步拿掉（新檔案第一格就不翻轉），
  開檔前的同步設定被還沒執行的非同步翻轉蓋掉時，開始播新檔時再送一次。加入檔案時檢查：.hlsl / .fx 之類不是 mpv 格式的、
  二進位檔、超過 2 MB、macOS 的 COMPUTE 著色器、路徑有清單分隔字元或不是 UTF-8 的都不收；找不到的檔案略過並提示，組合照舊。
  套用後第一次畫出影格起 3 秒或 30 格內畫面輸出有著色器的錯誤就改回之前用的組合並提示（上一個還沒確認能用就又換了時，
  改回最後一個確定能用的；剛啟動時、改了使用中組合的檔案時改成不使用）。軟體繪圖不跑著色器（選單停用），
  VITASCOPE_MPV_OPTS 指定了 glsl-shaders 時不改（翻轉照樣接在後面；glsl-shader-opts 不算）
  （自動：`picture::shader` 單元測試（檢查檔案、組清單、分隔字元、還原的狀態機）；`tests/picture.rs` 清單換兩次檔照舊、
  翻轉換檔就拿掉、翻轉還沒執行就換檔也不會帶到新檔案、使用者指定的不改；kittest 選單、設定頁的組合編輯、.hlsl 被拒絕、
  畫不出來時還原到確定能用的組合（存檔、提示、設定頁標出來）、軟體繪圖停用、英文介面；截圖檢查 `tests/picture_shot.rs`（RTX 3090）：反相的著色器讓中央平均亮度 16.9 → 238.4，
  編譯不過的著色器記下錯誤（`error C1503: undefined variable`）後自動還原，截圖時 16.9 跟沒有著色器一樣；
  簡化流程（RTX 3090 指定 gpu-dumb-mode、Linux 的 llvmpipe）不送著色器，畫面不變、也不會誤判成壞掉）
  - [ ] 手動確認：Anime4K、FSRCNNX 在 NVIDIA（以及有機會的話 AMD、Intel）顯示卡與 macOS 上實際的效果與速度
- [x] HDR → SDR 色調映射選項：曲線、目標亮度（自動 = 203，或 100–203 nits，只對 HDR 影片送）、色域對應（自動 / 裁切）、
  動態峰值偵測（畫面輸出的 OpenGL 支援時才顯示）；杜比視界 Profile 5 開檔時提示、媒體資訊註明顏色無法正確顯示。
  vo_gpu 的 gamma 曲線不列：它的著色器在 OpenGL 3.3 編譯不過，畫面變成一片藍。
  色調映射在最後輸出到螢幕時做，軟體繪圖的簡化流程也有，所以這些選項在簡化流程照常可以用。
  第十四批修正（對照 mpv 原始碼與真正的 HDR 影片，見第 7 節）：目標亮度超過 203 會把亮部裁成白色，上限改成 203、
  選單只列自動（203）/ 100 / 150，存過 400、1000 的設定讀成 203；SDR 影片（含還不知道、沒有影片）一律送 auto；
  「降低飽和度」跟自動是同一段程式，拿掉（存過的讀成自動）；動態峰值偵測改看 GL context（GLSL 4.20 + compute shader + SSBO）
  （自動：每條曲線、色域、目標亮度 mpv 都接受；目標亮度跟著影片換（HDR10 → 100、SDR → auto、HLG → 100，讀回 mpv 的值）；
  kittest 選單、設定頁、動態峰值偵測依能力顯示、HDR 播放中從選單或設定頁改目標亮度馬上送到 mpv、
  杜比視界 Profile 5 每個檔案提示一次（續播位置之類的提示先顯示完才提示）；
  截圖檢查 `tests/picture_shot.rs`：HDR10 樣本在每條曲線下 mpv 收到設定的曲線、畫面輸出沒有錯誤，
  中央平均亮度 RTX 3090 137–142、Linux 的 llvmpipe 72.6–100.1；
  亮度在 203 nits 以下的 HDR10 樣本（`mkv_hevc10_hdr10_mid`）目標亮度 100 比自動亮：RTX 3090 114.9 / 101.3、簡化流程 114.0 / 100.4、
  Linux 的 llvmpipe（本專案建置的引擎、系統的 libmpv 0.37 一樣）108.5 / 87.2
  （原本的 HDR10 樣本幾乎都是 1000 nits 以上的亮部，141.7 / 141.5 看不出差別）；
  SDR 畫面在目標亮度 100 跟自動逐像素一樣（RTX 3090、llvmpipe 的兩種引擎都是；修正前 100 nits 會把 SDR 也做色調映射：129.7 → 132.6）。
  真正的 HDR 影片（不放進專案，`VITASCOPE_HDR_SAMPLES`）：HEVC HDR10、AV1 HDR10、杜比視界 Profile 5 / 8.1 / 8.4、AV1 HLG、
  VP9 HDR10+ 都開得起來，媒體資訊依序是 HDR10、HDR10、Dolby Vision（Profile 5，顏色無法正確顯示）、Dolby Vision（Profile 8）×2、
  HLG、HDR10（這個 VP9 影片每一格 HDR10+ 的 average_maxrgb 都是 0，libplacebo 當成沒有 HDR10+；
  用 x265 的 `dhdr10-info` 產生的 HEVC HDR10+ 是 HDR10+）；Profile 5 有提示；HEVC HDR10 的中央平均亮度 100 nits 106.0、
  自動 93.8；這台 RTX 3090 的 GL context 是 OpenGL 3.3 · GLSL 3.30，動態峰值偵測不顯示；
  Linux 的 llvmpipe 是 OpenGL 4.5 · GLSL 4.50，可以用。截圖檢查確認引擎功能的 `compute_peak` 跟 GL context 的結果一樣）
  - [ ] 手動確認：真正的 HDR 影片在各曲線、目標亮度下的觀感，Windows 的 HDR 關閉、開啟各看一次
  - [ ] 之後：向 eframe / glutin 要 OpenGL 4.3 以上的 core context，NVIDIA 顯示卡的 Windows 上動態峰值偵測才能用
    （eframe 0.36 用預設的 context 設定，NVIDIA 給 3.3 core；mpv 對 GLSL 4.20 以下關掉 compute shader）

音訊
- [x] 選擇音訊輸出裝置：右鍵選單「音效 ▸ 輸出裝置」、「設定 → 音效」；預設裝置（跟隨系統）＋ 目前輸出方式的裝置
  （macOS 只列 coreaudio）。存了指定的裝置時啟動時同步讀一次裝置清單，不在就暫時用預設裝置並提示（設定照舊）；
  其他時候第一個畫面出來之後才開始觀察清單（列舉裝置可能很慢），清單變了就重新對照：裝置插回來自動切回去、播放中拔掉改用預設裝置
  （音訊輸出開不起來時 mpv 改用 null 輸出繼續播放（audio-fallback-to-null，純音樂檔也不會停）並提示一次，
  之後換裝置、獨佔模式時照新的設定重開，換檔、裝置清單變了時也重開再試一次；音軌被關掉的話（例如 VITASCOPE_MPV_OPTS 改回 mpv 原本的做法），
  換裝置、獨佔模式、轉成立體聲之後把原本的音軌選回來）。
  獨佔模式（audio-exclusive）在 Windows、macOS 顯示，Linux 只在 PipeWire 時顯示；多聲道轉成立體聲（audio-channels=stereo，
  ☑ 混音時避免破音 = audio-normalize-downmix，只在轉成立體聲時開，沒轉時跟 mpv 原本一樣）。
  VITASCOPE_MPV_OPTS（含 include=、profile= 間接）指定的音效選項不改、介面上停用
  （自動：`sound` 單元測試（wasapi、coreaudio、PipeWire 的裝置清單、對照存下的裝置）；`tests/sound.rs` 每個值 mpv 都接受、
  預設設定跟 mpv 原本的值一樣、只送有變的；kittest 選單、設定頁、找不到裝置時改用預設裝置、插回來切回去、拔掉後選回被關掉的音軌（假的裝置清單，CI 沒有音訊裝置也能跑）、
  轉成立體聲之後選回被關掉的音軌、強制用不存在的裝置時改用 null 照樣播放並提示、換檔與裝置清單變了時重開再試（`tests/sound.rs` 對照：不改用 null 時純音樂檔直接結束）、
  媒體資訊跟著換裝置、直通中不能轉立體聲、
  轉成立體聲、使用者指定的不改、英文介面）
  - [ ] 手動確認：實際切換喇叭／HDMI 電視、播放中拔插 USB DAC、獨佔模式時其他程式沒有聲音
- [x] 等化器：十段（31 Hz–16 kHz，每段 −12…+12 dB、0.5 dB 一格），預設平坦、重低音、人聲、古典、搖滾、流行、爵士、電子、
  高音加強與自訂；右鍵選單「音效」（等化器… / ☑ 等化器 / 等化器預設 ▸）、控制面板的「音效」分頁（十段滑桿、還原、
  ☑ 自動防止破音 = 前級降低最高那一段的量）、「設定 → 音效」。影戲自己的 af 濾鏡鏈：
  `@vs-eq:lavfi=[aformat=sample_rates=44100|48000|…,equalizer@b1…b10]`（先換取樣率：32 kHz、22.05 kHz 時 16 kHz 那一段在
  Nyquist 上會不穩定）、`@vs-limit:lavfi=[alimiter@lim=level_in=放大×前級:limit=0.98:level=0:…:latency=1]`。
  af 的字串是唯一的依據：結構改變時整條重設，拖滑桿時先送 af-command 馬上聽得到，放開滑桿、音量鍵停下來 300 毫秒之後改寫字串
  （af-command 的值跳轉後就被字串蓋掉）。引擎沒有 equalizer / aformat / alimiter 時停用；音訊直通時停用，
  開檔前、選音軌前、播放中打開直通時、重新開啟音訊輸出（換裝置、獨佔模式、轉成立體聲）前先清空 af，直通結束送 af "" 再送整條；
  輸出不支援直通（mpv 改回 PCM）時，確定解碼出來的是 PCM 之後設回濾鏡鏈。VITASCOPE_MPV_OPTS 指定了 af 時不改、停用
  （自動：`sound` 單元測試（每種組合的 af 字串、af-command、前級、預設值、延遲改寫的狀態機）；`tests/sound.rs` 每種濾鏡鏈都能播、
  每一段的 af-command 都成功、32 kHz / 22.05 kHz 沒有濾鏡被停用；ao=pcm 寫出 WAV 用 Goertzel 量測：b6（1 kHz）+12 dB →
  1 kHz +12.00 dB、b1 +12 dB → 1 kHz +0.01 dB、自動防止破音 → −0.00 dB；af-command +12 dB 之後改寫字串、跳轉 → +12.00 dB，
  只送 af-command 的話跳轉後 −0.00 dB（引擎的行為，所以要改寫字串）；直通前清空、直通後恢復沒有濾鏡被停用；
  kittest 選單、控制面板拖滑桿變成自訂、拖曳中只送 af-command、放開才存檔與改寫、設定頁、直通中停用、啟動時同步設定、
  換成會直通的音軌之前先清空、ao=null 只接受 float 時（直通開不起來）設回濾鏡鏈、之後轉成立體聲之前又先清空、
  預測不到的直通（VITASCOPE_MPV_OPTS 指定 audio-spdif）開始之後清空、結束之後設回、沒開檔時不送 af-command 直接改寫、
  使用者指定的 af 不改、英文介面）
  - [ ] 手動確認：實際聽各個預設，調整預設的值
- [x] 音量正規化、動態範圍壓縮（夜間模式）：「音效 ▸ 音量平衡」關閉 / 夜間模式（acompressor：−18 dB 以上 4:1、整體 +6 dB）/
  人聲平衡（speechnorm；只讓說話的音量一致，不會把對白拉到音效上面，所以不叫「對白加強」）/ 音量平均（dynaudnorm）。
  loudnorm 不提供（要 3 秒的前瞻、不能即時調整）。各種模式依引擎有沒有那個濾鏡停用，濾鏡鏈後面一定接限幅器
  （自動：ao=pcm 量測：0.5 秒大聲（0.9）、1.5 秒小聲（0.02）輪流的 1 kHz，夜間模式讓大聲與小聲段落的差 33.1 dB → 20.8 dB、
  峰值 0.980；三種模式的 af-command 都成功；kittest 選單、控制面板、設定頁）
  - [ ] 手動確認：實際影片的夜間模式、人聲平衡聽感
- [x] 聲道混音（5.1 → 2.0）：見上面「選擇音訊輸出裝置」的多聲道轉成立體聲（自動：kittest、`tests/sound.rs` 的 audio-channels、
  audio-normalize-downmix；ao=pcm 量測：只有中央聲道（0.3）的 5.1 → 左 0.212、右 0.212）
  - [ ] 手動確認：實際 5.1 影片轉立體聲的對白清晰度
- [x] 音量放大超過 100%：「音效 ▸ 音量上限」100 / 130 / 150 / 200%（設定頁也有）。超過 100% 時 mpv 的音量停在 100，
  多的部分在限幅器的輸入增益放大（三次方，跟 mpv 自己的音量曲線一樣，100% 接得上），不會爆音；引擎沒有 alimiter、
  或使用者指定了 af 時改用 mpv 自己的音量（volume-max，可能破音）。↑ ↓、滾輪、控制列的音量滑桿（範圍到音量上限）都可以超過 100%，
  提示「音量 150%（放大）」；存檔存總音量，但開啟時最多從 100% 開始
  （自動：ao=pcm 量測：−3 dBFS 的 1 kHz 在 200% 時峰值 0.9800、RMS +2.82 dB；level_in ×2 改寫字串、跳轉後 +6.02 dB；
  `tests/sound.rs` 總音量 150 = mpv 100 + 放大 50、沒有限幅器時 volume-max 提高；kittest 上限 150% 時 ↑ 到 150%、
  停下來後 af 的 level_in = 3.375、調低上限時音量跟著拉下來、存下 150% 的音量啟動時是 100%、使用者指定 af 時用 mpv 自己的音量）
  - [ ] 手動確認：150% / 200% 在筆電喇叭、耳機上的聽感（限幅器的失真）
- [x] 音訊直通（AC-3 / DTS / TrueHD 經 HDMI、S/PDIF 送到擴大機）：右鍵選單「音效 ▸ 音訊直通」、「設定 → 音效」勾選格式
  （AC-3、E-AC-3、DTS 預設勾，DTS-HD、TrueHD 要 HDMI 支援 HBR 預設不勾）→ audio-spdif。直通中選單註明「（使用中：AC-3）」，
  開始時提示；音量鍵、滾輪、音量滑桿、靜音（M、按鈕）與變速（C／X、選單的速度）不動作並提示（直通的資料不能調音量、
  不能重新取樣，mpv 的靜音也只是軟體音量；改回正常速度可以，存下的靜音不動），開始直通時靜音或 0% 在提示裡註明，
  速度不是 1× 就改回 1×（不然 mpv 丟掉、重複整個封包）；「載入音軌檔…」跟選單換音軌一樣先預測直通、清空濾鏡鏈；
  流暢播放改用略過或重複影格對齊螢幕（display-vdrop）。mpv 0.41 起播放中切換馬上生效；Linux tar.gz 用的系統 libmpv 0.40 以前
  要到下一個檔案才生效（提示「下一個檔案開始生效」）
  （自動：ao=null 也接受直通，`tests/sound.rs` AC-3、E-AC-3、DTS、TrueHD 樣本實際直通（audio-out-params 是 spdif-*），
  跟 `predict_spdif` 的預測一致，播放中關掉、再打開也會切換（系統的 libmpv 0.37 重新開檔後切換）；kittest 選單、設定頁的格式、直通中擋音量、
  靜音與變速、1.5× 開始直通時改回 1×、載入 AC-3 音軌檔時濾鏡鏈先清空）
  - [ ] 手動確認：接 AV 擴大機／電視實際直通 AC-3、DTS、TrueHD（擴大機顯示格式）、直通時其他程式沒有聲音

功能
- [ ] 自訂快捷鍵（提供 PotPlayer 風格的預設組）
- [ ] 書籤：在檔案中標記時間點並命名
- [ ] 開啟網址：HTTP、HLS（`.m3u8`）、DASH（`.mpd`；播放引擎已含 libxml2，本機的 DASH 有自動測試）
- [ ] 網站影片（透過 yt-dlp），可選畫質（本專案建置的 libmpv 沒有 Lua，不能用 mpv 內建的 ytdl_hook，要由影戲自己呼叫 yt-dlp）
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
- [ ] DVD / 藍光：ISO 或資料夾，依標題播放（不含光碟選單）。藍光要在 libmpv 加上 libbluray（LGPL）；
  DVD 要 libdvdnav / libdvdread（GPL-2.0-or-later，libmpv 會變成 GPL，跟影戲本身相容，但要另外決定）
- [ ] 擷取裝置：視訊鏡頭、擷取卡
- [ ] 網路來源：SMB、FTP、WebDAV 瀏覽，DLNA
- [ ] 遠端控制（手機網頁遙控）
- [ ] HDR 直通、Dolby Vision（⚠ 見風險 R3）
- [ ] Windows HDR 輸出，支援 NVIDIA RTX 視訊增強（RTX Video HDR / 超解析度）：驅動只對走 Direct3D 11 視訊處理的程式生效，
  目前影片畫面經 OpenGL 畫進 egui（8 位元 SDR，HDR 影片由 mpv 色調映射成 SDR），需要另外讓 mpv 用 D3D11 輸出（⚠ 見風險 R3）
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

### 4.3 CI

GitHub Actions 在 Windows / macOS / Ubuntu 三平台建置，並跑格式測試（軟解）與單元測試；Ubuntu 跑兩次，
分別用系統的 libmpv（tar.gz）與 AppImage 內含的 libmpv。三個平台的 libmpv 各有自己的建置流程（`libmpv-*.yml`）：
用新建置的 libmpv 跑完整的測試（macOS、Linux 另外實際開視窗截圖），發佈時一定要同時建置兩次、確認逐位元相同（`repro_check`），
比對通過才發佈成 prerelease。
發佈流程另外在三平台打包、實際執行 `--version`，並在 macOS / Linux 用自動截圖確認影片畫面畫得出來
（Linux 的虛擬螢幕是軟體繪圖，啟動三次、每次都要通過）。安裝程式也實際跑過：Windows 安裝 → 執行 → 檢查登錄檔 →
解除安裝 → 確認清乾淨；macOS 掛載 `.dmg` 檢查簽章並執行；Linux 的 AppImage 在本機與乾淨的 Debian 13 容器各播放一次，
tar.gz 的 `install.sh` 裝到暫時的家目錄檢查選單項目。

### 4.4 目前的測試數量

| 測試 | 數量 | 平台 |
|---|---|---|
| 格式矩陣 `tests/formats.rs` | 98 個樣本（常見 35、通用 37、罕見 26；CI 上 Linux、macOS 的 FFmpeg 少幾個編碼器，產生的樣本比較少） | Windows、macOS、Linux |
| 介面 `tests/ui.rs` | 171 | Windows、macOS、Linux |
| 媒體資訊 `tests/mediainfo.rs`、預覽縮圖 `tests/thumbs.rs` | 4、5 | Windows、macOS、Linux |
| 單一執行個體 `tests/instance.rs`（實際啟動好幾個程式、強制結束主視窗） | 6 | Windows、macOS、Linux |
| 播放核心 `tests/smoke.rs` + 單元測試 | 4 + 248（smoke 在 Linux 多 1 個：執行檔引用的 libmpv 函式；其中 2 個預設略過：1 個需要網路、1 個只印出 Windows 偵測更新率的各種方法；單元測試有些只在特定平台編譯，數字是 Windows 的） | Windows、macOS、Linux |
| 畫質 `tests/picture.rs`、音效 `tests/sound.rs`、非同步設定與引擎功能 `tests/async_opts.rs`、流暢播放 `tests/pacing.rs` | 10、14、8、2 | Windows、macOS、Linux |
| 播放引擎的建置內容 `tests/engine_build.rs` | 7（舊的引擎、系統的 libmpv 略過 L3 元件的部分） | Windows、macOS、Linux |
| 截圖檢查 `tests/picture_shot.rs`（會開視窗） | 6 + 2 個不開視窗的輔助測試 | Windows 本機（RTX 3090）、Linux 的 CI（虛擬螢幕）、macOS 的 libmpv 建置流程 |
| 實機節奏 `tests/pacing_window.rs`（會開全螢幕視窗，手動跑） | 4 + 1 個不開視窗的輔助測試 | Windows 本機（RTX 3090 + 120 Hz 電視） |
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
| R1 | macOS 打包 libmpv 及其相依 dylib 最麻煩 | 已解決：改用本專案建置的單一 `libmpv.2.dylib`（只依賴 macOS 內建的函式庫），打包後實際執行檢查 |
| R2 | macOS 已將 OpenGL 標為棄用 | 目前仍可正常使用（IINA 也是用 OpenGL 接 mpv），持續觀察 |
| R3 | 部分 HDR / Dolby Vision 功能只有 mpv 的 gpu-next 渲染器支援，嵌入式 render API 是否支援要看 mpv 版本；egui 也還沒有 HDR 輸出 | 已確認（第十四批）：render API 用的是舊的 vo_gpu，引擎建置的 libplacebo 關掉了 OpenGL，gpu-next 根本不能用。vo_gpu 不套用杜比視界的 RPU：Profile 8.1 / 8.4 照 HDR10 / HLG 的基礎層播放沒問題，Profile 5 沒有相容的基礎層，顏色是錯的（偏紫、偏綠），目前開檔時提示。要正確顯示 Profile 5，路線是讓引擎的 libplacebo 開 OpenGL、render API 改用 gpu-next；HDR 直通仍需要另外的輸出方式（mpv 原生視窗或 D3D11） |
| R4 | Wayland 不支援把 mpv 嵌進子視窗（`wid`） | 本來就採用 render API，不受影響 |
| R5 | 上次的視窗位置在已拔掉的螢幕上時，視窗可能開在看不到的地方 | 已解決：建立視窗前直接問作業系統有哪些螢幕（Windows、macOS、X11；`src/screens.rs`），上次的標題列不在任何螢幕上就交給系統擺放（eframe 在建立視窗前拿不到螢幕清單，winit 也不讓程式先查）。視窗配合影片後超出螢幕的情況另外處理（v0.2.0） |
| R6 | 全螢幕時字幕上移只對文字字幕（SRT 等）有效，ASS 字幕有自己的版面 | 觀察實際使用情況再決定是否處理 |
| R7 | macOS 的最低需求降到 macOS 11，但只在 GitHub 的 macOS 15 / 26 虛擬機測過，沒有在 11–14 的實機上跑過 | 建置時檢查每個目的檔的最低版本都是 11.0、記錄用到的較新 API（weak imports）；有使用者回報再處理 |
| R8 | AppImage 的 PulseAudio、libva 用系統的，系統沒有時換成只有函式名稱的替身（音訊改走 ALSA、硬體解碼改用軟體解碼） | CI 在沒有這兩個函式庫的 Debian 13 容器實際播放，並強制走到替身的進入點 |
| R9 | Wayland 不讓程式知道視窗在哪個螢幕、更新率多少；XWayland 的 RandR 更新率也只是合成器給的近似值 | 流暢播放在 Wayland 上維持一般播放（狀態顯示「偵測不到更新率」）；有可靠的介面（例如 wp_presentation）再加 |
| R10 | macOS 的 ProMotion（可變更新率）螢幕：回報 120 Hz，OpenGL 的 swap 可能只跑到 60 Hz | 防呆偵測到只跑到一半更新率時改用一半；沒有實機測過，列入手動確認 |
| R11 | 視窗縮到最小時 mpv 的影格沒人取（eframe 不畫隱藏的視窗），原本打算縮小時在背景把到時間的影格交給 mpv（排空影格） | 刻意不做：縮小再還原的實機測試（`pacing_window` 的 minimize_and_restore、`--shot` 縮小時的播放位置）位置都正確，聲音照常、畫面接得上。之後有問題再做；`VITASCOPE_PACING=no-drain` 目前只是保留的名稱 |
| R12 | NVIDIA 硬體解碼的零複製（CUDA 與 OpenGL 共用貼圖）：mpv 重複用解碼的畫面時，可能跟還在畫的上一格撞在一起，大場景切換時偶爾閃一下方塊（使用者回報過一次，還不能穩定重現） | 能重現時先比較關掉硬體解碼、`hwdec=nvdec-copy`、`opengl-glfinish=yes`；要修的話在取新影格前等上一次 render 的 GL fence（`glClientWaitSync`，平常幾微秒），或在自建的 libmpv 改 `hwdec_cuda_gl.c` 每格 map / unmap |
| R13 | 等化器預設、夜間模式、人聲平衡、音量平均的參數是照一般播放器的值和耳朵調的，沒有客觀標準 | ao=pcm 的量測只確認濾鏡有作用、不會破音；實際聽感列入手動確認，依回饋調整 |
| R14 | mpv 的 `tone-mapping=gamma` 著色器在 OpenGL 上編譯不過（對純量做 swizzle），畫面變成整片藍 | HDR 曲線不列 Gamma；之後可以在自建的 libmpv 加修正檔再開放 |
| R15 | 音訊直通在檔案一開始就被輸出拒絕時，這個引擎改回 PCM 之後會停住，跳轉一下才繼續播（第十批測試時發現，見第 7 節） | 測試用 `ao-null-format=float` 模擬並確認濾鏡鏈會設回；停住的部分要在引擎裡查，實機（擴大機、電視）確認時一起看 |

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
- **打包後一定要真的執行一次**：macOS 的 .app 打包成功，執行卻當掉。dylibbundler 會把每條既有的 rpath
  改寫成同一個路徑，產生重複的 LC_RPATH（新版 macOS 拒絕載入），還蓋掉 Swift 執行庫需要的 `/usr/lib/swift`。
  發佈流程現在打包後會執行 `--version`，並檢查 .app 裡沒有參照 Homebrew 路徑。
- **Mesa 的軟體繪圖（llvmpipe）下，mpv 完整的繪圖流程時常畫出全黑的畫面**（大約一半的啟動，沒有任何 GL 錯誤）。
  讀回貼圖的像素確認是 mpv 畫出來的內容就是黑的，不是貼到視窗的問題；改用 `gpu-dumb-mode` 每次都正常。
  現在偵測到 llvmpipe / softpipe 時自動切換，實體顯示卡維持完整流程。這種時有時無的問題，CI 要多啟動幾次才抓得到。
  排查時可以用 `VITASCOPE_DEBUG=1`（印出 mpv 的警告與像素取樣）和 `VITASCOPE_MPV_OPTS="名稱=值 …"`（額外指定 mpv 選項）。
- **跳章節要問 mpv 現在在哪一章，不要自己用時間算**：章節時間常常不對齊影格（藍光、mkvmerge 做的檔案到奈秒），
  跳過去之後畫面的時間比章節時間早一點點，自己算會一直判斷成上一章，「下一章」就卡住了。
- **mpv 的軌道編號是各類分開算的**：影片、音軌、字幕都有 1 號。用編號找軌道一定要限定類型，
  不然換字幕編碼時找到的是影片軌；判斷「選中的音軌」時也不能拿第二字幕的編號來排除。
- **mpv 的設定大多是全域的**：字幕延遲、音訊延遲、A-B 重播、第二字幕換檔後都會沿用，要自己決定哪些換檔時歸零。
- **mpv 0.37 和新版的「原始長寬比」值不同**：0.37 的預設是 -1、`no` 代表把像素當成正方形；0.40 起預設是 -2。
  重設時要寫回 `option-info/video-aspect-override/default-value`。
- **換檔時 mpv 會先還原畫面選項，舊檔案的事件可能晚到**：處理到舊檔案的畫面重設事件時再把調整補設回去，
  就會帶到新檔案。開新檔之後、新檔案開始之前不能補設。
- **可以選取的文字會攔下滑鼠點擊**：egui 的 label 預設可以選取文字，蓋在影片上的文字（例如歌名）要設成不能選取，
  不然點在上面不會暫停、右鍵也叫不出選單。
- **mpv 換旋轉、長寬比時會送兩次 VideoReconfig，第一次還是舊的參數**：只看第一次調整視窗，會用到改之前的形狀。
  改成改完之後一小段時間內每次都重新調整。檔案原本的形狀要讀 `video-dec-params`（不受任何調整影響），
  不要用第一次畫面設定好時的輸出參數（使用者那時可能已經按了調整）。
- **libmpv 的截圖走軟體路徑**：沒開 advanced control（開了跟我們的單執行緒繪圖會死結）時，截圖不含畫面輸出做的旋轉、
  也不含 GLSL 著色器（翻轉）；`screenshot-raw` 還會忽略裁切。要自己轉正。沒有畫面（vo=null）時 mpv 用濾鏡旋轉，
  截圖本來就是轉好的，所以要看 `video-out-params` 的 `rotate`（畫面輸出還要轉多少）。
- **Ctrl+C 不會變成按鍵事件**：egui 把它轉成 `Event::Copy`，要監聽那個事件。
- **`Ui::dnd_drag_source` 一按下就算開始拖**，裡面的按鈕收不到點擊；清單的每一列要用同時能點、能拖的元件
  （`Sense::click_and_drag()`），移動超過幾個像素才算拖曳。
- **右鍵選單比視窗高時，下面的項目點不到**（egui 不會自己捲動），要放進可以捲動的區域。
- **有開斷言的 libmpv（Linux、macOS 的套件）會在內部檢查失敗時直接中止整個程式**，Windows 的建置沒開，
  本機測試抓不到：預覽縮圖的軟體繪圖遇到標示旋轉 90° 的影片，算出的來源範圍超出影格就中止了。
  三個平台的 CI 都要跑同一組測試。
- **介面語言用「原地寫中英文」比對照表好維護**：漏寫英文編譯不過；`format!` 的參數兩邊都會檢查。
  語言記在執行緒上，平行跑的介面測試互不影響，但背景執行緒要自己帶著語言過去。
- **字型裡沒有的符號會變成方塊**：介面上的符號（例如 ✕、⋯）要確認內建字型有，不然換成一定有的（×、…）。
- **測試要先確認抓得到問題**：發佈前的審查找到的問題，先寫測試、再故意把修正拿掉看測試會不會失敗。
  這次有三個測試一開始其實抓不到（樣本只有一個關鍵影格、egui 測試每一幀時間前進 0.25 秒），改過才有用。
- **winit 沒有「Finder 開檔」的事件**，也不能換掉它的 app delegate：在 `NSApplicationWillFinishLaunchingNotification`
  的時候自己裝 Apple Event（`odoc`）處理器。太早裝會被 AppKit 的預設處理器蓋掉，太晚就錯過啟動時的開檔事件。
- **Windows 的具名管道要防別人搶先建立**：用 `FILE_FLAG_FIRST_PIPE_INSTANCE`、只給自己和 SYSTEM 的存取權限、
  拒絕遠端連線，連進來後再確認對方是同一位使用者；名稱帶工作階段編號和 SID，遠端桌面的另一位使用者不會連錯。
- **檔案總管選好幾個檔案按「開啟」，是每個檔案各啟動一次程式**：主視窗要等一小段時間把陸續送來的檔案合併，
  不然只會播到最後一個。
- **解除安裝程式只會移除自己寫的登錄機碼**：安裝後才在程式裡打開的檔案關聯，要讓解除安裝程式呼叫程式自己移除
  （`vitascope --unregister-associations`）。
- **AppImage 刻意不包的函式庫（excludelist），在乾淨的系統上不一定有**：winit 用 dlopen 載入的 libxkbcommon-x11，
  缺了就啟動失敗。只在 CI 機器上測看不出來，要在乾淨的容器再跑一次。要跟系統一致的函式庫（PulseAudio、libva）不包進去，
  系統沒有時改用只有函式名稱的替身（`packaging/linux/libmpv/stubs.py`），讓 libmpv 載入得了。
- **用檔名問 dpkg「這是哪個套件」會認錯**：AppImage 裡本專案建置的 `libmpv.so.2`、替身函式庫，跟 Ubuntu 套件的檔名一樣，
  `dpkg -S` 會把它們算成 Ubuntu 的 libmpv2、libpulse0；列授權清單時要先略過這些檔案。
- **mpv 預設允許共用函式庫留下未定義的符號**（meson 的 `b_lundef=false`）：macOS 版少編譯一個檔案（`osdep/utils-mac.c`）時連結照樣成功，
  選了音訊裝置、播到有聲音的影片才中止，自動測試的影片沒有聲音所以沒抓到。建置時要禁止（`-Db_lundef=true`），並用 `RTLD_NOW` 載入一次。
- **linuxdeploy 會改動它整理的每個函式庫**（用 patchelf 加 RUNPATH）：要跟發佈的檔案逐位元相同的（本專案建置的 libmpv.so.2、替身），
  要換回原檔，最後打包也不能再交給 linuxdeploy（直接用它內附的 AppImage 外掛），並逐位元比對。
- **別人建置的函式庫要逐一查授權**：用來播放的 Windows 版 libmpv-2.dll 裡有 GPL-2.0-only 的 libzvbi，
  跟 GPL-3.0 的 FFmpeg、Apache-2.0 的 OpenSSL 不相容，散布出去就違反授權。改成自己從原始碼建置 LGPL 的 libmpv，
  只放影戲用得到的元件，每個元件固定版本、核對雜湊，Release 附完整對應原始碼。
- **Git Bash 會把 `/D…` 之類的參數當成路徑轉換掉**：Inno Setup 的 ISCC 要在 PowerShell 或 cmd 執行。
- **好幾個視窗共用一個設定檔時，關閉的那個不能把整份舊設定寫回去**：存檔時只寫這個視窗上次讀檔或存檔之後改過的設定，
  其他的保留檔案裡現在的值。新版寫的、這版讀不懂的設定也要保留（只有讀不懂的那一項用預設值）。
  清單也一樣：整個陣列當成一個值的話，另一個視窗新增的像素著色器組合會不見；組合依編號合併，等化器的增益逐格合併。
- **解除安裝時程式還開著，執行檔刪不掉，關閉時還會把設定寫回去**：程式啟動時建立一個具名 mutex，
  安裝程式用 `AppMutex` 先請使用者關掉。
- **檔案關聯只認「登錄的那一個執行檔」**：同一台電腦上有安裝版、免安裝版、開發中的建置時，啟動時不能誰都去搶；
  只有原本登錄的執行檔不在了（免安裝版搬家）才重新登錄。
- **Desktop Entry 的 Exec 要跳脫兩層**（一般字串、再加引號規則），而 GNOME 的 GLib 找程式時用的是還沒展開 `%%` 的路徑：
  路徑裡有 `%` 的話只能改用 PATH 裡的程式。
- **檔案總管一次開很多檔案時，程式陸續啟動、中間會停頓**：已經收到好幾個時要等久一點才算一批；
  等太久先開了前面的，後面到的要接在同一個清單裡，不能把前面的換掉。
- **用腳本改程式碼，每一處都要確認真的改到了**：第四批有兩個修正因為腳本中途出錯沒有寫進去，
  測試也沒抓到（無畫面測試走不到那條路），是第六批的複查才發現。改完要看 diff，修正要有會失敗的測試。
- **FFmpeg 的 DASH 分離器被中斷時回傳「成功但沒有封包」**：H.264 需要解析器，上一層收到空封包會再要一次、自己不檢查中斷，
  兩層之間無限循環；mpv 換檔時等不到分離器結束，整個播放器卡死。只在中斷剛好落在重開片段的時候發生，
  快的電腦幾乎碰不到，macOS 的 CI 虛擬機約三成。卡住的程式用 `sample` / `lldb` 抓堆疊，沒有符號就用
  LC_FUNCTION_STARTS 找出函式、看它參照的字串認出是哪一個。修正檔 `ffmpeg-0002`。
- **跳轉途中 mpv 會先回報目標時間**：測試看到「時間已經到了」不代表最後停在那裡。往後跳超過片尾時，
  mpv 會退回最後一個關鍵影格（測試檔在 10.4 秒），測試只因為剛好看到途中的值才通過，只有慢的 macOS 虛擬機偶爾失敗。
  要多跑幾幀確認最後的狀態。
- **macOS 連結器的 LC_UUID 是對 strip 之前的內容算的**：那部分偶爾不固定，兩次建置就只差在 UUID。
  strip 之後用最終內容重新計算 UUID 再簽章。
- **mpv 的 `af-command` 只是暫時的**：跳轉、換音軌、換格式時 mpv 用 `af` 的字串重建 lavfi 濾鏡，即時改的值就沒了
  （等化器拖到 +12 dB、跳轉之後又回到 0）。即時調整之後一定要改寫字串；mpv 重設 `af` 時參數沒變的濾鏡會留著，只有改到的那一段重建。
  `af` 讀回來的寫法跟設定的不一樣（`lavfi=graph=%長度%…`），不能拿來比對送過什麼。
- **直通開不起來時 mpv 改回 PCM，判斷要看解碼的格式**：只看 `audio-out-params` 不夠，換檔時 mpv 沿用上一個檔案的音訊輸出
  （`gapless-audio` 預設 weak），新檔案的聲音出來之前輸出還是上一個檔案的 PCM；要看 `audio-params/format`（解碼器送進濾鏡鏈的格式，
  直通時是 `spdif-ac3`）。另外這個引擎在檔案一開始就改回 PCM 時會停住（core-idle，沒有濾鏡鏈也一樣），跳轉一下才繼續播
  （測試用 `ao-null-format=float` 模擬，見 `passthrough_refused_restores_the_eq_chain`）。
- **同步的檔案對話框會讓 eframe 整個停住**：rfd 的 `FileDialog` 在介面的執行緒上開，開著時一幀都不畫，
  mpv 照樣播聲音、影像停在那裡（mpv 每 200 ms 記一次 `not being called or stuck`），從 v0.1 就是這樣。
  Windows、Linux 改在背景執行緒開（rfd 的 `FileDialog` 本身可以送到別的執行緒，擁有者照樣是主視窗，Windows 上主視窗照樣按不到），
  結果用 channel 送回來；macOS 的 NSOpenPanel 一定要在主執行緒，開著時先暫停。同時只開一個（Linux 的 portal 對話框不一定擋得住主視窗，
  rfd 改用 zenity 時完全沒有擁有者）。開著時影片照樣播，可能已經換了檔案：字幕、音軌要記下開對話框時是哪個檔案。
- **依螢幕同步時介面停住，mpv 自己會略過晚了的影格追上**：display-resample 也會在差 20 ms 以上時略過影格
  （`handle_display_sync_frame` 的 `drop_repeat`），實測停 0.3～10 秒之後，1080p 約 0.15 秒、4K 軟體解碼 1 秒內就追上，剩 15 ms 左右再由聲音的速度慢慢修正。
  `VITASCOPE_DEBUG=pacing` 10 秒一次的 avsync 平均會把停住剛結束時讀到的那一筆（好幾秒）算進去，看起來像要十幾秒才追上；
  要看每秒的值（現在每秒印一筆）。保險起見（較舊的 mpv、解碼跟不上）停過之後 0.5 秒還差 50 ms 以上才暫時改用一般播放追上。
- **libmpv 預設的 BLOCK_FOR_TARGET_TIME 會卡住介面**：`mpv_render_context_render` 在介面的執行緒上等到影格的預定時間，
  每格約 39 ms，介面每秒只能更新 24 次左右。改成影格快到時間（大約一次螢幕更新內）才取，再讓 mpv 精準等待。
- **egui 的 `request_repaint_after` 會扣掉 `predicted_dt`**（eframe 沒有設定，固定 1/60 秒）：要等 38 ms 會提早約 17 ms 醒來，
  接著幾輪都是馬上重畫，一格影像畫了好幾輪。排喚醒時間要把 `predicted_dt` 加回去。
- **vo_libmpv 沒有回報螢幕更新率**（沒有 `VOCTRL_GET_DISPLAY_FPS`）：嵌在程式裡的 mpv 永遠不會依螢幕同步，
  要自己偵測更新率，用 `display-fps-override` 告訴它，再設 `video-sync=display-resample`。
- **`target_time` 的單位是奈秒**：render.h 的註解寫微秒，已經過時（libmpv 0.37 起都是 `mp_time_ns`）。
  也不能在執行時猜單位：mpv 的時鐘從程式啟動算起，剛啟動時奈秒的數值很小，看起來像微秒。
- **`display-sync-active` 播放中不會更新**：要知道有沒有在依螢幕同步，改看 `mistimed-frame-count` 有沒有值。
- **同步的 `set_property` 會插隊到還沒執行的非同步指令前面**（同步呼叫直接鎖住核心，非同步的是排隊）；非同步指令彼此之間照順序。
  同一個選項同時用兩種方式設定時，要想清楚最後是誰。
- **字串清單選項設成 `""` 會變成有一個空項目的清單 `[""]`**，不是空清單；要清空用 `change-list <選項> clr ""`。
- **Windows：觀察 `audio-device-list` 要讓程式一直有多執行緒 COM（MTA）**：mpv 在自己的執行緒初始化 MTA、關閉時拆掉，
  那是程式裡唯一的 MTA 時，系統的裝置通知還在用，關閉播放器時存取違規。啟動時先 `CoIncrementMTAUsage`，不再減回去。
- **Linux 直接連結新版 libmpv 才有的函式，舊的 libmpv 在程式開始之前就被載入器擋掉**：Rust 預設 `-z now`（完整 RELRO），
  `mpv_get_time_ns`（0.37 起）找不到時連 `--version` 都跑不了，`Mpv::new` 的版本檢查沒機會說明。Linux 改成執行時用
  `dlsym(RTLD_DEFAULT, …)` 找；`tests/smoke.rs` 在 Linux 用 `nm -D --undefined-only` 確認執行檔不直接需要 client API 2.0 之後的函式。
- **音訊輸出開不起來時 mpv 把音軌關掉，之後改裝置、獨佔模式也不會重開**（`reload_audio_output` 沒有輸出就直接返回），
  純音樂檔還會整個停止。開 `audio-fallback-to-null` 改用 null 輸出繼續播放，輸出一直在，改選項時 mpv 照新的設定重開；
  `current-ao` 變成 null（ao 不是自己指定 null）就是開不起來改用的，提示使用者。直通開不起來時 mpv 不用 null（先改回 PCM）。
  不過換檔時 mpv 沿用同一個輸出（gapless-audio 預設 weak，格式一樣就不重開），預設裝置的清單變了也不會自己重開：
  不處理的話之後的檔案一直沒有聲音，所以改用 null 之後換檔、裝置清單變了時送 `ao-reload` 再試真正的裝置。
  重開之後觀察到的 `current-ao` 可能還是重開前的 null（值一樣不會再通知），等一下直接問 mpv 再決定要不要提示。
- **vo_gpu 的 target-peak 超過 203 = 假裝輸出到 HDR 螢幕**：輸出的特性不知道時 mpv 當成 SDR，峰值 203 nits（MP_REF_WHITE）。
  目標亮度超過 203 時 `pass_color_map` 把它當成 HDR 輸出，最後的縮放用轉換函數的標稱峰值（gamma 2.2 是 1.0），
  203 以上到目標亮度之間的亮部直接裁成白色，不是壓縮。輸出 SDR 的播放器目標亮度只能 ≤ 203。
- **target-peak 也會改到 SDR 影片**：mpv 把 SDR 影片當成 203 nits，目標 100 時 SDR 影片也被色調映射、變亮變平，
  超過 203 時輸出改用 gamma 2.2；字幕、OSD 也走同一條路。只有 auto 不動 SDR，所以目標亮度只對 HDR 影片送。
- **hdr-compute-peak 要 GLSL 4.20**：mpv 對 GLSL 4.20 以下關掉 compute shader（有些驅動在舊版也宣稱支援），
  eframe 要的是 3.3 core，驅動可以給更新的：NVIDIA 的 Windows 驅動就給 3.3、macOS 是 4.1，動態峰值偵測從來沒有作用過
  （記錄裡的「Disabling HDR peak computation」）；AMD、Intel、Mesa 常給 4.6，所以要在執行時看 context，不能寫死平台。
- **HDR10+ 的平均亮度是 0 就當成沒有**：libplacebo（`pl_hdr_metadata_contains`）要 average_maxrgb 不是 0 才算 HDR10+，
  mpv 的 `video-params` 才有 scene-max-*。有些 HDR10+ 範例影片每一格都是 0，看起來是 HDR10；不是引擎拿不到。
- **HDR 的標記不一定在容器層**：只標在 HEVC 位元流裡的 HDR，ffprobe 的串流資訊（容器層）寫 unknown，mpv 解出影格後才知道是 PQ；
  杜比視界 Profile 5 的串流層 transfer 也是 unknown。y4m、沒有標記的原始影像沒有色彩資訊。判斷 HDR 要看 mpv 的 `video-params`，
  不是容器或 ffprobe。另外 `video-params` 是濾鏡之前的參數，用 `vf=format=gamma=pq` 改標記不會讓它變成 HDR。
- **產生的正弦波 WAV 可能被當成 MPEG-TS**：48 kHz 的 1 kHz 浮點正弦波每 192 位元組重複一次，剛好是 M2TS 的封包長度，
  FFmpeg 的偵測給 MPEG-TS 滿分、開不起來。測試指定 `demuxer-lavf-format=wav`。本專案建置的引擎也沒有 FFmpeg 的 `sine` 來源，
  測試用的聲音都在測試裡產生。
