#!/usr/bin/env bash
# 下載本專案建置的 libmpv 到 vendor/libmpv/<平台>/
#   macOS（Apple Silicon）：開發與發佈都要（.app 內含這個 libmpv.2.dylib）
#   Linux（x86_64）：只有 AppImage 用（tar.gz 與一般開發用系統的 libmpv；要用這份就設 VITASCOPE_LIBMPV=vendor）
# 來源：.github/workflows/libmpv-<macos|linux>.yml 發佈的 prerelease（tag libmpv-<平台>-rN；不會被當成影戲的新版本）
# 用法：bash scripts/fetch-libmpv.sh [--with-source <目錄>]
#   --with-source：另外下載對應原始碼包（發佈流程用：每個影戲 Release 都要附上）
# 版本固定在下面的 tag 與 SHA-256（取自該 release 的 SHA256SUMS），更新時一起改。（macOS 內建的 bash 3.2 也能跑）
set -euo pipefail
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) plat=macos-arm64; tag=libmpv-macos-arm64-r4; ext=zip
    sha=04a7ffd60397274eb58ea2d2c83222c9530b30e067c5355349c412f61b7e61f4
    src_sha=e6c43a43ad4579fb5928eb54a90c2343a7c1df63c009e9814b62f70c63db58aa ;;
  Linux-x86_64) plat=linux-x64; tag=libmpv-linux-x64-r3; ext=tar.xz
    sha=03156173bf1f30bd477170eff1778b203e224b0dd2c2566de2916d2e22b40e11
    src_sha=a13f2cf1cc0f8d447b1cc377e08324b5132865c2da54eda3a63389221f5b10cc ;;
  Darwin-*) echo "Intel Mac 沒有本專案建置的 libmpv：請用 Homebrew 的 mpv（brew install mpv），不用執行這個腳本" >&2; exit 1 ;;
  *) echo "這個平台沒有本專案建置的 libmpv：$(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
with_source=
while [ $# -gt 0 ]; do
  case $1 in
    --with-source) [ $# -ge 2 ] || { echo "--with-source 後面要接目錄" >&2; exit 1; }; with_source=$2; shift 2 ;;
    *) echo "不認得的參數：$1" >&2; exit 1 ;;
  esac
done
root=$(cd "$(dirname "$0")/.." && pwd)
dest=$root/vendor/libmpv/$plat
base=https://github.com/acer1204/VitaScope/releases/download/$tag
id=vitascope-$tag
marker=$dest/.fetched
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
get() { # 檔名 sha256 存到
  echo "下載 $base/$1"
  curl -fL --retry 3 -sS -o "$3" "$base/$1"
  got=$(sha256 "$3")
  [ "$got" = "$2" ] || { rm -f "$3"; echo "SHA-256 不符：$1 預期 $2，實際 $got" >&2; exit 1; }
}
if [ -f "$marker" ] && [ "$(cat "$marker")" = "$tag $sha" ]; then
  echo "libmpv 已是 $tag：$dest"
else
  # 先下載、核對、解壓到暫存資料夾，成功了才把舊版整個換掉（失敗時原本那一份還在）
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  get "$id.$ext" "$sha" "$tmp/$id.$ext"
  mkdir "$tmp/new"
  case $ext in
    zip) unzip -q "$tmp/$id.$ext" -d "$tmp/new" ;;
    tar.xz) tar -xJf "$tmp/$id.$ext" -C "$tmp/new" ;;
  esac
  rm -rf "$dest"; mkdir -p "$(dirname "$dest")"; mv "$tmp/new" "$dest"
  rm -rf "$tmp"
  # 壓縮檔裡的時間是固定的建置時間：改成現在，build.rs 比對大小與時間時才會換上新的檔案
  find "$dest" -type f -exec touch {} +
  printf '%s' "$tag $sha" > "$marker"
  echo "完成：$dest"
fi
if [ -n "$with_source" ]; then mkdir -p "$with_source"; get "$id-src.tar.xz" "$src_sha" "$with_source/$id-src.tar.xz"; fi
