#!/usr/bin/env python3
"""產生格式測試樣本。

輸出：
  samples/generated/<tier>/<id>.<ext>   外掛字幕放在影片旁邊、同檔名
  samples/generated/manifest.json       測試程式（tests/formats.rs）讀這份清單比對預期結果
  samples/generated/general/dash_h264_aac/manifest.mpd   本機的 DASH（不在 manifest.json；tests/engine_build.rs 用）
  samples/generated/net/…                本機 HTTP 伺服器播放用的 HLS、DASH、分開的影像與聲音（不在 manifest.json；tests/net.rs 用）
  samples/generated/pacing/pan_23976.mkv  流暢播放的實機測試（tests/pacing_window.rs）；只有 --tier pacing 才產生
  samples/generated/pacing/pan_4k10.mkv   同上，4K 10-bit（軟體解碼、GPU 畫一格比較久）
  samples/generated/unsupported/…        播放引擎解不了的編碼（不在 manifest.json；tests/decoders.rs 用）

用法：
  python scripts/gen_samples.py                 # 產生全部等級
  python scripts/gen_samples.py --tier common   # 只產生「常見」
  python scripts/gen_samples.py --tier net      # 只產生網路測試用的串流（HLS、DASH）
  python scripts/gen_samples.py --tier unsupported  # 只產生播放引擎解不了的編碼（AVS2、MP4 裡的 TTML 字幕）
  python scripts/gen_samples.py --force         # 已存在也重新產生
  python scripts/gen_samples.py --tier pacing   # 流暢播放實機測試用的 1080p、4K 平移影片（約 20 MB + 50 MB，CI 不需要）

需要 FFmpeg 7.1 以上的 full build（VVC 樣本需要 libvvenc）。各平台套件版的 FFmpeg 沒有的外部函式庫編碼器
（libvvenc、liboapv、libxavs2、libvo_amrwbenc…）會略過那個樣本並說明。
FFmpeg 沒有編碼器的 DSD（DSF / DFF）、PGS 圖形字幕、SCC、TTML 與罕見的文字字幕由這個程式自己寫。
FFmpeg 無法編碼的其他格式（VC-1、RV40、Bink、Smacker、Dolby Vision…）請把公開樣本放到 samples/external/。
"""

from __future__ import annotations

import argparse
import functools
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "samples" / "generated"
DUR = 3  # 每個樣本的秒數

TIERS = ("common", "general", "rare")

# 字幕內容。測試會在 SUB_PROBE_TIME 秒讀取畫面上的字幕，跟 SUB_PROBE_TEXT 比對。
# 文字只用 Big5 編得出來的字，Big5 樣本才能共用同一份內容。
# 第三句是一般對白，含足夠的繁簡差異字，讓播放器能從「內容」判斷繁體或簡體。
SUB_CUES = [
    (0.2, 1.4, "第一句字幕 First line"),
    (1.5, 2.6, "影戲播放器測試"),
    (2.7, 2.95, "這是我們的世界，他們會來這裡嗎？為什麼還沒到"),
]
# 簡體版：測試「有繁中就選繁中」
SUB_CUES_SIMP = [
    (0.2, 1.4, "第一句字幕 First line"),
    (1.5, 2.6, "影戏播放器测试"),
    (2.7, 2.95, "这是我们的世界，他们会来这里吗？为什么还没到"),
]
SUB_PROBE_TIME = 2.0
SUB_PROBE_TEXT = "影戲播放器測試"
# CEA-608 隱藏字幕（SCC）只有 ASCII：同樣的時間、英文的內容
CC_CUES = [
    (0.2, 1.4, "FIRST LINE"),
    (1.5, 2.6, "VITASCOPE TEST"),
    (2.7, 2.95, "THIRD"),
]
CC_PROBE_TEXT = "VITASCOPE TEST"

# 一般對白（自己寫的句子），放在 3 秒之後（影片結束後不會顯示），讓字幕檔長度和用字接近真實字幕。
# 實際影片庫裡「繁體字用 GBK 編碼」的長字幕，mpv 會誤判成 BIG5，整份變亂碼；短字幕反而猜得對。
DIALOGUE_TRAD = [
    "你今天怎麼這麼晚才回來", "我們一起去吃飯吧", "這件事情我會處理好的", "別擔心，一切都會好起來的",
    "他說明天會來學校", "你知道她為什麼生氣嗎", "我覺得這樣做不太好", "快點走吧，要遲到了",
    "謝謝你一直以來的照顧", "我們是最好的朋友", "這個問題很難回答", "你還記得那天發生的事嗎",
    "對不起，我不是故意的", "等一下，我馬上就來", "這裡的風景真漂亮", "你在說什麼啊",
    "沒關係，下次再說吧", "我想和你談談", "讓我們開始吧", "時間過得真快",
]
DIALOGUE_SIMP = [
    "你今天怎么这么晚才回来", "我们一起去吃饭吧", "这件事情我会处理好的", "别担心，一切都会好起来的",
    "他说明天会来学校", "你知道她为什么生气吗", "我觉得这样做不太好", "快点走吧，要迟到了",
    "谢谢你一直以来的照顾", "我们是最好的朋友", "这个问题很难回答", "你还记得那天发生的事吗",
    "对不起，我不是故意的", "等一下，我马上就来", "这里的风景真漂亮", "你在说什么啊",
    "没关系，下次再说吧", "我想和你谈谈", "让我们开始吧", "时间过得真快",
]


def long_cues(base, dialogue, repeat=6):
    """在測試用的三句之後，接上大量一般對白（從第 4 秒開始，每句 2 秒）"""
    lines = [d for _ in range(repeat) for d in dialogue]
    return base + [(4.0 + i * 2, 5.5 + i * 2, t) for i, t in enumerate(lines)]

# 常用的編碼參數
AAC = ["-c:a", "aac", "-b:a", "128k", "-ac", "2"]
MP3 = ["-c:a", "libmp3lame", "-b:a", "128k", "-ac", "2"]
OPUS = ["-c:a", "libopus", "-b:a", "96k", "-ac", "2"]
AC3 = ["-c:a", "ac3", "-b:a", "192k", "-ac", "2"]
FLAC = ["-c:a", "flac", "-ac", "2"]
PCM = ["-c:a", "pcm_s16le", "-ac", "2"]
X264 = ["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"]
X265 = ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-x265-params", "log-level=error"]
X265_10 = ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le", "-x265-params", "log-level=error"]
SVTAV1 = ["-c:v", "libsvtav1", "-preset", "12", "-pix_fmt", "yuv420p"]
VP9 = ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "500k", "-pix_fmt", "yuv420p"]
# 自訂（custom）樣本常用的輸入：跟一般樣本一樣的彩條（320×240、24 fps）+ 測試音
TEST_AV = ["-f", "lavfi", "-i", f"testsrc2=size=320x240:rate=24:duration={DUR}",
           "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={DUR}"]

HDR10_PARAMS = (
    "log-level=error:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:"
    "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400"
)
HLG_PARAMS = "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc"


@dataclass
class Sample:
    id: str
    tier: str
    ext: str
    v: list[str] | None  # 影像編碼參數；None = 純音訊
    a: list[str] | None  # 音訊編碼參數；None = 沒有音訊
    video: str | None = None  # 預期 mpv 回報的影像編碼名稱
    audio: str | None = None  # 預期 mpv 回報的音訊編碼名稱
    fmt: list[str] = field(default_factory=list)  # 容器參數（-f 之類）
    size: str = "320x240"
    rate: str = "24"
    vf: str | None = None
    sub_embed: str | None = None  # 內嵌字幕編碼：srt / ass / mov_text / webvtt
    # 外掛字幕：srt_utf8 / srt_big5 / srt_utf16 / ass / vtt / smi / microdvd / sup / mpl2 / subviewer / lrc /
    # jacosub / realtext / pjs / aqtitle / vplayer / ttml / dfxp / scc
    sub_ext: str | None = None
    # 自訂外掛字幕：(檔名後綴, 格式 srt/ass/ssa, 編碼, 繁簡 trad/simp)
    extra_subs: list[tuple[str, str, str, str]] = field(default_factory=list)
    subs: list[str] = field(default_factory=list)  # 預期的字幕編碼（內嵌 + 外掛）
    expect: dict = field(default_factory=dict)  # 其他預期：rotate、gamma、primaries、aspect
    post: str | None = None  # 後處理：rotate（加上手機直拍的旋轉中繼資料）
    ext_audio: bool = False  # 另外產生同名的外掛音軌 .mka（英語、660 Hz）
    note: str = ""
    dur: int = DUR
    # 完整的 ffmpeg 參數（輸入、對應、編碼），給多軌之類的特殊樣本用；{srt} 會換成字幕來源檔
    # （{ass}、{chapters}、{scc}、{sup} 也是）
    custom: list[str] | None = None
    # 不用 FFmpeg、由這個程式自己寫的媒體檔：dsf / dff（DSD，FFmpeg 沒有編碼器，每個平台都產生得了）
    writer: str | None = None
    # 需要的 FFmpeg 外部函式庫編碼器（各平台套件版的 FFmpeg 不一定有）：沒有時略過並說明，不算失敗
    needs: str | None = None
    # False = 檔頭寫不出正確的長度（AMV、FFmpeg 寫的 .ra）：manifest 的 duration 是 null，
    # 格式矩陣不檢查長度、不跳轉，直接播到結尾
    known_dur: bool = True

    @property
    def path(self) -> Path:
        return OUT / self.tier / f"{self.id}.{self.ext}"


def samples() -> list[Sample]:
    S = Sample
    return [
        # ───────────── 常見 ─────────────
        S("mp4_h264_aac", "common", "mp4", X264, AAC, "h264", "aac"),
        S("m4v_h264_aac", "common", "m4v", X264, AAC, "h264", "aac", fmt=["-f", "mp4"]),
        S("mp4_hevc_aac", "common", "mp4", X265 + ["-tag:v", "hvc1"], AAC, "hevc", "aac"),
        S("mp4_hevc10_aac", "common", "mp4", X265_10 + ["-tag:v", "hvc1"], AAC, "hevc", "aac", note="HEVC Main10"),
        S("mp4_hevc10_4k", "common", "mp4", X265_10 + ["-tag:v", "hvc1"], AAC, "hevc", "aac",
          size="3840x2160", note="4K HEVC 10-bit，硬體解碼測試用"),
        S("mp4_av1_aac", "common", "mp4", SVTAV1, AAC, "av1", "aac"),
        S("mp4_h264_aac_vfr", "common", "mp4", X264 + ["-fps_mode", "vfr"], AAC, "h264", "aac",
          rate="30", vf="select='not(mod(n\\,3))+lt(t\\,1)'", note="可變幀率（手機錄影）"),
        S("mov_h264_aac", "common", "mov", X264, AAC, "h264", "aac"),
        S("mov_hevc_aac_rot90", "common", "mov", X265 + ["-tag:v", "hvc1"], AAC, "hevc", "aac",
          post="rotate", expect={"rotate": 90}, note="手機直拍的旋轉中繼資料"),
        S("mkv_h264_flac", "common", "mkv", X264, FLAC, "h264", "flac"),
        S("mkv_h264hi10p_aac", "common", "mkv", ["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p10le",
                                                  "-profile:v", "high10"], AAC, "h264", "aac", note="Hi10P（只能軟解）"),
        S("mkv_hevc_ac3", "common", "mkv", X265, AC3, "hevc", "ac3"),
        S("mkv_av1_opus", "common", "mkv", SVTAV1, OPUS, "av1", "opus"),
        S("mkv_h264_aac_ass", "common", "mkv", X264, AAC, "h264", "aac", sub_embed="ass", subs=["ass"],
          note="內嵌 ASS 字幕 + 附加字型"),
        S("mkv_h264_aac_srt", "common", "mkv", X264, AAC, "h264", "aac", sub_embed="srt", subs=["subrip"]),
        S("mp4_h264_aac_movtext", "common", "mp4", X264, AAC, "h264", "aac", sub_embed="mov_text", subs=["mov_text"]),
        S("webm_vp9_opus", "common", "webm", VP9, OPUS, "vp9", "opus"),
        S("webm_vp9p2_opus", "common", "webm", VP9[:-2] + ["-pix_fmt", "yuv420p10le", "-profile:v", "2"], OPUS,
          "vp9", "opus", note="VP9 10-bit（Profile 2）"),
        S("webm_av1_opus", "common", "webm", SVTAV1, OPUS, "av1", "opus"),
        # FFmpeg 內建的 MPEG-4 編碼器 + XVID 標籤，跟 Xvid 檔案相容；
        # 不用 libxvid，因為 Linux / macOS 套件版的 FFmpeg 不一定有
        S("avi_xvid_mp3", "common", "avi", ["-c:v", "mpeg4", "-vtag", "XVID", "-q:v", "4"], MP3, "mpeg4", "mp3",
          note="AVI 最常見的組合（Xvid + MP3）"),
        S("extsub_srt_utf8", "common", "mp4", X264, AAC, "h264", "aac", sub_ext="srt_utf8", subs=["subrip"]),
        S("extsub_srt_big5", "common", "mp4", X264, AAC, "h264", "aac", sub_ext="srt_big5", subs=["subrip"],
          note="Big5 編碼的舊中文字幕"),
        S("extsub_srt_utf16", "common", "mp4", X264, AAC, "h264", "aac", sub_ext="srt_utf16", subs=["subrip"]),
        S("extsub_ass", "common", "mkv", X264, AAC, "h264", "aac", sub_ext="ass", subs=["ass"]),
        # ── 以下重現實際影片庫普查遇到的字幕問題（mpv 內建的 sub-auto 會出錯）──
        S("extsub_gbk_named_tw", "common", "mp4", X264, AAC, "h264", "aac", subs=["ass"],
          extra_subs=[(".zh-TW.ssa", "ssa", "gbk", "trad_long")], note="繁體字用 GBK 編碼、檔名標 zh-TW"),
        S("extsub_gbk_simp", "common", "mkv", X264, AAC, "h264", "aac", subs=["subrip"],
          extra_subs=[(".srt", "srt", "gbk", "simp_long")], expect={"sub_text": "影戏播放器测试"},
          note="GBK 編碼的簡體字幕"),
        S("extsub_ass_no_header", "general", "mkv", X264, AAC, "h264", "aac", subs=["ass"],
          extra_subs=[(".ass", "ass_noheader", "utf-8-sig", "trad")], note="ASS 少了 [Script Info] 標頭"),
        S("extsub_ssa_utf16", "common", "avi", ["-c:v", "mpeg4", "-vtag", "XVID", "-q:v", "4"], MP3, "mpeg4", "mp3",
          subs=["ass"], extra_subs=[(".ssa", "ssa", "utf-16", "trad")], note="UTF-16 的 SSA（mpv 會誤判成 Shift_JIS）"),
        S("extsub_doubledot_tc_sc", "common", "mkv", X264, AAC, "h264", "aac", subs=["ass", "ass"],
          extra_subs=[(".Zh-CN..ass", "ass", "utf-8-sig", "simp"), (".Zh-TW..ass", "ass", "utf-8-sig", "trad")],
          note="Zh-TW..ass 雙點檔名，要選到繁中"),
        S("extsub_big5_gb", "common", "mkv", X264, AAC, "h264", "aac", subs=["ass", "ass"],
          extra_subs=[(".gb.ass", "ass", "gbk", "simp"), (".big5.ass", "ass", "cp950", "trad")],
          note="big5.ass / gb.ass，要選到繁中"),
        S("extsub_unlabeled_content", "common", "mkv", X264, AAC, "h264", "aac", subs=["subrip", "subrip"],
          extra_subs=[(".1.srt", "srt", "utf-8", "simp"), (".2.srt", "srt", "utf-8", "trad")],
          note="沒標語言，靠內容判斷繁簡"),
        # 多音軌、多字幕：介面測試（tests/ui.rs）也用這個檔案測選單切換
        S("mkv_multitrack", "common", "mkv", X264, AAC, "h264", "aac", subs=["subrip", "subrip"], dur=20,
          note="雙音軌 + 雙字幕", custom=[
              "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=24:duration=20",
              "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=20",
              "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000:duration=20",
              "-i", "{srt}", "-i", "{srt}",
              "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map", "4:s",
              *X264, *AAC, "-c:s", "srt",
              "-metadata:s:a:0", "language=jpn", "-metadata:s:a:0", "title=日本語",
              "-metadata:s:a:1", "language=chi", "-metadata:s:a:1", "title=國語",
              "-metadata:s:s:0", "language=chi", "-metadata:s:s:0", "title=繁體中文",
              "-metadata:s:s:1", "language=eng", "-metadata:s:s:1", "title=English",
          ]),
        # 章節：介面測試用來測章節選單、跳章節、進度條上的刻度
        S("mkv_chapters", "common", "mkv", X264, AAC, "h264", "aac", dur=12, note="三個章節（片頭、本篇、片尾）",
          custom=[
              "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=24:duration=12",
              "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=12",
              "-i", "{chapters}",
              "-map", "0:v", "-map", "1:a", "-map_chapters", "2",
              *X264, *AAC,
          ]),
        # 沒有音軌的影片：介面測試用它測「暫停中逐格到結尾不換檔」（有音軌時 mpv 的行為不一樣）
        S("mp4_h264_noaudio", "common", "mp4", X264, None, "h264", None, note="只有影像、沒有音軌"),
        # 有專輯封面和標籤的 MP3：介面測試確認會顯示封面與歌名、演出者、專輯
        S("audio_mp3_cover", "general", "mp3", None, MP3, None, "mp3", note="專輯封面 + ID3 標籤", custom=[
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3",
            "-f", "lavfi", "-i", "testsrc2=size=300x300:rate=1:duration=1",
            # 不要加 -frames:v 1：會讓整個檔案在第一格就結束，音訊只剩 1 秒
            "-map", "0:a", "-map", "1:v", *MP3, "-c:v", "mjpeg",
            "-disposition:v:0", "attached_pic", "-id3v2_version", "3",
            "-metadata", "title=測試歌曲", "-metadata", "artist=影戲樂團", "-metadata", "album=範例專輯",
        ]),
        # 同名的外掛音軌（字幕組常附的 .mka）：介面測試確認會載入、但預設還是影片內建的音軌
        S("mkv_extaudio", "common", "mkv", X264, AAC, "h264", "aac", ext_audio=True, note="同名的外掛音軌 .mka"),
        # 一分半的小檔案：續播只記一分鐘以上的檔案，介面測試用它測續播
        S("mp4_long", "common", "mp4", X264, AAC, "h264", "aac", size="160x90", rate="10", dur=90,
          note="90 秒（續播測試）"),

        # ───────────── 通用 ─────────────
        S("ts_h264_aac", "general", "ts", X264, AAC, "h264", "aac", fmt=["-f", "mpegts"]),
        S("m2ts_h264_ac3", "general", "m2ts", X264, AC3, "h264", "ac3", fmt=["-f", "mpegts", "-mpegts_m2ts_mode", "1"]),
        S("mts_h264_ac3", "general", "mts", X264, AC3, "h264", "ac3", fmt=["-f", "mpegts", "-mpegts_m2ts_mode", "1"],
          note="AVCHD 攝影機"),
        S("ts_mpeg2_interlaced", "general", "ts", ["-c:v", "mpeg2video", "-b:v", "4M", "-flags", "+ilme+ildct",
                                                    "-top", "1", "-field_order", "tt"], ["-c:a", "mp2", "-ac", "2"],
          "mpeg2video", "mp2", size="720x480", rate="30000/1001", fmt=["-f", "mpegts"], note="隔行掃描（電視錄影）"),
        S("mpg_mpeg1_mp2", "general", "mpg", ["-c:v", "mpeg1video", "-b:v", "1M"], ["-c:a", "mp2", "-ac", "2"],
          "mpeg1video", "mp2", rate="25", fmt=["-f", "mpeg"]),
        S("vob_mpeg2_ac3_anamorphic", "general", "vob", ["-c:v", "mpeg2video", "-b:v", "4M", "-aspect", "16:9"], AC3,
          "mpeg2video", "ac3", size="720x480", rate="30000/1001", fmt=["-f", "vob"],
          expect={"aspect": 16 / 9}, note="DVD 變形寬螢幕（非方形像素）"),
        S("mkv_mpeg2_ac3", "general", "mkv", ["-c:v", "mpeg2video", "-b:v", "2M"], AC3, "mpeg2video", "ac3"),
        S("wmv_wmv2_wma", "general", "wmv", ["-c:v", "wmv2", "-b:v", "1M"], ["-c:a", "wmav2", "-b:a", "128k", "-ac", "2"],
          "wmv2", "wmav2", fmt=["-f", "asf"]),
        S("flv_h264_aac", "general", "flv", X264, AAC, "h264", "aac"),
        S("f4v_h264_aac", "general", "f4v", X264, AAC, "h264", "aac", fmt=["-f", "f4v"]),
        S("3gp_h263_aac", "general", "3gp", ["-c:v", "h263", "-b:v", "256k"], ["-c:a", "aac", "-b:a", "64k", "-ac", "1"],
          "h263", "aac", size="176x144", rate="15"),
        S("3g2_h264_aac", "general", "3g2", X264, ["-c:a", "aac", "-b:a", "64k", "-ac", "1"], "h264", "aac",
          size="176x144", rate="15"),
        S("ogv_theora_vorbis", "general", "ogv", ["-c:v", "libtheora", "-q:v", "6"], ["-c:a", "libvorbis", "-ac", "2"],
          "theora", "vorbis"),
        S("mov_prores_pcm", "general", "mov", ["-c:v", "prores_ks", "-profile:v", "0"], PCM, "prores", "pcm_s16le"),
        S("avi_mjpeg_pcm", "general", "avi", ["-c:v", "mjpeg", "-q:v", "5", "-pix_fmt", "yuvj420p"], PCM,
          "mjpeg", "pcm_s16le"),
        S("webm_vp8_vorbis", "general", "webm", ["-c:v", "libvpx", "-deadline", "realtime", "-b:v", "500k"],
          ["-c:a", "libvorbis", "-ac", "2"], "vp8", "vorbis"),
        S("mp4_mpeg4_aac", "general", "mp4", ["-c:v", "mpeg4", "-q:v", "4"], AAC, "mpeg4", "aac"),
        S("mkv_h264_eac3", "general", "mkv", X264, ["-c:a", "eac3", "-b:a", "192k", "-ac", "2"], "h264", "eac3"),
        S("mkv_h264_dts", "general", "mkv", X264, ["-c:a", "dca", "-strict", "-2", "-ac", "2"], "h264", "dts"),
        S("mkv_h264_vorbis", "general", "mkv", X264, ["-c:a", "libvorbis", "-ac", "2"], "h264", "vorbis"),
        S("mov_h264_alac", "general", "mov", X264, ["-c:a", "alac", "-ac", "2"], "h264", "alac"),
        S("mkv_h264_pcm", "general", "mkv", X264, PCM, "h264", "pcm_s16le"),
        S("mkv_h264_mp2", "general", "mkv", X264, ["-c:a", "mp2", "-ac", "2"], "h264", "mp2"),
        # 每 2 秒剛好一個關鍵影格（不因為畫面變化多插）：片段匯出（不重新編碼）的關鍵影格對齊測試用
        S("mkv_h264_gop2", "general", "mkv", X264 + ["-g", "48", "-keyint_min", "48", "-sc_threshold", "0"], AAC,
          "h264", "aac", dur=12, note="12 秒、每 2 秒一個關鍵影格（片段匯出）"),
        # 每 10 秒一個關鍵影格的 TS：用時間跳轉常落在關鍵影格之後（片段匯出要從更前面重讀）
        S("ts_h264_gop10", "general", "ts", X264 + ["-g", "240", "-keyint_min", "240", "-sc_threshold", "0"], AAC,
          "h264", "aac", fmt=["-f", "mpegts"], dur=20, note="20 秒、每 10 秒一個關鍵影格的 TS（片段匯出）"),
        # 每 0.5 秒一個關鍵影格的 TS（像電視錄影）：用時間跳轉常落在下一個關鍵影格（GIF 要從 A 前面讀）
        S("ts_h264_gop05", "general", "ts", X264 + ["-g", "12", "-keyint_min", "12", "-sc_threshold", "0"], AAC,
          "h264", "aac", fmt=["-f", "mpegts"], dur=20, note="20 秒、每 0.5 秒一個關鍵影格的 TS（轉成 GIF）"),
        # 一分半、每 2 秒一個關鍵影格的 TS（像電視錄影）：續播只記一分鐘以上的檔案，
        # 介面測試用它測續播、書籤在跳轉不準的格式裡的跳轉（主播放器往前多跳 1 秒不夠時要再往前）
        S("ts_h264_long", "general", "ts", X264 + ["-g", "48", "-keyint_min", "48", "-sc_threshold", "0"], AAC,
          "h264", "aac", size="160x90", fmt=["-f", "mpegts"], dur=90,
          note="90 秒、每 2 秒一個關鍵影格的 TS（續播、書籤的跳轉）"),
        # 影像比聲音晚 1.6 秒開始的 TS（電視錄影從 GOP 中間開始）：A 選在影像出來之前時，GIF 不算太短
        S("ts_h264_late_video", "general", "ts", X264, AAC, "h264", "aac", dur=6,
          note="影像比聲音晚 1.6 秒開始的 TS（轉成 GIF）", custom=[
              "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=24:duration=4.4",
              "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=6",
              "-map", "0:v", "-map", "1:a", "-vf", "setpts=PTS+1.6/TB", "-fps_mode", "passthrough",
              *X264, *AAC, "-f", "mpegts",
          ]),
        # 每 30 秒一個關鍵影格：A 之前的關鍵影格在讀取起點前面很遠（片段匯出往前讀的秒數）
        S("mkv_h264_gop30", "general", "mkv", X264 + ["-g", "720", "-keyint_min", "720", "-sc_threshold", "0"], AAC,
          "h264", "aac", dur=75, note="75 秒、每 30 秒一個關鍵影格（片段匯出）"),
        # Matroska 裡的 WebVTT，mpv 回報的編碼名稱是 webvtt-webm
        S("mkv_h264_aac_webvtt", "general", "mkv", X264, AAC, "h264", "aac", sub_embed="webvtt", subs=["webvtt-webm"]),
        S("extsub_vtt", "general", "mp4", X264, AAC, "h264", "aac", sub_ext="vtt", subs=["webvtt"]),
        S("extsub_smi", "general", "mp4", X264, AAC, "h264", "aac", sub_ext="smi", subs=["sami"]),
        S("mkv_hevc10_hdr10", "general", "mkv", ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le",
                                                  "-x265-params", HDR10_PARAMS, "-color_primaries", "bt2020",
                                                  "-color_trc", "smpte2084", "-colorspace", "bt2020nc"], AAC,
          "hevc", "aac", expect={"gamma": "pq", "primaries": "bt.2020"}, note="HDR10"),
        S("mkv_hevc10_hlg", "general", "mkv", ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le",
                                                "-x265-params", HLG_PARAMS, "-color_primaries", "bt2020",
                                                "-color_trc", "arib-std-b67", "-colorspace", "bt2020nc"], AAC,
          "hevc", "aac", expect={"gamma": "hlg", "primaries": "bt.2020"}, note="HLG"),
        # 上面的 HDR10 是彩條直接當成 PQ，幾乎都是 1000 nits 以上的亮部，目標亮度 100 跟 203 都壓到最亮、看不出差別；
        # 這個把亮度、飽和度都壓低（大多在 SDR 參考白 203 nits 以下），tests/picture_shot.rs 用來比較目標亮度
        S("mkv_hevc10_hdr10_mid", "general", "mkv", ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le",
                                                      "-x265-params", HDR10_PARAMS, "-color_primaries", "bt2020",
                                                      "-color_trc", "smpte2084", "-colorspace", "bt2020nc"], None,
          "hevc", vf="lutyuv=y=16+(val-16)*0.6:u=128+(val-128)*0.4:v=128+(val-128)*0.4", expect={"gamma": "pq", "primaries": "bt.2020"},
          note="HDR10，亮度大多在 203 nits 以下"),
        S("mp4_h264_120fps", "general", "mp4", X264, AAC, "h264", "aac", rate="120", note="高幀率"),
        S("audio_mp3", "general", "mp3", None, MP3, None, "mp3"),
        S("audio_aac", "general", "m4a", None, AAC, None, "aac"),
        S("audio_flac", "general", "flac", None, FLAC, None, "flac"),
        S("audio_opus", "general", "opus", None, OPUS, None, "opus"),
        S("audio_vorbis", "general", "ogg", None, ["-c:a", "libvorbis", "-ac", "2"], None, "vorbis"),
        S("audio_wav", "general", "wav", None, PCM, None, "pcm_s16le"),
        # 藍光的 PGS 圖形字幕（.sup）：FFmpeg 沒有 PGS 編碼器，由這個程式自己寫（pgs_bytes），每個平台都產生得了。
        # 外掛的 .sup 走影戲自己的外掛字幕流程（圖形字幕照原路徑交給 mpv）；MKV 內嵌的直接複製進去。
        # 先放在「罕見」，播放引擎與 Linux tar.gz 用的系統 libmpv 0.37（FFmpeg 6.1）都通過之後才改成「通用」（失敗會擋下）
        S("extsub_pgs", "general", "mp4", X264, AAC, "h264", "aac", sub_ext="sup", subs=["hdmv_pgs_subtitle"],
          expect={"sub_bitmap": True}, note="外掛的藍光 PGS 字幕（.sup）"),
        S("mkv_h264_pgs", "general", "mkv", X264, AAC, "h264", "aac", subs=["hdmv_pgs_subtitle"],
          expect={"sub_bitmap": True}, note="MKV 內嵌的藍光 PGS 字幕", custom=[
              *TEST_AV, "-i", "{sup}", "-map", "0:v", "-map", "1:a", "-map", "2:s", *X264, *AAC, "-c:s", "copy",
          ]),

        # ───────────── 罕見 ─────────────
        # 手機直拍轉存成 MKV：旋轉放在 Matroska 的容器層（mpv 自己的 MKV 解析器讀，不在影格上）。
        # FFmpeg 6.1 做出來的也正確（ProjectionPoseRoll = -90），但 Linux tar.gz 用的系統 mpv 0.37 沒有把 Matroska 的
        # 角度反過來、讀成 270 度（本專案的引擎會反），所以放在「罕見」（失敗只列在報告裡；ROADMAP 第 2.6 節）
        S("mkv_hevc_aac_rot90", "rare", "mkv", X265, AAC, "hevc", "aac",
          post="rotate", expect={"rotate": 90}, note="直拍影片轉存的 MKV（容器層的旋轉）"),
        S("rm_rv20_ac3", "rare", "rm", ["-c:v", "rv20", "-b:v", "500k"], AC3, "rv20", "ac3", note="RealMedia"),
        S("mxf_mpeg2_pcm", "rare", "mxf", ["-c:v", "mpeg2video", "-b:v", "8M", "-g", "1"],
          ["-c:a", "pcm_s16le", "-ac", "1"], "mpeg2video", "pcm_s16le", size="720x576", rate="25"),
        S("dv_dvvideo_pcm", "rare", "dv", ["-c:v", "dvvideo", "-pix_fmt", "yuv420p"], ["-c:a", "pcm_s16le", "-ac", "2"],
          "dvvideo", "pcm_s16le", size="720x576", rate="25", fmt=["-f", "dv"]),
        S("nut_ffv1_flac", "rare", "nut", ["-c:v", "ffv1"], FLAC, "ffv1", "flac"),
        S("ivf_vp8", "rare", "ivf", ["-c:v", "libvpx", "-deadline", "realtime", "-b:v", "500k"], None, "vp8"),
        S("y4m_raw", "rare", "y4m", ["-pix_fmt", "yuv420p"], None, "rawvideo", fmt=["-f", "yuv4mpegpipe"]),
        S("mkv_ffv1_flac", "rare", "mkv", ["-c:v", "ffv1"], FLAC, "ffv1", "flac", note="無損"),
        S("avi_huffyuv_pcm", "rare", "avi", ["-c:v", "huffyuv", "-pix_fmt", "yuv422p"], PCM, "huffyuv", "pcm_s16le"),
        S("avi_utvideo_pcm", "rare", "avi", ["-c:v", "utvideo"], PCM, "utvideo", "pcm_s16le"),
        S("mov_dnxhr_pcm", "rare", "mov", ["-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"], PCM,
          "dnxhd", "pcm_s16le", size="1280x720"),
        S("mov_qtrle_pcm", "rare", "mov", ["-c:v", "qtrle"], PCM, "qtrle", "pcm_s16le", note="QuickTime Animation"),
        S("avi_cinepak_pcm", "rare", "avi", ["-c:v", "cinepak"], PCM, "cinepak", "pcm_s16le"),
        S("avi_msvideo1_pcm", "rare", "avi", ["-c:v", "msvideo1", "-pix_fmt", "rgb555le"], PCM, "msvideo1", "pcm_s16le"),
        S("asf_msmpeg4v3_wma", "rare", "asf", ["-c:v", "msmpeg4v3", "-b:v", "1M"],
          ["-c:a", "wmav2", "-b:a", "128k", "-ac", "2"], "msmpeg4v3", "wmav2", note="DivX 3"),
        S("flv_flv1_mp3", "rare", "flv", ["-c:v", "flv", "-b:v", "500k"], ["-c:a", "libmp3lame", "-ar", "44100", "-ac", "2"],
          "flv1", "mp3", note="Sorenson Spark"),
        S("mov_svq1_pcm", "rare", "mov", ["-c:v", "svq1"], PCM, "svq1", "pcm_s16le", note="Sorenson Video 1"),
        S("mp4_vvc_aac", "rare", "mp4", ["-c:v", "libvvenc", "-preset", "faster", "-pix_fmt", "yuv420p10le"], AAC,
          "vvc", "aac", needs="libvvenc", note="H.266 / VVC"),
        S("mkv_dirac_flac", "rare", "mkv", ["-c:v", "vc2", "-b:v", "5M"], FLAC, "dirac", "flac"),
        S("mkv_hevc444_12bit", "rare", "mkv", ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv444p12le",
                                               "-x265-params", "log-level=error"], AAC, "hevc", "aac", note="12-bit 4:4:4"),
        S("mkv_h264_truehd", "rare", "mkv", X264, ["-c:a", "truehd", "-strict", "-2", "-ac", "2"], "h264", "truehd"),
        S("audio_wavpack", "rare", "wv", None, ["-c:a", "wavpack", "-ac", "2"], None, "wavpack"),
        S("audio_tta", "rare", "tta", None, ["-c:a", "tta", "-ac", "2"], None, "tta"),
        S("audio_amr", "rare", "amr", None, ["-c:a", "libopencore_amrnb", "-ar", "8000", "-ac", "1", "-b:a", "12.2k"],
          None, "amr_nb", needs="libopencore_amrnb"),
        S("audio_speex", "rare", "spx", None, ["-c:a", "libspeex", "-ar", "16000", "-ac", "1"], None, "speex",
          fmt=["-f", "ogg"], needs="libspeex"),
        # 罕見的純音訊副檔名（formats::AUDIO_RARE：能播，但不加入檔案關聯）。都是 FFmpeg 內建的編碼器，每個平台都產生得了。
        # .ra（RealAudio）在下面（audio_ra144）：rm 封裝寫不出長度，只檢查能播到結尾
        S("audio_aiff", "rare", "aiff", None, ["-c:a", "pcm_s16be", "-ac", "2"], None, "pcm_s16be", note="AIFF"),
        S("audio_aifc_sowt", "rare", "aifc", None, ["-c:a", "pcm_s16le", "-ac", "2"], None, "pcm_s16le",
          fmt=["-f", "aiff"], note="AIFF-C（sowt）"),
        S("audio_caf_alac", "rare", "caf", None, ["-c:a", "alac", "-ac", "2"], None, "alac", note="Apple Lossless 的 CAF"),
        S("extsub_microdvd", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="microdvd", subs=["microdvd"],
          note="影格制字幕"),

        # ── 罕見的影像編碼、容器（L3「罕見格式盡力支援」）。都是 FFmpeg 內建的編碼器，除非標了 needs ──
        S("mov_cfhd_pcm", "rare", "mov", ["-c:v", "cfhd", "-pix_fmt", "yuv422p10le"], PCM, "cfhd", "pcm_s16le",
          note="CineForm"),
        S("avi_magicyuv_pcm", "rare", "avi", ["-c:v", "magicyuv"], PCM, "magicyuv", "pcm_s16le", note="MagicYUV（無損）"),
        S("avi_msmpeg4v2_mp3", "rare", "avi", ["-c:v", "msmpeg4v2", "-b:v", "800k"], MP3, "msmpeg4v2", "mp3",
          note="MS-MPEG4 v2（影片庫普查遇過）"),
        S("wmv_wmv1_wmav1", "rare", "wmv", ["-c:v", "wmv1", "-b:v", "800k"], ["-c:a", "wmav1", "-b:a", "128k", "-ac", "2"],
          "wmv1", "wmav1", fmt=["-f", "asf"], note="WMV 7 + WMA 1"),
        S("rmvb_rv10_ra144", "rare", "rmvb", ["-c:v", "rv10", "-b:v", "500k"],
          ["-c:a", "real_144", "-ar", "8000", "-ac", "1"], "rv10", "ra_144", fmt=["-f", "rm"],
          note=".rmvb + RealAudio 14.4"),
        # AMV（MP3 隨身聽）：FFmpeg 的 AMV 封裝不寫真正的長度（檔頭說 1 秒左右），只檢查能播到結尾
        S("amv_amv_adpcm", "rare", "amv", ["-c:v", "amv", "-pix_fmt", "yuvj420p"],
          ["-c:a", "adpcm_ima_amv", "-ar", "22050", "-ac", "1", "-block_size", "882", "-strict", "-1"],
          "amv", "adpcm_ima_amv", size="160x120", rate="25", known_dur=False, note="AMV（檔頭沒有正確的長度）"),
        S("wtv_mpeg2_ac3", "rare", "wtv", ["-c:v", "mpeg2video", "-b:v", "4M"], AC3, "mpeg2video", "ac3",
          size="720x480", rate="30000/1001", fmt=["-f", "wtv"], note="Windows Media Center 錄影"),
        S("mxf_jpeg2000_pcm", "rare", "mxf", ["-c:v", "jpeg2000", "-pix_fmt", "yuv422p"], ["-c:a", "pcm_s16le", "-ac", "1"],
          "jpeg2000", "pcm_s16le", rate="25", note="JPEG 2000（廣播、DCP）"),
        S("avi_zmbv_pcm", "rare", "avi", ["-c:v", "zmbv", "-pix_fmt", "bgr0"], PCM, "zmbv", "pcm_s16le",
          note="DOSBox 錄影"),
        S("mov_rpza_pcm", "rare", "mov", ["-c:v", "rpza", "-pix_fmt", "rgb555be"], PCM, "rpza", "pcm_s16le",
          note="舊 QuickTime（Apple Video）"),
        S("mov_hap_pcm", "rare", "mov", ["-c:v", "hap"], PCM, "hap", "pcm_s16le", needs="hap",
          note="Hap（要有 libsnappy 的 FFmpeg）"),
        S("mp4_apv_aac", "rare", "mp4", ["-c:v", "liboapv", "-pix_fmt", "yuv422p10le"], AAC, "apv", "aac",
          needs="liboapv", note="APV（Samsung 的專業錄影編碼）"),
        # 播放引擎解不了的 AVS2（libdavs2 是 GPL）：影戲說明不支援，聲音照常播到結尾
        S("mkv_avs2_aac", "rare", "mkv", ["-c:v", "libxavs2", "-pix_fmt", "yuv420p"], AAC, None, "aac",
          needs="libxavs2", expect={"missing": ["avs2"]}, note="AVS2（不支援，只播放聲音）"),

        # ── 罕見的音訊 ──
        S("audio_dsf", "rare", "dsf", None, None, None, "dsd_lsbf_planar", writer="dsf", note="DSD（DSF）"),
        S("audio_dff", "rare", "dff", None, None, None, "dsd_msbf", writer="dff", note="DSD（DSDIFF）"),
        S("audio_ac3", "rare", "ac3", None, AC3, None, "ac3", note="AC-3 原始串流"),
        S("audio_dts", "rare", "dts", None, ["-c:a", "dca", "-strict", "-2", "-ac", "2"], None, "dts",
          note="DTS 原始串流"),
        S("wav_adpcm_ms", "rare", "wav", None, ["-c:a", "adpcm_ms", "-ac", "2"], None, "adpcm_ms", note="MS ADPCM 的 WAV"),
        S("audio_amrwb", "rare", "amr", None, ["-c:a", "libvo_amrwbenc", "-ar", "16000", "-ac", "1", "-b:a", "23.85k"],
          None, "amr_wb", needs="libvo_amrwbenc", note="AMR-WB"),
        # FFmpeg 的 rm 封裝寫的 .ra 沒有長度、時間也不對（播放位置一直在 0 附近），只檢查能播到結尾
        S("audio_ra144", "rare", "ra", None, ["-c:a", "real_144", "-ar", "8000", "-ac", "1"], None, "ra_144",
          fmt=["-f", "rm"], known_dur=False, note="RealAudio 14.4（.ra，FFmpeg 寫不出長度）"),

        # ── 罕見的文字字幕（這個程式自己寫；影戲自己的外掛字幕流程，轉成 UTF-8、不加 BOM）──
        S("extsub_mpl2", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="mpl2", subs=["mpl2"], note="MPL2（.txt）"),
        S("extsub_subviewer", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="subviewer", subs=["subviewer"],
          note="SubViewer 2（mpv 0.37 只顯示第一句）"),
        S("extsub_lrc", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="lrc", subs=["text"], note="LRC 歌詞"),
        S("extsub_jacosub", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="jacosub", subs=["jacosub"],
          note="JACOsub"),
        S("extsub_realtext", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="realtext", subs=["realtext"],
          note="RealText"),
        S("extsub_pjs", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="pjs", subs=["pjs"], note="PJS"),
        S("extsub_aqtitle", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="aqtitle", subs=["text"], note="AQTitle"),
        S("extsub_vplayer", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="vplayer", subs=["vplayer"],
          note="VPlayer（.txt）"),
        # TTML / DFXP：FFmpeg 讀不了，影戲轉成 SRT 再交給 mpv
        S("extsub_ttml", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="ttml", subs=["subrip"],
          note="TTML（影戲轉成 SRT）"),
        S("extsub_dfxp", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="dfxp", subs=["subrip"],
          note="DFXP（舊版 TTML，影戲轉成 SRT）"),
        # CEA-608 隱藏字幕只有 ASCII
        S("extsub_scc", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="scc", subs=["eia_608"],
          expect={"sub_text": CC_PROBE_TEXT}, note="SCC（CEA-608 隱藏字幕）"),
        S("mov_h264_cc608", "rare", "mov", X264, AAC, "h264", "aac", subs=["eia_608"],
          expect={"sub_text": CC_PROBE_TEXT}, note="MOV 裡的 CEA-608 字幕軌", custom=[
              "-f", "lavfi", "-i", f"testsrc2=size=320x240:rate=30000/1001:duration={DUR}",
              "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={DUR}",
              "-i", "{scc}", "-map", "0:v", "-map", "1:a", "-map", "2:s", *X264, *AAC, "-c:s", "copy",
          ]),

        # ── 罕見的圖形字幕：從 PGS 轉成 DVD、DVB 字幕 ──
        # FFmpeg 解 PGS 時每句的長度是「不知道」（2^32 毫秒），直接轉出去的 MKV 長度變成 49 天；
        # -fix_sub_duration 用下一句的時間補上長度
        S("mkv_h264_dvdsub", "rare", "mkv", X264, AAC, "h264", "aac", subs=["dvd_subtitle"],
          expect={"sub_bitmap": True}, note="DVD 圖形字幕（VobSub）", custom=[
              *TEST_AV, "-fix_sub_duration", "-i", "{sup}", "-map", "0:v", "-map", "1:a", "-map", "2:s", *X264, *AAC,
              "-c:s", "dvdsub",
          ]),
        S("ts_mpeg2_dvbsub", "rare", "ts", ["-c:v", "mpeg2video", "-b:v", "2M"], AC3, "mpeg2video", "ac3",
          subs=["dvb_subtitle"], expect={"sub_bitmap": True}, note="數位電視的 DVB 圖形字幕", custom=[
              *TEST_AV, "-fix_sub_duration", "-i", "{sup}", "-map", "0:v", "-map", "1:a", "-map", "2:s",
              "-c:v", "mpeg2video", "-b:v", "2M", *AC3, "-c:s", "dvbsub", "-f", "mpegts",
          ]),
    ]


# ───────────── 字幕檔 ─────────────

def _ts(t: float, sep: str = ",") -> str:
    h, rem = divmod(int(round(t * 1000)), 3_600_000)
    m, rem = divmod(rem, 60_000)
    s, ms = divmod(rem, 1000)
    return f"{h:02}:{m:02}:{s:02}{sep}{ms:03}"


def chapters_text() -> str:
    """FFmpeg 的 metadata 檔：三個章節，約 4 秒一章。
    時間刻意不對齊影格（跟藍光、mkvmerge 做的檔案一樣到奈秒）：跳到章節後畫面的時間會比章節時間早一點點"""
    starts = [0, 4_037_366_667, 8_041_366_667, 12_000_000_000]
    lines = [";FFMETADATA1"]
    for i, title in enumerate(["片頭", "本篇", "片尾"]):
        lines += ["[CHAPTER]", "TIMEBASE=1/1000000000", f"START={starts[i]}", f"END={starts[i + 1]}", f"title={title}"]
    return "\n".join(lines) + "\n"


def srt_text(cues=SUB_CUES) -> str:
    return "".join(f"{i}\n{_ts(a)} --> {_ts(b)}\n{t}\n\n" for i, (a, b, t) in enumerate(cues, 1))


def vtt_text(cues=SUB_CUES) -> str:
    return "WEBVTT\n\n" + "".join(f"{_ts(a, '.')} --> {_ts(b, '.')}\n{t}\n\n" for a, b, t in cues)


def _ass_ts(t: float) -> str:
    cs = int(round(t * 100))
    return f"{cs // 360000}:{cs // 6000 % 60:02}:{cs // 100 % 60:02}.{cs % 100:02}"


def ssa_text(cues=SUB_CUES) -> str:
    """舊式 SSA（v4.00）。實際影片庫裡有 UTF-16 編碼的 SSA，mpv 會把它誤判成 Shift_JIS。"""
    events = "".join(f"Dialogue: Marked=0,{_ass_ts(a)},{_ass_ts(b)},Default,,0000,0000,0000,,{t}\n" for a, b, t in cues)
    return (
        "[Script Info]\nScriptType: v4.00\nPlayResX: 320\nPlayResY: 240\n\n"
        "[V4 Styles]\n"
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, "
        "BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding\n"
        "Style: Default,Arial,20,16777215,65535,65535,0,0,0,1,2,1,2,10,10,10,0,1\n\n"
        "[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n" + events
    )


def ass_text(cues=SUB_CUES) -> str:
    events = "".join(
        f"Dialogue: 0,{_ass_ts(a)},{_ass_ts(b)},Default,,0,0,0,,{{\\fad(100,100)}}{t}\n" for a, b, t in cues
    )
    return (
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n"
        "[V4+ Styles]\n"
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, "
        "Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, "
        "MarginL, MarginR, MarginV, Encoding\n"
        "Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1\n\n"
        "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n" + events
    )


def smi_text() -> str:
    body = "".join(
        f"<SYNC Start={int(a * 1000)}><P Class=ZHTW>{t}\n<SYNC Start={int(b * 1000)}><P Class=ZHTW>&nbsp;\n"
        for a, b, t in SUB_CUES
    )
    return (
        "<SAMI>\n<HEAD>\n<STYLE TYPE=\"text/css\">\n<!--\nP { margin-left:8pt; }\n"
        ".ZHTW { Name:Chinese; lang:zh-TW; }\n-->\n</STYLE>\n</HEAD>\n<BODY>\n" + body + "</BODY>\n</SAMI>\n"
    )


def microdvd_text(fps: float = 24.0) -> str:
    # 第一行 {1}{1}<fps> 是 MicroDVD 慣用的幀率標頭；
    # FFmpeg 的格式偵測也要求前三行都符合 {n}{n} 格式，只有兩條字幕會偵測失敗
    header = f"{{1}}{{1}}{fps:.3f}\n"
    return header + "".join(f"{{{round(a * fps)}}}{{{round(b * fps)}}}{t}\n" for a, b, t in SUB_CUES)


# ── 罕見的文字字幕（tests/subs_formats.rs 用同樣的寫法、英文的內容測過每一種）──

def _centis(t: float) -> str:
    """`HH:MM:SS.cc`（百分之一秒）"""
    return _ts(t, ".")[:-1]


def mpl2_text(cues=SUB_CUES) -> str:
    """MPL2：[開始][結束]，單位 0.1 秒"""
    return "".join(f"[{round(a * 10)}][{round(b * 10)}]{t}\n" for a, b, t in cues)


def subviewer_text(cues=SUB_CUES) -> str:
    """SubViewer 2.0"""
    head = ("[INFORMATION]\n[TITLE]test\n[AUTHOR]\n[SOURCE]\n[PRG]\n[FILEPATH]\n[DELAY]0\n[CD TRACK]0\n[COMMENT]\n"
            "[END INFORMATION]\n[SUBTITLE]\n[COLF]&HFFFFFF,[STYLE]bd,[SIZE]18,[FONT]Arial\n")
    return head + "".join(f"{_centis(a)},{_centis(b)}\n{t}\n\n" for a, b, t in cues)


def _lrc_time(t: float) -> str:
    cs = int(round(t * 100))
    return f"[{cs // 6000:02}:{cs // 100 % 60:02}.{cs % 100:02}]"


def lrc_text(cues=SUB_CUES) -> str:
    """LRC 歌詞：每句的開始，結束時放一行空的"""
    return "[ar:影戲]\n[ti:測試]\n" + "".join(f"{_lrc_time(a)}{t}\n{_lrc_time(b)}\n" for a, b, t in cues)


def jacosub_text(cues=SUB_CUES) -> str:
    def jts(t: float) -> str:
        cs = int(round(t * 100))
        return f"{cs // 360000}:{cs // 6000 % 60:02}:{cs // 100 % 60:02}.{cs % 100:02}"
    return "#TITLE test\n#TIMERES 100\n" + "".join(f"{jts(a)} {jts(b)} D {t}\n" for a, b, t in cues)


def realtext_text(cues=SUB_CUES) -> str:
    body = "".join(f'<time begin="{_centis(a)}" end="{_centis(b)}"/><clear/>{t}\n' for a, b, t in cues)
    return f'<window type="generic" duration="00:00:{DUR:02}.00" >\n{body}</window>\n'


def pjs_text(cues=SUB_CUES) -> str:
    """PJS：開始,結束,"文字"，單位 0.1 秒"""
    return "".join(f'{round(a * 10)},{round(b * 10)},"{t}"\n' for a, b, t in cues)


def aqtitle_text(cues=SUB_CUES) -> str:
    """AQTitle：影格編號（25 fps），結束時放一段空的"""
    return "".join(f"-->> {round(a * 25)}\n{t}\n\n-->> {round(b * 25)}\n\n" for a, b, t in cues)


def vplayer_text(cues=SUB_CUES) -> str:
    """VPlayer：時間:文字，結束時放一行空的"""
    return "".join(f"{_centis(a)}:{t}\n{_centis(b)}:\n" for a, b, t in cues)


def ttml_text(cues=SUB_CUES) -> str:
    """TTML（現行的命名空間、時鐘時間）。第二句是斜體、在畫面上方的區域（轉成 SRT 的 `<i>`、`{\\an8}`）"""
    body = ""
    for i, (a, b, t) in enumerate(cues):
        attrs = ' style="it" region="top"' if i == 1 else ""
        body += f'<p begin="{_ts(a, ".")}" end="{_ts(b, ".")}"{attrs}>{t}</p>\n'
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="zh-TW">\n'
        '<head><styling><style xml:id="it" tts:fontStyle="italic"/></styling>\n'
        '<layout><region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%"/></layout></head>\n'
        f"<body><div>\n{body}</div></body></tt>\n"
    )


def dfxp_text(cues=SUB_CUES) -> str:
    """DFXP（舊版的命名空間、`ttp:tickRate`、`t` 偏移時間、`<br/>`、斜體的 span）"""
    body = ""
    for i, (a, b, t) in enumerate(cues):
        if i == 0:
            t = t.replace(" ", "<br/>", 1)
        elif i == 1:
            t = f'<span tts:fontStyle="italic">{t[:2]}</span>{t[2:]}'
        body += f'<p begin="{round(a * 10_000_000)}t" end="{round(b * 10_000_000)}t">{t}</p>\n'
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<tt xmlns="http://www.w3.org/2006/10/ttaf1" xmlns:tts="http://www.w3.org/2006/10/ttaf1#styling" '
        'xmlns:ttp="http://www.w3.org/2006/10/ttaf1#parameter" ttp:tickRate="10000000" xml:lang="zh-TW">\n'
        f"<body><div>\n{body}</div></body></tt>\n"
    )


def scc_text(cues=CC_CUES) -> str:
    """SCC（CEA-608 pop-on 字幕，30 fps 不丟格的時間碼）。時間碼一定要遞增，不然時間會亂掉"""
    def parity(b: int) -> int:  # 奇同位
        return b | 0x80 if bin(b).count("1") % 2 == 0 else b

    def pairs(text: str) -> str:
        bs = [parity(ord(c)) for c in text]
        if len(bs) % 2:
            bs.append(0x80)
        return " ".join(f"{bs[i]:02x}{bs[i + 1]:02x}" for i in range(0, len(bs), 2))

    def tc(t: float) -> str:
        s, fr = divmod(int(round(t * 30)), 30)
        return f"{s // 3600:02}:{s // 60 % 60:02}:{s % 60:02}:{fr:02}"

    lines = ["Scenarist_SCC V1.0", ""]
    for a, b, t in cues:
        # 載入 pop-on、清掉暫存、移到第 15 列、文字、顯示；結束時清掉畫面
        lines += [f"{tc(a)}\t9420 9420 94ae 94ae 9470 9470 {pairs(t)} 942f 942f", "", f"{tc(b)}\t942c 942c", ""]
    return "\n".join(lines)


# ── 藍光的 PGS 圖形字幕（.sup）：FFmpeg 沒有 PGS 編碼器，自己寫最小的檔案。每句是一個白色的長方形 ──

def pgs_bytes(cues=SUB_CUES, size=(320, 240)) -> bytes:
    width, height = size
    x, y, w, h = 100, 180, 120, 30

    def seg(pts: float, typ: int, payload: bytes) -> bytes:
        t = int(round(pts * 90000))
        return b"PG" + struct.pack(">IIBH", t, t, typ, len(payload)) + payload

    def rle_rows() -> bytes:
        # 每列：一段 w 個像素的顏色 1，再接列尾 00 00
        run = bytes([0x00, 0xC0 | (w >> 8), w & 0xFF, 1]) if w >= 64 else bytes([0x00, 0x80 | w, 1])
        return (run + b"\x00\x00") * h

    def pcs(comp: int, state: int, objects: int) -> bytes:
        body = struct.pack(">HHBHBBBB", width, height, 0x10, comp, state, 0, 0, objects)
        if objects:
            body += struct.pack(">HBBHH", 0, 0, 0, x, y)
        return body

    wds = struct.pack(">BBHHHH", 1, 0, x, y, w, h)
    # 調色盤：顏色 1 是白色（Y=235、Cr=Cb=128、不透明）
    pds = struct.pack(">BB", 0, 0) + struct.pack(">BBBBB", 1, 235, 128, 128, 255)
    data = rle_rows()
    ods = struct.pack(">HBB", 0, 0, 0xC0) + (len(data) + 4).to_bytes(3, "big") + struct.pack(">HH", w, h) + data
    out = bytearray()
    comp = 0
    for a, b, _ in cues:
        # 出現：PCS（新的 epoch）、WDS、PDS、ODS、END；消失：沒有物件的 PCS、WDS、END
        out += seg(a, 0x16, pcs(comp, 0x80, 1)) + seg(a, 0x17, wds) + seg(a, 0x14, pds) + seg(a, 0x15, ods)
        out += seg(a, 0x80, b"")
        out += seg(b, 0x16, pcs(comp + 1, 0x00, 0)) + seg(b, 0x17, wds) + seg(b, 0x80, b"")
        comp += 2
    return bytes(out)


def write_ext_sub(kind: str, video: Path) -> None:
    stem = video.with_suffix("")
    if kind == "sup":
        Path(f"{stem}.sup").write_bytes(pgs_bytes())
        return
    table = {
        "srt_utf8": (".srt", srt_text, "utf-8"),
        "srt_big5": (".srt", srt_text, "cp950"),
        "srt_utf16": (".srt", srt_text, "utf-16"),  # Python 的 utf-16 會加 BOM
        "ass": (".ass", ass_text, "utf-8-sig"),
        "vtt": (".vtt", vtt_text, "utf-8"),
        "smi": (".smi", smi_text, "utf-8"),
        "microdvd": (".sub", microdvd_text, "utf-8"),
        "mpl2": (".txt", mpl2_text, "utf-8"),
        "subviewer": (".sub", subviewer_text, "utf-8"),
        "lrc": (".lrc", lrc_text, "utf-8"),
        "jacosub": (".jss", jacosub_text, "utf-8"),
        "realtext": (".rt", realtext_text, "utf-8"),
        "pjs": (".pjs", pjs_text, "utf-8"),
        "aqtitle": (".aqt", aqtitle_text, "utf-8"),
        "vplayer": (".txt", vplayer_text, "utf-8"),
        "ttml": (".ttml", ttml_text, "utf-8"),
        "dfxp": (".dfxp", dfxp_text, "utf-8"),
        "scc": (".scc", scc_text, "ascii"),
    }
    ext, make, enc = table[kind]
    Path(f"{stem}{ext}").write_text(make(), encoding=enc, newline="\r\n" if ext in (".srt", ".smi") else "\n")


def write_extra_sub(video: Path, suffix: str, fmt: str, encoding: str, variant: str) -> None:
    """寫一個自訂檔名、格式、編碼、繁簡的外掛字幕（重現實際影片庫遇到的情況）"""
    cues = {
        "trad": SUB_CUES,
        "simp": SUB_CUES_SIMP,
        "trad_long": long_cues(SUB_CUES, DIALOGUE_TRAD),
        "simp_long": long_cues(SUB_CUES_SIMP, DIALOGUE_SIMP),
    }[variant]
    make = {"srt": srt_text, "ass": ass_text, "ssa": ssa_text, "ass_noheader": ass_text}[fmt]
    text = make(cues)
    if fmt == "ass_noheader":
        # 實際遇過的壞檔：少了開頭的 [Script Info]，FFmpeg 認不出格式
        text = text.replace("[Script Info]\n", "", 1)
    stem = str(video.with_suffix(""))
    Path(stem + suffix).write_text(text, encoding=encoding, newline="\r\n")


def find_font() -> Path | None:
    """MKV 附加字型測試用。找不到就不附加。"""
    for p in (
        "C:/Windows/Fonts/arial.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ):
        if Path(p).exists():
            return Path(p)
    return None


# ───────────── 這個程式自己寫的媒體檔（FFmpeg 沒有編碼器）─────────────
# DSD 64（2.8224 MHz）、立體聲，內容是 DSD 的靜音樣式 0x69

DSD_RATE = 2_822_400
DSD_CHANNELS = 2


def _dsd_silence(secs: int) -> bytes:
    return bytes([0x69]) * (DSD_RATE * secs // 8)


def dsf_bytes(secs: int = DUR) -> bytes:
    """DSF（Sony）：每個聲道 4096 位元組一塊、輪流放，最後一塊補零；位元順序是低位先（LSB first）"""
    block = 4096
    silence = _dsd_silence(secs)
    data = bytearray()
    for i in range(0, len(silence), block):
        chunk = silence[i:i + block].ljust(block, b"\0")
        data += chunk * DSD_CHANNELS
    # fmt：版本 1、DSD raw、聲道型態 2（立體聲）、聲道數、取樣率、1 位元、每聲道的取樣數、每塊大小、保留
    fmt = b"fmt " + struct.pack("<QIIIIIIQII", 52, 1, 0, 2, DSD_CHANNELS, DSD_RATE, 1, len(silence) * 8, block, 0)
    dat = b"data" + struct.pack("<Q", 12 + len(data)) + bytes(data)
    total = 28 + len(fmt) + len(dat)
    return b"DSD " + struct.pack("<QQQ", 28, total, 0) + fmt + dat


def dff_bytes(secs: int = DUR) -> bytes:
    """DSDIFF（Philips）：大端序的 chunk，聲道逐位元組交錯，位元順序是高位先（MSB first）"""
    def chunk(cid: bytes, body: bytes) -> bytes:
        return cid + struct.pack(">Q", len(body)) + body + (b"\0" if len(body) % 2 else b"")

    silence = _dsd_silence(secs)
    name = b"not compressed"
    cmpr = chunk(b"CMPR", b"DSD " + bytes([len(name)]) + name + (b"\0" if (len(name) + 1) % 2 else b""))
    prop = chunk(b"PROP", b"SND " + chunk(b"FS  ", struct.pack(">I", DSD_RATE))
                 + chunk(b"CHNL", struct.pack(">H", DSD_CHANNELS) + b"SLFTSRGT") + cmpr)
    inter = bytearray(len(silence) * DSD_CHANNELS)
    for c in range(DSD_CHANNELS):
        inter[c::DSD_CHANNELS] = silence
    body = b"DSD " + chunk(b"FVER", struct.pack(">I", 0x01050000)) + prop + chunk(b"DSD ", bytes(inter))
    return b"FRM8" + struct.pack(">Q", len(body)) + body


WRITERS = {"dsf": dsf_bytes, "dff": dff_bytes}


# ───────────── 產生 ─────────────

def build_cmd(s: Sample, out: Path, sub_src: dict[str, Path], font: Path | None) -> list[str]:
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
    if s.custom:
        # {srt}、{chapters} 之類的換成事先產生的來源檔
        src = {f"{{{k}}}": str(v) for k, v in sub_src.items()}
        return cmd + [src.get(a, a) for a in s.custom] + [str(out)]
    maps: list[str] = []
    idx = 0
    if s.v is not None:
        cmd += ["-f", "lavfi", "-i", f"testsrc2=size={s.size}:rate={s.rate}:duration={s.dur}"]
        maps += ["-map", f"{idx}:v"]
        idx += 1
    if s.a is not None:
        cmd += ["-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={s.dur}"]
        maps += ["-map", f"{idx}:a"]
        idx += 1
    if s.sub_embed:
        src = sub_src["ass" if s.sub_embed == "ass" else "srt"]
        cmd += ["-i", str(src)]
        maps += ["-map", f"{idx}:s"]
        idx += 1
    cmd += maps
    if s.sub_embed == "ass" and font is not None:
        cmd += ["-attach", str(font), "-metadata:s:t", "mimetype=application/x-truetype-font"]
    if s.vf:
        cmd += ["-vf", s.vf]
    cmd += s.v or []
    cmd += s.a or []
    if s.sub_embed:
        cmd += ["-c:s", s.sub_embed]
    cmd += s.fmt
    cmd.append(str(out))
    return cmd


def remove_old(s: Sample) -> None:
    """清掉這個樣本以前產生的檔案（包括外掛字幕）：舊的字幕檔會被當成多出來的軌道，
    舊的樣本檔會留在 manifest.json 裡。"""
    if s.path.parent.is_dir():
        for old in s.path.parent.glob(f"{s.id}.*"):
            old.unlink()


def generate(s: Sample, sub_src: dict[str, Path], font: Path | None, tmp: Path) -> str | None:
    """產生一個樣本；成功回傳 None，失敗回傳錯誤訊息。"""
    out = s.path
    out.parent.mkdir(parents=True, exist_ok=True)
    remove_old(s)
    if s.writer:
        out.write_bytes(WRITERS[s.writer]())
        return None
    target = tmp / f"{s.id}.{s.ext}" if s.post else out
    r = subprocess.run(build_cmd(s, target, sub_src, font), capture_output=True, text=True, encoding="utf-8",
                       errors="replace")
    if r.returncode != 0:
        return r.stderr.strip().splitlines()[-1] if r.stderr.strip() else f"ffmpeg exit {r.returncode}"
    if s.post == "rotate":
        # 手機直拍：ffmpeg 7+ 用 display_rotation（「逆時針」角度，-90 = 播放時順時針轉 90 度）；
        # 舊版（例如 Ubuntu 24.04 的 6.1）沒有這個選項，改用 rotate 中繼資料（順時針角度）
        base = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
        attempts = [
            base + ["-display_rotation:v:0", "-90", "-i", str(target), "-c", "copy", str(out)],
            base + ["-i", str(target), "-c", "copy", "-metadata:s:v:0", "rotate=90", str(out)],
        ]
        for cmd in attempts:
            r = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
            if r.returncode == 0:
                break
        else:
            return r.stderr.strip() or f"ffmpeg exit {r.returncode}"
    if s.ext_audio:
        mka = out.with_suffix(".mka")
        r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                            "-f", "lavfi", "-i", f"sine=frequency=660:sample_rate=48000:duration={s.dur}",
                            "-c:a", "flac", "-metadata:s:a:0", "language=eng", str(mka)],
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        if r.returncode != 0:
            return r.stderr.strip() or f"ffmpeg exit {r.returncode}"
    if s.sub_ext:
        write_ext_sub(s.sub_ext, out)
    for extra in s.extra_subs:
        write_extra_sub(out, *extra)
    return None


# ───────────── DASH（本機的 MPD + 片段）─────────────
# 播放引擎的 DASH 分離器測試用（tests/engine_build.rs）。不放進 manifest.json：
# 舊的播放引擎沒有 DASH 分離器，格式矩陣（tests/formats.rs）會把它當成失敗。
# 片段用 dashenc 預設的 .m4s 檔名：FFmpeg 的 DASH 分離器只開副檔名是 m4s、mp4… 的本機片段
DASH_DIR = OUT / "general" / "dash_h264_aac"


def has_dash_muxer() -> bool:
    r = subprocess.run(["ffmpeg", "-hide_banner", "-h", "muxer=dash"], capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    return r.returncode == 0 and "Muxer dash" in r.stdout


def generate_dash(force: bool) -> str | None:
    """產生 general/dash_h264_aac/manifest.mpd；成功或略過回傳 None，失敗回傳錯誤訊息。"""
    mpd = DASH_DIR / "manifest.mpd"
    if mpd.exists() and not force:
        print("  略過  general dash_h264_aac（已存在）")
        return None
    if not has_dash_muxer():
        print("  略過  general dash_h264_aac（這個 ffmpeg 沒有 dash 封裝格式）")
        return None
    # 舊的片段要清掉（片段數量可能不同）
    shutil.rmtree(DASH_DIR, ignore_errors=True)
    DASH_DIR.mkdir(parents=True)
    # 在輸出資料夾裡執行、只給檔名：Windows 的路徑是反斜線，dashenc 認不出資料夾，片段會寫到目前的資料夾
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                        "-f", "lavfi", "-i", f"testsrc2=size=320x240:rate=24:duration={DUR}",
                        "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={DUR}",
                        "-map", "0:v", "-map", "1:a", *X264, "-g", "24", *AAC,
                        "-f", "dash", "-seg_duration", "1", mpd.name],
                       cwd=DASH_DIR, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if r.returncode != 0 or not mpd.exists():
        shutil.rmtree(DASH_DIR, ignore_errors=True)
        return r.stderr.strip().splitlines()[-1] if r.stderr.strip() else f"ffmpeg exit {r.returncode}"
    print("  完成  general dash_h264_aac")
    return None


# ───────────── 網路（本機 HTTP 伺服器播放，tests/net.rs；不在 manifest.json）─────────────
# 跟格式矩陣無關（HLS、DASH 要透過 HTTP 才是網路串流的路徑），舊的播放引擎沒有 DASH 分離器。
# 片段都是 1 秒，播放器很快就讀到下一段；多畫質的串流兩個畫質大小、位元率都不同，測試看選到哪一個
NET_DIR = OUT / "net"
NET_SRC = ["-f", "lavfi", "-i", f"testsrc2=size=320x240:rate=24:duration={DUR}",
           "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={DUR}"]
# 兩個畫質：320×240（約 400 kb/s）與 160×120（約 100 kb/s）
NET_TWO_VIDEOS = ["-filter_complex", "[0:v]split=2[hi][lo0];[lo0]scale=160:120[lo]",
                  "-map", "[hi]", "-map", "[lo]", *X264, "-g", "24", "-b:v:0", "400k", "-b:v:1", "100k"]


def _ffmpeg(args: list[str], cwd: Path) -> str | None:
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args],
                       cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if r.returncode != 0:
        return r.stderr.strip().splitlines()[-1] if r.stderr.strip() else f"ffmpeg exit {r.returncode}"
    return None


def generate_net(force: bool) -> list[str]:
    """產生 net/ 底下的樣本；回傳失敗的說明（略過的不算失敗）。
    在輸出資料夾裡執行、只給檔名：Windows 的路徑是反斜線，hls / dash 封裝認不出資料夾"""
    jobs = [
        # 一般的 HLS（單一畫質，1 秒的片段）
        ("hls_vod", "index.m3u8", None,
         [*NET_SRC, "-map", "0:v", "-map", "1:a", *X264, "-g", "24", *AAC,
          "-f", "hls", "-hls_time", "1", "-hls_playlist_type", "vod",
          "-hls_segment_filename", "seg%d.ts", "index.m3u8"]),
        # 兩個畫質的 HLS（master playlist）
        ("hls_multi", "master.m3u8", None,
         [*NET_SRC, *NET_TWO_VIDEOS, "-map", "1:a", "-map", "1:a", *AAC,
          "-f", "hls", "-hls_time", "1", "-hls_playlist_type", "vod", "-master_pl_name", "master.m3u8",
          "-var_stream_map", "v:0,a:0 v:1,a:1", "-hls_segment_filename", "v%v_seg%d.ts", "v%v.m3u8"]),
        # 兩個畫質 + 一條聲音的 DASH
        ("dash_multi", "manifest.mpd", "dash",
         [*NET_SRC, *NET_TWO_VIDEOS, "-map", "1:a", *AAC,
          "-f", "dash", "-seg_duration", "1", "-adaptation_sets", "id=0,streams=v id=1,streams=a",
          "manifest.mpd"]),
    ]
    errors = []
    for name, main, muxer, args in jobs:
        out = NET_DIR / name
        if (out / main).exists() and not force:
            print(f"  略過  net     {name}（已存在）")
            continue
        if muxer == "dash" and not has_dash_muxer():
            print(f"  略過  net     {name}（這個 ffmpeg 沒有 dash 封裝格式）")
            continue
        # 舊的片段要清掉（片段數量可能不同）
        shutil.rmtree(out, ignore_errors=True)
        out.mkdir(parents=True)
        err = _ffmpeg(args, out)
        if err or not (out / main).exists():
            shutil.rmtree(out, ignore_errors=True)
            errors.append(f"{name}: {err or '沒有產生 ' + main}")
            print(f"  失敗  net     {name}: {err}")
        else:
            print(f"  完成  net     {name}")
    # 網站影片（之後的 yt-dlp 測試）：分開的影像、聲音與字幕
    NET_DIR.mkdir(parents=True, exist_ok=True)
    for name, args in [
        ("video_only.mp4", [*NET_SRC, "-map", "0:v", *X264, "-g", "24", "-movflags", "+faststart", "video_only.mp4"]),
        ("audio_only.m4a", [*NET_SRC, "-map", "1:a", *AAC, "-movflags", "+faststart", "audio_only.m4a"]),
    ]:
        if (NET_DIR / name).exists() and not force:
            print(f"  略過  net     {name}（已存在）")
            continue
        err = _ffmpeg(args, NET_DIR)
        if err:
            errors.append(f"{name}: {err}")
            print(f"  失敗  net     {name}: {err}")
        else:
            print(f"  完成  net     {name}")
    (NET_DIR / "sub.vtt").write_text(vtt_text(), encoding="utf-8")
    return errors


# ───────────── 播放引擎解不了的編碼（tests/decoders.rs；不在 manifest.json）─────────────
# 影戲要說明是哪個編碼解不了、其他的照常播。這些是 tests/decoders.rs 檢查提示與重開的樣本，不放進 manifest：
# 格式矩陣只看得出「解不了的那條軌道之外照常播」（罕見的 mkv_avs2_aac 用 missing 標出），整個檔案都解不了的、
# mpv 先選到解不了的字幕之類的情況要另外測。
# - mp4_h264_aac_ttml：MP4 裡的 TTML 字幕（stpp，FFmpeg 只有編碼器、沒有解碼器，每個平台都解不了；FFmpeg 內建的編碼器，
#   每個平台都產生得了）＋ 一條 mov_text。TTML 那條標成預設：mpv 會先選它（解不了），影戲改選 mov_text
# - mkv_avs2_aac / mkv_avs2：AVS2 影像（libdavs2 是 GPL，播放引擎沒有，系統的 FFmpeg 通常也沒有）；要有 libxavs2 的 FFmpeg
#   （Windows 的 full build）才產生得了，沒有的平台測試會略過
UNSUPPORTED_DIR = OUT / "unsupported"


def generate_unsupported(force: bool) -> None:
    """產生 unsupported/ 底下的樣本；這個 FFmpeg 沒有的編碼器略過（不算失敗）"""
    UNSUPPORTED_DIR.mkdir(parents=True, exist_ok=True)
    video = ["-f", "lavfi", "-i", f"testsrc2=size=320x240:rate=24:duration={DUR}"]
    audio = ["-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={DUR}"]
    with tempfile.TemporaryDirectory() as td:
        srt = Path(td) / "src.srt"
        srt.write_text(srt_text(), encoding="utf-8")
        jobs = [
            ("mp4_h264_aac_ttml.mp4", None,
             [*video, *audio, "-i", str(srt), "-i", str(srt),
              "-map", "0:v", "-map", "1:a", "-map", "2:s", "-map", "3:s", *X264, *AAC,
              "-c:s:0", "ttml", "-c:s:1", "mov_text",
              "-metadata:s:s:0", "language=eng", "-metadata:s:s:1", "language=eng",
              "-disposition:s:0", "default", "-disposition:s:1", "0"]),
            ("mkv_avs2_aac.mkv", "libxavs2",
             [*video, *audio, "-map", "0:v", "-map", "1:a", "-c:v", "libxavs2", "-pix_fmt", "yuv420p", *AAC]),
            ("mkv_avs2.mkv", "libxavs2", [*video, "-c:v", "libxavs2", "-pix_fmt", "yuv420p"]),
        ]
        for name, encoder, args in jobs:
            out = UNSUPPORTED_DIR / name
            if out.exists() and not force:
                print(f"  略過  unsupported {name}（已存在）")
                continue
            if encoder and not has_encoder(encoder):
                print(f"  略過  unsupported {name}（這個 ffmpeg 沒有 {encoder}）")
                continue
            err = _ffmpeg([*args, str(out)], UNSUPPORTED_DIR)
            if err:
                out.unlink(missing_ok=True)
                print(f"  失敗  unsupported {name}: {err}")
            else:
                print(f"  完成  unsupported {name}")


@functools.cache
def has_encoder(name: str) -> bool:
    r = subprocess.run(["ffmpeg", "-hide_banner", "-h", f"encoder={name}"], capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    return r.returncode == 0 and f"Encoder {name}" in r.stdout


# ───────────── 流暢播放（實機測試用，不在 manifest.json）─────────────
# 1920×1080、23.976 fps、20 秒：每格往左平移 16 像素，左上角是影格編號。
# 卡頓（某一格多停一次更新）在平移的畫面上最明顯；tests/pacing_window.rs 用 mpv 的記錄算每格顯示幾次更新
PACING = OUT / "pacing" / "pan_23976.mkv"
# 同上，3840×2160 10-bit H.264、16 秒：顯示卡不能硬體解碼（軟體解碼，render 要上傳大貼圖），
# GPU 畫一格比較久；比較畫面輸出挑時間取影格時 GPU 來不來得及（tests/pacing_window.rs）
PACING_4K = OUT / "pacing" / "pan_4k10.mkv"


def generate_pacing(force: bool) -> str | None:
    err = generate_pan(PACING, 1920, 1080, 20, ["-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"], force)
    if err:
        return err
    return generate_pan(PACING_4K, 3840, 2160, 16, ["-preset", "ultrafast", "-crf", "23", "-pix_fmt", "yuv420p10le"],
                        force)


def generate_pan(out: Path, w: int, h: int, secs: int, enc: list[str], force: bool) -> str | None:
    if out.exists() and not force:
        print(f"  略過  pacing  {out.stem}（已存在）")
        return None
    out.parent.mkdir(parents=True, exist_ok=True)
    pan = rf"crop={w}:{h}:x='mod(n*{w // 120}\,{w})':y=0"
    font = find_font()
    cwd = None
    if font is not None:
        # 在字型的資料夾裡執行、只給檔名：Windows 路徑的「C:」在濾鏡參數裡要跳脫
        cwd = font.parent
        pan += (f",drawtext=fontfile={font.name}:text='%{{frame_num}}':fontsize={h // 9}:fontcolor=white"
                ":box=1:boxcolor=black@0.7:boxborderw=16:x=60:y=60")
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                        "-f", "lavfi", "-i", f"testsrc2=size={2 * w}x{h}:rate=24000/1001:duration={secs}",
                        "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={secs}",
                        "-map", "0:v", "-map", "1:a", "-vf", pan,
                        "-c:v", "libx264", *enc, "-g", "48",
                        *AAC, str(out)],
                       cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if r.returncode != 0:
        return r.stderr.strip().splitlines()[-1] if r.stderr.strip() else f"ffmpeg exit {r.returncode}"
    print(f"  完成  pacing  {out.stem}")
    return None


def manifest_entry(s: Sample) -> dict:
    e = {
        "id": s.id,
        "tier": s.tier,
        "file": s.path.relative_to(OUT).as_posix(),
        "video": s.video,
        "audio": s.audio,
        "subs": s.subs,
        # null：檔頭沒有正確的長度，格式矩陣不檢查長度、直接播到結尾
        "duration": s.dur if s.known_dur else None,
        "note": s.note,
    }
    expect = dict(s.expect)
    # sub_bitmap：圖形字幕，播放中檢查有沒有出現（沒有文字可以比對）；missing：預期播放引擎解不了的編碼
    if s.subs and not expect.get("sub_bitmap"):
        # 預設預期繁體字幕；只有簡體字幕的樣本用 expect={"sub_text": ...} 指定
        e["sub_probe"] = {"time": SUB_PROBE_TIME, "text": expect.pop("sub_text", SUB_PROBE_TEXT)}
    e.update(expect)
    return e


def main() -> int:
    # Windows 主控台（例如 CI）預設編碼不是 UTF-8，印中文會出錯
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")

    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tier", choices=(*TIERS, "all", "pacing", "net", "unsupported"), default="all")
    ap.add_argument("--force", action="store_true", help="已存在的樣本也重新產生")
    args = ap.parse_args()

    if shutil.which("ffmpeg") is None:
        print("找不到 ffmpeg，請先安裝並加入 PATH", file=sys.stderr)
        return 1
    if args.tier == "pacing":
        err = generate_pacing(args.force)
        if err:
            print(f"  失敗  pacing: {err}")
            return 2
        return 0
    if args.tier == "net":
        return 2 if generate_net(args.force) else 0
    if args.tier == "unsupported":
        generate_unsupported(args.force)
        return 0

    all_samples = samples()
    todo = [s for s in all_samples if args.tier in ("all", s.tier)]
    font = find_font()
    failed: list[tuple[Sample, str]] = []

    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        sub_src = {"srt": tmp / "src.srt", "ass": tmp / "src.ass", "chapters": tmp / "chapters.txt",
                   "scc": tmp / "src.scc", "sup": tmp / "src.sup"}
        sub_src["srt"].write_text(srt_text(), encoding="utf-8")
        sub_src["ass"].write_text(ass_text(), encoding="utf-8")
        sub_src["chapters"].write_text(chapters_text(), encoding="utf-8")
        sub_src["scc"].write_text(scc_text(), encoding="ascii")
        sub_src["sup"].write_bytes(pgs_bytes())

        for s in todo:
            if s.path.exists() and not args.force:
                print(f"  略過  {s.tier:<7} {s.id}（已存在）")
                continue
            if s.needs and not has_encoder(s.needs):
                # 各平台套件版的 FFmpeg 不一定有外部函式庫的編碼器：略過（測試就沒有這個樣本），不算失敗。
                # --force 時刪掉別的 FFmpeg 以前產生的舊檔，manifest 才會跟這個 FFmpeg 做得出來的一致
                remove_old(s)
                print(f"  略過  {s.tier:<7} {s.id}（這個 ffmpeg 沒有 {s.needs}）")
                continue
            err = generate(s, sub_src, font, tmp)
            if err:
                failed.append((s, err))
                print(f"  失敗  {s.tier:<7} {s.id}: {err}")
            else:
                print(f"  完成  {s.tier:<7} {s.id}")

    dash_error = generate_dash(args.force) if args.tier in ("all", "general") else None
    if dash_error:
        print(f"  失敗  general dash_h264_aac: {dash_error}")
        if os.environ.get("GITHUB_ACTIONS"):
            print(f"::warning::這個平台的 FFmpeg 產生不了 DASH 樣本：{dash_error}")
    # 網路測試用的串流：產生不了的測試會略過（看得到警告），不擋其他樣本
    net_errors = generate_net(args.force) if args.tier == "all" else []
    # 播放引擎解不了的編碼：產生不了的（沒有那個編碼器）測試會略過
    if args.tier == "all":
        generate_unsupported(args.force)
    if net_errors and os.environ.get("GITHUB_ACTIONS"):
        print(f"::warning::這個平台的 FFmpeg 產生不了網路測試的樣本：{'; '.join(net_errors)}")

    # manifest 只列出實際存在的樣本，測試程式不必再處理「產生失敗」的情況
    entries = [manifest_entry(s) for s in all_samples if s.path.exists()]
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "manifest.json").write_text(json.dumps(entries, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"\nmanifest：{len(entries)} 個樣本 → {OUT / 'manifest.json'}")
    if failed:
        print(f"{len(failed)} 個樣本產生失敗：", ", ".join(s.id for s, _ in failed))
        general = [s.id for s, _ in failed if s.tier == "general"]
        if general and os.environ.get("GITHUB_ACTIONS"):
            # 各平台套件版的 FFmpeg 編碼器不同，通用樣本缺幾個不擋 CI，但要看得到
            print(f"::warning::這個平台的 FFmpeg 產生不了通用樣本：{', '.join(general)}")
        # 常見樣本一定要齊全，否則「常見格式全數通過」就沒有意義
        if any(s.tier == "common" for s, _ in failed):
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
