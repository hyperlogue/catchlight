# /// script
# dependencies = ["cbor2"]
# ///
"""Generate tests/models/welded_seam.clp — the weld regression model.

Two 3x3-grid Parts stacked vertically with a coincident seam at y=0 and a
"pull" param that deform-shifts the whole top part by (60, 40). The seam is
welded per-vertex with weights 1.0 / 0.5 / 0.0 left-to-right, so one render at
pull=1 shows all three weight regimes: B stretching up to follow A, both
meeting midway, and A's corner staying pinned to B.

Written through a separate Python `.clp` writer, so the fixture exercises an
independent writer against catchlight's reader rather than round-tripping our
own. That writer lives outside this repo; point `CLP_WRITER` at a directory
containing an importable `clp` module to regenerate. Deterministic output.

The generated fixture is committed at tests/models/welded_seam.clp,
so running this is only needed to change it.

    CLP_WRITER=/path/to/writer uv run scripts/build_welded_seam_clp.py
"""

from __future__ import annotations

import os
import struct
import sys
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

_writer = os.environ.get("CLP_WRITER")
if not _writer:
    raise SystemExit(
        "CLP_WRITER is unset: this generator needs an external Python `.clp` "
        "writer. The fixture it produces is already committed at "
        "tests/models/welded_seam.clp."
    )
sys.path.insert(0, _writer)

import clp  # noqa: E402

COLS, ROWS = 3, 3
HALF_W = 150.0
PART_H = 120.0
PULL = (60.0, 40.0)
SEAM_WEIGHTS = [1.0, 0.5, 0.0]


def solid_png(rgb: tuple[int, int, int], size: int = 64) -> bytes:
    """Minimal deterministic RGBA PNG (no PIL, fixed zlib level)."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data))
        )

    row = b"\x00" + bytes(rgb + (255,)) * size
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    idat = zlib.compress(row * size, 9)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", idat)
        + chunk(b"IEND", b"")
    )


def grid_part(albedo: int, y_top: float) -> clp.Part:
    """A 3x3-vertex grid spanning x in [-HALF_W, HALF_W], y in
    [y_top - PART_H, y_top], verts row-major from the seam-most row up/down so
    both parts' seam vertices are indices 0..2."""
    verts: list[float] = []
    uvs: list[float] = []
    y_seam = y_top - PART_H if y_top > 0 else y_top
    for r in range(ROWS):
        # Part A (y_top > 0) grows up from its seam row; part B grows down.
        y = (y_seam + r * PART_H / (ROWS - 1)) if y_top > 0 else (y_top - r * PART_H / (ROWS - 1))
        for c in range(COLS):
            x = -HALF_W + c * 2.0 * HALF_W / (COLS - 1)
            verts += [x, y]
            uvs += [(x + HALF_W) / (2.0 * HALF_W), (y_top - y) / PART_H]
    indices: list[int] = []
    for r in range(ROWS - 1):
        for c in range(COLS - 1):
            v00 = r * COLS + c
            v01 = v00 + 1
            v10 = v00 + COLS
            v11 = v10 + 1
            indices += [v00, v01, v11, v00, v11, v10]
    return clp.Part(
        mesh=clp.ClpMesh(verts=verts, uvs=uvs, indices=("U16", indices)),
        albedo=albedo,
    )


def main() -> None:
    root = clp.ClpNode(kind=clp.Empty(), name="root")
    upper = clp.ClpNode(kind=grid_part(0, y_top=PART_H), parent=0, name="upper")
    lower = clp.ClpNode(kind=grid_part(1, y_top=0.0), parent=0, name="lower")

    n_verts = COLS * ROWS
    pull = clp.ClpParam(
        name="pull",
        min=(0.0, 0.0),
        max=(1.0, 0.0),
        defaults=(0.0, 0.0),
        axis_points_x=[0.0, 1.0],
        axis_points_y=[0.0],
        bindings=[
            clp.ClpBinding(
                node=1,
                values=clp.Deform(
                    clp.ClpMatrix(
                        width=2,
                        height=1,
                        data=[[0.0] * (2 * n_verts), list(PULL) * n_verts],
                    )
                ),
            )
        ],
    )

    doc = clp.ClpDocument(
        nodes=[root, upper, lower],
        params=[pull],
        welds=[
            clp.ClpWeld(
                a=1,
                b=2,
                pairs=[
                    clp.ClpWeldPair(a_vert=i, b_vert=i, weight=w)
                    for i, w in enumerate(SEAM_WEIGHTS)
                ],
            )
        ],
    )
    textures = [
        clp.ClpTexture(clp.TextureEncoding.Png, clp.TextureAlpha.Straight, solid_png((230, 140, 60))),
        clp.ClpTexture(clp.TextureEncoding.Png, clp.TextureAlpha.Straight, solid_png((50, 160, 170))),
    ]

    out = REPO / "tests" / "models" / "welded_seam.clp"
    out.write_bytes(clp.encode(doc, textures))
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
