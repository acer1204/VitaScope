#!/usr/bin/env python3
"""libmpv-2.dll 的元件清單：THIRD-PARTY-WINDOWS.md（隨安裝包附上）與 components.json。"""
import json, sys
from pathlib import Path

pins = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
pkg = Path(sys.argv[2])
bid, tc = pins["build_id"], pins["toolchain"]
tag = f"libmpv-win64-{bid}"
src = f"vitascope-libmpv-win64-{bid}-src.tar.xz"
rel = f"https://github.com/acer1204/VitaScope/releases/tag/{tag}"

def source(c):
    if c["kind"] == "git":
        u = c["url"].removesuffix(".git")
        return f"{u} commit [`{c['commit'][:12]}`]({u}/commit/{c['commit']})"
    return f"[{c['file']}]({c['url']})，SHA-256 `{c['sha256']}`"

rows = "\n".join(f"| {c['name']} | {c['version']} | {source(c)} | {c['license']} | `licenses/{c['name']}/` |"
                 for c in pins["components"])
md = f"""# Windows 安裝包內含的元件（libmpv-2.dll）

`libmpv-2.dll` 由本專案從原始碼建置（`{tag}`，`.github/workflows/libmpv-windows.yml`）：mpv 以 `-Dgpl=false`、
FFmpeg 不加 `--enable-gpl` / `--enable-version3` 建置，連同下表的函式庫靜態連結成一個 DLL，整體依 **LGPL-2.1-or-later** 散布。
各元件的授權條文在同資料夾的 `licenses/<元件>/`。

## 對應原始碼

- 全部元件的原始碼（與建置時用的檔案逐位元相同）、建置腳本與設定：`{src}`，附在每個使用這個 DLL 的影戲版本的
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
(pkg / "THIRD-PARTY-WINDOWS.md").write_text(md, encoding="utf-8")
(pkg / "components.json").write_text(json.dumps(
    {"build_id": bid, "toolchain": tc, "components": pins["components"]}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
