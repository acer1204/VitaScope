# 影戲 VitaScope：Windows 版 libmpv-2.dll 的完整對應原始碼

這個壓縮檔是 Windows 版影戲使用的 `libmpv-2.dll`（x86_64）的完整對應原始碼，跟建置時用的檔案逐位元相同。

- `pins.json`：每個元件的版本、下載位置與 SHA-256 / git commit，以及編譯器（llvm-mingw）的版本
- `upstream/`：各元件的原始碼（官方發佈的壓縮檔，或指定 commit 的 `git archive`），`SHA256SUMS` 是它們的雜湊
- `build/build.sh`：建置腳本（mpv 以 `-Dgpl=false`、FFmpeg 以 LGPL 選項建置，靜態連結成一個 DLL）
- `build/patches/`：建置時套用的修正檔（每個檔案開頭說明原因）
- `build/notices.py`：產生元件清單 `THIRD-PARTY-WINDOWS.md`
- `build/libmpv-windows.yml`：GitHub Actions 的建置流程（主機工具的版本、執行順序）
- `build/toolchain-licenses/`：靜態連結進 DLL 的編譯器執行庫（LLVM、mingw-w64）的授權條文

## 重新建置

在 Ubuntu 24.04（x86_64）上：

1. 安裝 `nasm pkgconf autoconf automake libtool make jq zip xz-utils python3`，以及 `requirements.txt` 指定版本的 meson、ninja
2. 下載 `pins.json` 裡的 llvm-mingw 並核對 SHA-256，解壓到某個目錄
3. 建立可寫入的 `/opt/vsbuild`，把這個資料夾放進去後執行：

   ```sh
   bash build/build.sh <llvm-mingw 目錄> <輸出目錄>
   ```

建置過程不連網路。編譯器執行庫（libc++、libunwind、compiler-rt、mingw-w64）的原始碼另外附在同一個 Release。

授權：整個 DLL 依 LGPL-2.1-or-later 散布；各元件的授權見 `THIRD-PARTY-WINDOWS.md` 與 `licenses/`。
