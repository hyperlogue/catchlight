"""Encode a film from a per-frame compositing function. Needs numpy and ffmpeg.

    compose.encode(frame, out, size=(1920, 1080), fps=60, frames=range(n), init=setup, jobs=2)

`frame(i)` returns film frame i as RGB, float 0..1 or uint8, shaped (height,
width, 3). It runs in worker processes, so `frame` and `init` are module-level
functions; build per-process state (a `passes.Shots`, fonts, caches) in `init`.
ffmpeg is $CATCHLIGHT_FFMPEG, else on PATH.
"""

from __future__ import annotations

import os
import subprocess
from multiprocessing import Pool
from pathlib import Path
from typing import Callable, Iterable

import numpy as np

FFMPEG = os.environ.get("CATCHLIGHT_FFMPEG", "ffmpeg")
_frame: Callable[[int], np.ndarray] | None = None


def _setup(frame, init, initargs) -> None:
    global _frame
    _frame = frame
    if init:
        init(*initargs)


def _bytes(index: int) -> bytes:
    image = _frame(index)
    if image.dtype != np.uint8:
        image = (np.clip(image, 0, 1) * 255 + 0.5).astype(np.uint8)
    return image.tobytes()


def encode(frame: Callable[[int], np.ndarray], out: Path | str, *, size: tuple[int, int], fps: float,
           frames: Iterable[int], init: Callable | None = None, initargs: tuple = (), jobs: int = 2,
           crf: int = 15, preset: str = "slow") -> None:
    """Composite `frames` in `jobs` processes and encode them, in order, as H.264 without audio.

    Each process holds its own decoders and caches, so memory grows with jobs:
    measure one process on the heaviest scene before raising it. A master at
    crf 15 is for muxing from; `mux` makes the delivery file.
    """
    frames = list(frames)
    width, height = size
    encoder = subprocess.Popen(
        [FFMPEG, "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{width}x{height}",
         "-r", str(fps), "-i", "-", "-c:v", "libx264", "-preset", preset, "-crf", str(crf),
         "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(out)], stdin=subprocess.PIPE)
    try:
        with Pool(jobs, _setup, (frame, init, initargs)) as pool:
            for done, data in enumerate(pool.imap(_bytes, frames, chunksize=4)):
                if len(data) != width * height * 3:
                    raise ValueError(f"frame {frames[done]} is not {width}x{height} RGB")
                encoder.stdin.write(data)
                if done % 300 == 0:
                    print(f"{done}/{len(frames)}", flush=True)
    finally:
        encoder.stdin.close()
        code = encoder.wait()
    if code:
        raise RuntimeError(f"ffmpeg exited {code} encoding {out}")


def mux(video: Path | str, audio: Path | str, out: Path | str, *, crf: int = 20, preset: str = "slow",
        audio_bitrate: str = "192k") -> None:
    """The delivery file: the master re-encoded at `crf` with the audio mix as AAC."""
    subprocess.run(
        [FFMPEG, "-y", "-v", "error", "-i", str(video), "-i", str(audio), "-map", "0:v", "-map", "1:a",
         "-c:v", "libx264", "-preset", preset, "-crf", str(crf), "-pix_fmt", "yuv420p",
         "-c:a", "aac", "-b:a", audio_bitrate, "-movflags", "+faststart", str(out)], check=True)
