"""Test helpers. The PNG writer uses only `zlib`/`struct` to keep tests
dependency-free."""

from __future__ import annotations

import json
import os
import struct
import tempfile
import zlib
from pathlib import Path


def png_bytes(width: int, height: int, inset: int = 4) -> bytes:
    """The transparent margin gives `mesh_auto` a contour other than the border."""
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
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("not a PNG")
    width, height = struct.unpack("!II", data[16:24])
    return width, height


def write_manifest(directory: str | os.PathLike[str]) -> Path:
    """The reference has a `/` so attachment names must carry it verbatim."""
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
    """Launch temp directories not yet removed."""
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
    return {
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
