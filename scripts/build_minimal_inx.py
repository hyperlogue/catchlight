# /// script
# dependencies = ["numpy", "Pillow"]
# ///
"""
Build a minimal .inx puppet for cross-engine rendering tests.

The sample model isolation configs (yp_11_lip_only, yp_11_inner_only, etc.) tried to
narrow the diff source by zeroing parts' opacities. That doesn't work cleanly
because inochi2d's Drawable.draw early-returns on enabled/visibility, so
opacity=0 effectively disables the whole subtree — masking the diff we want
to study.

Instead, build a tiny puppet from scratch: one root + one or two textured
quads with no params, no physics, no masking. Anything the two engines
disagree on must come from the rendering pipeline alone.

Outputs:
  tests/models/single_alpha_quad.inx     (one Part, soft-alpha disk)
  tests/models/quad_over_bg.inx          (BG quad + soft-alpha disk on top)

Inx layout (see crates/catchlight-core/src/formats/inx.rs):
  8-byte magic "TRNSRTS\0"
  4-byte BE payload length, then JSON
  8-byte "TEX_SECT", 4-byte BE texture count, then per-texture:
    4-byte BE length, 1-byte encoding (0=PNG), raw bytes

Texture bytes follow inochi2d's premultiplied-in-sRGB convention so the file
plays cleanly with both engines without storage-side translation getting in
the way of the cross-engine comparison.
"""

from __future__ import annotations
from pathlib import Path
import io
import json
import struct
import numpy as np
from PIL import Image

OUT_DIR = Path(__file__).resolve().parent.parent / "tests" / "models"
OUT_DIR.mkdir(parents=True, exist_ok=True)

# ---- texture authoring -----------------------------------------------------


def soft_disk_premul_srgb(size=256, color=(220, 30, 60), feather_px=24) -> bytes:
    """Soft-alpha red disk on transparent background, premultiplied in sRGB
    byte space (inochi2d's storage convention)."""
    cx, cy = size / 2 - 0.5, size / 2 - 0.5
    yy, xx = np.indices((size, size), dtype=np.float32)
    d = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2)
    radius = size / 2 - feather_px
    # alpha=1 for d<=radius, ramps linearly to 0 over feather_px, then 0
    alpha = np.clip((radius + feather_px - d) / feather_px, 0.0, 1.0)
    rgb = np.stack([np.full_like(alpha, c, dtype=np.float32) for c in color], axis=-1)
    # Premultiply in sRGB byte space: rgb_byte * alpha (no gamma conversion).
    rgb_premul = (rgb * alpha[..., None]).round().clip(0, 255).astype(np.uint8)
    a = (alpha * 255).round().clip(0, 255).astype(np.uint8)
    rgba = np.concatenate([rgb_premul, a[..., None]], axis=-1)
    img = Image.fromarray(rgba, mode="RGBA")
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def solid_quad_premul_srgb(size=256, rgb=(80, 120, 200)) -> bytes:
    arr = np.zeros((size, size, 4), dtype=np.uint8)
    arr[..., 0] = rgb[0]
    arr[..., 1] = rgb[1]
    arr[..., 2] = rgb[2]
    arr[..., 3] = 255
    img = Image.fromarray(arr, mode="RGBA")
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def gradient_quad_premul_srgb(size=256) -> bytes:
    """Opaque 2D gradient: R ramps left→right, B ramps top→bottom, G held
    mid. Every blend mode then sees a backdrop that spans dark↔bright across
    each foreground cell, so modes like Multiply/Screen/Darken/Lighten produce
    visibly different output instead of collapsing on a flat color."""
    yy, xx = np.indices((size, size), dtype=np.float32)
    r = 40 + (xx / (size - 1)) * 190
    b = 40 + (yy / (size - 1)) * 190
    g = np.full_like(r, 130.0)
    arr = np.stack([r, g, b, np.full_like(r, 255.0)], axis=-1)
    img = Image.fromarray(arr.round().clip(0, 255).astype(np.uint8), mode="RGBA")
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


# ---- node authoring --------------------------------------------------------

NEXT_UUID = [1000]


def uuid() -> int:
    NEXT_UUID[0] += 1
    return NEXT_UUID[0]


def quad_part(
    name: str,
    tex_index: int,
    *,
    half_size=128.0,
    translate=(0.0, 0.0, 0.0),
    zsort=0.0,
    opacity=1.0,
    masks: list[dict] | None = None,
    blend_mode: str = "Normal",
) -> dict:
    """A single Part rendering a centred quad of side 2*half_size, mapped
    to the full UV [0,1]² of the given texture. Two triangles, four verts."""
    h = half_size
    p = {
        "uuid": uuid(),
        "name": name,
        "type": "Part",
        "enabled": True,
        "zsort": zsort,
        "transform": {
            "trans": [translate[0], translate[1], translate[2]],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "mesh": {
            "verts": [-h, -h, h, -h, h, h, -h, h],
            "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
            "indices": [0, 1, 2, 0, 2, 3],
            "origin": [0.0, 0.0],
        },
        "textures": [tex_index, 0xFFFFFFFF, 0xFFFFFFFF],
        "blend_mode": blend_mode,
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "emissionStrength": 1.0,
        "mask_threshold": 0.5,
        "opacity": opacity,
    }
    if masks is not None:
        p["masks"] = masks
    return p


def make_puppet(children: list[dict]) -> dict:
    return {
        "meta": {
            "name": "minimal_repro",
            "version": "v0.7.2",
            "preservePixels": False,
        },
        "physics": {"pixelsPerMeter": 1000.0, "gravity": 9.8},
        "nodes": {
            "uuid": uuid(),
            "name": "root",
            "type": "Node",
            "enabled": True,
            "zsort": 0.0,
            "transform": {
                "trans": [0.0, 0.0, 0.0],
                "rot": [0.0, 0.0, 0.0],
                "scale": [1.0, 1.0],
            },
            "lockToRoot": False,
            "children": children,
        },
        "param": [],
        "automation": None,
        "animations": None,
        "groups": [],
    }


# ---- inx serialiser --------------------------------------------------------

MAGIC = b"TRNSRTS\0"
TEX_SECT = b"TEX_SECT"


def write_inx(path: Path, payload: dict, png_textures: list[bytes]) -> None:
    out = bytearray()
    out += MAGIC
    payload_bytes = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    out += struct.pack(">I", len(payload_bytes))
    out += payload_bytes
    out += TEX_SECT
    out += struct.pack(">I", len(png_textures))
    for png in png_textures:
        out += struct.pack(">I", len(png))
        out += b"\x00"  # encoding 0 = PNG
        out += png
    path.write_bytes(out)
    print(f"wrote {path} ({len(out):,} bytes, {len(png_textures)} textures)")


# ---- builds ----------------------------------------------------------------


def build_single_alpha_quad():
    NEXT_UUID[0] = 1000
    disk_png = soft_disk_premul_srgb(size=512, color=(220, 30, 60), feather_px=64)
    payload = make_puppet(
        [
            quad_part("Disk", tex_index=0, half_size=200.0, zsort=0.0),
        ]
    )
    write_inx(OUT_DIR / "single_alpha_quad.inx", payload, [disk_png])


def build_quad_over_bg():
    NEXT_UUID[0] = 2000
    bg_png = solid_quad_premul_srgb(size=64, rgb=(80, 120, 200))
    disk_png = soft_disk_premul_srgb(size=512, color=(220, 30, 60), feather_px=64)
    payload = make_puppet(
        [
            quad_part("BG", tex_index=0, half_size=300.0, zsort=10.0),
            quad_part("Disk", tex_index=1, half_size=200.0, zsort=0.0),
        ]
    )
    write_inx(OUT_DIR / "quad_over_bg.inx", payload, [bg_png, disk_png])


def build_disk_masked_by_disk():
    """Mimic the sample model's Mouth Inner ← masked by ← Face arrangement: a soft red
    disk masked by a soft (slightly smaller) blue disk that lives only in the
    stencil pass. Should reproduce any mask-discard / stencil-vs-alpha-mask
    discrepancy without any params, physics, opacity overrides."""
    NEXT_UUID[0] = 3000
    bg_png = solid_quad_premul_srgb(size=64, rgb=(245, 235, 220))
    mask_png = soft_disk_premul_srgb(size=512, color=(120, 200, 230), feather_px=32)
    fg_png = soft_disk_premul_srgb(size=512, color=(220, 30, 60), feather_px=64)
    mask_part = quad_part("Mask", tex_index=1, half_size=160.0, zsort=1.0)
    masked_part = quad_part(
        "Masked",
        tex_index=2,
        half_size=220.0,
        zsort=0.0,
        masks=[{"source": mask_part["uuid"], "mode": "Mask"}],
    )
    payload = make_puppet(
        [
            quad_part("BG", tex_index=0, half_size=300.0, zsort=10.0),
            mask_part,
            masked_part,
        ]
    )
    write_inx(OUT_DIR / "disk_masked_by_disk.inx", payload, [bg_png, mask_png, fg_png])


def build_multiply_blend():
    """A red disk with Multiply blend over a cream background — Multiply is
    the blend mode that's been historically tricky to get pixel-identical
    between OpenGL and wgpu (premul vs straight alpha + sRGB encode)."""
    NEXT_UUID[0] = 4000
    bg_png = solid_quad_premul_srgb(size=64, rgb=(245, 235, 220))
    fg_png = soft_disk_premul_srgb(size=512, color=(220, 30, 60), feather_px=48)
    payload = make_puppet(
        [
            quad_part("BG", tex_index=0, half_size=300.0, zsort=10.0),
            quad_part(
                "Disk", tex_index=1, half_size=200.0, zsort=0.0, blend_mode="Multiply"
            ),
        ]
    )
    write_inx(OUT_DIR / "multiply_blend.inx", payload, [bg_png, fg_png])


def build_blend_modes_grid():
    """One puppet exercising every blend mode the renderer implements, so a
    regression in any blend-state branch or blend shader (fs_overlay,
    fs_color_burn, fs_linear_burn) shows up as a localized cell in the diff
    heatmap. A 2D-gradient background sits under a 4×4 grid of feathered
    disks; each disk is a Part with one distinct blend_mode. Cell order is
    row-major and matches BLEND_GRID_ORDER below — keep the two in sync so a
    failing cell maps back to a mode by position."""
    NEXT_UUID[0] = 5000
    bg_png = gradient_quad_premul_srgb(size=512)
    fg_png = soft_disk_premul_srgb(size=256, color=(210, 175, 70), feather_px=20)

    # Renderer BlendMode order (crates/catchlight-wgpu/src/renderer.rs).
    modes = [
        "Normal",
        "Multiply",
        "ColorDodge",
        "LinearDodge",
        "Screen",
        "ClipToLower",
        "SliceFromLower",
        "Overlay",
        "ColorBurn",
        "LinearBurn",
        "Darken",
        "Lighten",
        "Add",
        "Inverse",
        "Subtract",
    ]
    centers = [-165.0, -55.0, 55.0, 165.0]  # 4×4 grid within ±220 model units
    # INX uses lower zsort in front, so the source BG must have the highest
    # value. Import reflects the values into Catchlight's higher-front frame.
    children = [quad_part("BG", tex_index=0, half_size=260.0, zsort=10.0)]
    for i, mode in enumerate(modes):
        cx = centers[i % 4]
        cy = centers[i // 4]
        children.append(
            quad_part(
                f"{i:02d}_{mode}",
                tex_index=1,
                half_size=52.0,
                translate=(cx, cy, 0.0),
                zsort=0.0,
                blend_mode=mode,
            )
        )
    write_inx(OUT_DIR / "blend_modes.inx", make_puppet(children), [bg_png, fg_png])


def build_blend_modes_composite():
    """Dst-in-shader disks (Overlay/ColorBurn/LinearBurn) as children of a
    Composite, pinning the reference semantics (inochi2d KHR advanced
    blending inside inBeginComposite): each disk blends against the
    composite's own buffer — the gray pad where it overlaps it, plain disk
    color where the composite is transparent — never against the root
    gradient behind the composite. Each disk straddles a pad edge so both
    halves are visible in one cell; a root-level ColorBurn disk over the
    gradient gives in-image contrast with the nested ones."""
    NEXT_UUID[0] = 6000
    bg_png = gradient_quad_premul_srgb(size=512)
    pad_png = solid_quad_premul_srgb(size=64, rgb=(200, 200, 200))
    fg_png = soft_disk_premul_srgb(size=256, color=(210, 175, 70), feather_px=20)

    composite_children = [quad_part("Pad", tex_index=1, half_size=130.0, zsort=5.0)]
    nested = [
        ("Overlay", (-130.0, 0.0)),
        ("ColorBurn", (0.0, -130.0)),
        ("LinearBurn", (130.0, 0.0)),
    ]
    for i, (mode, (cx, cy)) in enumerate(nested):
        composite_children.append(
            quad_part(
                f"{i:02d}_{mode}",
                tex_index=2,
                half_size=52.0,
                translate=(cx, cy, 0.0),
                zsort=0.0,
                blend_mode=mode,
            )
        )
    composite = {
        "uuid": uuid(),
        "name": "Composite",
        "type": "Composite",
        "enabled": True,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 1.0,
        "children": composite_children,
    }
    children = [
        quad_part("BG", tex_index=0, half_size=260.0, zsort=10.0),
        composite,
        quad_part(
            "Root_ColorBurn",
            tex_index=2,
            half_size=52.0,
            translate=(0.0, 200.0, 0.0),
            zsort=-1.0,
            blend_mode="ColorBurn",
        ),
    ]
    write_inx(
        OUT_DIR / "blend_modes_composite.inx",
        make_puppet(children),
        [bg_png, pad_png, fg_png],
    )


def build_nested_composite():
    """An isolating Composite inside another Composite — the case inochi2d
    supports (`Composite.scanPartsRecurse` appends a nested Composite to the
    enclosing one's `toRender`, and `Composite.draw` draws it inside the
    enclosing beginComposite/endComposite scope) and catchlight used to get
    wrong by hoisting it to the root.

    The inner composite isolates on opacity (0.65, so not a pass-through
    group) rather than on a blend mode, keeping the check independent of
    per-mode blend state: its slot blits into the OUTER's slot, and the
    outer then fades the whole group by its own 0.5 — so the inner disk
    lands at a net 0.325 and `Front` (lower source zsort) paints over it inside
    the group.

    Escaping to the root instead is loud here: the disk would land at 0.65
    straight on the framebuffer, unfaded by the outer, and sorted against
    the root's drawables rather than the outer's children."""
    NEXT_UUID[0] = 7000
    bg_png = gradient_quad_premul_srgb(size=512)
    pad_png = solid_quad_premul_srgb(size=64, rgb=(210, 210, 210))
    disk_png = soft_disk_premul_srgb(size=256, color=(210, 90, 60), feather_px=18)
    front_png = solid_quad_premul_srgb(size=64, rgb=(60, 90, 190))

    inner = {
        "uuid": uuid(),
        "name": "InnerComposite",
        "type": "Composite",
        "enabled": True,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 0.65,
        # Straddles the pad edge, so both "over the pad" and "over the
        # outer's transparent region" are visible in one image.
        "children": [
            quad_part(
                "InnerDisk",
                tex_index=2,
                half_size=110.0,
                translate=(90.0, 90.0, 0.0),
                zsort=0.0,
            )
        ],
    }
    outer = {
        "uuid": uuid(),
        "name": "OuterComposite",
        "type": "Composite",
        "enabled": True,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 0.5,
        "children": [
            quad_part("Pad", tex_index=1, half_size=130.0, zsort=5.0),
            inner,
            quad_part(
                "Front",
                tex_index=3,
                half_size=45.0,
                translate=(120.0, 120.0, 0.0),
                zsort=-5.0,
            ),
        ],
    }
    children = [
        quad_part("BG", tex_index=0, half_size=260.0, zsort=10.0),
        outer,
    ]
    write_inx(
        OUT_DIR / "nested_composite.inx",
        make_puppet(children),
        [bg_png, pad_png, disk_png, front_png],
    )


def build_composite_masks():
    NEXT_UUID[0] = 8000
    bg_png = gradient_quad_premul_srgb(size=512)
    mask_png = soft_disk_premul_srgb(size=256, color=(255, 255, 255), feather_px=18)
    red_png = solid_quad_premul_srgb(size=64, rgb=(220, 55, 70))
    green_png = solid_quad_premul_srgb(size=64, rgb=(55, 200, 110))
    gold_png = solid_quad_premul_srgb(size=64, rgb=(220, 175, 55))

    source_parts = [
        quad_part("LeftMask", 1, half_size=70.0, translate=(-140.0, 0.0, 0.0)),
        quad_part("CenterMask", 1, half_size=70.0, translate=(0.0, 0.0, 0.0)),
    ]
    for part in source_parts:
        part["enabled"] = False
    source = {
        "uuid": uuid(),
        "name": "CompositeMaskSource",
        "type": "Composite",
        "enabled": True,
        "zsort": 5.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 0.75,
        "children": source_parts,
    }
    source_binding = [{"source": source["uuid"], "mode": "Mask"}]

    part_target = quad_part(
        "PartTarget",
        2,
        half_size=88.0,
        translate=(-140.0, 0.0, 0.0),
        zsort=0.0,
        masks=source_binding,
    )
    composite_target = {
        "uuid": uuid(),
        "name": "CompositeTarget",
        "type": "Composite",
        "enabled": True,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Normal",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 1.0,
        "masks": source_binding,
        "children": [quad_part("CompositeContent", 3, half_size=88.0)],
    }

    advanced_mask = quad_part(
        "AdvancedMask",
        1,
        half_size=70.0,
        translate=(140.0, 0.0, 0.0),
        zsort=5.0,
    )
    advanced_mask["enabled"] = False
    advanced = {
        "uuid": uuid(),
        "name": "AdvancedMaskedComposite",
        "type": "Composite",
        "enabled": True,
        "zsort": 0.0,
        "transform": {
            "trans": [0.0, 0.0, 0.0],
            "rot": [0.0, 0.0, 0.0],
            "scale": [1.0, 1.0],
        },
        "lockToRoot": False,
        "blend_mode": "Overlay",
        "tint": [1.0, 1.0, 1.0],
        "screenTint": [0.0, 0.0, 0.0],
        "mask_threshold": 0.5,
        "opacity": 1.0,
        "masks": [{"source": advanced_mask["uuid"], "mode": "Mask"}],
        "children": [
            quad_part(
                "AdvancedContent",
                4,
                half_size=88.0,
                translate=(140.0, 0.0, 0.0),
            )
        ],
    }

    children = [
        quad_part("BG", 0, half_size=260.0, zsort=10.0),
        source,
        advanced_mask,
        part_target,
        composite_target,
        advanced,
    ]
    write_inx(
        OUT_DIR / "composite_masks.inx",
        make_puppet(children),
        [bg_png, mask_png, red_png, green_png, gold_png],
    )


if __name__ == "__main__":
    build_single_alpha_quad()
    build_quad_over_bg()
    build_disk_masked_by_disk()
    build_multiply_blend()
    build_blend_modes_grid()
    build_blend_modes_composite()
    build_nested_composite()
    build_composite_masks()
