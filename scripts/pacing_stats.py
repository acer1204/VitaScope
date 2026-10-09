#!/usr/bin/env python3
"""流暢播放的量測：分析 mpv 的 dump-stats 與記錄檔（log-file）。

用法：
  python scripts/pacing_stats.py <資料夾> [螢幕更新率 Hz] [略過開頭幾秒]

<資料夾> 裡要有 tests/pacing_window.rs 產生的 stats.txt（dump-stats）與 mpv.log（log-file，
msg-level=all=v,cplayer=trace）。自己量的時候這樣開影戲（路徑不能有空白）：

  set VITASCOPE_MPV_OPTS=dump-stats=C:/tmp/p/stats.txt log-file=C:/tmp/p/mpv.log msg-level=all=v,cplayer=trace
  vitascope.exe --new-window 影片.mkv

會印出：
- 每格影像的時間（mpv 交出影格 → 影戲取走 → flip 完成）：影戲比預定的時間晚多少取走影格
- flip 間隔換算成幾次螢幕更新的分布（一般播放時 23.976 fps 在 120 Hz 上應該是 5，偶爾 6）
- 依螢幕同步時每格的更新次數（cplayer trace 的 vsyncs=），以及 vo-delayed / drop-vo 的次數
"""

from __future__ import annotations

import re
import statistics as st
import sys
from pathlib import Path


def stats_report(path: Path, vsync_hz: float, skip_s: float) -> None:
    """dump-stats：每一行是「奈秒時間 事件」或「奈秒時間 value 數值 名稱」"""
    vs = 1000.0 / vsync_hz
    events: list[tuple[float, str]] = []
    values: dict[str, list[tuple[float, float]]] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        parts = line.split()
        if len(parts) < 2:
            continue
        try:
            t = int(parts[0]) / 1e6  # 奈秒 → 毫秒
        except ValueError:
            continue
        if parts[1] == "value" and len(parts) >= 4:
            values.setdefault(parts[3], []).append((t, float(parts[2])))
            continue
        events.append((t, " ".join(parts[1:])))

    # 每一格新的影像：畫面輸出交出影格（更新通知）→ 等到預定時間 → 影戲取走（render）→ flip 完成
    frames = []
    cur: dict | None = None
    last_noframe = -1e9
    for t, e in events:
        if e == "glcb-noframe":
            last_noframe = t
        elif e == "end video-draw":
            cur = {"draw_end": t}
        elif e == "start video-flip" and cur is not None:
            cur["flip_start"] = t
        elif e.startswith("glcb-render") and cur is not None and "render" not in cur and t - last_noframe > 0.05:
            cur["render"] = t
        elif e == "end video-flip" and cur is not None:
            cur["flip_end"] = t
            frames.append(cur)
            cur = None

    t0 = frames[0]["draw_end"] if frames else 0
    frames = [f for f in frames if "render" in f and "flip_start" in f and f["draw_end"] - t0 > skip_s * 1000]
    print(f"[dump-stats] 分析 {len(frames)} 格（略過開頭 {skip_s} 秒）")
    if not frames:
        return
    late = [f["render"] - f["flip_start"] for f in frames]
    lead = [f["flip_start"] - f["draw_end"] for f in frames]
    print(f"  影格提早交出（ms）：中位數 {st.median(lead):.2f}  最少 {min(lead):.2f}  最多 {max(lead):.2f}")
    print(f"  影戲取走影格比預定晚（ms）：中位數 {st.median(late):.2f}  "
          f"p95 {sorted(late)[int(len(late) * 0.95)]:.2f}  最多 {max(late):.2f}")
    print(f"  晚取走的格數：{sum(1 for x in late if x > 0)}；晚超過一次更新：{sum(1 for x in late if x > vs)}")
    fe = [f["flip_end"] for f in frames]
    d = [b - a for a, b in zip(fe, fe[1:])]
    if d:
        print(f"  flip 間隔（ms）：中位數 {st.median(d):.3f}  標準差 {st.pstdev(d):.3f}  最短 {min(d):.3f}  最長 {max(d):.3f}")
        print("  flip 間隔（幾次更新）：", histogram(round(x / vs) for x in d))
    for name in ("vsync-diff", "jitter"):
        v = [x for (t, x) in values.get(name, []) if t - t0 > skip_s * 1000]
        if not v:
            continue
        if name == "vsync-diff":
            v = [x * 1000 for x in v]
        print(f"  {name}：{len(v)} 筆  中位數 {st.median(v):.4f}  標準差 {st.pstdev(v):.4f}  "
              f"最小 {min(v):.4f}  最大 {max(v):.4f}")
        if name == "vsync-diff":
            print("    換算成幾次更新：", histogram(round(x / vs) for x in v))
    for name in ("vo-delayed", "drop-vo"):
        n = sum(1 for t, e in events if e == name and t - t0 > skip_s * 1000)
        print(f"  {name}：{n} 次")


LOG_LINE = re.compile(r"^\[\s*([0-9.]+)\]\[(\w)\]\[([^\]]+)\] (.*)$")


def log_report(path: Path, skip_s: float) -> None:
    """mpv 的記錄：依螢幕同步用的更新率、每格的更新次數、等不到畫面的時間"""
    vsyncs: list[tuple[float, int]] = []
    stuck: list[float] = []
    assumed = None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        m = LOG_LINE.match(line)
        if not m:
            continue
        t, text = float(m.group(1)), m.group(4)
        if "FPS for display sync" in text and assumed is None:
            assumed = text
        elif " vsyncs=" in text or text.startswith("vsyncs="):
            n = re.search(r"vsyncs=(\d+)", text)
            if n:
                vsyncs.append((t, int(n.group(1))))
        elif "not being called or stuck" in text:
            stuck.append(t)
    print(f"[mpv.log] {assumed or '沒有依螢幕同步（沒有 Assuming … FPS）'}")
    if vsyncs:
        t0 = vsyncs[0][0]
        inside = [n for t, n in vsyncs if t - t0 >= skip_s]
        fives = sum(1 for n in inside if n == 5)
        print(f"  每格更新次數（略過開頭 {skip_s} 秒）：{len(inside)} 格，分布 {histogram(inside)}，"
              f"5 次的比例 {fives * 100 / max(len(inside), 1):.2f}%")
    print(f"  等不到畫面（render() not being called or stuck）：{len(stuck)} 次 {stuck[:10]}")


def histogram(items) -> dict:
    h: dict = {}
    for k in items:
        h[k] = h.get(k, 0) + 1
    return dict(sorted(h.items()))


def main() -> int:
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    folder = Path(sys.argv[1])
    vsync_hz = float(sys.argv[2]) if len(sys.argv) > 2 else 120.0
    skip_s = float(sys.argv[3]) if len(sys.argv) > 3 else 1.0
    found = False
    if (folder / "stats.txt").exists():
        stats_report(folder / "stats.txt", vsync_hz, skip_s)
        found = True
    if (folder / "mpv.log").exists():
        log_report(folder / "mpv.log", skip_s)
        found = True
    if not found:
        print(f"{folder} 裡沒有 stats.txt 或 mpv.log", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
