# Windows 安裝包內含的元件（libmpv-2.dll）

Windows 版的播放引擎 `libmpv-2.dll` 來自 [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake)
的 [`20261006`](https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261006) 版
（`mpv-dev-x86_64-20261006-git-6c092d978b.7z`，SHA-256 `d10d0994bd1398813da87fdb299961b378c71561bae1d135cb2b641b5864d7eb` 是其中的 DLL）。
它把 mpv、FFmpeg 與下表的函式庫靜態連結成一個檔案，以 GPL 選項建置（FFmpeg 使用 `--enable-gpl --enable-version3`），
整體依 **GPL-3.0-or-later** 散布。各元件自己的授權條文在同資料夾的 `licenses/<元件>/`。

## 怎麼對應到原始碼

- **建置腳本**（建置方式、選項與修正檔）：mpv-winbuild-cmake 的
  commit [`05a60b3cfd04e3e3b89918f4a27f3dde2935dff2`](https://github.com/shinchiro/mpv-winbuild-cmake/tree/05a60b3cfd04e3e3b89918f4a27f3dde2935dff2)，
  由 [GitHub Actions run 37391591956](https://github.com/shinchiro/mpv-winbuild-cmake/actions/runs/37391591956)（x86_64，clang）建置。
- **各元件的版本**：建置腳本大多不固定版本，建置開始時（2026-10-06 00:01:56 UTC）把每個元件更新到它追蹤的分支的最新版。
  下表的 commit 就是各分支在那個時間的最新版，並對照過建置紀錄裡的更新紀錄；mpv、FFmpeg 的版本也跟 DLL 裡記錄的版本一致
  （mpv `v0.41.0-1102-g6c092d978`、FFmpeg `N-127218-g47313ad3f`）。有固定版本（壓縮檔或指定 commit）的元件就是那個版本。
- **修正檔**：curl、fontconfig、libbs2b、libvpl、luajit、mujs、openssl、spirv-cross 在建置時套用了建置腳本 `packages/` 裡的修正檔，
  lame 套用了它自己 `debian/patches` 裡的修正；libbluray、libdvdread、libjxl、libmodplug、libplacebo、libpsl、libva、libzimg、shaderc、svtav1 只改了建置設定。
  這些都在上面那個 commit 的建置腳本裡，curl、fontconfig 的修正檔也附在 `licenses/` 裡。
- **編譯器附帶的執行庫**：LLVM（libc++、libunwind、compiler-rt）與 mingw-w64（C 執行庫、winpthreads）的部分程式碼也靜態連結在 DLL 裡，版本列在下表。

如果需要某個元件的對應原始碼而上面的連結已經失效，請到 https://github.com/acer1204/VitaScope/issues 告知，我們會提供；
各元件的公開儲存庫也大多由 [Software Heritage](https://archive.softwareheritage.org/) 長期保存，可以用 commit 查到。

## 元件清單

「DLL 內」= 程式碼靜態連結在 `libmpv-2.dll` 裡；「標頭檔」= 只用到它的標頭檔（定義、內嵌函式編譯進呼叫它的元件）。

| 元件 | 內含方式 | 原始碼 | 版本 | 授權 | 授權條文 |
|---|---|---|---|---|---|
| amf-headers | 標頭檔 | https://github.com/GPUOpen-LibrariesAndSDKs/AMF | [`8c648005e07d4309033282bfd9947df2c7e76104`](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/commit/8c648005e07d4309033282bfd9947df2c7e76104) | MIT | `licenses/amf-headers/` |
| angle-headers | 標頭檔 | https://github.com/google/angle | [`0cb8023c01f92b29f3738ea7472d06f8f059ed84`](https://github.com/google/angle/commit/0cb8023c01f92b29f3738ea7472d06f8f059ed84) | BSD-3-Clause AND Apache-2.0 AND MIT-Khronos-old | `licenses/angle-headers/` |
| aom | DLL 內 | https://aomedia.googlesource.com/aom | `b10eed72ebc006292fa26d5f132bf03ef66e7a04` | BSD-2-Clause AND LicenseRef-AOM-Patent-License-1.0 AND BSD-3-Clause AND MIT AND ISC | `licenses/aom/` |
| avisynth-headers | 標頭檔 | https://github.com/AviSynth/AviSynthPlus | [`21fdc997f9724b994896ba5520ddf64d677976b3`](https://github.com/AviSynth/AviSynthPlus/commit/21fdc997f9724b994896ba5520ddf64d677976b3) | GPL-2.0-or-later WITH AdditionRef-AviSynth-C-Interface-exception | `licenses/avisynth-headers/` |
| brotli | DLL 內 | https://github.com/google/brotli | [`392b261debb0f478811ddacb98b118da36b06158`](https://github.com/google/brotli/commit/392b261debb0f478811ddacb98b118da36b06158) | MIT | `licenses/brotli/` |
| bzip2 | DLL 內 | https://gitlab.com/bzip2/bzip2 | `66c46b8c9436613fd81bc5d03f63a61933a4dcc3` | bzip2-1.0.6 | `licenses/bzip2/` |
| c-ares | DLL 內 | https://github.com/c-ares/c-ares | [`f4156c12c8b36f5cb8f8a53658d44b24263c6ad1`](https://github.com/c-ares/c-ares/commit/f4156c12c8b36f5cb8f8a53658d44b24263c6ad1) | MIT AND BSD-3-Clause AND Unicode-3.0 | `licenses/c-ares/` |
| curl | DLL 內 | https://github.com/curl/curl | [`24ff74fa8db2dd84b2acd190a1bcc04b7d69e1e7`](https://github.com/curl/curl/commit/24ff74fa8db2dd84b2acd190a1bcc04b7d69e1e7) | curl AND ISC | `licenses/curl/` |
| dav1d | DLL 內 | https://code.videolan.org/videolan/dav1d | `7f12cf23560430c02a83e67bb68eec74d93ce5fd` | BSD-2-Clause AND ISC | `licenses/dav1d/` |
| davs2 | DLL 內 | https://github.com/saindriches/davs2 | [`f50435051b72c168c2b566c544e27fcff71ba61a`](https://github.com/saindriches/davs2/commit/f50435051b72c168c2b566c544e27fcff71ba61a) | GPL-2.0-or-later AND ISC | `licenses/davs2/` |
| fast_float | DLL 內 | https://github.com/fastfloat/fast_float | [`f3f02c8ad0afd8181166dabce6a9e69f8aec24de`](https://github.com/fastfloat/fast_float/commit/f3f02c8ad0afd8181166dabce6a9e69f8aec24de) | Apache-2.0 OR BSL-1.0 OR MIT | `licenses/fast_float/` |
| ffmpeg | DLL 內 | https://github.com/FFmpeg/FFmpeg | [`47313ad3f9f33b892384b976fb7c09b3c85fd74e`](https://github.com/FFmpeg/FFmpeg/commit/47313ad3f9f33b892384b976fb7c09b3c85fd74e) | GPL-3.0-or-later | `licenses/ffmpeg/` |
| fontconfig | DLL 內 | https://gitlab.freedesktop.org/fontconfig/fontconfig | `f477dc2185135a0c0eae3f56f17d9dd08d0c3afc` | HPND-sell-variant AND MIT AND Unicode-DFS-2016 AND LicenseRef-fontconfig-public-domain (MIT-style/HPND file notices; see COPYING) | `licenses/fontconfig/` |
| freetype2 | DLL 內 | https://github.com/freetype/freetype | [`25167984d2435930f9178e8d9d2ca3bdfa71a4e7`](https://github.com/freetype/freetype/commit/25167984d2435930f9178e8d9d2ca3bdfa71a4e7) | (FTL OR GPL-2.0-or-later) AND MIT | `licenses/freetype2/` |
| fribidi | DLL 內 | https://github.com/fribidi/fribidi | [`24f15eee832eafa5d63319b666d50e488668ce31`](https://github.com/fribidi/fribidi/commit/24f15eee832eafa5d63319b666d50e488668ce31) | LGPL-2.1-or-later | `licenses/fribidi/` |
| glad | DLL 內 | https://github.com/Dav1dde/glad | [`0a7cf1af9e25e02821587bd4095807fc90b6426a`](https://github.com/Dav1dde/glad/commit/0a7cf1af9e25e02821587bd4095807fc90b6426a) | MIT (generator); generated loader code: (WTFPL OR CC0-1.0) AND Apache-2.0; bundled Khronos headers: MIT-Khronos-old | `licenses/glad/` |
| glslang | DLL 內 | https://github.com/KhronosGroup/glslang | [`8ba5ca7cae66a5306a50b6d1db50875c50a5c77a`](https://github.com/KhronosGroup/glslang/commit/8ba5ca7cae66a5306a50b6d1db50875c50a5c77a) | BSD-3-Clause AND BSD-2-Clause AND MIT-Khronos-old AND AML-glslang AND Apache-2.0 AND GPL-3.0-or-later WITH Bison-exception-2.2 | `licenses/glslang/` |
| graphengine | DLL 內 | https://github.com/sekrit-twc/graphengine | [`91c6af4c795c5396d8b974f24b4d2e2ecca04e2d`](https://github.com/sekrit-twc/graphengine/commit/91c6af4c795c5396d8b974f24b4d2e2ecca04e2d) | WTFPL | `licenses/graphengine/` |
| harfbuzz | DLL 內 | https://github.com/harfbuzz/harfbuzz | [`c7a7457b7385f33178e8cf87615ca077a810bbe7`](https://github.com/harfbuzz/harfbuzz/commit/c7a7457b7385f33178e8cf87615ca077a810bbe7) | MIT-Modern-Variant AND MIT | `licenses/harfbuzz/` |
| highway | DLL 內 | https://github.com/google/highway | [`60583da084085665a790c7e51ae81120f3efe17a`](https://github.com/google/highway/commit/60583da084085665a790c7e51ae81120f3efe17a) | Apache-2.0 OR BSD-3-Clause | `licenses/highway/` |
| lame | DLL 內 | https://gitlab.com/shinchiro/lame | `9a5b35c430a2e6d150d65a96c7d7acdd64962a8f` | LGPL-2.0-or-later AND LGPL-2.1-or-later | `licenses/lame/` |
| lcms2 | DLL 內 | https://github.com/mm2/Little-CMS | [`15c24e7ded91184c38c46c096804fa7272765737`](https://github.com/mm2/Little-CMS/commit/15c24e7ded91184c38c46c096804fa7272765737) | MIT | `licenses/lcms2/` |
| libarchive | DLL 內 | https://github.com/libarchive/libarchive | [`2305801c3467adc3f2cecdb6999afdd336f9f80e`](https://github.com/libarchive/libarchive/commit/2305801c3467adc3f2cecdb6999afdd336f9f80e) | BSD-2-Clause AND BSD-3-Clause AND (CC0-1.0 OR OpenSSL OR Apache-2.0) AND LicenseRef-Public-Domain | `licenses/libarchive/` |
| libaribcaption | DLL 內 | https://github.com/xqq/libaribcaption | [`c64c23b8905ba514b87c9789269e9f66f949ffe0`](https://github.com/xqq/libaribcaption/commit/c64c23b8905ba514b87c9789269e9f66f949ffe0) | MIT | `licenses/libaribcaption/` |
| libass | DLL 內 | https://github.com/libass/libass | [`f61db567e6593df3470e91594bcd4ad2d0473aff`](https://github.com/libass/libass/commit/f61db567e6593df3470e91594bcd4ad2d0473aff) | ISC | `licenses/libass/` |
| libbluray | DLL 內 | https://code.videolan.org/videolan/libbluray | `a24f4fad4d62893de647abc8671397747b2359dd` | LGPL-2.1-or-later | `licenses/libbluray/` |
| libbs2b | DLL 內 | https://github.com/alexmarsev/libbs2b | [`5ca2d59888df047f1e4b028e3a2fd5be8b5a7277`](https://github.com/alexmarsev/libbs2b/commit/5ca2d59888df047f1e4b028e3a2fd5be8b5a7277) | MIT | `licenses/libbs2b/` |
| libdvdcss | DLL 內 | https://code.videolan.org/videolan/libdvdcss | `811ce97a1d4d1c9d936f4b2707f533a9ef768251` | GPL-2.0-or-later | `licenses/libdvdcss/` |
| libdvdnav | DLL 內 | https://code.videolan.org/videolan/libdvdnav | `8147ccd35e5aae4afdd21171cf7b6b4d8f179d28` | GPL-2.0-or-later | `licenses/libdvdnav/` |
| libdvdread | DLL 內 | https://code.videolan.org/videolan/libdvdread | `0f30df53125b564ef45903714403ce0a1e6c5e9e` | GPL-2.0-or-later | `licenses/libdvdread/` |
| libiconv | DLL 內 | https://ftp.gnu.org/pub/gnu/libiconv/libiconv-1.18.tar.gz | libiconv-1.18.tar.gz, SHA-256 3b08f5f4f9b4eb82f151a7040bfd6fe6c6fb922efe4b1659c66ea933276965e8 | LGPL-2.1-or-later | `licenses/libiconv/` |
| libjpeg | DLL 內 | https://github.com/libjpeg-turbo/libjpeg-turbo | [`09316bf956a1bb585d93813baf509a80f63a328f`](https://github.com/libjpeg-turbo/libjpeg-turbo/commit/09316bf956a1bb585d93813baf509a80f63a328f) | IJG AND BSD-3-Clause AND Zlib | `licenses/libjpeg/` |
| libjxl | DLL 內 | https://github.com/libjxl/libjxl | [`8ec4d2e8e3a4012481ec48a44873d14abc2b17f8`](https://github.com/libjxl/libjxl/commit/8ec4d2e8e3a4012481ec48a44873d14abc2b17f8) | BSD-3-Clause | `licenses/libjxl/` |
| libmodplug | DLL 內 | https://github.com/Konstanty/libmodplug | [`d1b97ed0020bc620a059d3675d1854b40bd2608d`](https://github.com/Konstanty/libmodplug/commit/d1b97ed0020bc620a059d3675d1854b40bd2608d) | LicenseRef-PublicDomain | `licenses/libmodplug/` |
| libmysofa | DLL 內 | https://github.com/hoene/libmysofa | [`648eed03472e6720a1ea45d1a1f86c4efb569ff9`](https://github.com/hoene/libmysofa/commit/648eed03472e6720a1ea45d1a1f86c4efb569ff9) | BSD-3-Clause | `licenses/libmysofa/` |
| libopenmpt | DLL 內 | https://lib.openmpt.org/files/libopenmpt/src/libopenmpt-0.7.12+release.autotools.tar.gz | libopenmpt-0.7.12+release.autotools.tar.gz, SHA-256 79ab3ce3672601e525b5cc944f026c80c03032f37d39caa84c8ca3fdd75e0c98 | BSD-3-Clause AND (BSL-1.0 OR BSD-3-Clause) | `licenses/libopenmpt/` |
| libplacebo | DLL 內 | https://github.com/haasn/libplacebo | [`0d043c7f6f79cd3687c023454bdacbe615e4d96f`](https://github.com/haasn/libplacebo/commit/0d043c7f6f79cd3687c023454bdacbe615e4d96f) | LGPL-2.1-or-later AND BSD-3-Clause | `licenses/libplacebo/` |
| libpng | DLL 內 | https://github.com/glennrp/libpng | [`d76d5106f041b97b3462d15bce738f2a4412de47`](https://github.com/glennrp/libpng/commit/d76d5106f041b97b3462d15bce738f2a4412de47) | libpng-2.0 AND Libpng | `licenses/libpng/` |
| libpsl | DLL 內 | https://github.com/rockdaboot/libpsl | [`aa3a80e18f25caf3916636c8668dcc0fff7c016d`](https://github.com/rockdaboot/libpsl/commit/aa3a80e18f25caf3916636c8668dcc0fff7c016d) | MIT AND BSD-3-Clause AND MPL-2.0 | `licenses/libpsl/` |
| libsamplerate | DLL 內 | https://github.com/libsndfile/libsamplerate | [`0844c208f683527c08ea8a80acc13b398aa9c8bf`](https://github.com/libsndfile/libsamplerate/commit/0844c208f683527c08ea8a80acc13b398aa9c8bf) | BSD-2-Clause | `licenses/libsamplerate/` |
| libsdl2 | DLL 內 | https://github.com/libsdl-org/SDL | [`4c2d9014afda49553c76f7045529207bb593f9b5`](https://github.com/libsdl-org/SDL/commit/4c2d9014afda49553c76f7045529207bb593f9b5) | Zlib AND BSD-3-Clause AND (BSD-3-Clause OR GPL-3.0-only OR LicenseRef-HIDAPI-orig) AND SunPro | `licenses/libsdl2/` |
| libsixel | DLL 內 | https://github.com/saitoha/libsixel | [`af12e5bc6f3f69ad83479da85ddb371420da9b17`](https://github.com/saitoha/libsixel/commit/af12e5bc6f3f69ad83479da85ddb371420da9b17) | MIT AND LicenseRef-kmiya-sixel AND LicenseRef-ppmquant AND LicenseRef-stb-public-domain AND LicenseRef-xterm-graphics | `licenses/libsixel/` |
| libsoxr | DLL 內 | https://gitlab.com/shinchiro/soxr | `945b592b70470e29f917f4de89b4281fbbd540c0` | LGPL-2.1-or-later AND LicenseRef-PFFFT-FFTPACKv5 AND LicenseRef-Ooura-FFT | `licenses/libsoxr/` |
| libsrt | DLL 內 | https://github.com/Haivision/srt | [`b1551573c74b529b8fdc4d93f1eee4c947017bf9`](https://github.com/Haivision/srt/commit/b1551573c74b529b8fdc4d93f1eee4c947017bf9) | MPL-2.0 AND BSD-3-Clause | `licenses/libsrt/` |
| libssh | DLL 內 | https://gitlab.com/libssh/libssh-mirror | `ee5627aaf630fc0c304b12757e6654773f4c1033` | LGPL-2.1-or-later AND BSD-2-Clause AND BSD-3-Clause AND ISC | `licenses/libssh/` |
| libudfread | DLL 內 | https://code.videolan.org/videolan/libudfread | `b0bc6957e7e07d5f35391f210596d9eb71cffdd9` | LGPL-2.1-or-later | `licenses/libudfread/` |
| libunibreak | DLL 內 | https://github.com/adah1972/libunibreak | [`28a2756b864c343f438cd22537d49d394d4666a5`](https://github.com/adah1972/libunibreak/commit/28a2756b864c343f438cd22537d49d394d4666a5) | Zlib | `licenses/libunibreak/` |
| libva | DLL 內 | https://github.com/intel/libva | [`f3fefceb192ca28abf588aadf2392350aebf46ce`](https://github.com/intel/libva/commit/f3fefceb192ca28abf588aadf2392350aebf46ce) | MIT | `licenses/libva/` |
| libvpl | DLL 內 | https://github.com/intel/libvpl | [`674d015bcb294bc39fa276e99a652ea045423e82`](https://github.com/intel/libvpl/commit/674d015bcb294bc39fa276e99a652ea045423e82) | MIT | `licenses/libvpl/` |
| libvpx | DLL 內 | https://chromium.googlesource.com/webm/libvpx | `0a6f769e44989222d99ded46daa1e9763637cc19` | BSD-3-Clause AND ISC | `licenses/libvpx/` |
| libwebp | DLL 內 | https://chromium.googlesource.com/webm/libwebp | `a1d89ff209ca01e7a87aca64317201890bac2749` | BSD-3-Clause | `licenses/libwebp/` |
| libxml2 | DLL 內 | https://github.com/GNOME/libxml2 | [`c43dc98d27ac315a48d93dbd399c6c22cf7125b1`](https://github.com/GNOME/libxml2/commit/c43dc98d27ac315a48d93dbd399c6c22cf7125b1) | MIT AND ISC-Veillard | `licenses/libxml2/` |
| libzimg | DLL 內 | https://github.com/sekrit-twc/zimg | [`67e0603271c080e22c8429856dd4a8a56587e61e`](https://github.com/sekrit-twc/zimg/commit/67e0603271c080e22c8429856dd4a8a56587e61e) | WTFPL | `licenses/libzimg/` |
| libzvbi | DLL 內 | https://github.com/zapping-vbi/zvbi | [`d3a5ee9f2b047bf16cd1ee5ccf6ec05ee75409d0`](https://github.com/zapping-vbi/zvbi/commit/d3a5ee9f2b047bf16cd1ee5ccf6ec05ee75409d0) | LGPL-2.0-or-later AND GPL-2.0-only AND LGPL-2.1-or-later AND MIT | `licenses/libzvbi/` |
| llvm | DLL 內 | https://github.com/llvm/llvm-project | [`45a9d73790257968607978479cbc1d1956047522`](https://github.com/llvm/llvm-project/commit/45a9d73790257968607978479cbc1d1956047522) | Apache-2.0 WITH LLVM-exception AND (NCSA OR MIT) | `licenses/llvm/` |
| luajit | DLL 內 | https://github.com/openresty/luajit2 | [`f75bf45e8b869a8ee4562ab0183322c3ebf2d9f3`](https://github.com/openresty/luajit2/commit/f75bf45e8b869a8ee4562ab0183322c3ebf2d9f3) | MIT | `licenses/luajit/` |
| mingw-w64 | DLL 內 | https://github.com/mingw-w64/mingw-w64 | [`5ea8e9facd013b815f5f29f20ef26a566319a3de`](https://github.com/mingw-w64/mingw-w64/commit/5ea8e9facd013b815f5f29f20ef26a566319a3de) | ZPL-2.1 AND ISC AND BSD-2-Clause AND BSD-3-Clause AND MIT AND SunPro AND LicenseRef-mingw-w64-runtime-other | `licenses/mingw-w64/` |
| mpv | DLL 內 | https://github.com/mpv-player/mpv | [`6c092d978b73a54f8b945e29f88a4015c7dacd89`](https://github.com/mpv-player/mpv/commit/6c092d978b73a54f8b945e29f88a4015c7dacd89) | GPL-2.0-or-later | `licenses/mpv/` |
| mujs | DLL 內 | https://codeberg.org/ccxvii/mujs | `8a32c397b28fe45747ac4e9e4f3dca049825eda7` | ISC | `licenses/mujs/` |
| nghttp2 | DLL 內 | https://github.com/nghttp2/nghttp2 | [`7a6d122aac527b65434314c2a115f07281e08dbf`](https://github.com/nghttp2/nghttp2/commit/7a6d122aac527b65434314c2a115f07281e08dbf) | MIT | `licenses/nghttp2/` |
| nghttp3 | DLL 內 | https://github.com/ngtcp2/nghttp3 | [`2304973e5a0c8b1fa4bb380b47945a000357f87f`](https://github.com/ngtcp2/nghttp3/commit/2304973e5a0c8b1fa4bb380b47945a000357f87f) | MIT | `licenses/nghttp3/` |
| ngtcp2 | DLL 內 | https://github.com/ngtcp2/ngtcp2 | [`b9fc4d55ce6035a4cc76dc456ced6371e98f9214`](https://github.com/ngtcp2/ngtcp2/commit/b9fc4d55ce6035a4cc76dc456ced6371e98f9214) | MIT | `licenses/ngtcp2/` |
| nvcodec-headers | 標頭檔 | https://github.com/FFmpeg/nv-codec-headers | [`eddcea9e27f6b772057c9b3f87de2cc1737faffc`](https://github.com/FFmpeg/nv-codec-headers/commit/eddcea9e27f6b772057c9b3f87de2cc1737faffc) | MIT | `licenses/nvcodec-headers/` |
| ogg | DLL 內 | https://github.com/xiph/ogg | [`be05b13e98b048f0b5a0f5fa8ce514d56db5f822`](https://github.com/xiph/ogg/commit/be05b13e98b048f0b5a0f5fa8ce514d56db5f822) | BSD-3-Clause | `licenses/ogg/` |
| openal-soft | DLL 內 | https://github.com/kcat/openal-soft | [`662c24995a6e809c0d1b0802fb189aa81a8ce7de`](https://github.com/kcat/openal-soft/commit/662c24995a6e809c0d1b0802fb189aa81a8ce7de) | LGPL-2.0-or-later AND BSD-3-Clause AND MIT AND Apache-2.0 AND LicenseRef-pffft | `licenses/openal-soft/` |
| openssl | DLL 內 | https://github.com/openssl/openssl | [`d8bf6cdd4849925c30e4f1911c7acb49cb34b702`](https://github.com/openssl/openssl/commit/d8bf6cdd4849925c30e4f1911c7acb49cb34b702) | Apache-2.0 | `licenses/openssl/` |
| opus | DLL 內 | https://github.com/xiph/opus | [`503d81b138d76621aae4b12786e90de48aa8db3a`](https://github.com/xiph/opus/commit/503d81b138d76621aae4b12786e90de48aa8db3a) | BSD-3-Clause | `licenses/opus/` |
| rubberband | DLL 內 | https://github.com/breakfastquay/rubberband | [`e4296ac80b1170018a110bc326fd0d45a0eb27d6`](https://github.com/breakfastquay/rubberband/commit/e4296ac80b1170018a110bc326fd0d45a0eb27d6) | GPL-2.0-or-later | `licenses/rubberband/` |
| shaderc | DLL 內 | https://github.com/google/shaderc | [`ba3e587dbc13d423c713e964ac08e094731a034d`](https://github.com/google/shaderc/commit/ba3e587dbc13d423c713e964ac08e094731a034d) | Apache-2.0 | `licenses/shaderc/` |
| speex | DLL 內 | https://github.com/xiph/speex | [`05895229896dc942d453446eba6f9f5ddcf95422`](https://github.com/xiph/speex/commit/05895229896dc942d453446eba6f9f5ddcf95422) | BSD-3-Clause | `licenses/speex/` |
| spirv-cross | DLL 內 | https://github.com/KhronosGroup/SPIRV-Cross | [`aa217aeb6c9f0ace7a0ab233b28807edf45eb165`](https://github.com/KhronosGroup/SPIRV-Cross/commit/aa217aeb6c9f0ace7a0ab233b28807edf45eb165) | (Apache-2.0 OR MIT) AND MIT AND MIT-Khronos-old | `licenses/spirv-cross/` |
| spirv-headers | 標頭檔 | https://github.com/KhronosGroup/SPIRV-Headers | [`86f980c731e62ae4eaf383d320449d71687936bf`](https://github.com/KhronosGroup/SPIRV-Headers/commit/86f980c731e62ae4eaf383d320449d71687936bf) | MIT | `licenses/spirv-headers/` |
| spirv-tools | DLL 內 | https://github.com/KhronosGroup/SPIRV-Tools | [`1ba1f5b8fe921e46091d18370f91fb46450ff786`](https://github.com/KhronosGroup/SPIRV-Tools/commit/1ba1f5b8fe921e46091d18370f91fb46450ff786) | Apache-2.0 | `licenses/spirv-tools/` |
| subrandr | DLL 內 | https://github.com/afishhh/subrandr | [`eaf47d947e754565fe60e88ffe9665a2a32b18d5`](https://github.com/afishhh/subrandr/commit/eaf47d947e754565fe60e88ffe9665a2a32b18d5) | MPL-2.0 | `licenses/subrandr/` |
| svtav1 | DLL 內 | https://gitlab.com/AOMediaCodec/SVT-AV1 | `1394731134e0ca891e11db3cd98144d1aa9a7b72` | BSD-3-Clause-Clear AND BSD-2-Clause AND LicenseRef-AOMedia-Patent-License-1.0 AND ISC AND MIT AND BSD-3-Clause | `licenses/svtav1/` |
| uavs3d | DLL 內 | https://github.com/uavs3/uavs3d | [`0e20d2c291853f196c68922a264bcd8471d75b68`](https://github.com/uavs3/uavs3d/commit/0e20d2c291853f196c68922a264bcd8471d75b68) | BSD-3-Clause | `licenses/uavs3d/` |
| uchardet | DLL 內 | https://gitlab.freedesktop.org/uchardet/uchardet | `06029ec3340cdf6bf9a6a537dafb3f39eda0560e` | MPL-1.1 OR GPL-2.0-or-later OR LGPL-2.1-or-later | `licenses/uchardet/` |
| vapoursynth | 標頭檔 | https://github.com/vapoursynth/vapoursynth | [`e46204429041e95a881b61eedddd46c08f9a307c`](https://github.com/vapoursynth/vapoursynth/commit/e46204429041e95a881b61eedddd46c08f9a307c) | LGPL-2.1-or-later AND WTFPL | `licenses/vapoursynth/` |
| vorbis | DLL 內 | https://github.com/xiph/vorbis | [`c2aa86b05e981c96bf381fc6aa11cdd03eccc2fb`](https://github.com/xiph/vorbis/commit/c2aa86b05e981c96bf381fc6aa11cdd03eccc2fb) | BSD-3-Clause | `licenses/vorbis/` |
| vulkan | 標頭檔 | https://github.com/KhronosGroup/Vulkan-Loader | [`b82e310e99c20bfac8dd3123c9e939f81ce54b89`](https://github.com/KhronosGroup/Vulkan-Loader/commit/b82e310e99c20bfac8dd3123c9e939f81ce54b89) | Apache-2.0 AND MIT AND HPND-Kevlin-Henney | `licenses/vulkan/` |
| vulkan-header | 標頭檔 | https://github.com/KhronosGroup/Vulkan-Headers | [`c46850864f4661461b0f6cb9922c058ffea4915e`](https://github.com/KhronosGroup/Vulkan-Headers/commit/c46850864f4661461b0f6cb9922c058ffea4915e) | Apache-2.0 OR MIT | `licenses/vulkan-header/` |
| x264 | DLL 內 | https://code.videolan.org/videolan/x264 | `0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee` | GPL-2.0-or-later AND MIT-Khronos-old | `licenses/x264/` |
| x265 | DLL 內 | https://github.com/Multicorewareinc/x265 | [`d978725394991e13a07a8002e0ff8b7d01eea08a`](https://github.com/Multicorewareinc/x265/commit/d978725394991e13a07a8002e0ff8b7d01eea08a) | GPL-2.0-or-later AND MIT | `licenses/x265/` |
| xxhash | DLL 內 | https://github.com/Cyan4973/xxHash | [`680bf463fa1ca0461b9a7c2dab7556e1f54cf4cf`](https://github.com/Cyan4973/xxHash/commit/680bf463fa1ca0461b9a7c2dab7556e1f54cf4cf) | BSD-2-Clause | `licenses/xxhash/` |
| xz | DLL 內 | https://github.com/tukaani-project/xz | [`3d078b52adbff566ccfc51067dfbf742ecf3ef86`](https://github.com/tukaani-project/xz/commit/3d078b52adbff566ccfc51067dfbf742ecf3ef86) | 0BSD | `licenses/xz/` |
| zlib | DLL 內 | https://github.com/zlib-ng/zlib-ng | [`72aceeac37bdd0704df14aac1823b4f80f4a5cf3`](https://github.com/zlib-ng/zlib-ng/commit/72aceeac37bdd0704df14aac1823b4f80f4a5cf3) | Zlib | `licenses/zlib/` |
| zstd | DLL 內 | https://github.com/facebook/zstd | [`7ab1ac9d0800bf3400b819e8e1987cc69e8f4286`](https://github.com/facebook/zstd/commit/7ab1ac9d0800bf3400b819e8e1987cc69e8f4286) | BSD-3-Clause OR GPL-2.0-only | `licenses/zstd/` |

## 只在建置時用到（不在 DLL 裡）

| 元件 | 原始碼 | 版本 | 授權 |
|---|---|---|---|
| expat | https://github.com/libexpat/libexpat | [`a76b1174c1ea52abb639a4c69d52b630a8a106a2`](https://github.com/libexpat/libexpat/commit/a76b1174c1ea52abb639a4c69d52b630a8a106a2) | MIT |
| game-music-emu | https://bitbucket.org/mpyne/game-music-emu | `a5216b1dafe476b6de36a316a151c64df575197a` | LGPL-2.1-or-later (GPL-2.0-or-later if the MAME YM2612 emulator is used) |
| lzo | https://www.oberhumer.com/opensource/lzo/download/lzo-2.10.tar.gz | lzo-2.10.tar.gz, SHA-1 4924676a9bae5db58ef129dc1cebce3baa3c4b5d | GPL-2.0-or-later |
| opus-dnn | https://media.xiph.org/opus/models/opus_data-8a07d57c4fce6fb30f23b3e0d264004e04f1d7b421f5392ef61543d021a439af.tar.gz | SHA-256 8a07d57c4fce6fb30f23b3e0d264004e04f1d7b421f5392ef61543d021a439af | BSD-3-Clause |
