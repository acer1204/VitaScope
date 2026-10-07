#!/usr/bin/env bash
# 從原始碼包建置 Linux x86_64 的 libmpv.so.2（AppImage 用）：mpv（-Dgpl=false）+ FFmpeg（LGPL）與相依函式庫靜態連結成一個共用函式庫。
# glibc、libstdc++、OpenSSL、fontconfig、ALSA、PulseAudio、libva 用使用者系統的（動態連結，不包進 AppImage）。
# 只用原始碼包裡的檔案，不連網路。先在 pins.json 的容器裡執行 build/setup-base.sh。
#   bash <原始碼包>/build/build.sh <輸出目錄>
set -Eeuo pipefail
# 任何一步失敗都印出是哪一行、哪個指令；錯誤訊息一律寫到 stderr（stdout 被導到檔案時也看得到）
trap 'echo "::error::${BASH_SOURCE[0]##*/} 第 $LINENO 行失敗（exit $?）：$BASH_COMMAND" >&2' ERR
BUNDLE=$(cd "$(dirname "$0")/.." && pwd)
OUT=$(realpath -m "$1")
PINS=$BUNDLE/pins.json
ID=vitascope-libmpv-$(jq -r '"\(.platform)-\(.build_id)"' "$PINS")
ROOT=/opt/vsbuild; PREFIX=$ROOT/prefix; WORK=$ROOT/work
JOBS=$(nproc)
MAP="-ffile-prefix-map=$ROOT=/vsbuild"
rm -rf "$PREFIX" "$WORK" "$OUT"; mkdir -p "$PREFIX" "$WORK/src" "$WORK/build" "$OUT/logs"
SOURCE_DATE_EPOCH=$(jq -r .source_date_epoch "$PINS"); export SOURCE_DATE_EPOCH
export CC=gcc-13 CXX=g++-13 AR=ar RANLIB=ranlib NM=nm STRIP=strip
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS CPATH LIBRARY_PATH
export PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig   # 自己建置的優先；系統的（fontconfig、libva…）在預設路徑
export LC_ALL=C TZ=UTC
for v in http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy; do export "$v=http://127.0.0.1:9"; done
export no_proxy='' NO_PROXY=''
export GIT_CEILING_DIRECTORIES=$ROOT
pin() { jq -r --arg n "$1" ".components[] | select(.name == \$n) | .$2" "$PINS"; }
die() { echo "::error::$*" >&2; exit 1; }
group() { echo "::group::$*"; }; endgroup() { echo "::endgroup::"; }
src() { # 與 macOS 的相同（解開、套用 build/patches/<元件>-*.patch）
  local d=$WORK/src/$1 p; rm -rf "$d"; mkdir -p "$d"
  tar -xf "$BUNDLE/upstream/$(pin "$1" file)" -C "$d" --strip-components=1
  for p in "$BUNDLE"/build/patches/"$1"-*.patch; do
    [[ -e $p ]] || continue
    patch -p1 -N -s -d "$d" < "$p" >&2 || die "$1：修正檔 ${p##*/} 套用失敗"
  done
  echo "$d"; }

# pkg-config 一律加 --static：自己建置的靜態函式庫連同 Libs.private 一起連結。
# prefer_static 關閉：系統的 -lssl、-lm、-lstdc++ 由 meson 找成 .so（不會把 glibc / OpenSSL 的 .a 連進來）
printf '#!/bin/sh\nexec pkg-config --static "$@"\n' > "$WORK/pkg-config"; chmod +x "$WORK/pkg-config"
cat > "$WORK/native.ini" <<EOF
[binaries]
c = 'gcc-13'
cpp = 'g++-13'
ar = 'ar'
strip = 'strip'
pkg-config = '$WORK/pkg-config'
nasm = 'nasm'
[built-in options]
buildtype = 'release'
default_library = 'static'
prefer_static = false
wrap_mode = 'nodownload'
pkg_config_path = ['$PREFIX/lib/pkgconfig']
c_args = ['$MAP']
cpp_args = ['$MAP']
EOF
meson_build() {
  local name=$1 s=$2 b=$WORK/build/$1; shift 2
  meson setup "$b" "$s" --native-file "$WORK/native.ini" --prefix "$PREFIX" --libdir lib "$@"
  meson compile -C "$b"; meson install -C "$b" --no-rebuild
  meson introspect "$b" --buildoptions > "$OUT/logs/meson-$name.json"
}
autotools_build() { # 原始碼目錄 [configure 選項...]（要連進共用函式庫，一律 -fPIC）
  local s=$1; shift
  ( cd "$s"; [[ -x configure ]] || autoreconf -fi
    ./configure --prefix="$PREFIX" --disable-shared --enable-static --with-pic \
      "CFLAGS=-O2 -fPIC $MAP" "CXXFLAGS=-O2 -fPIC $MAP" "$@"
    make -j"$JOBS"; make install )
}

group zlib
( cd "$(src zlib)" && CFLAGS="-O2 -fPIC $MAP" ./configure --prefix="$PREFIX" --static && make -j"$JOBS" install ); endgroup
group dav1d
meson_build dav1d "$(src dav1d)" -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_docs=false -Denable_asm=true; endgroup
group freetype
meson_build freetype "$(src freetype)" -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=system -Dtests=disabled; endgroup
group fribidi
meson_build fribidi "$(src fribidi)" -Ddocs=false -Dbin=false -Dtests=false; endgroup
group harfbuzz
meson_build harfbuzz "$(src harfbuzz)" \
  -D{glib,gobject,cairo,chafa,png,zlib,icu,freetype,raster,vector,gpu,gpu_demo,subset,tests,introspection,docs,utilities,benchmark}=disabled; endgroup
group libunibreak
autotools_build "$(src libunibreak)"; endgroup
group libass
# fontconfig 用系統的（讀系統的 /etc/fonts 與字型快取）；FreeType、HarfBuzz、FriBidi 用自己建置的靜態版本
meson_build libass "$(src libass)" -Dfontconfig=enabled -Ddirectwrite=disabled -Dcoretext=disabled -Dlibunibreak=enabled \
  -Dasm=enabled -Drequire-system-font-provider=true -D{test,compare,profile,fuzz,checkasm}=disabled; endgroup
group zimg
# STL_LIBS 寫進 zimg.pc 的 Libs.private：靜態的 libzimg.a 用到 libm（log10f…），FFmpeg 用 C 連結器檢查 libzimg 時
# 只有 -lstdc++ 會失敗（libm 只是 libstdc++.so 的相依，ld 不會拿來解析 libzimg.a 的符號）
s=$(src zimg); ( cd "$s" && ./autogen.sh )
STL_LIBS='-lstdc++ -lm' autotools_build "$s" --disable-testapp --disable-example --disable-unit-test \
  "CXXFLAGS=-O2 -fPIC $MAP -include exception"; endgroup
group nv-codec-headers
make -C "$(src nv-codec-headers)" PREFIX="$PREFIX" install; endgroup
group libplacebo
meson_build libplacebo "$(src libplacebo)" \
  -D{vulkan,vk-proc-addr,opengl,gl-proc-addr,d3d11,glslang,shaderc,lcms,libdovi,unwind,xxhash}=disabled -Ddovi=enabled -D{demos,tests,bench,fuzz}=false; endgroup
group libxml2
# DASH（FFmpeg 的 dash 分離器）只用到核心的樹狀 API；minimum 關掉其他模組。執行緒要開：多個 mpv（縮圖、片段輸出）可能同時解析
# meson.build 會執行 git describe（沒有 git 時 meson 直接失敗，建置容器的 git 見 pins.json 的 base.tools）；GIT_CEILING_DIRECTORIES 讓它找不到儲存庫，版本字串不帶 -GIT…
meson_build libxml2 "$(src libxml2)" -Dminimum=true -Dthreads=enabled \
  -D{c14n,catalog,debugging,docs,history,html,http,iconv,icu,iso8859x,legacy,modules,output,pattern,push,python,reader,readline,regexps,relaxng,sax1,schemas,schematron,thread-alloc,tls,valid,writer,xinclude,xpath,xptr,zlib}=disabled; endgroup

# FFmpeg 要編進去的編碼器、封裝格式、協定、濾鏡（三個平台相同，說明見 Windows 的 build.sh）；建置後逐一確認都有啟用
ENCODERS=png,gif,ac3
MUXERS=spdif,matroska,matroska_audio,webm,mov,mp4,ipod,mpegts,adts,gif,mp3,flac,ogg,opus,wav
PROTOCOLS=file,data,crypto,http,https,httpproxy,tcp,tls,udp,rtp,rtmp,rtmps,rtmpt,ftp,mmsh,mmst,rtmpts,srtp
FILTERS=buffer,buffersink,abuffer,abuffersink,format,aformat,null,anull,scale,aresample,rotate,hflip,vflip,crop,xstack,bwdif,testsrc2
FILTERS=$FILTERS,equalizer,bass,treble,acompressor,alimiter,dynaudnorm,speechnorm,loudnorm,pan,fps,split,palettegen,paletteuse,transpose
FILTERS=$FILTERS,zscale,tonemap
group ffmpeg
s=$(src ffmpeg); b=$WORK/build/ffmpeg; mkdir -p "$b"
# --disable-demuxer=imf：--enable-libxml2 也會打開 IMF 分離器，用不到（多一個解析 XML 的入口）
( cd "$b" && { "$s/configure" --prefix="$PREFIX" \
    --target-os=linux --arch=x86_64 --cc=gcc-13 --cxx=g++-13 \
    --pkg-config=pkg-config --pkg-config-flags=--static --extra-cflags="$MAP" \
    --enable-pic --enable-static --disable-shared --disable-programs --disable-doc --disable-debug \
    --disable-autodetect --enable-pthreads --disable-iconv \
    --enable-zlib --enable-openssl --enable-libdav1d --enable-libxml2 --enable-libzimg --disable-demuxer=imf \
    --enable-vaapi --enable-ffnvcodec --enable-cuda --enable-nvdec \
    --disable-encoders --enable-encoder="$ENCODERS" --disable-muxers --enable-muxer="$MUXERS" \
    --disable-devices --enable-indev=lavfi \
    --disable-protocols --enable-protocol="$PROTOCOLS" \
    --disable-filters --enable-filter="$FILTERS" \
    || { tail -n 80 ffbuild/config.log; exit 1; }; } && make -j"$JOBS" && make install )
cfg=$(cat "$b/config.h" "$b/config_components.h")
for want in 'CONFIG_GPL 0' 'CONFIG_VERSION3 0' 'CONFIG_NONFREE 0' 'FFMPEG_LICENSE "LGPL version 2.1 or later"' \
            'CONFIG_LIBZVBI 0' 'CONFIG_GNUTLS 0' 'CONFIG_OPENSSL 1' 'CONFIG_LIBDAV1D 1' 'CONFIG_ZLIB 1' 'HAVE_PTHREADS 1' \
            'CONFIG_VAAPI 1' 'CONFIG_NVDEC 1' 'CONFIG_VDPAU 0' 'CONFIG_XLIB 0' 'CONFIG_LIBDRM 0' 'CONFIG_VULKAN 0' \
            'CONFIG_HEVC_VAAPI_HWACCEL 1' 'CONFIG_AV1_VAAPI_HWACCEL 1' 'CONFIG_HEVC_NVDEC_HWACCEL 1' \
            'CONFIG_LAVFI_INDEV 1' 'CONFIG_HLS_DEMUXER 1' \
            'CONFIG_LIBXML2 1' 'CONFIG_LIBZIMG 1' 'CONFIG_DASH_DEMUXER 1' 'CONFIG_IMF_DEMUXER 0' 'CONFIG_AAC_ADTSTOASC_BSF 1' \
            'CONFIG_H264_MP4TOANNEXB_BSF 1' 'CONFIG_HEVC_MP4TOANNEXB_BSF 1' 'CONFIG_VP9_SUPERFRAME_BSF 1' 'CONFIG_EQ_FILTER 0'; do
  grep -qxF "#define $want" <<<"$cfg" || die "FFmpeg 設定不符：缺少 #define $want"
done
for k in ENCODER:$ENCODERS MUXER:$MUXERS PROTOCOL:$PROTOCOLS FILTER:$FILTERS; do
  for n in $(tr ',' ' ' <<<"${k#*:}"); do
    case $n in buffer|buffersink|abuffer|abuffersink) continue ;; esac   # 永遠編進去，沒有 CONFIG_ 巨集
    grep -qxF "#define CONFIG_$(tr a-z A-Z <<<"$n")_${k%%:*} 1" <<<"$cfg" || die "FFmpeg 設定不符：$n 沒有啟用"
  done
done; endgroup

group mpv
s=$(src mpv)
printf '%s\n' "$(pin mpv version)" > "$s/MPV_VERSION"
# --exclude-libs：靜態函式庫的符號不對外（系統的 fontconfig、Mesa 載入的 FreeType、zlib 不會綁到我們的）
# libstdc++：zimg、libplacebo 是 C++，用系統的 libstdc++.so.6（mpv 用 C 連結器，要明講）
LINK='-Wl,--exclude-libs,ALL -Wl,--push-state,--no-as-needed -lstdc++ -Wl,--pop-state -Wl,--build-id=sha1'
meson_build mpv "$s" --default-library=shared -Dc_link_args="$LINK" -Dcpp_link_args="$LINK" \
  -Dauto_features=disabled -Dgpl=false -Dlibmpv=true -Dcplayer=false -Dtests=false -Dfuzzers=false -Dbuild-date=false -Dlua=disabled \
  -Dvector=enabled -Dzlib=enabled -Dzimg=enabled -Dlibavdevice=enabled \
  -Dgl=enabled -Dplain-gl=enabled -Dvaapi=enabled -Dvaapi-drm=enabled \
  -Dcuda-hwaccel=enabled -Dcuda-interop=enabled -Dpulse=enabled -Dalsa=enabled
features=$(grep -m1 'List of enabled features: ' "$WORK/build/mpv/meson-logs/meson-log.txt" | sed 's/.*List of enabled features: //')
want='alsa clone cuda-hwaccel cuda-interop ffmpeg ffnvcodec gl glibc-thread-name glob glob-posix libass libavdevice libdl libplacebo linux-fstatfs memrchr posix posix-shm ppoll pthread-condattr-setclock pulse vaapi vaapi-drm vector vt.h zimg zimg-st428 zlib'
[[ $features == "$want" ]] || die "mpv 的功能跟預期不同：$features"   # 第一次成功建置後定案
for f in x11 wayland drm egl pipewire jack vdpau vulkan cplugins; do
  [[ " $features " != *" $f "* ]] || die "不該啟用 $f"; done; endgroup

group 打包與檢查
diff <(cd "$PREFIX/lib" && ls *.a | sort) <(printf '%s\n' libass.a libavcodec.a libavdevice.a libavfilter.a libavformat.a \
  libavutil.a libdav1d.a libfreetype.a libfribidi.a libharfbuzz.a liblinebreak.a libplacebo.a libswresample.a libswscale.a \
  libunibreak.a libxml2.a libz.a libzimg.a | sort) || die "prefix 裡的函式庫跟預期不同"
pkg=$OUT/$ID; mkdir -p "$pkg/lib" "$pkg/include/mpv" "$pkg/licenses"
so=$pkg/lib/libmpv.so.2
cp -L "$PREFIX/lib/libmpv.so.2" "$so"   # 裝好的那一份：meson install 已去掉建置用的 rpath
ln -s libmpv.so.2 "$pkg/lib/libmpv.so"
cp "$PREFIX/include/mpv/"*.h "$pkg/include/mpv/"
# 系統提供的函式庫要真的是動態連結：這些符號在 libmpv 裡必須是「未定義、由系統的 .so 提供」
undef=$(nm -D --undefined-only "$so" | awk '{print $2}' | sed 's/@.*//')
for s in SSL_CTX_new FcInitLoadConfig snd_pcm_open pa_threaded_mainloop_new vaInitialize vaGetDisplayDRM; do
  grep -qx "$s" <<<"$undef" || die "$s 不是由系統函式庫提供（被靜態連結了，或沒用到）"; done
strip --strip-unneeded "$so"
readelf -dW "$so" > "$OUT/dynamic.txt"
sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' "$OUT/dynamic.txt" | sort > "$OUT/needed.txt"
cat "$OUT/needed.txt"
# ld-linux-x86-64.so.2：用到執行緒區域變數（__tls_get_addr）時會直接依賴 glibc 的動態載入器
diff "$OUT/needed.txt" <(printf '%s\n' ld-linux-x86-64.so.2 libasound.so.2 libc.so.6 libcrypto.so.3 libfontconfig.so.1 \
  libgcc_s.so.1 libm.so.6 libpulse.so.0 libssl.so.3 libstdc++.so.6 libva-drm.so.2 libva.so.2 | sort) \
  || die "libmpv.so.2 依賴的系統函式庫跟預期不同"
grep -q '(SONAME).*\[libmpv\.so\.2\]' "$OUT/dynamic.txt" || die "SONAME 不是 libmpv.so.2"
! grep -qE '\((RPATH|RUNPATH)\)' "$OUT/dynamic.txt" || die "libmpv.so.2 不該有 RPATH / RUNPATH"
[[ $(readelf -lW "$so" | awk '$1 == "GNU_STACK" {print $7}') == RW ]] || die "堆疊不能可執行（glibc 2.41 起拒絕載入）"
nm -D --defined-only "$so" | awk '{print $3}' | sort > "$OUT/exports.txt"
for f in mpv_create mpv_initialize mpv_free mpv_render_context_create mpv_render_context_render; do
  grep -qx "$f" "$OUT/exports.txt" || die "沒有匯出 $f"; done
! grep -vE '^(mpv_|_init$|_fini$)' "$OUT/exports.txt" || die "匯出了 mpv_ 以外的符號"
for s in libzvbi libx264 libx265; do ! grep -qa "$s" "$so" || die "libmpv.so.2 裡出現 $s"; done
glibc=$(objdump -T "$so" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)
glibcxx=$(objdump -T "$so" | grep -o 'GLIBCXX_[0-9.]*' | sed 's/GLIBCXX_//' | sort -uV | tail -1)
# 替身函式庫：系統沒有 libpulse / libva 時 AppRun 改用（同時檢查每個外部符號都由預期的系統函式庫提供）
python3 "$BUNDLE/build/stubs.py" "$so" "$pkg/fallback" gcc-13 | tee "$OUT/logs/stubs.log"
# FFmpeg 裡帶 MIT / BSD 等寬鬆授權聲明的檔案（從實際編譯的目的檔找）：聲明原文放 licenses/ffmpeg/permissive/
python3 "$BUNDLE/build/ffmpeg_notices.py" "$WORK/src/ffmpeg" "$WORK/build/ffmpeg" "$pkg/licenses/ffmpeg/permissive"
# 授權條文：直接取自建置用的原始碼（.h、.c 只取開頭的授權註解；.c 保留路徑，加上 .txt）
jq -c '.components[]' "$PINS" | while read -r c; do
  n=$(jq -r .name <<<"$c")
  jq -r '.license_files[]' <<<"$c" | while read -r f; do
    case $f in
      *.h) mkdir -p "$pkg/licenses/$n"; sed -n '1,/\*\//p' "$WORK/src/$n/$f" > "$pkg/licenses/$n/LICENSE.txt" ;;
      *.c) mkdir -p "$(dirname "$pkg/licenses/$n/$f")"; sed -n '1,/\*\//p' "$WORK/src/$n/$f" > "$pkg/licenses/$n/$f.txt" ;;
      *) install -D -m 644 "$WORK/src/$n/$f" "$pkg/licenses/$n/$f" ;;
    esac
  done
done
python3 "$BUNDLE/build/notices.py" "$PINS" "$pkg"
{ echo "$ID"
  echo "base: $(jq -r .base.image "$PINS")；apt 快照 $(jq -r .base.apt_snapshot "$PINS")"
  echo "runner: ${ImageOS:-} ${ImageVersion:-}"
  echo
  # shellcheck disable=SC2046 # 套件名稱要拆成多個參數
  dpkg-query -W -f='${Package} ${Version}\n' $(jq -r '.base.packages | keys[]' "$PINS") $(jq -r '.base.tools[]' "$PINS") | sort
  # 不用「| head -1」：讀到第一行就關掉管線，有些工具會當成寫入錯誤而失敗
  for t in gcc-13 g++-13 ld meson ninja nasm pkg-config python3 autoconf automake libtoolize; do
    v=$("$t" --version 2>&1); printf '%s: %s\n' "$t" "${v%%$'\n'*}"
  done
  echo; echo "FFmpeg configure: $(sed -n 's/^#define FFMPEG_CONFIGURATION "\(.*\)"$/\1/p' "$b/config.h")"
  echo; echo "mpv enabled features: $features"
  echo; echo "依賴的系統函式庫（NEEDED）："; cat "$OUT/needed.txt"
  echo "符號版本最高：GLIBC_$glibc、GLIBCXX_${glibcxx:-（無）}"
  echo; (cd "$pkg" && sha256sum lib/libmpv.so.2 fallback/*/*)
} > "$pkg/BUILDINFO.txt"
cp "$OUT/needed.txt" "$OUT/exports.txt" "$pkg/"
(cd "$pkg" && sha256sum lib/libmpv.so.2 fallback/*/*) > "$OUT/SHA256SUMS"
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner --format=gnu \
    -C "$pkg" -cf - . | xz -T1 -6 > "$OUT/$ID.tar.xz"
ls -l "$so" "$OUT/$ID.tar.xz"; endgroup
