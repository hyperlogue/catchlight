"""Render specs for a film's shots. Standard library only.

A shot is a window of the film's parameter timeline rendered at one framing.
Its passes share one animation, so their motion responses match frame for
frame. `render --schema spec` documents every request field a pass may carry.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Mapping, Sequence

# quality -> (capture every Nth frame, scale multiplier)
QUALITY = {"preview": (4, 0.5), "final": (1, 1.0)}
TRANSPARENT = {"srgb": [0, 0, 0], "alpha": 0}
# Multiply-blended parts vanish over transparent black; give their pass this.
WHITE = {"srgb": [1, 1, 1], "alpha": 1}


def animation(tracks: Mapping[str, Sequence[float]], fps: float, start: int, count: int) -> dict:
    """A spec animation holding `count` frames of the sampled tracks from `start`."""
    return {
        "timestep": 1.0 / fps, "length": count,
        "lanes": [{"param": param, "interpolation": "Linear",
                   "keyframes": [{"frame": i, "value": round(float(v), 5)}
                                 for i, v in enumerate(values[start:start + count])]}
                  for param, values in tracks.items()],
    }


def shot(name: str, *, tracks: Mapping[str, Sequence[float]], fps: float, t0: float, t1: float,
         rect: Sequence[float], scale: float, passes: Mapping[str, dict] | None = None,
         quality: str = "final", pre: float = 0.0, split: bool = False,
         video: str | None = "mkv") -> tuple[dict[str, dict], dict]:
    """Specs and compositing metadata for one shot.

    tracks: param id -> one value per film frame, the whole film's timeline.
    t0, t1: film seconds the shot covers; give each side a margin for cuts.
    pre:    seconds simulated and captured before t0, so motion responses have
            settled into the motion by the time the shot is on screen.
    passes: pass name -> request fields that differ from the shared ones.
    split:  one spec, and one CLI run, per pass; use when a shot exceeds the
            CLI's pixel budget or to spread passes over parallel runs.
    Returns ({spec name: spec}, meta); `write` stores both.
    """
    every, mult = QUALITY[quality]
    passes = dict(passes or {"base": {}})
    start = max(0, int(round((t0 - pre) * fps)))
    start -= start % every
    count = min(int(round(t1 * fps)) - start + 1, min(len(v) for v in tracks.values()) - start)
    if count < 1:
        raise ValueError(f"{name}: no frames between {t0} and {t1}")
    shared = {"rect": list(rect), "scale": scale * mult, "animation": {"source": "spec", "name": "a"},
              "motion_responses": {"mode": "on"}, "frames": {"count": count, "every": every},
              "background": TRANSPARENT}
    if video:
        shared["video"] = {"format": video}
    requests = {pas: shared | dict(extra) for pas, extra in passes.items()}
    clip = animation(tracks, fps, start, count)
    groups = {f"{name}.{pas}": {pas: req} for pas, req in requests.items()} if split else {name: requests}
    specs = {spec: {"schema": 1, "animations": {"a": clip}, "requests": reqs} for spec, reqs in groups.items()}
    meta = {"start": start, "frames": count, "every": every, "fps": fps, "rect": list(rect),
            "scale": scale * mult, "t0": t0, "t1": t1,
            "passes": {pas: f"{name}.{pas}" if split else name for pas in passes}}
    return specs, meta


def write(directory: Path | str, shots: Mapping[str, tuple[dict[str, dict], dict]]) -> None:
    """Replace `directory`'s specs with these shots and write shots.json beside them."""
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    for stale in directory.glob("*.spec.json"):
        stale.unlink()
    for specs, _ in shots.values():
        for name, spec in specs.items():
            (directory / f"{name}.spec.json").write_text(json.dumps(spec))
    (directory / "shots.json").write_text(json.dumps({name: meta for name, (_, meta) in shots.items()}, indent=1))
