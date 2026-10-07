# 影戲 VitaScope：macOS 版 libmpv.2.dylib 的完整對應原始碼

這個壓縮檔是 macOS 版影戲（Apple Silicon）使用的 `libmpv.2.dylib` 的完整對應原始碼，跟建置時用的檔案逐位元相同。

- `pins.json`：每個元件的版本、下載位置與 SHA-256 / git commit，以及 Xcode、macOS SDK、最低系統版本
- `requirements.txt`：建置工具 meson、ninja 的版本與雜湊
- `upstream/`：各元件的原始碼（官方發佈的壓縮檔，或指定 commit 的 `git archive`），`SHA256SUMS` 是它們的雜湊
- `build/build.sh`：建置腳本（mpv 以 `-Dgpl=false`、FFmpeg 以 LGPL 選項建置，靜態連結成一個 dylib）
- `build/patches/`：建置時套用的修正檔（每個檔案開頭說明原因）
- `build/notices.py`：產生元件清單 `THIRD-PARTY-MACOS.md`
- `build/ffmpeg_notices.py`：從 FFmpeg 實際編譯的檔案找出帶 MIT / BSD 等寬鬆授權聲明的原始碼，附上聲明原文（`licenses/ffmpeg/permissive/`）
- `build/libmpv-macos.yml`：GitHub Actions 的建置流程（主機工具的版本、執行順序）

## 重新建置

在 Apple Silicon 的 Mac 上：

1. 安裝 `pins.json` 指定的 Xcode，並把 `DEVELOPER_DIR` 設成它的 `Contents/Developer`（`pins.json` 的 `toolchain.developer_dir`）
2. 用 Homebrew 安裝 `autoconf automake libtool pkgconf jq`
3. 用 `requirements.txt` 安裝 meson、ninja：

   ```sh
   python3 -m venv venv && venv/bin/pip install --only-binary=:all: --require-hashes -r requirements.txt
   export PATH="$PWD/venv/bin:$PATH"
   ```

4. 建立可寫入的 `/opt/vsbuild`，把這個資料夾放進去後執行：

   ```sh
   bash build/build.sh <輸出目錄>
   ```

建置過程不連網路。dylib 只依賴 macOS 內建的函式庫與 framework（清單在輸出的 `BUILDINFO.txt`）。

授權：整個 dylib 依 LGPL-2.1-or-later 散布。各元件的授權條文在 `upstream/` 各自的原始碼裡；元件清單 `THIRD-PARTY-MACOS.md` 與整理好的
`licenses/` 在建置結果 `vitascope-libmpv-macos-arm64-rN.zip` 裡（同一個 Release）。
