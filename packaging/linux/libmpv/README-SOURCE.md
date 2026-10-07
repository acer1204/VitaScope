# 影戲 VitaScope：Linux 版 libmpv.so.2 的完整對應原始碼

這個壓縮檔是 Linux AppImage 版影戲（x86_64）使用的 `libmpv.so.2` 的完整對應原始碼，跟建置時用的檔案逐位元相同。

- `pins.json`：每個元件的版本、下載位置與 SHA-256 / git commit，以及建置用的容器映像（digest 固定）、apt 快照日期與套件版本
- `requirements.txt`：建置工具 meson、ninja 的版本與雜湊
- `upstream/`：各元件的原始碼（官方發佈的壓縮檔，或指定 commit 的 `git archive`），`SHA256SUMS` 是它們的雜湊
- `build/setup-base.sh`：在容器裡安裝固定版本的編譯器與 -dev 套件
- `build/build.sh`：建置腳本（mpv 以 `-Dgpl=false`、FFmpeg 以 LGPL 選項建置，靜態連結成一個共用函式庫）
- `build/stubs.py`：產生 AppImage 的替身函式庫（系統沒有 libpulse、libva 時用）
- `build/patches/`：建置時套用的修正檔（每個檔案開頭說明原因）
- `build/notices.py`：產生元件清單 `THIRD-PARTY-LINUX.md`
- `build/libmpv-linux.yml`：GitHub Actions 的建置流程（執行順序）

## 重新建置

1. 用 `pins.json` 的 `base.image` 開一個容器：`docker run -it <base.image> bash`
2. 在容器裡安裝 `jq xz-utils ca-certificates`（`apt-get update && apt-get install -y --no-install-recommends …`）
3. 把這個資料夾放到 `/opt/vsbuild` 底下，在資料夾裡執行：

   ```sh
   bash build/setup-base.sh pins.json
   python3 -m venv /opt/venv && /opt/venv/bin/pip install --only-binary=:all: --require-hashes -r requirements.txt
   export PATH="/opt/venv/bin:$PATH"
   bash build/build.sh <輸出目錄>
   ```

`setup-base.sh` 從 Ubuntu 的 apt 快照（`snapshot.ubuntu.com`）安裝套件；之後的建置不連網路。
glibc、OpenSSL、fontconfig、ALSA、PulseAudio、libva 用使用者系統的（動態連結），不包含在這裡。

授權：整個 libmpv.so.2 依 LGPL-2.1-or-later 散布；各元件的授權見 `THIRD-PARTY-LINUX.md` 與 `licenses/`。
