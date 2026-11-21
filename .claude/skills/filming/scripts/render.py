#!/usr/bin/env python3
"""Render shot specs with catchlight-cli, several runs at a time.

    render.py <model.clm> <spec-dir> <out-dir> [name-prefix ...] [--jobs N] [--pack]

Each <spec-dir>/<name>.spec.json renders into <out-dir>/<name>/, replacing
what was there, with the CLI's output in <out-dir>/<name>.log. --pack folds
the per-frame geometry and trace sidecars into one .npz per request (numpy).

The CLI is $CATCHLIGHT_CLI, else this checkout's release build, else
`cargo run --release`. It encodes video with $CATCHLIGHT_FFMPEG, else ffmpeg
on PATH.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]


def cli() -> list[str]:
    if os.environ.get("CATCHLIGHT_CLI"):
        return [os.environ["CATCHLIGHT_CLI"]]
    built = REPO / "target/release/catchlight-cli"
    if built.exists():
        return [str(built)]
    return ["cargo", "run", "--quiet", "--release", "--manifest-path", str(REPO / "Cargo.toml"),
            "-p", "catchlight-cli", "--"]


def pack(directory: Path) -> None:
    """Per-frame geometry JSON -> <request>--geometry.npz; trace JSONL -> <request>--trace.npz."""
    import numpy as np

    geometry: dict[str, dict] = {}
    for path in sorted(directory.glob("*--geometry.json")):
        request, index = re.fullmatch(r"(.+?)(?:--(\d+))?--geometry", path.stem).groups()
        data = json.loads(path.read_text())
        slot = geometry.setdefault(request, {"frames": [], "xy": {}, "tri": {}})
        slot["frames"].append(int(index or 0))
        for node, mesh in data["nodes"].items():
            slot["xy"].setdefault(node, []).append(np.asarray(mesh["world"], np.float32)[:, :2])
            slot["tri"].setdefault(node, np.asarray(mesh["triangles"], np.int32))
        path.unlink()
    for request, slot in geometry.items():
        arrays = {"frames": np.asarray(slot["frames"], np.int32)}
        for node, frames in slot["xy"].items():
            arrays[f"xy:{node}"] = np.stack(frames)
            arrays[f"tri:{node}"] = slot["tri"][node]
        np.savez_compressed(directory / f"{request}--geometry.npz", **arrays)
    for path in sorted(directory.glob("*--trace.jsonl")):
        rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        keys = sorted(rows[0]["effective"]) if rows else []
        np.savez_compressed(path.with_suffix(".npz"), frames=np.asarray([r["frame"] for r in rows], np.int32),
                            keys=np.asarray(keys),
                            values=np.asarray([[r["effective"][k] for k in keys] for r in rows], np.float32))
        path.unlink()


def render(model: Path, spec: Path, out: Path, packed: bool) -> tuple[str, bool, str]:
    name = spec.name.removesuffix(".spec.json")
    target, log = out / name, out / f"{name}.log"
    shutil.rmtree(target, ignore_errors=True)
    began = time.monotonic()
    with log.open("wb") as sink:
        code = subprocess.run([*cli(), "render", str(model), "--spec", str(spec), "--out-dir", str(target)],
                              stdout=sink, stderr=subprocess.STDOUT).returncode
    manifest = target / "run.json"
    complete = code == 0 and manifest.exists() and json.loads(manifest.read_text()).get("complete") is True
    if not complete:
        lines = log.read_text(errors="replace").strip().splitlines()
        return name, False, lines[-1][:300] if lines else f"exit {code}"
    if packed:
        pack(target)
    return name, True, f"{time.monotonic() - began:.0f}s"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("model", type=Path)
    parser.add_argument("specs", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("prefixes", nargs="*")
    parser.add_argument("--jobs", type=int, default=3, help="concurrent CLI runs (default 3)")
    parser.add_argument("--pack", action="store_true")
    args = parser.parse_args()
    specs = [s for s in sorted(args.specs.glob("*.spec.json"))
             if not args.prefixes or any(s.name.startswith(p) for p in args.prefixes)]
    if not specs:
        print(f"no specs in {args.specs} match", file=sys.stderr)
        return 2
    args.out.mkdir(parents=True, exist_ok=True)
    failed = 0
    with ThreadPoolExecutor(args.jobs) as pool:
        for name, ok, note in pool.map(lambda s: render(args.model, s, args.out, args.pack), specs):
            print(f"{'ok  ' if ok else 'FAIL'} {name} {note}", flush=True)
            failed += not ok
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
