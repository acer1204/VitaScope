#!/usr/bin/env python3
"""產生格式測試樣本。

輸出：
  samples/generated/<tier>/<id>.<ext>   外掛字幕放在影片旁邊、同檔名
  samples/generated/manifest.json       測試程式（tests/formats.rs）讀這份清單比對預期結果

用法：
  python scripts/gen_samples.py                 # 產生全部等級
  python scripts/gen_samples.py --tier common   # 只產生「常見」
  python scripts/gen_samples.py --force         # 已存在也重新產生

需要 FFmpeg 7.1 以上的 full build（VVC 樣本需要 libvvenc）。
FFmpeg 無法編碼的格式（VC-1、RV40、PGS、Dolby Vision…）請把公開樣本放到 samples/external/。
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
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
SUB_CUES = [
    (0.2, 1.4, "第一句字幕 First line"),
    (1.5, 2.6, "影戲播放器測試"),
]
SUB_PROBE_TIME = 2.0
SUB_PROBE_TEXT = "影戲播放器測試"

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
    sub_ext: str | None = None  # 外掛字幕：srt_utf8 / srt_big5 / srt_utf16 / ass / vtt / smi / microdvd
    subs: list[str] = field(default_factory=list)  # 預期的字幕編碼（內嵌 + 外掛）
    expect: dict = field(default_factory=dict)  # 其他預期：rotate、gamma、primaries、aspect
    post: str | None = None  # 後處理：rotate（加上手機直拍的旋轉中繼資料）
    note: str = ""
    dur: int = DUR
    # 完整的 ffmpeg 參數（輸入、對應、編碼），給多軌之類的特殊樣本用；{srt} 會換成字幕來源檔
    custom: list[str] | None = None

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
        S("mp4_h264_120fps", "general", "mp4", X264, AAC, "h264", "aac", rate="120", note="高幀率"),
        S("audio_mp3", "general", "mp3", None, MP3, None, "mp3"),
        S("audio_aac", "general", "m4a", None, AAC, None, "aac"),
        S("audio_flac", "general", "flac", None, FLAC, None, "flac"),
        S("audio_opus", "general", "opus", None, OPUS, None, "opus"),
        S("audio_vorbis", "general", "ogg", None, ["-c:a", "libvorbis", "-ac", "2"], None, "vorbis"),
        S("audio_wav", "general", "wav", None, PCM, None, "pcm_s16le"),

        # ───────────── 罕見 ─────────────
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
          "vvc", "aac", note="H.266 / VVC"),
        S("mkv_dirac_flac", "rare", "mkv", ["-c:v", "vc2", "-b:v", "5M"], FLAC, "dirac", "flac"),
        S("mkv_hevc444_12bit", "rare", "mkv", ["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv444p12le",
                                               "-x265-params", "log-level=error"], AAC, "hevc", "aac", note="12-bit 4:4:4"),
        S("mkv_h264_truehd", "rare", "mkv", X264, ["-c:a", "truehd", "-strict", "-2", "-ac", "2"], "h264", "truehd"),
        S("audio_wavpack", "rare", "wv", None, ["-c:a", "wavpack", "-ac", "2"], None, "wavpack"),
        S("audio_tta", "rare", "tta", None, ["-c:a", "tta", "-ac", "2"], None, "tta"),
        S("audio_amr", "rare", "amr", None, ["-c:a", "libopencore_amrnb", "-ar", "8000", "-ac", "1", "-b:a", "12.2k"],
          None, "amr_nb"),
        S("audio_speex", "rare", "spx", None, ["-c:a", "libspeex", "-ar", "16000", "-ac", "1"], None, "speex",
          fmt=["-f", "ogg"]),
        S("extsub_microdvd", "rare", "mp4", X264, AAC, "h264", "aac", sub_ext="microdvd", subs=["microdvd"],
          note="影格制字幕"),
    ]


# ───────────── 字幕檔 ─────────────

def _ts(t: float, sep: str = ",") -> str:
    h, rem = divmod(int(round(t * 1000)), 3_600_000)
    m, rem = divmod(rem, 60_000)
    s, ms = divmod(rem, 1000)
    return f"{h:02}:{m:02}:{s:02}{sep}{ms:03}"


def srt_text() -> str:
    return "".join(f"{i}\n{_ts(a)} --> {_ts(b)}\n{t}\n\n" for i, (a, b, t) in enumerate(SUB_CUES, 1))


def vtt_text() -> str:
    return "WEBVTT\n\n" + "".join(f"{_ts(a, '.')} --> {_ts(b, '.')}\n{t}\n\n" for a, b, t in SUB_CUES)


def ass_text() -> str:
    def ass_ts(t: float) -> str:
        cs = int(round(t * 100))
        return f"{cs // 360000}:{cs // 6000 % 60:02}:{cs // 100 % 60:02}.{cs % 100:02}"

    events = "".join(
        f"Dialogue: 0,{ass_ts(a)},{ass_ts(b)},Default,,0,0,0,,{{\\fad(100,100)}}{t}\n" for a, b, t in SUB_CUES
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


def write_ext_sub(kind: str, video: Path) -> None:
    stem = video.with_suffix("")
    table = {
        "srt_utf8": (".srt", srt_text, "utf-8"),
        "srt_big5": (".srt", srt_text, "cp950"),
        "srt_utf16": (".srt", srt_text, "utf-16"),  # Python 的 utf-16 會加 BOM
        "ass": (".ass", ass_text, "utf-8-sig"),
        "vtt": (".vtt", vtt_text, "utf-8"),
        "smi": (".smi", smi_text, "utf-8"),
        "microdvd": (".sub", microdvd_text, "utf-8"),
    }
    ext, make, enc = table[kind]
    Path(f"{stem}{ext}").write_text(make(), encoding=enc, newline="\r\n" if ext in (".srt", ".smi") else "\n")


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


# ───────────── 產生 ─────────────

def build_cmd(s: Sample, out: Path, sub_src: dict[str, Path], font: Path | None) -> list[str]:
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
    if s.custom:
        return cmd + [str(sub_src["srt"]) if a == "{srt}" else a for a in s.custom] + [str(out)]
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


def generate(s: Sample, sub_src: dict[str, Path], font: Path | None, tmp: Path) -> str | None:
    """產生一個樣本；成功回傳 None，失敗回傳錯誤訊息。"""
    out = s.path
    out.parent.mkdir(parents=True, exist_ok=True)
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
    if s.sub_ext:
        write_ext_sub(s.sub_ext, out)
    return None


def manifest_entry(s: Sample) -> dict:
    e = {
        "id": s.id,
        "tier": s.tier,
        "file": s.path.relative_to(OUT).as_posix(),
        "video": s.video,
        "audio": s.audio,
        "subs": s.subs,
        "duration": s.dur,
        "note": s.note,
    }
    if s.subs:
        e["sub_probe"] = {"time": SUB_PROBE_TIME, "text": SUB_PROBE_TEXT}
    e.update(s.expect)
    return e


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tier", choices=(*TIERS, "all"), default="all")
    ap.add_argument("--force", action="store_true", help="已存在的樣本也重新產生")
    args = ap.parse_args()

    if shutil.which("ffmpeg") is None:
        print("找不到 ffmpeg，請先安裝並加入 PATH", file=sys.stderr)
        return 1

    all_samples = samples()
    todo = [s for s in all_samples if args.tier in ("all", s.tier)]
    font = find_font()
    failed: list[tuple[Sample, str]] = []

    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        sub_src = {"srt": tmp / "src.srt", "ass": tmp / "src.ass"}
        sub_src["srt"].write_text(srt_text(), encoding="utf-8")
        sub_src["ass"].write_text(ass_text(), encoding="utf-8")

        for s in todo:
            if s.path.exists() and not args.force:
                print(f"  略過  {s.tier:<7} {s.id}（已存在）")
                continue
            err = generate(s, sub_src, font, tmp)
            if err:
                failed.append((s, err))
                print(f"  失敗  {s.tier:<7} {s.id}: {err}")
            else:
                print(f"  完成  {s.tier:<7} {s.id}")

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
