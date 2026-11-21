"""Read rendered passes back for compositing. Needs numpy and ffmpeg.

A pass is one lossless FFV1 video (<pass>.mkv). Each is read through one
sequential ffmpeg decoder, restarted with a seek on a backward or far-forward
jump and stopped once idle, so a process holds decoders only for the passes
of the scene it is compositing. ffmpeg is $CATCHLIGHT_FFMPEG, else on PATH.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import numpy as np

FFMPEG = os.environ.get("CATCHLIGHT_FFMPEG", "ffmpeg")
FFPROBE = str(Path(FFMPEG).with_name("ffprobe")) if os.sep in FFMPEG else "ffprobe"
SKIP = 12     # frames worth decoding past rather than seeking; FFV1's default keyframe interval
IDLE = 64     # reads by this process a decoder may sit unused before it is stopped


class Pass:
    def __init__(self, path: Path | str, rate: float):
        """rate: video frames per second, the film's fps / the shot's `every`."""
        self.path, self.rate = str(path), rate
        if not Path(self.path).exists():
            raise FileNotFoundError(self.path)
        probe = subprocess.run(
            [FFPROBE, "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height",
             "-of", "csv=p=0", self.path], check=True, capture_output=True, text=True)
        self.width, self.height = (int(x) for x in probe.stdout.strip().split(",")[:2])
        self._proc, self._next = None, 0

    def read(self, index: int) -> np.ndarray:
        """Video frame `index` as straight-alpha RGBA uint8, read-only."""
        if self._proc is None or index < self._next or index > self._next + SKIP:
            self.close()
            # Half a frame early: the first frame out is `index` whatever the container rounds timestamps to.
            self._proc = subprocess.Popen(
                [FFMPEG, "-v", "error", "-threads", "2", "-ss", f"{max(index - 0.5, 0) / self.rate:.6f}",
                 "-i", self.path, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            self._next = index
        size = self.width * self.height * 4
        while True:
            data = self._proc.stdout.read(size)
            if len(data) != size:
                self.close()
                raise EOFError(f"{self.path}: no frame {index}")
            self._next += 1
            if self._next > index:
                return np.frombuffer(data, np.uint8).reshape(self.height, self.width, 4)

    def close(self) -> None:
        if self._proc is not None:
            self._proc.kill()
            self._proc.wait()
            self._proc = None


class Shots:
    """Rendered shots addressed by film time, from the shots.json `shots.write` produced."""

    def __init__(self, specs: Path | str, out: Path | str):
        self.meta = json.loads((Path(specs) / "shots.json").read_text())
        self.out = Path(out)
        self._passes: dict[tuple[str, str], Pass] = {}
        self._used: dict[tuple[str, str], int] = {}
        self._tick = 0

    def directory(self, shot: str, pas: str) -> Path:
        """Where the pass's video and sidecars (geometry, trace) were written."""
        return self.out / self.meta[shot]["passes"][pas]

    def index(self, shot: str, t: float) -> int:
        """The video frame showing film time t, clamped to the shot."""
        m = self.meta[shot]
        return min(max(int(round(t * m["fps"])) - m["start"], 0), m["frames"] - 1) // m["every"]

    def frame(self, shot: str, pas: str, t: float) -> np.ndarray:
        """The pass at film time t as straight-alpha RGBA uint8; cache it if you revisit frames."""
        key, m = (shot, pas), self.meta[shot]
        self._tick += 1
        if key not in self._passes:
            self._passes[key] = Pass(self.directory(shot, pas) / f"{pas}.mkv", m["fps"] / m["every"])
        self._used[key] = self._tick
        for other, used in self._used.items():
            if self._tick - used > IDLE:
                self._passes[other].close()
        return self._passes[key].read(self.index(shot, t))

    def close(self) -> None:
        for pas in self._passes.values():
            pas.close()


def premultiply(rgba: np.ndarray) -> np.ndarray:
    """Straight RGBA uint8 -> premultiplied RGBA float32 in 0..1, the form `over` blends."""
    out = rgba.astype(np.float32) / 255
    out[..., :3] *= out[..., 3:]
    return out


def from_white(rgba: np.ndarray) -> np.ndarray:
    """A pass rendered over opaque white -> the premultiplied layer that composites back to it over white."""
    colour = rgba[..., :3].astype(np.float32) / 255
    alpha = (1 - colour).max(-1, keepdims=True)
    straight = np.clip(1 - (1 - colour) / np.maximum(alpha, 1e-4), 0, 1)
    return np.concatenate([straight * alpha, alpha], -1)


def over(canvas: np.ndarray, layer: np.ndarray, x: int = 0, y: int = 0, opacity: float = 1.0) -> None:
    """Blend a premultiplied RGBA layer onto an RGB float canvas in place, top-left at (x, y), clipped."""
    h, w = canvas.shape[:2]
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + layer.shape[1], w), min(y + layer.shape[0], h)
    if x0 >= x1 or y0 >= y1:
        return
    part = layer[y0 - y:y1 - y, x0 - x:x1 - x] * opacity
    region = canvas[y0:y1, x0:x1]
    region *= 1 - part[..., 3:]
    region += part[..., :3]
