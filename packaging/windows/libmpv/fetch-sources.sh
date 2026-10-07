#!/usr/bin/env bash
# 依 pins.json 取得 libmpv-2.dll 每個元件的原始碼並逐一核對（壓縮檔比 SHA-256；git 比 commit 與子模組 gitlink），
# 做成原始碼包。建置只用這個包（建置腳本也在包裡），發佈時原封不動附上，確保原始碼與 DLL 完全對應。
#   bash fetch-sources.sh <下載快取> <輸出目錄>              → <輸出>/vitascope-libmpv-win64-<build_id>-src.tar.xz
#   bash fetch-sources.sh --toolchain <下載快取> <輸出目錄>  → 靜態連結進 DLL 的編譯器執行庫的原始碼
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
pins=$here/pins.json
mode=bundle
if [[ ${1:-} == --toolchain ]]; then mode=toolchain; shift; fi
cache=$(realpath -m "$1"); out=$(realpath -m "$2")
mkdir -p "$cache/git" "$out"
epoch=$(jq -r .source_date_epoch "$pins")
id=vitascope-libmpv-win64-$(jq -r .build_id "$pins")
die() { echo "::error::$*" >&2; exit 1; }

get_tarball() { # 目的地 名稱 網址 sha256 檔名
  local c=$cache/$5
  if ! { [[ -s $c ]] && echo "$4  $c" | sha256sum -c --quiet >/dev/null 2>&1; }; then
    curl -fsSL --retry 3 -o "$c.part" "$3" || die "$2：下載失敗 $3"
    mv "$c.part" "$c"
    echo "$4  $c" | sha256sum -c --quiet >/dev/null 2>&1 || die "$2：SHA-256 不符（$3）"
  fi
  cp "$c" "$1/$5"
}

git_at() { # 目錄 網址 commit：只抓這個 commit（GitHub 可直接抓 SHA）
  [[ -d $1 ]] || { git init -q "$1"; git -C "$1" remote add origin "$2"; }
  git -C "$1" fetch -q --depth=1 origin "$3" || die "抓不到 $2 的 $3"
  [[ $(git -C "$1" rev-parse FETCH_HEAD) == "$3" ]] || die "$2：commit 不符"
}

get_git() { # 目的地 名稱 網址 commit 檔名 [子模組路徑=commit ...]
  local dest=$1 name=$2 url=$3 commit=$4 file=$5; shift 5
  local g=$cache/git/$name top=${file%%.tar*} tar=$cache/$name.tar sub path want got surl sg
  git_at "$g" "$url" "$commit"
  git -C "$g" archive --format=tar --prefix="$top/" "$commit" > "$tar"
  for sub in "$@"; do
    path=${sub%%=*}; want=${sub#*=}
    got=$(git -C "$g" rev-parse "$commit:$path")
    [[ $got == "$want" ]] || die "$name/$path：gitlink 是 $got，pins.json 寫 $want"
    surl=$(git -C "$g" config --blob="$commit:.gitmodules" --get "submodule.$path.url")
    sg=$cache/git/$name--${path//\//_}
    git_at "$sg" "$surl" "$want"
    git -C "$sg" archive --format=tar --prefix="$top/$path/" "$want" > "$sg.tar"
    tar --concatenate -f "$tar" "$sg.tar"
  done
  case $file in
    *.tar) cp "$tar" "$dest/$file" ;;
    *.tar.xz) xz -T1 -6 -c "$tar" > "$dest/$file" ;;
    *) die "$name：不支援的檔名 $file" ;;
  esac
}

fetch_all() { # jq 路徑 目的地 → 印出「sha256  檔名」
  jq -c "$1[]" "$pins" | while read -r e; do
    local name kind file subs=()
    name=$(jq -r .name <<<"$e"); kind=$(jq -r .kind <<<"$e"); file=$(jq -r .file <<<"$e")
    case $kind in
      tarball) get_tarball "$2" "$name" "$(jq -r .url <<<"$e")" "$(jq -r .sha256 <<<"$e")" "$file" ;;
      git) mapfile -t subs < <(jq -r '(.submodules // {}) | to_entries[] | "\(.key)=\(.value)"' <<<"$e")
           get_git "$2" "$name" "$(jq -r .url <<<"$e")" "$(jq -r .commit <<<"$e")" "$file" "${subs[@]}" ;;
      *) die "$name：不認得的 kind「$kind」" ;;
    esac
    printf '%s  %s\n' "$(sha256sum "$2/$file" | cut -d' ' -f1)" "$file"
  done
}

if [[ $mode == toolchain ]]; then fetch_all .toolchain_sources "$out"; exit 0; fi

dest=$out/$id-src
rm -rf "$dest"; mkdir -p "$dest/upstream" "$dest/build"
fetch_all .components "$dest/upstream" > "$dest/upstream/SHA256SUMS"
cat "$dest/upstream/SHA256SUMS"
# 「控制編譯與安裝的腳本」也是對應原始碼的一部分
cp "$pins" "$here/requirements.txt" "$dest/"
cp "$here/README-SOURCE.md" "$dest/README.md"
cp "$here/build.sh" "$here/notices.py" "$repo/.github/workflows/libmpv-windows.yml" "$dest/build/"
cp -r "$here/toolchain-licenses" "$dest/build/"
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --format=gnu \
    -C "$out" -cf - "$id-src" | xz -T1 -6 > "$out/$id-src.tar.xz"
ls -l "$out/$id-src.tar.xz"
