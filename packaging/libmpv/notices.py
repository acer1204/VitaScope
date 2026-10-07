#!/usr/bin/env python3
"""本專案建置的 libmpv 的元件清單（隨安裝包附上）與 components.json。
   python3 notices.py <pins.json> <輸出資料夾>
平台取自 pins.json 的 platform（win64、macos-arm64、linux-x64；沒寫就是 win64）。"""
import json, sys
from pathlib import Path

pins = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
pkg = Path(sys.argv[2])
plat, bid = pins.get("platform", "win64"), pins["build_id"]
tag = f"libmpv-{plat}-{bid}"
src = f"vitascope-libmpv-{plat}-{bid}-src.tar.xz"
rel = f"https://github.com/acer1204/VitaScope/releases/tag/{tag}"

def source(c):
    if c["kind"] == "git":
        u = c["url"].removesuffix(".git")
        return f"{u} commit [`{c['commit'][:12]}`]({u}/commit/{c['commit']})"
    return f"[{c['file']}]({c['url']})，SHA-256 `{c['sha256']}`"

rows = "\n".join(f"| {c['name']} | {c['version']} | {source(c)} | {c['license']} | `licenses/{c['name']}/` |"
                 for c in pins["components"])
TABLE = f"""## 元件清單

| 元件 | 版本 | 原始碼 | 授權 | 授權條文 |
|---|---|---|---|---|
{rows}
"""
CREDITS = """## 致謝

- Portions of this software are copyright © 2026 The FreeType Project (https://freetype.org). All rights reserved.
- This software is based in part on the work of the Independent JPEG Group.
"""
INTRO = ("由本專案從原始碼建置（`{tag}`，`.github/workflows/{wf}`）：mpv 以 `-Dgpl=false`、\n"
         "FFmpeg 不加 `--enable-gpl` / `--enable-version3` 建置，連同下表的函式庫靜態連結成一個{kind}，"
         "整體依 **LGPL-2.1-or-later** 散布。\n各元件的授權條文在同資料夾的 `licenses/<元件>/`。")

def win64():
    tc = pins["toolchain"]
    md = f"""# Windows 安裝包內含的元件（libmpv-2.dll）

`libmpv-2.dll` 由本專案從原始碼建置（`{tag}`，`.github/workflows/libmpv-windows.yml`）：mpv 以 `-Dgpl=false`、
FFmpeg 不加 `--enable-gpl` / `--enable-version3` 建置，連同下表的函式庫靜態連結成一個 DLL，整體依 **LGPL-2.1-or-later** 散布。
各元件的授權條文在同資料夾的 `licenses/<元件>/`。

## 對應原始碼

- 全部元件的原始碼（與建置時用的檔案逐位元相同）、建置腳本、設定與修正檔（`build/patches/`）：`{src}`，附在每個使用這個 DLL 的影戲版本的
  Release 上，也在 {rel}。解開後執行 `bash build/build.sh <llvm-mingw 目錄> <輸出目錄>` 可重新建置。
- 編譯器：[llvm-mingw {tc['version']}]({tc['url']})（SHA-256 `{tc['sha256']}`；{tc['llvm']}、mingw-w64 `{tc['mingw_w64'][:12]}`、UCRT）。
  它的 libc++、libunwind、compiler-rt 與 mingw-w64 執行庫有一部分靜態連結在 DLL 裡，原始碼在 {rel}。

## 元件清單

| 元件 | 版本 | 原始碼 | 授權 | 授權條文 |
|---|---|---|---|---|
{rows}
| LLVM 執行庫（libc++、libc++abi、libunwind、compiler-rt builtins） | {tc['llvm']} | `llvm-project-*.src.tar.xz` | Apache-2.0 WITH LLVM-exception | `licenses/llvm/` |
| mingw-w64 執行庫（CRT、winpthreads） | `{tc['mingw_w64'][:12]}` | `mingw-w64-*.tar.xz` | ZPL-2.1 AND MIT AND BSD-3-Clause 等（見條文） | `licenses/mingw-w64/` |

## 致謝

- Portions of this software are copyright © 2026 The FreeType Project (https://freetype.org). All rights reserved.
- This software is based in part on the work of the Independent JPEG Group.

## 沒有包含

libzvbi（Teletext）、OpenSSL、x264 / x265 等編碼器、Lua、Vulkan：影戲用不到，或授權與 GPL-3.0 不相容。
"""
    return "THIRD-PARTY-WINDOWS.md", md, {"build_id": bid, "toolchain": tc, "components": pins["components"]}

def macos():
    tc = pins["toolchain"]
    md = f"""# macOS 版內含的元件（libmpv.2.dylib）

`VitaScope.app/Contents/Frameworks/libmpv.2.dylib` {INTRO.format(tag=tag, wf="libmpv-macos.yml", kind=" dylib")}

## 對應原始碼

- 全部元件的原始碼（與建置時用的檔案逐位元相同）、建置腳本、設定與修正檔（`build/patches/`）：`{src}`，
  附在每個使用這個 dylib 的影戲版本的 Release 上，也在 {rel}。
  解開後在 Apple Silicon 的 macOS 上執行 `bash build/build.sh <輸出目錄>` 可重新建置（步驟見包裡的 `README.md`）。
- mpv 的 `meson.build` 有兩處修改（`build/patches/`，檔案開頭說明原因）：沒有 Cocoa 介面程式碼時也啟用 VideoToolbox 的 OpenGL 互通，
  也編譯 CoreAudio 音訊輸出用到的 `osdep/utils-mac.c`。
- 編譯器：Xcode {tc['version']}（{tc['build']}）、macOS SDK {tc['sdk']}；最低系統版本 macOS {tc['deployment_target']}。
  dylib 只用到 macOS 內建的函式庫與 framework（libSystem、libc++、CoreFoundation、VideoToolbox、Security 等，完整清單在
  `BUILDINFO.txt`），它們屬於作業系統，不隨影戲散布。編譯器的 compiler-rt 執行庫有一小部分靜態連結在 dylib 裡
  （Apache-2.0 WITH LLVM-exception，以二進位形式散布不需要另附聲明）。

## 換成自己建置的 libmpv

可以把 `VitaScope.app/Contents/Frameworks/libmpv.2.dylib` 換成自己修改、重新建置的版本（相同的 libmpv C API），
換完後執行 `codesign --force --deep --sign - VitaScope.app` 重新簽章。

{TABLE}
{CREDITS}
## 沒有包含

libzvbi（Teletext）、OpenSSL、x264 / x265 等編碼器、Lua、Vulkan、mpv 的 Swift / Cocoa 介面程式碼：影戲用不到，或授權與 GPL-3.0 不相容。
"""
    return "THIRD-PARTY-MACOS.md", md, {"platform": plat, "build_id": bid,
            "deployment_target": tc["deployment_target"], "toolchain": tc, "components": pins["components"]}

def linux():
    base = pins["base"]
    md = f"""# Linux AppImage 內含的元件（libmpv.so.2）

AppImage 裡的 `usr/lib/libmpv.so.2` {INTRO.format(tag=tag, wf="libmpv-linux.yml", kind="共用函式庫")}

## 對應原始碼

- 全部元件的原始碼（與建置時用的檔案逐位元相同）、建置腳本、設定與修正檔（`build/patches/`）：`{src}`，
  附在每個包含這個 libmpv.so.2 的影戲版本的 Release 上，也在 {rel}。
  解開後在 Ubuntu 24.04 容器（`{base['image']}`）裡依序執行 `bash build/setup-base.sh pins.json`、
  `bash build/build.sh <輸出目錄>` 可重新建置（步驟見包裡的 `README.md`）。
- mpv 的 `meson.build` 有一處修改（`build/patches/`）：VA-API 硬體解碼不再連帶要求 KMS 畫面輸出（libdrm、libdisplay-info）。
- 編譯器：Ubuntu 24.04 的 GCC（gcc-13 {base['packages']['gcc-13']}），套件取自 Ubuntu apt 快照 `{base['apt_snapshot']}`。
  GCC 由標頭檔產生的程式碼適用 GCC Runtime Library Exception，沒有另外的散布條件。

## 由系統提供（不包含在 AppImage 裡）

glibc、libstdc++、libgcc_s、OpenSSL 3（`libssl.so.3`、`libcrypto.so.3`：HTTPS 與憑證檢查）、fontconfig（字型）、
ALSA（`libasound.so.2`）、PulseAudio 用戶端（`libpulse.so.0`）、libva（`libva.so.2`、`libva-drm.so.2`：Intel / AMD 硬體解碼）、
OpenGL 驅動程式；NVIDIA 的 `libcuda.so.1`、`libnvcuvid.so.1` 在需要時才載入。這些要跟系統的音效伺服器、顯示卡驅動與
安全性更新一致，所以用系統的。

## 替身函式庫（`usr/lib/fallback/`）

系統沒有 `libpulse.so.0` 或 libva 時，AppImage 的啟動腳本（AppRun）改用這裡的替身，影戲才啟動得了：音訊改走 ALSA，
硬體解碼改用軟體解碼。替身由本專案的 `build/stubs.py` 產生，只有函式名稱、沒有原函式庫的程式碼，依影戲的授權（GPL-3.0-or-later）散布。

## 換成自己建置的 libmpv

用 `--appimage-extract` 解開後替換 `squashfs-root/usr/lib/libmpv.so.2`，或執行時用 `LD_LIBRARY_PATH` 指向自己建置的版本（相同的 libmpv C API）。

{TABLE}
{CREDITS}
## 沒有包含

libzvbi（Teletext）、x264 / x265 等編碼器、Lua、Vulkan、mpv 的 X11 / Wayland / DRM 畫面輸出、JACK、VDPAU：影戲用不到，或授權與 GPL-3.0 不相容。
"""
    return "THIRD-PARTY-LINUX.md", md, {"platform": plat, "build_id": bid, "base": base, "components": pins["components"]}

name, md, meta = {"win64": win64, "macos-arm64": macos, "linux-x64": linux}[plat]()
(pkg / name).write_text(md, encoding="utf-8")
(pkg / "components.json").write_text(json.dumps(meta, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
