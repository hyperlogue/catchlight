"""What the tests need that the package does not provide.

The PNG writer is here because the package has no runtime dependencies and the
tests inherit that: a texture the editor can decode has to be built out of
`zlib` and `struct` rather than out of Pillow.
"""

from __future__ import annotations

import json
import os
import struct
import tempfile
import zlib
from pathlib import Path


def png_bytes(width: int, height: int, inset: int = 4) -> bytes:
    """An RGBA PNG: an opaque square inset into a transparent field.

    The transparent margin is what makes it worth tracing — `mesh_auto`'s
    contour reads alpha, and an image opaque to its own edge has a contour that
    is just the image's border.
    """
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            solid = inset <= x < width - inset and inset <= y < height - inset
            rows += bytes((220, 90, 60, 255) if solid else (0, 0, 0, 0))
    header = struct.pack("!IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + _chunk(b"IHDR", header)
        + _chunk(b"IDAT", zlib.compress(bytes(rows), 9))
        + _chunk(b"IEND", b"")
    )


def _chunk(kind: bytes, payload: bytes) -> bytes:
    body = kind + payload
    return struct.pack("!I", len(payload)) + body + struct.pack("!I", zlib.crc32(body))


def write_png(path: str | os.PathLike[str], width: int = 32, height: int = 32) -> Path:
    target = Path(path)
    target.write_bytes(png_bytes(width, height))
    return target


def png_size(data: bytes) -> tuple[int, int]:
    """A PNG's width and height, read out of its IHDR.

    The package has no dependencies and these tests inherit that, so a preview
    is checked by parsing eight bytes of header rather than by decoding it.
    """
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("not a PNG")
    width, height = struct.unpack("!II", data[16:24])
    return width, height


def write_manifest(directory: str | os.PathLike[str]) -> Path:
    """A one-part manifest and the image it names, written under `directory`.

    The texture reference is `images/face.png` rather than a bare name so the
    tests exercise a reference with a separator in it — which is what the
    attachment name has to carry verbatim.
    """
    root = Path(directory)
    (root / "images").mkdir(parents=True, exist_ok=True)
    write_png(root / "images" / "face.png")
    manifest = root / "model.json"
    manifest.write_text(
        json.dumps(
            {
                "name": "akari",
                "textures": [{"id": "face", "path": "images/face.png"}],
                "nodes": [
                    {
                        "id": "face",
                        "kind": "part",
                        "texture": "face",
                        "mesh": {"auto": "quad"},
                    }
                ],
            }
        )
    )
    return manifest


def launched_directories() -> set[Path]:
    """Every temp directory a `launch` has made and not yet removed.

    A launch that failed has to leave none of these, which is only checkable
    from outside the launch that made them.
    """
    return set(Path(tempfile.gettempdir()).glob("catchlight-server-*"))


def is_running(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:  # pragma: no cover - someone else's process
        return True
    return True


def minimal_structure(texture: str = "tex-0") -> dict:
    """A `.clm` structure with one part, spelled as the format's
    serde spells it.

    Written out by hand rather than read back off a saved file, because that
    is the shape `import_json` exists for: a client that *authored* a
    structure and holds its images, with no container anywhere.
    """
    return {
        "physics": {"pixels_per_meter": 1000.0, "gravity": 9.8},
        "nodes": [
            _node("root", None, "root", "Group"),
            _node(
                "node-1",
                "root",
                "Body",
                {
                    "Part": {
                        "mesh": {
                            "verts": [-16.0, 16.0, 16.0, 16.0, 16.0, -16.0, -16.0, -16.0],
                            "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                            "indices": {"U16": [0, 1, 2, 0, 2, 3]},
                            "origin": [0.0, 0.0],
                        },
                        "albedo": texture,
                        "opacity": 1.0,
                        "blend_mode": "Normal",
                        "tint": [1.0, 1.0, 1.0],
                        "screen_tint": [0.0, 0.0, 0.0],
                        "masks": [],
                        "mask_threshold": 0.5,
                        "slots": [],
                    }
                },
            ),
        ],
        "params": [],
        "bindings": [],
        "welds": [],
        "animations": [],
    }


def _node(node_id: str, parent: str | None, name: str, kind: object) -> dict:
    return {
        "id": node_id,
        "parent": parent,
        "name": name,
        "enabled": True,
        "z_order": 0.0,
        "transform": {
            "translation": [0.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lock_to_root": False,
        "kind": kind,
    }
