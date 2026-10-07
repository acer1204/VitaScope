#!/usr/bin/env bash
# 從原始碼包建置 macOS（Apple Silicon）的 libmpv.2.dylib：mpv（-Dgpl=false）+ FFmpeg（LGPL）與相依函式庫靜態連結成一個 dylib，
# 只依賴 macOS 內建的函式庫與 framework。只用原始碼包裡的檔案，不連網路（代理指向不存在的位址，誤下載會直接失敗）。
#   bash <原始碼包>/build/build.sh <輸出目錄>
# 主機需要：Apple Silicon、pins.json 指定的 Xcode（用 DEVELOPER_DIR 選）、可寫入的 /opt/vsbuild、
#           jq、python3、meson、ninja、pkg-config、make、autoconf、automake、libtool（glibtoolize）、zip
# （只用 macOS 內建 bash 3.2 也能跑的語法）
set -Eeuo pipefail
# 任何一步失敗都印出是哪一行、哪個指令；錯誤訊息一律寫到 stderr（stdout 被導到檔案時也看得到）
trap 'echo "::error::${BASH_SOURCE[0]##*/} 第 $LINENO 行失敗（exit $?）：$BASH_COMMAND" >&2' ERR
BUNDLE=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$1"; OUT=$(cd "$1" && pwd)
PINS=$BUNDLE/pins.json
ID=vitascope-libmpv-$(jq -r '"\(.platform)-\(.build_id)"' "$PINS")
ROOT=/opt/vsbuild          # 固定路徑：FFmpeg、mpv 會把建置參數（含路徑）記在 dylib 裡
PREFIX=$ROOT/prefix; WORK=$ROOT/work
JOBS=$(sysctl -n hw.ncpu)
MAP="-ffile-prefix-map=$ROOT=/vsbuild"
die() { echo "::error::$*" >&2; exit 1; }
group() { echo "::group::$*"; }; endgroup() { echo "::endgroup::"; }
tc() { jq -r ".toolchain.$1" "$PINS"; }
pin() { jq -r --arg n "$1" ".components[] | select(.name == \$n) | .$2" "$PINS"; }
DT=$(tc deployment_target)

# 編譯器與 SDK 要跟 pins.json 一致：GitHub 換掉映像裡的 Xcode 時在這裡失敗，不會悄悄換編譯器
[[ $(uname -m) == arm64 ]] || die "要在 Apple Silicon 上建置"
xv=$(xcodebuild -version)
[[ $xv == "Xcode $(tc version)"*"Build version $(tc build)" ]] || die "Xcode 不是 $(tc version)（$(tc build)）：$xv"
[[ $(xcrun --sdk macosx --show-sdk-version) == "$(tc sdk)" ]] || die "macOS SDK 不是 $(tc sdk)"

rm -rf "$PREFIX" "$WORK" "$OUT"; mkdir -p "$PREFIX" "$WORK/src" "$WORK/build" "$OUT/logs"
SOURCE_DATE_EPOCH=$(jq -r .source_date_epoch "$PINS"); export SOURCE_DATE_EPOCH
SDKROOT=$(xcrun --sdk macosx --show-sdk-path); export SDKROOT
export MACOSX_DEPLOYMENT_TARGET=$DT ZERO_AR_DATE=1
export CC=clang CXX=clang++ OBJC=clang AR=ar RANLIB=ranlib NM=nm STRIP=strip LIBTOOLIZE=glibtoolize
unset CFLAGS CXXFLAGS OBJCFLAGS CPPFLAGS LDFLAGS CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH LIBRARY_PATH PKG_CONFIG_PATH
export PKG_CONFIG_LIBDIR=$PREFIX/lib/pkgconfig   # 只找自己建置的 .pc：Homebrew 裝的函式庫不會被用到
export LC_ALL=C TZ=UTC
for v in http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy; do export "$v=http://127.0.0.1:9"; done
export no_proxy='' NO_PROXY=''
export GIT_CEILING_DIRECTORIES=$ROOT   # 原始碼沒有 .git；別讓 FFmpeg 的 version.sh 找到上層儲存庫

src() { local d=$WORK/src/$1 p; rm -rf "$d"; mkdir -p "$d"
        tar -xf "$BUNDLE/upstream/$(pin "$1" file)" -C "$d" --strip-components=1
        for p in "$BUNDLE"/build/patches/"$1"-*.patch; do   # 修正檔，說明在每個檔案開頭
          [[ -e $p ]] || continue
          patch -p1 -N -s -d "$d" < "$p" >&2 || die "$1：修正檔 ${p##*/} 套用失敗"
        done
        echo "$d"; }

cat > "$WORK/native.ini" <<EOF
[binaries]
c = 'clang'
cpp = 'clang++'
objc = 'clang'
ar = 'ar'
strip = 'strip'
pkg-config = 'pkg-config'
[built-in options]
buildtype = 'release'
default_library = 'static'
prefer_static = true
wrap_mode = 'nodownload'
c_args = ['-mmacosx-version-min=$DT', '$MAP']
cpp_args = ['-mmacosx-version-min=$DT', '$MAP']
objc_args = ['-mmacosx-version-min=$DT', '$MAP']
c_link_args = ['-mmacosx-version-min=$DT']
cpp_link_args = ['-mmacosx-version-min=$DT']
EOF

meson_build() { # 名稱 原始碼目錄 [選項...]（編譯紀錄留下來：最後檢查有沒有超過最低版本的警告）
  local name=$1 s=$2 b=$WORK/build/$1; shift 2
  { meson setup "$b" "$s" --native-file "$WORK/native.ini" --prefix "$PREFIX" --libdir lib "$@"
    meson compile -C "$b"; meson install -C "$b" --no-rebuild; } 2>&1 | tee "$OUT/logs/build-$name.log"
  meson introspect "$b" --buildoptions > "$OUT/logs/meson-$name.json"
}
autotools_build() { # 名稱 原始碼目錄 [configure 選項...]
  local name=$1 s=$2; shift 2
  ( cd "$s"; [[ -x configure ]] || autoreconf -fi
    ./configure --host=aarch64-apple-darwin --prefix="$PREFIX" --disable-shared --enable-static \
      "CFLAGS=-O2 -mmacosx-version-min=$DT $MAP" "CXXFLAGS=-O2 -mmacosx-version-min=$DT $MAP" "$@"
    make -j"$JOBS"; make install ) 2>&1 | tee "$OUT/logs/build-$name.log"
}

group zlib
s=$(src zlib); ( cd "$s" && CFLAGS="-O2 -mmacosx-version-min=$DT $MAP" ./configure --prefix="$PREFIX" --static \
  && make -j"$JOBS" install ) 2>&1 | tee "$OUT/logs/build-zlib.log"; endgroup
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
autotools_build libunibreak "$(src libunibreak)"; endgroup
group libass
meson_build libass "$(src libass)" -Dfontconfig=disabled -Ddirectwrite=disabled -Dcoretext=enabled -Dlibunibreak=enabled \
  -Dasm=enabled -Drequire-system-font-provider=true -D{test,compare,profile,fuzz,checkasm}=disabled; endgroup
group zimg
# zimg 3.0.6 用了 std::exception_ptr 卻沒有 #include <exception>：用編譯選項補上（同 Windows）；C++ 執行庫是系統的 libc++
s=$(src zimg); ( cd "$s" && ./autogen.sh )
STL_LIBS=-lc++ autotools_build zimg "$s" --disable-testapp --disable-example --disable-unit-test \
  "CXXFLAGS=-O2 -mmacosx-version-min=$DT $MAP -include exception"; endgroup
group libplacebo
meson_build libplacebo "$(src libplacebo)" \
  -D{vulkan,vk-proc-addr,opengl,gl-proc-addr,d3d11,glslang,shaderc,lcms,libdovi,unwind,xxhash}=disabled -Ddovi=enabled -D{demos,tests,bench,fuzz}=false; endgroup

group ffmpeg
s=$(src ffmpeg); b=$WORK/build/ffmpeg; mkdir -p "$b"
( cd "$b" && { "$s/configure" --prefix="$PREFIX" \
    --arch=aarch64 --target-os=darwin --cc=clang --cxx=clang++ --objcc=clang \
    --pkg-config=pkg-config --pkg-config-flags=--static \
    --extra-cflags="-mmacosx-version-min=$DT $MAP" --extra-objcflags="-mmacosx-version-min=$DT $MAP" \
    --extra-ldflags="-mmacosx-version-min=$DT" \
    --enable-static --disable-shared --disable-programs --disable-doc --disable-debug \
    --disable-autodetect --enable-pthreads --disable-iconv \
    --enable-zlib --enable-securetransport --enable-libdav1d --enable-videotoolbox \
    --disable-encoders --enable-encoder=png --disable-muxers --enable-muxer=spdif \
    --disable-devices --enable-indev=lavfi \
    --disable-protocols --enable-protocol=file,data,crypto,http,https,httpproxy,tcp,tls,udp,rtp,rtmp,rtmps,rtmpt \
    --disable-filters --enable-filter=buffer,buffersink,abuffer,abuffersink,format,aformat,null,anull,scale,aresample,rotate,hflip,vflip,crop,xstack,bwdif,testsrc2 \
    || { tail -n 80 ffbuild/config.log; exit 1; }; } && make -j"$JOBS" && make install ) 2>&1 | tee "$OUT/logs/build-ffmpeg.log"
cfg=$(cat "$b/config.h" "$b/config_components.h")
for want in 'CONFIG_GPL 0' 'CONFIG_VERSION3 0' 'CONFIG_NONFREE 0' 'FFMPEG_LICENSE "LGPL version 2.1 or later"' \
            'CONFIG_LIBZVBI 0' 'CONFIG_OPENSSL 0' 'CONFIG_GNUTLS 0' 'CONFIG_SECURETRANSPORT 1' 'CONFIG_LIBDAV1D 1' 'CONFIG_ZLIB 1' \
            'HAVE_PTHREADS 1' 'CONFIG_VIDEOTOOLBOX 1' 'CONFIG_AUDIOTOOLBOX 0' 'CONFIG_AVFOUNDATION_INDEV 0' \
            'CONFIG_H264_VIDEOTOOLBOX_HWACCEL 1' 'CONFIG_HEVC_VIDEOTOOLBOX_HWACCEL 1' 'CONFIG_VP9_VIDEOTOOLBOX_HWACCEL 1' \
            'CONFIG_AV1_VIDEOTOOLBOX_HWACCEL 1' 'CONFIG_PNG_ENCODER 1' 'CONFIG_LAVFI_INDEV 1' 'CONFIG_TESTSRC2_FILTER 1' \
            'CONFIG_HTTPS_PROTOCOL 1' 'CONFIG_HLS_DEMUXER 1'; do
  grep -qxF "#define $want" <<<"$cfg" || die "FFmpeg 設定不符：缺少 #define $want"
done; endgroup

group mpv
s=$(src mpv)
printf '%s\n' "$(pin mpv version)" > "$s/MPV_VERSION"   # 原始碼包沒有 .git：mpv-version 才是 mpv v0.41.0-1102-g6c092d978
echo '_mpv_*' > "$WORK/mpv.exp"   # 只匯出 mpv_*：靜態連結的 FFmpeg、libplacebo、libass… 的符號不對外
meson_build mpv "$s" --default-library=shared \
  -Dc_link_args="-mmacosx-version-min=$DT -lc++ -Wl,-exported_symbols_list,$WORK/mpv.exp -Wl,-dead_strip_dylibs" \
  -Dauto_features=disabled -Dgpl=false -Dlibmpv=true -Dcplayer=false -Dtests=false -Dfuzzers=false -Dbuild-date=false -Dlua=disabled \
  -Dvector=enabled -Dzlib=enabled -Dzimg=enabled -Dlibavdevice=enabled -Dcoreaudio=enabled \
  -Dgl=enabled -Dplain-gl=enabled -Dvideotoolbox-gl=enabled
features=$(grep -m1 'List of enabled features: ' "$WORK/build/mpv/meson-logs/meson-log.txt" | sed 's/.*List of enabled features: //')
# 第一次成功建置後依實際結果定案（同 Windows）
want='bsd-fstatfs coreaudio darwin ffmpeg gl glob glob-posix libass libavdevice libdl libplacebo mac-thread-name posix posix-shm vector videotoolbox-gl zimg zimg-st428 zlib'
[[ $features == "$want" ]] || die "mpv 的功能跟預期不同：$features"; endgroup

group 打包與檢查
diff <(cd "$PREFIX/lib" && ls *.a | sort) <(printf '%s\n' libass.a libavcodec.a libavdevice.a libavfilter.a libavformat.a \
  libavutil.a libdav1d.a libfreetype.a libfribidi.a libharfbuzz.a liblinebreak.a libplacebo.a libswresample.a libswscale.a \
  libunibreak.a libz.a libzimg.a | sort) || die "prefix 裡的函式庫跟預期不同"
# 每個目的檔的最低系統版本都要是 $DT（比它新的 API 只在較新的 macOS 才有）
for a in "$PREFIX"/lib/*.a; do otool -l "$a" | awk '/LC_BUILD_VERSION/ {b=1} b && /minos/ {print $2; b=0}'; done | sort -u > "$OUT/object-minos.txt"
[[ $(cat "$OUT/object-minos.txt") == "$DT" ]] || die "有目的檔的最低版本不是 $DT：$(tr '\n' ' ' < "$OUT/object-minos.txt")"
! grep -hE 'built for newer macOS version|no platform load command|is only available on macOS' "$OUT"/logs/*.log \
  || die "建置紀錄裡有超過最低版本 $DT 的警告"

pkg=$OUT/$ID; mkdir -p "$pkg/include/mpv" "$pkg/licenses"
d=$pkg/libmpv.2.dylib
cp "$WORK/build/mpv/libmpv.2.dylib" "$d"   # 用建置資料夾裡的：meson install 會把 install name 改成絕對路徑
cp "$PREFIX/include/mpv/"*.h "$pkg/include/mpv/"
strip -S -x "$d"
codesign --force --sign - "$d"; codesign --verify --strict "$d"   # strip 會讓連結時的 ad-hoc 簽章失效
[[ $(otool -D "$d" | tail -1) == "@rpath/libmpv.2.dylib" ]] || die "install name 不是 @rpath/libmpv.2.dylib"
[[ $(lipo -archs "$d") == arm64 ]] || die "不是 arm64"
[[ $(vtool -show-build "$d" | awk '/minos/ {print $2}') == "$DT" ]] || die "dylib 的最低版本不是 $DT"
! otool -l "$d" | grep -q LC_RPATH || die "dylib 不該有 rpath"
otool -L "$d" | tail -n +3 | awk '{print $1}' | sort > "$OUT/imports.txt"
cat "$OUT/imports.txt"
! grep -vE '^(/usr/lib/|/System/Library/Frameworks/)' "$OUT/imports.txt" || die "依賴了 macOS 沒有內建的函式庫"
! grep -E '/libz\.|/libiconv\.|/libssl|/libcrypto' "$OUT/imports.txt" || die "用到了系統的 zlib / iconv / OpenSSL（應該是靜態連結的 zlib、SecureTransport）"
nm -gU "$d" | awk '{print $3}' | sort > "$OUT/exports.txt"
for f in _mpv_create _mpv_initialize _mpv_free _mpv_render_context_create _mpv_render_context_render; do
  grep -qx "$f" "$OUT/exports.txt" || die "沒有匯出 $f"; done
! grep -v '^_mpv_' "$OUT/exports.txt" || die "匯出了 mpv_ 以外的符號"
nm -m "$d" | grep 'weak external' > "$OUT/weak-imports.txt" || true   # 比 $DT 新的 API（記錄下來看）
for s in libzvbi libx264 libx265; do ! grep -qa "$s" "$d" || die "dylib 裡出現 $s"; done
# 授權條文：直接取自建置用的原始碼（.h 只取開頭的授權註解）
jq -c '.components[]' "$PINS" | while read -r c; do
  n=$(jq -r .name <<<"$c")
  jq -r '.license_files[]' <<<"$c" | while read -r f; do
    mkdir -p "$(dirname "$pkg/licenses/$n/$f")"
    if [[ $f == *.h ]]; then sed -n '1,/\*\//p' "$WORK/src/$n/$f" > "$pkg/licenses/$n/LICENSE.txt"
    else cp "$WORK/src/$n/$f" "$pkg/licenses/$n/$f"; fi
  done
done
python3 "$BUNDLE/build/notices.py" "$PINS" "$pkg"
{ echo "$ID"
  echo "Xcode: $(echo $xv)；SDK $(xcrun --sdk macosx --show-sdk-version)；最低系統版本 macOS $DT"
  echo "runner: ${ImageOS:-} ${ImageVersion:-}"
  # 不用「| head -1」：讀到第一行就關掉管線，有些工具會當成寫入錯誤而失敗
  for t in clang meson ninja pkg-config python3 autoconf automake glibtoolize; do
    v=$("$t" --version 2>&1); printf '%s: %s\n' "$t" "${v%%$'\n'*}"
  done
  echo; echo "FFmpeg configure: $(sed -n 's/^#define FFMPEG_CONFIGURATION "\(.*\)"$/\1/p' "$b/config.h")"
  echo; echo "mpv enabled features: $features"
  echo; echo "依賴的系統函式庫："; cat "$OUT/imports.txt"
  echo; echo "weak imports："; cat "$OUT/weak-imports.txt"
  echo; (cd "$pkg" && shasum -a 256 libmpv.2.dylib)
} > "$pkg/BUILDINFO.txt"
(cd "$pkg" && shasum -a 256 libmpv.2.dylib) > "$OUT/SHA256SUMS"
(cd "$pkg" && find . -type f | LC_ALL=C sort | zip -X -q -9 "$OUT/$ID.zip" -@)
ls -l "$d" "$OUT/$ID.zip"; endgroup
