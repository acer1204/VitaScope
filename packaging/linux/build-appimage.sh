#!/usr/bin/env bash
# 組 Linux AppImage：影戲執行檔＋本專案建置的 libmpv.so.2（vendor/libmpv/linux-x64/，bash scripts/fetch-libmpv.sh 下載）。
#   bash packaging/linux/build-appimage.sh <vitascope 執行檔> <版本（純數字）> <輸出 .AppImage> [授權聲明資料夾]
# 在 ubuntu-24.04 上執行：winit、glutin 用 dlopen 載入的視窗函式庫（libxkbcommon-x11、libXcursor…）取自這台機器的 Ubuntu 套件。
# 另外寫出 <輸出資料夾>/appimage-glibc.txt（AppImage 需要的最低 glibc）。
set -Eeuo pipefail
# 任何一步失敗都印出是哪一行、哪個指令；錯誤訊息一律寫到 stderr（stdout 被導到檔案時也看得到）
trap 'echo "::error::${BASH_SOURCE[0]##*/} 第 $LINENO 行失敗（exit $?）：$BASH_COMMAND" >&2' ERR
bin=$(realpath "$1"); version=$2; out=$(realpath -m "$3"); notices=${4:-}
repo=$(cd "$(dirname "$0")/../.." && pwd)
vendor=$repo/vendor/libmpv/linux-x64
pkg=$repo/packaging/linux
id=io.github.acer1204.vitascope
sys=/usr/lib/x86_64-linux-gnu
die() { echo "::error::$*" >&2; exit 1; }
LINUXDEPLOY_URL=https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
LINUXDEPLOY_SHA256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
# AppImage 開頭的執行環境也釘住版本（不指定會下載會變動的 continuous）
RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
RUNTIME_COMMIT=dd6cebedcbddde9c82f89b011e8e1d40b6e43868   # 20251108 這個 tag 指向的 commit（授權聲明寫原始碼位置用）
export APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=1   # runner 不保證有 FUSE；libmpv.so.2 已經 strip 過
[[ -f $vendor/lib/libmpv.so.2 ]] || die "找不到 $vendor/lib/libmpv.so.2，請先執行 bash scripts/fetch-libmpv.sh"
work=$(mktemp -d); app=$work/AppDir
mkdir -p "$(dirname "$out")"
ld=$work/linuxdeploy.AppImage; runtime=$work/runtime-x86_64
curl -sSfL --retry 3 -o "$ld" "$LINUXDEPLOY_URL"; echo "$LINUXDEPLOY_SHA256  $ld" | sha256sum -c -; chmod +x "$ld"
# 解開來用：最後一步要直接呼叫裡面的 AppImage 外掛
( cd "$work" && "$ld" --appimage-extract >/dev/null )
ld=$work/squashfs-root/AppRun; plugin=$work/squashfs-root/usr/bin/linuxdeploy-plugin-appimage
[[ -x $ld && -x $plugin ]] || die "linuxdeploy 解開後找不到 AppRun 或 linuxdeploy-plugin-appimage"
curl -sSfL --retry 3 -o "$runtime" "$RUNTIME_URL"; echo "$RUNTIME_SHA256  $runtime" | sha256sum -c -
# libpulse、libva、OpenSSL 要跟使用者的系統（音效伺服器、顯示卡驅動、安全性更新）一致：每次執行 linuxdeploy 都要排除
excl=(--exclude-library 'libpulse.so*' --exclude-library 'libva.so*' --exclude-library 'libva-drm.so*'
      --exclude-library 'libssl.so*' --exclude-library 'libcrypto.so*'
      # 只有系統的 fontconfig（FreeType）、libxcb、libwayland-client 才用到的函式庫：使用者的系統有那些函式庫，就一定有這些
      --exclude-library 'libpng16.so*' --exclude-library 'libbrotli*.so*' --exclude-library 'libbz2.so*'
      --exclude-library 'libXau.so*' --exclude-library 'libXdmcp.so*' --exclude-library 'libbsd.so*' --exclude-library 'libmd.so*'
      --exclude-library 'libffi.so*')
# linuxdeploy 用 ldd 找相依，連間接相依都會列出來：
# - 先找到本專案建置的 libmpv.so.2（runner 上另外裝著 Ubuntu 的 libmpv2，給 tar.gz）
# - libpulse、libva 讓它看替身：不然真品背後的 libsndfile、libsystemd… 會被當成相依包進來
ldpath=$vendor/lib:$vendor/fallback/pulse:$vendor/fallback/va:$vendor/fallback/va-drm

doc=$app/usr/share/doc/vitascope
mkdir -p "$app/usr/share/metainfo" "$doc" "$app/usr/lib/fallback"
sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$(date -u +%F)/" "$pkg/$id.appdata.xml.in" > "$app/usr/share/metainfo/$id.appdata.xml"
appstreamcli validate --no-net "$app/usr/share/metainfo/$id.appdata.xml"
cp "$repo/README.md" "$doc/"
if [[ -n $notices ]]; then cp "$notices"/* "$doc/"; else cp "$repo/LICENSE" "$repo/packaging/THIRD-PARTY-NOTICES.md" "$doc/"; fi
icons=$work/icons; mkdir -p "$icons"; cp "$repo/packaging/icons/icon-256.png" "$icons/$id.png"
# winit、glutin 執行時才載入（dlopen）的視窗函式庫，ldd 看不到，要明講。libX11、libX11-xcb、libwayland-client、
# libEGL、libGL 依 AppImage 的慣例用系統的（excludelist），其餘的最小安裝不一定有（例如乾淨的 Debian 13 沒有 libXcursor、libXi）
libs=()
for l in libxkbcommon-x11.so.0 libXcursor.so.1 libXi.so.6 libwayland-cursor.so.0 libwayland-egl.so.1; do libs+=(--library "$sys/$l"); done
LD_LIBRARY_PATH=$ldpath "$ld" --appdir "$app" --executable "$bin" \
  --desktop-file "$pkg/$id.desktop" --icon-file "$icons/$id.png" --custom-apprun "$pkg/AppRun" \
  "${libs[@]}" "${excl[@]}"
# linuxdeploy 會替 usr/lib 的每個函式庫加上 RUNPATH=$ORIGIN（patchelf）。本專案建置的 libmpv.so.2 換回原檔，
# 跟 prerelease 的檔案逐位元相同（它依賴的都是系統的函式庫或替身，不需要 RUNPATH）；替身最後才放進去，不經過 linuxdeploy
cp "$vendor/lib/libmpv.so.2" "$app/usr/lib/libmpv.so.2"
cp -r "$vendor/fallback/." "$app/usr/lib/fallback/"
same_as_vendor() { # AppDir 的 usr/lib → 跟 vendor 逐位元比對 libmpv.so.2 與替身
  cmp "$1/libmpv.so.2" "$vendor/lib/libmpv.so.2" || die "AppImage 裡的 libmpv.so.2 跟 prerelease 的不同"
  local f
  for f in "$vendor"/fallback/*/*; do
    cmp "$1/fallback/${f#"$vendor/fallback/"}" "$f" || die "AppImage 裡的替身 ${f#"$vendor/"} 跟 prerelease 的不同"
  done
}
same_as_vendor "$app/usr/lib"
# 授權：libmpv.so.2 的元件清單與條文 → 其餘 Ubuntu 套件 → AppImage 執行環境
cp "$vendor/THIRD-PARTY-LINUX.md" "$doc/"; cp -r "$vendor/licenses" "$doc/licenses"
bash "$repo/packaging/linux-third-party.sh" "$app" "$doc/THIRD-PARTY-LINUX.md" libmpv.so.2 'fallback/*'
cat >> "$doc/THIRD-PARTY-LINUX.md" <<EOF

## AppImage 執行環境

AppImage 檔案開頭的執行環境是 [AppImage/type2-runtime](https://github.com/AppImage/type2-runtime) 的 20251108 版
（commit \`$RUNTIME_COMMIT\`；$RUNTIME_URL，SHA-256 \`$RUNTIME_SHA256\`；MIT）。其中靜態連結了 libfuse 3.15.0（LGPL-2.1，
含 type2-runtime 的修正檔）、squashfuse 0.5.2（BSD-2-Clause），以及 Alpine Linux 3.21 的 zstd（BSD-3-Clause 或 GPL-2.0）、
zlib（Zlib）與 musl（MIT）。建置腳本與修正檔：https://github.com/AppImage/type2-runtime/tree/$RUNTIME_COMMIT
（\`scripts/\`、\`patches/\`；各元件的下載位置與 SHA-256 寫在 \`scripts/common/install-dependencies.sh\`）。
EOF
# 直接用 AppImage 外掛打包：再跑一次 linuxdeploy（--output appimage）又會改動 libmpv.so.2 與替身
( cd "$(dirname "$out")" && LDAI_OUTPUT=$(basename "$out") LDAI_RUNTIME_FILE=$runtime "$plugin" --appdir "$app" )
# 打包好的 AppImage 裡也要是原檔
( cd "$work" && rm -rf squashfs-root && "$out" --appimage-extract 'usr/lib/*' >/dev/null )
same_as_vendor "$work/squashfs-root/usr/lib"
# usr/lib 只能有這些（第一次執行後定案）
got=$(cd "$app/usr/lib" && find . -name '*.so*' \( -type f -o -type l \) | sed 's|^\./||' | LC_ALL=C sort)
want=$(printf '%s\n' fallback/pulse/libpulse.so.0 fallback/va-drm/libva-drm.so.2 fallback/va/libva.so.2 \
       libmpv.so.2 libxcb-xkb.so.1 libxkbcommon-x11.so.0 libxkbcommon.so.0 libXcursor.so.1 libXext.so.6 libXfixes.so.3 \
       libXi.so.6 libXrender.so.1 libwayland-cursor.so.0 libwayland-egl.so.1 | LC_ALL=C sort)
[[ $got == "$want" ]] || die "AppImage 的 usr/lib 跟預期不同：$(echo $got)"
glibc=$(find "$app" -type f \( -name '*.so*' -o -path '*/usr/bin/*' \) -exec objdump -T {} + 2>/dev/null \
        | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)
echo "AppImage 最低需求：glibc $glibc"; echo "$glibc" > "$(dirname "$out")/appimage-glibc.txt"
ls -l "$out"
