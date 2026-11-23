"""The placement file that pairs cut-out part PNGs with where each goes.

    {
      "catchlight_layers": 1,
      "layers": [
        {"name": "Head", "z": 1},
        {"name": "Face", "file": "face.png", "parent": "Head",
         "z": 2, "offset": [0.0, 0.4], "opacity": 1.0,
         "mesh": {"mode": "grid", "cols": 4, "rows": 3}}
      ]
    }

A layer without `file` is a group. `z` is draw order (higher in front,
accumulating down the tree); `offset` is in model space. `file` is relative
to the placement's directory, and parents must be declared before children.
"""

from __future__ import annotations

import json
import os
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

from .protocol_gen import AutoMesh, parse_auto_mesh

__all__ = [
    "LAYERS_VERSION",
    "Layer",
    "LayersError",
    "Placement",
    "read_layers",
    "write_layers",
]

LAYERS_VERSION = 1

_VERSION_KEY = "catchlight_layers"


class LayersError(ValueError):
    """A placement this module will not read. The message names the layer."""


@dataclass(frozen=True)
class Layer:
    """A part, or a group when it has no `file`."""

    name: str
    z: float = 0.0
    file: str | None = None
    parent: str | None = None
    offset: tuple[float, float] = (0.0, 0.0)
    opacity: float = 1.0
    mesh: AutoMesh | None = None

    @property
    def is_group(self) -> bool:
        return self.file is None

    def to_wire(self) -> dict[str, Any]:
        out: dict[str, Any] = {"name": self.name, "z": self.z}
        if self.file is not None:
            out["file"] = self.file
        if self.parent is not None:
            out["parent"] = self.parent
        if self.offset != (0.0, 0.0):
            out["offset"] = [self.offset[0], self.offset[1]]
        if self.opacity != 1.0:
            out["opacity"] = self.opacity
        if self.mesh is not None:
            out["mesh"] = self.mesh.to_wire()
        return out


@dataclass(frozen=True)
class Placement:

    root: Path
    layers: tuple[Layer, ...]

    def path_of(self, layer: Layer) -> Path:
        if layer.file is None:
            raise LayersError(f"layer {layer.name!r} is a group and has no file")
        return self.root / PurePosixPath(layer.file)


def read_layers(path: str | os.PathLike[str]) -> Placement:
    source = Path(os.fspath(path))
    try:
        placement = json.loads(source.read_bytes())
    except json.JSONDecodeError as bad:
        raise LayersError(f"{source.name} is not JSON: {bad}") from bad
    if not isinstance(placement, Mapping):
        raise LayersError(f"{source.name} is not a placement object")

    version = placement.get(_VERSION_KEY)
    if version != LAYERS_VERSION:
        raise LayersError(
            f"{source.name} carries {_VERSION_KEY} {version!r}, not {LAYERS_VERSION}"
        )
    raw = placement.get("layers")
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        raise LayersError(f"{source.name} has no layers list")

    root = source.parent
    layers: list[Layer] = []
    seen: set[str] = set()
    for index, entry in enumerate(raw):
        layer = _layer(entry, index)
        if layer.name in seen:
            raise LayersError(f"layer {layer.name!r} is declared twice")
        if layer.parent is not None and layer.parent not in seen:
            raise LayersError(
                f"layer {layer.name!r} names the parent {layer.parent!r}, "
                "which is not declared before it"
            )
        seen.add(layer.name)
        if layer.file is not None and not (root / PurePosixPath(layer.file)).is_file():
            raise LayersError(
                f"layer {layer.name!r} names the file {layer.file!r}, which is not there"
            )
        layers.append(layer)
    return Placement(root=root, layers=tuple(layers))


def write_layers(placement: Placement, path: str | os.PathLike[str]) -> Path:
    """The root is not written: it is wherever the file lands."""
    target = Path(os.fspath(path))
    placement = {
        _VERSION_KEY: LAYERS_VERSION,
        "layers": [layer.to_wire() for layer in placement.layers],
    }
    target.write_text(json.dumps(placement, indent=2) + "\n", encoding="utf-8")
    return target


def _layer(entry: Any, index: int) -> Layer:
    where = f"layer {index}"
    if not isinstance(entry, Mapping):
        raise LayersError(f"{where} is not an object")
    name = entry.get("name")
    if not isinstance(name, str) or not name:
        raise LayersError(f"{where} has no name")
    where = f"layer {name!r}"

    file = entry.get("file")
    if file is not None:
        if not isinstance(file, str) or not file:
            raise LayersError(f"{where} has a file that is not a path")
        relative = PurePosixPath(file)
        if relative.is_absolute() or ".." in relative.parts:
            raise LayersError(
                f"{where} names the file {file!r}, which is not below the placement"
            )

    parent = entry.get("parent")
    if parent is not None and (not isinstance(parent, str) or not parent):
        raise LayersError(f"{where} has a parent that is not a name")

    offset = entry.get("offset", (0.0, 0.0))
    if (
        not isinstance(offset, Sequence)
        or isinstance(offset, (str, bytes))
        or len(offset) != 2
    ):
        raise LayersError(f"{where} has an offset that is not two numbers")

    mesh_wire = entry.get("mesh")
    mesh: AutoMesh | None = None
    if mesh_wire is not None:
        if not isinstance(mesh_wire, Mapping):
            raise LayersError(f"{where} has a mesh that is not an object")
        try:
            mesh = parse_auto_mesh(mesh_wire)
        except ValueError as bad:
            raise LayersError(
                f"{where} has a mesh this editor does not know: {bad}"
            ) from bad

    return Layer(
        name=name,
        z=_number(entry.get("z", 0.0), where, "z"),
        file=file,
        parent=parent,
        offset=(
            _number(offset[0], where, "offset"),
            _number(offset[1], where, "offset"),
        ),
        opacity=_number(entry.get("opacity", 1.0), where, "opacity"),
        mesh=mesh,
    )


def _number(value: Any, where: str, field: str) -> float:
    """`True` is rejected even though Python treats it as an int."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise LayersError(f"{where} has a {field} that is not a number: {value!r}")
    return float(value)
