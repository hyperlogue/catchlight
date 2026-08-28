# /// script
# requires-python = ">=3.10"
# dependencies = ["Pillow"]
# ///
"""Transcode the inline textures of an inochi2d .inx/.inp file from TGA to PNG.

The .inx/.inp container is: `TRNSRTS\\0` magic, a BE-u32-length-prefixed JSON
payload, a `TEX_SECT` block of length-prefixed textures (each with a 1-byte
encoding: 0=PNG, 1=TGA, 2=BC7), then an optional `EXT_SECT` vendor block.

Textures are stored barely-compressed (RLE TGA). PNG is lossless on the same
pixels and typically ~3x smaller, so this is a pure asset-packaging win — the
catchlight loader decodes any encoding to RGBA8 before its premultiply/bleed
passes, so a pixel-identical re-encode leaves the render untouched.

This rewrite copies the JSON payload and the vendor section *verbatim* (the
float-heavy param data is never re-serialized) and only swaps each TGA blob
(encoding 1) for a PNG blob (encoding 0). PNG/BC7 blobs are passed through.

By default it re-decodes every PNG it writes and asserts the pixels match the
source TGA, aborting on any mismatch — the only way the conversion could alter
the render is if the two codecs disagreed on the source bytes.
"""

import argparse
import io
import struct
import sys
from pathlib import Path

from PIL import Image

MAGIC = b"TRNSRTS\0"
TEX_SECT = b"TEX_SECT"
ENC_PNG, ENC_TGA, ENC_BC7 = 0, 1, 2
ENC_NAMES = {ENC_PNG: "PNG", ENC_TGA: "TGA", ENC_BC7: "BC7"}


class Reader:
    def __init__(self, buf: bytes):
        self.buf = buf
        self.off = 0

    def take(self, n: int) -> bytes:
        if self.off + n > len(self.buf):
            raise ValueError(f"unexpected EOF: wanted {n} bytes at offset {self.off}")
        out = self.buf[self.off : self.off + n]
        self.off += n
        return out

    def u32(self) -> int:
        return struct.unpack(">I", self.take(4))[0]


def encode_png(
    tga_bytes: bytes, *, optimize: bool, compress_level: int
) -> tuple[bytes, Image.Image]:
    img = Image.open(io.BytesIO(tga_bytes))
    img.load()
    out = io.BytesIO()
    # Preserve the source mode (RGBA vs RGB) so the re-encode is faithful;
    # the loader's into_rgba8() handles either identically.
    img.save(out, format="PNG", optimize=optimize, compress_level=compress_level)
    return out.getvalue(), img


def png_pixels_match_tga(png_bytes: bytes, tga_img: Image.Image) -> bool:
    rt = Image.open(io.BytesIO(png_bytes))
    rt.load()
    if rt.size != tga_img.size or rt.mode != tga_img.mode:
        return False
    return rt.tobytes() == tga_img.tobytes()


def convert(buf: bytes, *, optimize: bool, compress_level: int, verify: bool) -> bytes:
    r = Reader(buf)
    if r.take(8) != MAGIC:
        raise ValueError("not an inochi2d container (bad TRNSRTS magic)")

    out = bytearray()
    out += MAGIC

    payload_len = r.u32()
    payload = r.take(payload_len)
    out += struct.pack(">I", payload_len) + payload

    sect = r.take(8)
    if sect != TEX_SECT:
        raise ValueError(f"expected TEX_SECT, found {sect!r}")
    out += TEX_SECT

    tex_count = r.u32()
    out += struct.pack(">I", tex_count)

    converted = saved = orig_tex_bytes = new_tex_bytes = 0
    for i in range(tex_count):
        tex_len = r.u32()
        enc = r.take(1)[0]
        data = r.take(tex_len)
        orig_tex_bytes += tex_len

        if enc == ENC_TGA:
            png, img = encode_png(
                data, optimize=optimize, compress_level=compress_level
            )
            if verify and not png_pixels_match_tga(png, img):
                raise ValueError(
                    f"texture[{i}] PNG re-encode does NOT match source TGA pixels — aborting"
                )
            saved += tex_len - len(png)
            converted += 1
            data, enc = png, ENC_PNG

        new_tex_bytes += len(data)
        out += struct.pack(">I", len(data)) + bytes([enc]) + data

    # Everything past the texture block (EXT_SECT + vendor payloads) is copied
    # byte-for-byte.
    out += r.buf[r.off :]

    print(
        f"  textures: {tex_count} total, {converted} TGA->PNG converted\n"
        f"  texture bytes: {orig_tex_bytes:,} -> {new_tex_bytes:,} "
        f"(saved {saved:,} B, {100 * saved / max(orig_tex_bytes, 1):.0f}%)"
    )
    return bytes(out)


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("input", type=Path, help="source .inx/.inp file")
    ap.add_argument(
        "output",
        type=Path,
        nargs="?",
        help="destination file (default: <input-stem>_png.inx next to input)",
    )
    ap.add_argument(
        "--no-optimize",
        dest="optimize",
        action="store_false",
        help="skip PNG filter optimization (faster, slightly larger files)",
    )
    ap.add_argument(
        "--compress-level", type=int, default=9, help="zlib level 0-9 (default 9)"
    )
    ap.add_argument(
        "--no-verify-pixels",
        dest="verify",
        action="store_false",
        help="skip the per-texture PNG==TGA pixel-identity check (not recommended)",
    )
    args = ap.parse_args()

    out_path = args.output or args.input.with_name(f"{args.input.stem}_png.inx")
    if out_path.resolve() == args.input.resolve():
        ap.error("refusing to overwrite the input; choose a different output path")

    buf = args.input.read_bytes()
    print(f"{args.input} ({len(buf):,} B)")
    new_buf = convert(
        buf,
        optimize=args.optimize,
        compress_level=args.compress_level,
        verify=args.verify,
    )
    out_path.write_bytes(new_buf)
    print(
        f"-> {out_path} ({len(new_buf):,} B, "
        f"{100 * len(new_buf) / max(len(buf), 1):.0f}% of original)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
