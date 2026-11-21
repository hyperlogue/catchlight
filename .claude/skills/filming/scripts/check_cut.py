#!/usr/bin/env python3
"""Check a finished cut before publishing it. Standard library and ffmpeg.

    check_cut.py <cut.mp4> [--frames N] [--seconds S] [--audio] [--against previous.mp4]

Prints the video's frame count, duration and streams, and exits 1 when they
miss the expectations given. --against reports per-frame PSNR against a
previous cut of the same length: a recomposite of unchanged scenes sits above
40 dB, and a far lower worst frame names the place to look.
"""

from __future__ import annotations

import argparse
from fractions import Fraction
import os
import re
import subprocess
import sys
from pathlib import Path

FFMPEG = os.environ.get("CATCHLIGHT_FFMPEG", "ffmpeg")
FFPROBE = str(Path(FFMPEG).with_name("ffprobe")) if os.sep in FFMPEG else "ffprobe"


def probe(path: str, *entries: str) -> str:
    return subprocess.run([FFPROBE, "-v", "error", *entries, "-of", "default=nw=1", path],
                          check=True, capture_output=True, text=True).stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cut")
    parser.add_argument("--frames", type=int)
    parser.add_argument("--seconds", type=float)
    parser.add_argument("--audio", action="store_true", help="require an audio stream")
    parser.add_argument("--against")
    args = parser.parse_args()
    video = dict(line.split("=", 1) for line in probe(
        args.cut, "-count_packets", "-select_streams", "v:0", "-show_entries",
        "stream=codec_name,width,height,r_frame_rate,nb_read_packets:format=duration").splitlines())
    audio = probe(args.cut, "-select_streams", "a:0", "-show_entries", "stream=codec_name").strip()
    frames, seconds = int(video["nb_read_packets"]), float(video["duration"])
    print(f"{video['codec_name']} {video['width']}x{video['height']} at {video['r_frame_rate']} fps, "
          f"{frames} frames, {seconds:.3f} s, audio: {audio.removeprefix('codec_name=') or 'none'}")
    problems = []
    if args.frames is not None and frames != args.frames:
        problems.append(f"expected {args.frames} frames")
    if args.seconds is not None and abs(seconds - args.seconds) > 0.05:
        problems.append(f"expected {args.seconds} s")
    if args.audio and not audio:
        problems.append("no audio stream")
    if args.against:
        stats = subprocess.run(
            [FFMPEG, "-v", "error", "-i", args.cut, "-i", args.against, "-lavfi", "[0:v][1:v]psnr=stats_file=-",
             "-f", "null", "-"], check=True, capture_output=True, text=True).stdout
        values = [100.0 if v == "inf" else float(v) for v in re.findall(r"psnr_avg:(\S+)", stats)]
        if len(values) != frames:
            problems.append(f"{args.against} has {len(values)} comparable frames")
        if values:
            worst = min(range(len(values)), key=values.__getitem__)
            rate = float(Fraction(video["r_frame_rate"]))
            print(f"against {args.against}: mean {sum(values) / len(values):.1f} dB, "
                  f"worst {values[worst]:.1f} dB at frame {worst} ({worst / rate:.2f} s)")
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
