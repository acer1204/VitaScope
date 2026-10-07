#!/usr/bin/env python3
"""AppImage 的替身函式庫（usr/lib/fallback/<名稱>/）。

系統沒有 libpulse.so.0 或 libva.so.2 / libva-drm.so.2 時（最小安裝、容器、沒裝 VA-API 的桌面），AppRun 讓
libmpv.so.2 改載入這裡的替身，影戲才啟動得了：
- 進入點回傳 NULL，mpv 就放棄這條路：pa_threaded_mainloop_new → PulseAudio 輸出失敗、改用 ALSA；
  vaGetDisplayDRM → 建不出 VA-API 裝置、改用軟體解碼。第一次呼叫時在 stderr 印一行說明
- 其他函式只有進入點成功之後才會被呼叫；萬一被呼叫就印出名稱並 abort()，不回傳猜測的值
每個替身只定義 libmpv.so.2 從那個函式庫用到的函式，連同符號版本（PULSE_0 之類），名稱與版本取自建置環境裡
真正的函式庫。同時檢查 libmpv.so.2 的每個外部符號都由預期的系統函式庫提供。
   python3 stubs.py <libmpv.so.2> <輸出資料夾> <C 編譯器>
"""
import os, re, subprocess, sys, tempfile
from pathlib import Path

lib, out, cc = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), sys.argv[3]
STUBS = {  # soname: (子資料夾, 回傳 NULL 的進入點, 進入點被呼叫時的說明)
    "libpulse.so.0": ("pulse", {"pa_threaded_mainloop_new"}, "系統沒有 libpulse.so.0，不使用 PulseAudio（改用 ALSA）"),
    "libva.so.2": ("va", set(), ""),
    "libva-drm.so.2": ("va-drm", {"vaGetDisplayDRM"}, "系統沒有 libva，硬體解碼（VA-API）改用軟體解碼"),
}
SYSTEM = {"libc.so.6", "libm.so.6", "ld-linux-x86-64.so.2", "libstdc++.so.6", "libgcc_s.so.1",
          "libssl.so.3", "libcrypto.so.3", "libfontconfig.so.1", "libasound.so.2", *STUBS}
LIBDIRS = [Path("/usr/lib/x86_64-linux-gnu"), Path("/lib/x86_64-linux-gnu"), Path("/lib64")]

def readelf(*args):
    return subprocess.run(["readelf", "-W", *args], check=True, capture_output=True, text=True).stdout

def dynsyms(path):  # (名稱, 版本或 None, 是否已定義, 類型, 綁定)
    for line in readelf("--dyn-syms", str(path)).splitlines():
        f = line.split()
        if len(f) >= 8 and re.fullmatch(r"\d+:", f[0]):
            name, _, ver = f[7].partition("@")
            yield name, ver.lstrip("@") or None, f[6] != "UND", f[3], f[4]

def system_lib(soname):
    for d in LIBDIRS:
        if (d / soname).exists():
            return d / soname
    sys.exit(f"找不到系統的 {soname}")

needed = re.findall(r"\(NEEDED\)\s+Shared library: \[(.+?)\]", readelf("-d", str(lib)))
if extra := sorted(set(needed) - SYSTEM):
    sys.exit(f"libmpv.so.2 依賴了預期以外的函式庫：{extra}")
provider = {}
for so in needed:
    for name, ver, defined, _, _ in dynsyms(system_lib(so)):
        if defined:
            provider.setdefault((name, ver), so)
            provider.setdefault((name, None), so)
wants, missing = {so: set() for so in STUBS}, []
for name, ver, defined, typ, bind in dynsyms(lib):
    if defined or bind == "WEAK":
        continue
    so = provider.get((name, ver))
    if so is None:
        missing.append(f"{name}@{ver}" if ver else name)
    elif so in STUBS:
        if typ != "FUNC":
            sys.exit(f"{so} 的 {name} 是 {typ}，不是函式，沒辦法做替身")
        wants[so].add((name, ver))
if missing:
    sys.exit("這些符號找不到提供它的系統函式庫：" + "、".join(sorted(missing)))

for so, (sub, entries, note) in STUBS.items():
    syms = sorted(wants[so])
    names = {n for n, _ in syms}
    if not syms:
        sys.exit(f"libmpv.so.2 沒有用到 {so}：替身清單要更新")
    if lost := entries - names:
        sys.exit(f"{so} 的進入點 {sorted(lost)} 不在 libmpv.so.2 用到的函式裡：替身要重新檢查")
    # 有版本的符號放進同名的版本節點；沒有版本的（libva 大部分的函式）不寫進版本描述檔，
    # 連結器給它基本版本，跟原函式庫一樣（libva 只有 vaCreateSurfaces 有版本）
    versions = sorted({v for _, v in syms if v})
    unversioned = any(v is None for _, v in syms)
    c = ["#include <stdio.h>", "#include <stdlib.h>",
         "static void unsupported(const char *name) {",
         f'    fprintf(stderr, "vitascope：{so} 的替身不支援 %s（請安裝系統的 {so}）\\n", name);',
         "    abort();", "}"]
    if entries:
        c += ["static void note(void) {", "    static int shown;",
              f'    if (!shown++) fputs("vitascope：{note}\\n", stderr);', "}"]
    for name in sorted(names):
        c.append(f"void *{name}(void) {{ note(); return NULL; }}" if name in entries
                 else f'void {name}(void) {{ unsupported("{name}"); }}')
    d = out / sub
    d.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as t:
        (Path(t) / "stub.c").write_text("\n".join(c) + "\n", encoding="utf-8")
        cmd = [cc, "-shared", "-fPIC", "-O2", f"-Wl,-soname,{so}", "-Wl,-z,noexecstack", "-Wl,--build-id=sha1",
               "-o", str(d / so), str(Path(t) / "stub.c")]
        if versions:
            m = Path(t) / "stub.map"
            m.write_text("".join(f"{v} {{\n  global:\n" + "".join(f"    {n};\n" for n, vv in syms if vv == v)
                                 + ("  local: *;\n" if i == 0 and not unversioned else "") + "};\n"
                                 for i, v in enumerate(versions)),
                         encoding="utf-8")
            cmd.append(f"-Wl,--version-script,{m}")
        subprocess.run(cmd, check=True)
    vers = versions + (["無版本"] if unversioned else [])
    print(f"{sub}/{so}：{len(names)} 個函式（{'、'.join(vers)}），回傳 NULL 的進入點 {sorted(entries) or '無'}")

# 只靠替身也要載入得了（所有符號立即解析）
env = dict(os.environ, LD_BIND_NOW="1", LD_LIBRARY_PATH=":".join(str(out / s) for s, _, _ in STUBS.values()))
ldd = subprocess.run(["ldd", str(lib)], env=env, check=True, capture_output=True, text=True).stdout
for so, (sub, _, _) in STUBS.items():
    if f"{so} => {out / sub / so}" not in ldd:
        sys.exit(f"ldd 沒有用到 {so} 的替身：\n{ldd}")
subprocess.run([sys.executable, "-c", f"import ctypes; ctypes.CDLL({str(lib)!r})"], env=env, check=True)
print("libmpv.so.2 只靠替身也載入得了（LD_BIND_NOW=1）")
