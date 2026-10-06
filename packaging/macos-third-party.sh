#!/usr/bin/env bash
# 列出 .app 裡包進去的 Homebrew 函式庫（套件、版本、授權、原始碼網址），並複製各自的授權檔。
# 用法：packaging/macos-third-party.sh dist/VitaScope.app
#
# 只用 macOS 內建的 bash 3.2 也能跑的語法（沒有關聯陣列）。

set -euo pipefail
app="$1"
res="$app/Contents/Resources"
out="$res/THIRD-PARTY-MACOS.md"
cellar="$(brew --cellar)"
mkdir -p "$res/licenses"

cat > "$out" <<'EOF'
# macOS 安裝包內含的函式庫

以下函式庫來自 Homebrew，位於 `VitaScope.app/Contents/Frameworks`。
各套件的授權檔在 `VitaScope.app/Contents/Resources/licenses/<套件>/`，原始碼網址是 Homebrew 建置時使用的確切版本。

| 套件 | 版本 | 授權 | 原始碼 |
|---|---|---|---|
EOF

seen=" "
missing=0
for lib in "$app"/Contents/Frameworks/*.dylib; do
  name=$(basename "$lib")
  # 在 Cellar 裡找出這個檔案屬於哪個套件：<cellar>/<套件>/<版本>/lib/<檔名>
  src=$(find "$cellar" -path "*/lib/$name" -print -quit 2>/dev/null || true)
  if [ -z "$src" ]; then
    echo "| \`$name\` | ? | ? | ? |" >> "$out"
    echo "::warning::找不到 $name 屬於哪個 Homebrew 套件"
    missing=$((missing + 1))
    continue
  fi
  rel="${src#"$cellar"/}"
  formula="${rel%%/*}"
  rest="${rel#*/}"
  version="${rest%%/*}"
  case "$seen" in *" $formula "*) continue ;; esac
  seen="$seen$formula "

  info=$(brew info --json=v2 --formula "$formula")
  license=$(echo "$info" | jq -r '.formulae[0].license | if type == "string" then . elif . == null then "?" else tojson end')
  url=$(echo "$info" | jq -r '.formulae[0].urls.stable.url // .formulae[0].homepage // "?"')
  echo "| $formula | $version | $license | $url |" >> "$out"

  mkdir -p "$res/licenses/$formula"
  find "$cellar/$formula/$version" -maxdepth 2 -type f \
    \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' -o -iname 'NOTICE*' \) \
    -exec cp {} "$res/licenses/$formula/" \;
done

count=$(echo "$seen" | wc -w | tr -d ' ')
echo "列出 $count 個 Homebrew 套件（$missing 個檔案找不到來源）"
