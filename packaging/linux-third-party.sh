#!/usr/bin/env bash
# 列出 AppImage 裡包進去的函式庫屬於哪個 Ubuntu 套件（原始碼套件、版本、授權檔位置）。
# 用法：packaging/linux-third-party.sh AppDir 輸出.md
#
# 在建置 AppImage 的同一台機器上執行（要查 dpkg 的資料庫）。

set -euo pipefail
appdir="$1"
out="$2"

# shellcheck source=/dev/null
. /etc/os-release
cat > "$out" <<EOF
# Linux AppImage 內含的函式庫

以下函式庫來自 ${PRETTY_NAME} 的套件，位於 AppImage 的 \`usr/lib\`。
各套件的授權（Debian 格式的 copyright 檔）在 AppImage 的 \`usr/share/doc/<套件>/copyright\`。
原始碼可用 \`apt-get source <原始碼套件>=<版本>\` 取得，或到 https://launchpad.net/ubuntu/+source/<原始碼套件>/<版本> 下載。

| 函式庫 | 二進位套件 | 原始碼套件 | 版本 |
|---|---|---|---|
EOF

missing=0
while IFS= read -r lib; do
  name=$(basename "$lib")
  # 系統上同名的檔案屬於哪個套件（AppDir 裡的是複製過來的）
  pkg=$(dpkg -S "/usr/lib/x86_64-linux-gnu/$name" "/lib/x86_64-linux-gnu/$name" 2>/dev/null | head -1 | cut -d: -f1 || true)
  if [ -z "$pkg" ]; then
    echo "| \`$name\` | ? | ? | ? |" >> "$out"
    echo "::warning::找不到 $name 屬於哪個套件"
    missing=$((missing + 1))
    continue
  fi
  src=$(dpkg-query -W -f '${source:Package}\t${source:Version}' "$pkg:amd64" 2>/dev/null \
        || dpkg-query -W -f '${source:Package}\t${source:Version}' "$pkg")
  echo "| \`$name\` | $pkg | ${src%%$'\t'*} | ${src#*$'\t'} |" >> "$out"
done < <(find "$appdir/usr/lib" -type f -name '*.so*' | sort)

count=$(grep -c '^| `' "$out" || true)
echo "列出 $count 個函式庫（$missing 個找不到來源）"
