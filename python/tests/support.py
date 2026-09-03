"""What the tests need that the package does not provide.

The PNG writer is here because the package has no runtime dependencies and the
tests inherit that: a texture the editor can decode has to be built out of
`zlib` and `struct` rather than out of Pillow.
"""

from __future__ import annotations

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
