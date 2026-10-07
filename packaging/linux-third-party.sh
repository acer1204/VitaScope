#!/usr/bin/env bash
# 列出 AppImage 裡包進去的函式庫屬於哪個 Ubuntu 套件（原始碼套件、版本），並補齊授權檔：
# - 每個套件的 copyright 檔（linuxdeploy 會放大部分，手動放進去的備用函式庫沒有）
# - copyright 檔參照的授權全文（/usr/share/common-licenses/GPL-2 之類，AppImage 裡原本沒有）
# 用法：packaging/linux-third-party.sh AppDir 輸出.md [略過的檔案（相對於 usr/lib，可用萬用字元）…]
#   本專案自己建置的 libmpv.so.2、替身函式庫不是 Ubuntu 套件，要略過（不然 dpkg 會照檔名錯認成 Ubuntu 的套件）。
#   輸出檔已經有內容（例如 libmpv 的元件清單）時接在後面。
#
# 在建置 AppImage 的同一台機器上執行（要查 dpkg 的資料庫）。找不到來源的函式庫會讓這個腳本失敗：
# 發佈的安裝包一定要列得出每個函式庫的原始碼版本（GPL 的要求）。

set -euo pipefail
appdir="$1"
out="$2"
shift 2
skip=("$@")
doc="$appdir/usr/share/doc"

# shellcheck source=/dev/null
. /etc/os-release
if [ ! -s "$out" ]; then
  echo "# Linux AppImage 內含的函式庫" > "$out"
fi
cat >> "$out" <<EOF

## 其他函式庫（${PRETTY_NAME} 的套件）

以下函式庫來自 ${PRETTY_NAME} 的套件，位於 AppImage 的 \`usr/lib\`。
各套件的授權（Debian 格式的 copyright 檔）在 AppImage 的 \`usr/share/doc/<套件>/copyright\`；
其中參照的 \`/usr/share/common-licenses/…\`（GPL-2、LGPL-2.1 等全文）在 AppImage 的 \`usr/share/common-licenses/\`。
原始碼可用 \`apt-get source <原始碼套件>=<版本>\` 取得，或到 https://launchpad.net/ubuntu/+source/<原始碼套件>/<版本> 下載。

| 函式庫 | 二進位套件 | 原始碼套件 | 版本 |
|---|---|---|---|
EOF

# 系統上同名的檔案屬於哪個套件（AppDir 裡的是複製過來的）。
# update-alternatives 管理的（例如 libblas）要先找到實際的檔案，dpkg 才認得
owner() {
  local name="$1" dir path
  for dir in /usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu; do
    for path in "$dir/$name" "$(readlink -f "$dir/$name" 2>/dev/null || true)"; do
      [ -e "$path" ] || continue
      dpkg -S "$path" 2>/dev/null | head -1 | cut -d: -f1 && return 0
    done
  done
  # 放在子資料夾裡的（例如 pulseaudio/libpulsecommon-*.so）：用檔名找，路徑結尾要完全一樣
  dpkg -S "*/$name" 2>/dev/null \
    | awk -v n="/$name" '{ p = $NF; if (substr(p, length(p) - length(n) + 1) == n) { sub(/:.*/, "", $1); print $1; exit } }' \
    | grep . && return 0
  return 1
}

missing=0
while IFS= read -r lib; do
  rel=${lib#"$appdir/usr/lib/"}
  for pat in "${skip[@]}"; do
    # shellcheck disable=SC2053 # pat 是萬用字元
    [[ $rel == $pat ]] && continue 2
  done
  name=$(basename "$lib")
  pkg=$(owner "$name" || true)
  if [ -z "$pkg" ]; then
    echo "| \`$name\` | ? | ? | ? |" >> "$out"
    echo "::error::找不到 $name 屬於哪個套件"
    missing=$((missing + 1))
    continue
  fi
  src=$(dpkg-query -W -f '${source:Package}\t${source:Version}' "$pkg:amd64" 2>/dev/null \
        || dpkg-query -W -f '${source:Package}\t${source:Version}' "$pkg")
  echo "| \`$name\` | $pkg | ${src%%$'\t'*} | ${src#*$'\t'} |" >> "$out"
  # linuxdeploy 沒放 copyright 檔的（例如手動放進去的備用函式庫）
  if [ ! -e "$doc/$pkg/copyright" ] && [ -e "/usr/share/doc/$pkg/copyright" ]; then
    install -Dm644 "/usr/share/doc/$pkg/copyright" "$doc/$pkg/copyright"
  fi
done < <(find "$appdir/usr/lib" -type f -name '*.so*' | sort)

# copyright 檔參照的授權全文（GPL 要求附上全文）。句尾的「.」不是檔名的一部分（GFDL-1.3. → GFDL-1.3）
licenses=0
while IFS= read -r f; do
  if [ -e "$f" ]; then
    install -Dm644 "$(readlink -f "$f")" "$appdir$f"
    licenses=$((licenses + 1))
  else
    echo "::warning::copyright 檔參照的 $f 不存在"
  fi
done < <(grep -rhoE '/usr/share/common-licenses/[A-Za-z0-9+_-]+([.][A-Za-z0-9+_-]+)*' "$doc" 2>/dev/null | sort -u)

count=$(grep -c '^| `' "$out" || true)
echo "列出 $count 個函式庫、附上 $licenses 份授權全文（$missing 個找不到來源）"
[ "$missing" -eq 0 ]
