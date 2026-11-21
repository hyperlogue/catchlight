#!/usr/bin/env python3
"""The smallest whole film: one shot of a model swinging one parameter.

    example.py <model.clm> --param <id> [--amplitude 1] [--work film-example] [--quality preview|final]

Writes <work>/shots, <work>/out and <work>/film-<quality>.mp4. Copy it as the
skeleton of a real film: timeline -> shots -> render -> compose -> check.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

import numpy as np

import compose
import passes
import render
import shots

FPS, SECONDS, SIZE = 60, 3.0, (1280, 720)
_shots: passes.Shots | None = None
_size = SIZE


def setup(work: str, quality: str, size: tuple[int, int]) -> None:
    global _shots, _size
    _shots = passes.Shots(Path(work) / "shots" / quality, Path(work) / "out" / quality)
    _size = size


def frame(index: int) -> np.ndarray:
    width, height = _size
    canvas = np.full((height, width, 3), (0.93, 0.94, 0.97), np.float32)
    layer = passes.premultiply(_shots.frame("main", "base", index / FPS))
    passes.over(canvas, layer, (width - layer.shape[1]) // 2, (height - layer.shape[0]) // 2)
    return canvas


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("model", type=Path)
    parser.add_argument("--param", required=True)
    parser.add_argument("--amplitude", type=float, default=1.0)
    parser.add_argument("--work", type=Path, default=Path("film-example"))
    parser.add_argument("--quality", choices=list(shots.QUALITY), default="preview")
    args = parser.parse_args()

    # Timeline: one value per film frame for every animated param.
    count = int(SECONDS * FPS)
    tracks = {args.param: [args.amplitude * math.sin(2 * math.pi * i / count) for i in range(count)]}

    # Framing: the model's whole bounds, scaled to the film's height at final quality.
    bounds = json.loads(subprocess.run([*render.cli(), "bounds", str(args.model)], check=True,
                                       capture_output=True, text=True).stdout)["overall"]
    (x0, y0), (x1, y1) = bounds["min"], bounds["max"]
    rect = [x0, y0, x1 - x0, y1 - y0]
    specs = args.work / "shots" / args.quality
    shots.write(specs, {"main": shots.shot("main", tracks=tracks, fps=FPS, t0=0.0, t1=SECONDS, rect=rect,
                                           scale=SIZE[1] / rect[3], quality=args.quality)})

    out = args.work / "out" / args.quality
    out.mkdir(parents=True, exist_ok=True)
    name, ok, note = render.render(args.model, specs / "main.spec.json", out, packed=False)
    print("ok  " if ok else "FAIL", name, note)
    if not ok:
        return 1

    mult = shots.QUALITY[args.quality][1]               # preview passes are half scale; so is the film
    size = (int(SIZE[0] * mult), int(SIZE[1] * mult))
    film = args.work / f"film-{args.quality}.mp4"
    compose.encode(frame, film, size=size, fps=FPS, frames=range(count), init=setup,
                   initargs=(str(args.work), args.quality, size), jobs=2, crf=20, preset="veryfast")
    here = Path(__file__).resolve().parent
    return subprocess.run([sys.executable, str(here / "check_cut.py"), str(film), "--frames", str(count)]).returncode


if __name__ == "__main__":
    sys.exit(main())
