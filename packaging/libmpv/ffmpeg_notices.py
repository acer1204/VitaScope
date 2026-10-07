#!/usr/bin/env python3
"""FFmpeg 裡帶寬鬆授權（MIT、BSD、ISC 之類）聲明的原始碼：把聲明原文附在 licenses/ffmpeg/permissive/。

FFmpeg 整體是 LGPL，但有些檔案（例如 libavfilter/ebur128.c 來自 libebur128）另外帶有 MIT / BSD 聲明，
這類授權要求散布二進位檔時附上聲明原文。建置完 FFmpeg 後，從建置資料夾實際編譯出來的每個目的檔（*.o）
找回對應的原始碼（.c / .S / .asm…），再沿著 #include "…" / %include "…" 找出所有用到的原始碼與標頭；
開頭 4000 個字元內有寬鬆授權字句的檔案，把開頭的第一個註解（授權字句在後面另一個註解裡時，連同那個註解）
寫到 <輸出>/<相對路徑>.txt，並寫一份 README.txt 索引。輸出只跟原始碼與目的檔清單有關（排序、不含時間），
兩次建置逐位元相同。

   python3 ffmpeg_notices.py <FFmpeg 原始碼> <FFmpeg 建置資料夾> <輸出資料夾>
   python3 ffmpeg_notices.py <FFmpeg 原始碼> <FFmpeg 建置資料夾> <輸出資料夾> --objects <目的檔清單>
       （清單每行一個相對於建置資料夾的 .o 路徑；給測試用，不必真的建置）
"""
from __future__ import annotations

import argparse
import os
import re
import sys

# 寬鬆授權的字句：MIT / X11、BSD、ISC、舊式的「Permission to use, copy…」與 zlib 的「Permission is granted to anyone」
GRANTS = re.compile(
    r"Permission is hereby granted|Redistribution and use|Permission to use, copy|Permission is granted to anyone", re.I
)
HEAD = 4000
# 目的檔可能的來源（同名的都掃，寧可多附）
SOURCE_EXTS = (".c", ".S", ".asm", ".m", ".cpp")
INCLUDE = re.compile(r'^[ \t]*(?:#[ \t]*include|%[ \t]*include)[ \t]*"([^"]+)"', re.M)


def read(path: str) -> str:
    # latin-1：每個位元組對應一個字元，寫回時逐位元相同（作者名字可能是 UTF-8）
    with open(path, "rb") as f:
        return f.read().decode("latin-1")


def comment_blocks(text: str, asm: bool) -> list[tuple[int, int]]:
    """註解的範圍（/* … */、// 行；.asm 另有 ; 行）。連續的行註解（中間只隔一個換行）合成一段。
    範圍從註解所在那一行的開頭算起"""
    spans: list[tuple[int, int]] = []
    line_comment = False  # 上一段是不是行註解
    i, n = 0, len(text)
    quote = None
    while i < n:
        c = text[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote or c == "\n":
                quote = None
            i += 1
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            spans.append((i, end))
            line_comment = False
            i = end
        elif text.startswith("//", i) or (asm and c == ";"):
            end = text.find("\n", i)
            end = n if end < 0 else end
            prev = spans[-1][1] if spans else 0
            if line_comment and text[prev:i].strip() == "" and text.count("\n", prev, i) <= 1:
                spans[-1] = (spans[-1][0], end)
            else:
                spans.append((i, end))
            line_comment = True
            i = end
        elif c in "\"'" and not asm:
            quote = c
            i += 1
        else:
            i += 1
    return [(text.rfind("\n", 0, a) + 1, b) for a, b in spans]


def notice(text: str, path: str) -> str | None:
    """第一個註解；授權字句在後面的註解裡時，連同含授權字句的每個註解（中間以 [...] 隔開）"""
    hits = [m.start() for m in GRANTS.finditer(text[:HEAD])]
    if not hits:
        return None
    blocks = comment_blocks(text[: HEAD * 4], path.endswith(".asm"))
    if not blocks:  # 沒有註解（不太可能）：保守地取到第一個授權字句那一行結束
        end = text.find("\n", hits[0])
        return text[: len(text) if end < 0 else end] + "\n"
    # 每個授權字句都要落在某個註解裡：沒有的話代表註解的解析有漏，寧可停下來，不要附上不完整的聲明
    if lost := [h for h in hits if not any(a <= h < b for a, b in blocks)]:
        raise ValueError(f"{path}：位置 {lost} 的授權字句不在任何註解裡")
    keep = [0] + [k for k, (a, b) in enumerate(blocks) if any(a <= h < b for h in hits)]
    parts = [text[blocks[k][0]:blocks[k][1]].rstrip() + "\n" for k in sorted(set(keep))]
    return "[...]\n".join(parts)


def main() -> int:
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("build")
    ap.add_argument("out")
    ap.add_argument("--objects", help="目的檔清單（預設：建置資料夾裡所有的 *.o）")
    a = ap.parse_args()
    src, build = os.path.realpath(a.src), os.path.realpath(a.build)

    if a.objects:
        with open(a.objects, encoding="utf-8") as f:
            objs = sorted({l.strip().replace("\\", "/") for l in f if l.strip()})
    else:
        objs = sorted(os.path.relpath(os.path.join(d, n), build).replace(os.sep, "/")
                      for d, _, files in os.walk(build) for n in files if n.endswith(".o"))
    if not objs:
        print(f"::error::{build} 裡沒有目的檔（*.o）", file=sys.stderr)
        return 1

    # 目的檔 → 原始碼（原始碼樹或建置資料夾裡建置時產生的原始碼；同名的都算）
    todo: list[str] = []
    missing = []
    for o in objs:
        stem = o[:-2] if o.endswith(".o") else o
        found = [p for root in (src, build) for ext in SOURCE_EXTS
                 if os.path.isfile(p := os.path.normpath(os.path.join(root, stem + ext)))]
        if not found:
            missing.append(o)
        todo += found
    if missing:
        print("::error::找不到這些目的檔的原始碼：" + " ".join(missing), file=sys.stderr)
        return 1

    # 沿著 #include "…" 找所有用到的檔案（跟 FFmpeg 的 -I 一樣找檔案所在資料夾、原始碼根目錄、建置資料夾；
    # 再加上各函式庫的資料夾，寧可多找）
    roots = [src, build] + [os.path.join(src, d) for d in ("libavformat", "libavcodec", "libavutil", "libavfilter")]
    seen: set[str] = set()
    isfile: dict[str, bool] = {}
    while todo:
        f = todo.pop()
        if f in seen:
            continue
        seen.add(f)
        for inc in INCLUDE.findall(read(f)):
            for d in [os.path.dirname(f)] + roots:
                p = os.path.normpath(os.path.join(d, inc))
                if p not in isfile:
                    isfile[p] = os.path.isfile(p)
                if isfile[p]:
                    todo.append(p)
                    break

    def under(p: str, root: str) -> bool:
        return os.path.commonpath([p, root]) == root

    notices = {}
    for f in seen:
        if under(f, src):
            rel = os.path.relpath(f, src)
        elif f.endswith(".h"):
            continue  # config.h 之類由 configure 產生的標頭，不是原始碼
        else:
            rel = os.path.relpath(f, build)
        n = notice(read(f), f)
        if n is not None:
            notices[rel.replace(os.sep, "/")] = n

    for rel, n in notices.items():
        dest = os.path.join(a.out, rel + ".txt")
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "wb") as f:
            f.write(n.encode("latin-1"))
    lines = [
        "FFmpeg 原始碼裡帶有寬鬆授權（MIT、BSD、ISC 等）聲明的檔案",
        "Files in FFmpeg carrying permissive (MIT / BSD / ISC-style) notices",
        "",
        "這些檔案編譯進了 libmpv（或被編譯進去的檔案引用）。FFmpeg 整體依 LGPL-2.1-or-later 散布，",
        "這些檔案的聲明另外要求附上原文：每個檔案開頭的授權註解在同資料夾的 <路徑>.txt。",
        "These files are compiled into libmpv (or included by files that are). Each file's",
        "notice is reproduced in <path>.txt next to this README.",
        "",
    ] + sorted(notices)
    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.out, "README.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")
    print(f"FFmpeg：{len(objs)} 個目的檔、掃描 {len(seen)} 個原始碼與標頭，{len(notices)} 個帶寬鬆授權聲明")
    for rel in sorted(notices):
        print(f"  {rel}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
