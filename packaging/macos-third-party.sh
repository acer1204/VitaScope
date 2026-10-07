#!/usr/bin/env bash
# 列出 .app 裡包進去的 Homebrew 函式庫（套件、版本、授權、原始碼與建置配方），並複製各自的授權檔。
# 用法：packaging/macos-third-party.sh dist/VitaScope.app
#
# 只用 macOS 內建的 bash 3.2 也能跑的語法（沒有關聯陣列）。
# 找不到來源、或找不到授權檔的套件會讓這個腳本失敗：發佈的安裝包要列得出每個函式庫的原始碼（GPL 的要求）。

set -euo pipefail
app="$1"
res="$app/Contents/Resources"
out="$res/THIRD-PARTY-MACOS.md"
cellar="$(brew --cellar)"
mkdir -p "$res/licenses"

cat > "$out" <<'EOF'
# macOS 安裝包內含的函式庫

以下函式庫來自 Homebrew，位於 `VitaScope.app/Contents/Frameworks`。
各套件的授權檔在 `VitaScope.app/Contents/Resources/licenses/<套件>/`。
「原始碼」是上游的原始碼（有固定的版本時附上 commit）；「建置配方」是 Homebrew 建置時使用的 formula
（含套用的修正檔與建置選項），連到建置當時 homebrew-core 的那一個版本。

| 套件 | 版本 | 授權 | 原始碼 | 建置配方 |
|---|---|---|---|---|
EOF

seen=" "
missing=0
nolicense=0
for lib in "$app"/Contents/Frameworks/*.dylib; do
  name=$(basename "$lib")
  # 在 Cellar 裡找出這個檔案屬於哪個套件：<cellar>/<套件>/<版本>/lib/<檔名>
  src=$(find "$cellar" -path "*/lib/$name" -print -quit 2>/dev/null || true)
  if [ -z "$src" ]; then
    echo "| \`$name\` | ? | ? | ? | ? |" >> "$out"
    echo "::error::找不到 $name 屬於哪個 Homebrew 套件"
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
  # 從 git 建置的套件（例如 x264）網址本身沒有版本：附上固定的 commit
  url=$(echo "$info" | jq -r '.formulae[0].urls.stable as $s
    | if $s == null then (.formulae[0].homepage // "?")
      elif ($s.revision // "") != "" then "\($s.url) @ \($s.revision)"
      else $s.url end')
  # 建置當時 homebrew-core 的版本（安裝紀錄裡有；沒有的話用目前的）
  receipt="$cellar/$formula/$version/INSTALL_RECEIPT.json"
  head=$(jq -r '.source.tap_git_head // empty' "$receipt" 2>/dev/null || true)
  [ -n "$head" ] || head=$(echo "$info" | jq -r '.formulae[0].tap_git_head // "HEAD"')
  case "$formula" in
    lib*) dir=lib ;;
    *) dir=$(printf '%s' "$formula" | cut -c1 | tr '[:upper:]' '[:lower:]') ;;
  esac
  recipe="https://github.com/Homebrew/homebrew-core/blob/$head/Formula/$dir/$formula.rb"
  echo "| $formula | $version | $license | $url | $recipe |" >> "$out"

  dest="$res/licenses/$formula"
  mkdir -p "$dest"
  find "$cellar/$formula/$version" -maxdepth 2 -type f \
    \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' -o -iname 'NOTICE*' \) \
    -exec cp {} "$dest/" \;
  # 安裝好的套件裡沒有授權檔（例如 glib 放在原始碼的 LICENSES/ 裡）：從 Homebrew 的原始碼壓縮檔取
  if [ -z "$(ls -A "$dest")" ]; then
    if brew fetch --build-from-source --formula "$formula" >/dev/null 2>&1; then
      tarball=$(brew --cache --build-from-source --formula "$formula")
      tmp=$(mktemp -d)
      if tar -xf "$tarball" -C "$tmp" 2>/dev/null; then
        find "$tmp" -maxdepth 4 -type f \
          \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' -o -path '*/LICENSES/*' \) \
          -exec cp {} "$dest/" \;
      fi
      rm -rf "$tmp"
    fi
  fi
  if [ -z "$(ls -A "$dest")" ]; then
    echo "::error::$formula 找不到授權檔"
    nolicense=$((nolicense + 1))
  fi
done

count=$(echo "$seen" | wc -w | tr -d ' ')
echo "列出 $count 個 Homebrew 套件（$missing 個檔案找不到來源、$nolicense 個套件找不到授權檔）"
[ "$missing" -eq 0 ] && [ "$nolicense" -eq 0 ]
