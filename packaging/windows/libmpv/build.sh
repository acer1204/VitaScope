#!/usr/bin/env bash
# 從原始碼包建置 Windows x86_64 的 libmpv-2.dll：mpv（-Dgpl=false）+ FFmpeg（LGPL）與相依函式庫靜態連結成一個 DLL。
# 只用原始碼包裡的檔案，不連網路（代理指向不存在的位址，誤下載會直接失敗）。
#   bash <原始碼包>/build/build.sh <llvm-mingw 目錄> <輸出目錄>
# 主機需要：/opt/vsbuild 可寫入、jq、python3、meson、ninja、nasm、pkg-config、make、autoconf、automake、libtool、zip
set -euo pipefail
BUNDLE=$(cd "$(dirname "$0")/.." && pwd)
TOOLCHAIN=$(realpath "$1"); OUT=$(realpath -m "$2")
PINS=$BUNDLE/pins.json
ID=vitascope-libmpv-win64-$(jq -r .build_id "$PINS")
ROOT=/opt/vsbuild          # 固定路徑：FFmpeg、mpv 會把建置參數（含路徑）記在 DLL 裡
PREFIX=$ROOT/prefix; WORK=$ROOT/work
HOST=x86_64-w64-mingw32; JOBS=$(nproc)
MAP="-ffile-prefix-map=$ROOT=/vsbuild"
rm -rf "$PREFIX" "$WORK" "$OUT"; mkdir -p "$PREFIX" "$WORK/src" "$WORK/build" "$OUT/logs"

SOURCE_DATE_EPOCH=$(jq -r .source_date_epoch "$PINS"); export SOURCE_DATE_EPOCH
export PATH=$TOOLCHAIN/bin:$PATH
export PKG_CONFIG_LIBDIR=$PREFIX/lib/pkgconfig PKG_CONFIG_PATH=
export CC=$HOST-clang CXX=$HOST-clang++ AR=llvm-ar RANLIB=llvm-ranlib NM=llvm-nm STRIP=llvm-strip
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS
export LC_ALL=C TZ=UTC
for v in http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy; do export "$v=http://127.0.0.1:9"; done
export no_proxy='' NO_PROXY=''
export GIT_CEILING_DIRECTORIES=$ROOT   # 原始碼沒有 .git；別讓 FFmpeg 的 version.sh 找到上層儲存庫

pin() { jq -r --arg n "$1" ".components[] | select(.name == \$n) | .$2" "$PINS"; }
die() { echo "::error::$*"; exit 1; }
group() { echo "::group::$*"; }; endgroup() { echo "::endgroup::"; }
src() { local d=$WORK/src/$1; rm -rf "$d"; mkdir -p "$d"
        tar -xf "$BUNDLE/upstream/$(pin "$1" file)" -C "$d" --strip-components=1; echo "$d"; }

cat > "$WORK/cross.ini" <<EOF
[binaries]
c = '$HOST-clang'
cpp = '$HOST-clang++'
ar = 'llvm-ar'
nm = 'llvm-nm'
strip = 'llvm-strip'
windres = '$HOST-windres'
dlltool = '$HOST-dlltool'
pkg-config = 'pkg-config'
nasm = 'nasm'
[properties]
pkg_config_libdir = '$PREFIX/lib/pkgconfig'
[built-in options]
buildtype = 'release'
default_library = 'static'
prefer_static = true
wrap_mode = 'nodownload'
c_args = ['$MAP']
cpp_args = ['$MAP']
[host_machine]
system = 'windows'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF

meson_build() { # 名稱 原始碼目錄 [選項...]
  local name=$1 s=$2 b=$WORK/build/$1; shift 2
  meson setup "$b" "$s" --cross-file "$WORK/cross.ini" --prefix "$PREFIX" --libdir lib "$@"
  meson compile -C "$b"; meson install -C "$b" --no-rebuild
  meson introspect "$b" --buildoptions > "$OUT/logs/meson-$name.json"
}
autotools_build() { # 原始碼目錄 [configure 選項...]
  local s=$1; shift
  ( cd "$s"; [[ -x configure ]] || autoreconf -fi
    CFLAGS="-O2 $MAP" CXXFLAGS="-O2 $MAP" ./configure --host=$HOST --prefix="$PREFIX" --disable-shared --enable-static "$@"
    make -j"$JOBS"; make install )
}

group zlib
( cd "$(src zlib)" && CHOST=$HOST CFLAGS="-O2 $MAP" ./configure --prefix="$PREFIX" --static && make -j"$JOBS" install ); endgroup
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
meson_build libass "$(src libass)" -Dfontconfig=disabled -Ddirectwrite=enabled -Dcoretext=disabled -Dlibunibreak=enabled \
  -Dasm=enabled -Drequire-system-font-provider=true -D{test,compare,profile,fuzz,checkasm}=disabled; endgroup
group zimg
# zimg 3.0.6 的 api/zimg.cpp 用了 std::exception_ptr 卻沒有 #include <exception>（舊的標準函式庫會間接引入），
# 新版 libc++ 不再間接引入：用編譯選項補上，原始碼不改
s=$(src zimg); ( cd "$s" && ./autogen.sh ); STL_LIBS=-lc++ autotools_build "$s" --disable-testapp --disable-example --disable-unit-test   "CXXFLAGS=-O2 $MAP -include exception"; endgroup
group nv-codec-headers
make -C "$(src nv-codec-headers)" PREFIX="$PREFIX" install; endgroup
group libplacebo
meson_build libplacebo "$(src libplacebo)" \
  -D{vulkan,vk-proc-addr,opengl,gl-proc-addr,d3d11,glslang,shaderc,lcms,libdovi,unwind,xxhash}=disabled -Ddovi=enabled -D{demos,tests,bench,fuzz}=false; endgroup

group ffmpeg
s=$(src ffmpeg); b=$WORK/build/ffmpeg; mkdir -p "$b"
( cd "$b" && { "$s/configure" --prefix="$PREFIX" \
    --target-os=mingw32 --arch=x86_64 --enable-cross-compile --cross-prefix=$HOST- \
    --cc=$HOST-clang --cxx=$HOST-clang++ --ar=llvm-ar --ranlib=llvm-ranlib --nm=llvm-nm --strip=llvm-strip \
    --pkg-config=pkg-config --pkg-config-flags=--static --extra-cflags="$MAP" \
    --enable-static --disable-shared --disable-programs --disable-doc --disable-debug \
    --disable-autodetect --enable-w32threads --disable-iconv \
    --enable-zlib --enable-schannel --enable-libdav1d \
    --enable-d3d11va --enable-dxva2 --enable-ffnvcodec --enable-cuda --enable-nvdec \
    --disable-encoders --enable-encoder=png --disable-muxers --enable-muxer=spdif \
    --disable-devices --enable-indev=lavfi \
    --disable-protocols --enable-protocol=file,data,crypto,http,https,httpproxy,tcp,tls,udp,rtp,rtmp,rtmps,rtmpt \
    --disable-filters --enable-filter=buffer,buffersink,abuffer,abuffersink,format,aformat,null,anull,scale,aresample,rotate,hflip,vflip,crop,xstack,bwdif,testsrc2 \
    || { tail -n 80 ffbuild/config.log; exit 1; }; } && make -j"$JOBS" && make install )
cfg=$(cat "$b/config.h" "$b/config_components.h")
for want in 'CONFIG_GPL 0' 'CONFIG_VERSION3 0' 'CONFIG_NONFREE 0' 'FFMPEG_LICENSE "LGPL version 2.1 or later"' \
            'CONFIG_LIBZVBI 0' 'CONFIG_OPENSSL 0' 'CONFIG_SCHANNEL 1' 'CONFIG_LIBDAV1D 1' 'CONFIG_ZLIB 1' 'HAVE_W32THREADS 1' \
            'CONFIG_D3D11VA 1' 'CONFIG_DXVA2 1' 'CONFIG_NVDEC 1' 'CONFIG_HEVC_NVDEC_HWACCEL 1' 'CONFIG_AV1_D3D11VA_HWACCEL 1' \
            'CONFIG_PNG_ENCODER 1' 'CONFIG_LAVFI_INDEV 1' 'CONFIG_TESTSRC2_FILTER 1' 'CONFIG_HTTPS_PROTOCOL 1' 'CONFIG_HLS_DEMUXER 1'; do
  grep -qxF "#define $want" <<<"$cfg" || die "FFmpeg 設定不符：缺少 #define $want"
done; endgroup

group mpv
s=$(src mpv)
# 原始碼包沒有 .git：寫入 git describe 的結果，mpv-version 才是 mpv v0.41.0-1102-g6c092d978（其餘原始碼不動）
printf '%s\n' "$(pin mpv version)" > "$s/MPV_VERSION"
meson_build mpv "$s" --default-library=shared \
  -Dc_link_args='-static -lc++ -Wl,--no-insert-timestamp' -Dcpp_link_args='-static -Wl,--no-insert-timestamp' \
  -Dauto_features=disabled -Dgpl=false -Dlibmpv=true -Dcplayer=false -Dtests=false -Dfuzzers=false -Dbuild-date=false -Dlua=disabled \
  -Dwin32-threads=enabled -Dvector=enabled -Dwasapi=enabled -Dzlib=enabled -Dzimg=enabled -Dlibavdevice=enabled \
  -Dgl=enabled -Dgl-win32=enabled -Dgl-dxinterop=enabled \
  -Dd3d-hwaccel=enabled -Dd3d9-hwaccel=enabled -Dgl-dxinterop-d3d9=enabled -Dcuda-hwaccel=enabled -Dcuda-interop=enabled
features=$(sed -n 's/.*List of enabled features: //p' "$WORK/build/mpv/meson-logs/meson-log.txt" | head -1)
want='cuda-hwaccel cuda-interop d3d-hwaccel d3d9-hwaccel dos-paths ffmpeg ffnvcodec gl gl-dxinterop gl-dxinterop-d3d9 gl-win32 glob glob-win32 libass libavdevice libplacebo vector wasapi win32 win32-desktop win32-threads zimg zimg-st428 zlib'
[[ $features == "$want" ]] || die "mpv 的功能跟預期不同：$features"; endgroup

group 打包與檢查
diff <(cd "$PREFIX/lib" && ls *.a | sort) <(printf '%s\n' libass.a libavcodec.a libavdevice.a libavfilter.a libavformat.a \
  libavutil.a libdav1d.a libfreetype.a libfribidi.a libharfbuzz.a libmpv.dll.a libplacebo.a libswresample.a libswscale.a \
  libunibreak.a libz.a libzimg.a | sort) || die "prefix 裡的函式庫跟預期不同"
pkg=$OUT/$ID; mkdir -p "$pkg/include/mpv" "$pkg/licenses"
cp "$PREFIX/bin/libmpv-2.dll" "$PREFIX/lib/libmpv.dll.a" "$pkg/"; cp "$PREFIX/include/mpv/"*.h "$pkg/include/mpv/"
llvm-strip --strip-all "$pkg/libmpv-2.dll"
dll=$pkg/libmpv-2.dll
llvm-readobj --coff-imports "$dll" | sed -n 's/^ *Name: //p' | sort -fu > "$OUT/imports.txt"
llvm-readobj --coff-exports "$dll" | sed -n 's/^ *Name: //p' | sort > "$OUT/exports.txt"
cat "$OUT/imports.txt"
! grep -Ei '^(vulkan-1|lib[^.]*|av[a-z]+-[0-9]+|sw[a-z]+-[0-9]+|zlib1|dav1d)\.dll$' "$OUT/imports.txt" || die "依賴了需要另外附上的 DLL"
for f in mpv_create mpv_initialize mpv_free mpv_render_context_create mpv_render_context_render; do
  grep -qx "$f" "$OUT/exports.txt" || die "沒有匯出 $f"; done
! grep -v '^mpv_' "$OUT/exports.txt" || die "匯出了 mpv_ 以外的符號"
for s in libzvbi libx264 libx265; do ! grep -qa "$s" "$dll" || die "DLL 裡出現 $s"; done
# 授權條文：直接取自建置用的原始碼（.h 只取開頭的授權註解）
jq -c '.components[]' "$PINS" | while read -r c; do
  n=$(jq -r .name <<<"$c")
  jq -r '.license_files[]' <<<"$c" | while read -r f; do
    if [[ $f == *.h ]]; then mkdir -p "$pkg/licenses/$n"; sed -n '1,/\*\//p' "$WORK/src/$n/$f" > "$pkg/licenses/$n/LICENSE.txt"
    else install -D -m 644 "$WORK/src/$n/$f" "$pkg/licenses/$n/$f"; fi
  done
done
cp -r "$BUNDLE/build/toolchain-licenses/." "$pkg/licenses/"
python3 "$BUNDLE/build/notices.py" "$PINS" "$pkg"
{ echo "$ID"
  echo "toolchain: $(jq -r '.toolchain | "\(.name) \(.version) sha256:\(.sha256)"' "$PINS")"
  echo "runner: ${ImageOS:-} ${ImageVersion:-}"
  for t in "$CC" meson ninja nasm pkg-config python3 autoconf automake libtoolize; do printf '%s: ' "$t"; "$t" --version 2>&1 | head -1; done
  echo; echo "FFmpeg configure: $(sed -n 's/^#define FFMPEG_CONFIGURATION "\(.*\)"$/\1/p' "$b/config.h")"
  echo; echo "mpv enabled features: $features"
  echo; (cd "$pkg" && sha256sum libmpv-2.dll libmpv.dll.a)
} > "$pkg/BUILDINFO.txt"
(cd "$pkg" && sha256sum libmpv-2.dll libmpv.dll.a) > "$OUT/SHA256SUMS"
(cd "$pkg" && find . -type f | LC_ALL=C sort | zip -X -q -9 "$OUT/$ID.zip" -@)
ls -l "$dll" "$OUT/$ID.zip"; endgroup
