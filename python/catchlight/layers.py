"""The reference-image pipeline's handoff: cut-out PNGs, and where each one goes.

A step outside this package turns a few reference images into one RGBA PNG per
part and one placement file beside them. This module reads that file and
nothing else — it decodes no image, segments nothing, and knows no more about a
layer than the name of its bytes and where the part belongs.

    {
      "catchlight_layers": 1,
      "layers": [
        {"name": "Head", "z": 1},
        {"name": "Face", "file": "face.png", "parent": "Head",
         "z": 2, "offset": [0.0, 0.4], "opacity": 1.0,
         "mesh": {"mode": "grid", "cols": 4, "rows": 3}}
      ]
    }

`name` and `z` are the only required fields. `z` is draw order the way
`TreeNode.z_order` and the node patch mean it — higher draws in front, and it
accumulates down the tree. `offset` is the part's translation in the model's
space, `opacity` defaults to 1, and `mesh` is an `AutoMesh` exactly as
`to_wire()` emits it; absent, the editor traces the alpha with its own
defaults.

Invariants this module enforces:

- **A layer with no `file` is a group.** There is no `kind` field: a part is a
  layer that has bytes and a group is a layer that has none, so a placement
  cannot name a part with nothing to draw.

- **A `file` is a sibling of the placement, not a path.** It resolves against
  the placement file's own directory, and one that is absolute or that climbs
  out with `..` is refused. A placement is a thing to hand somebody along with
  the directory it names, so it must not carry a path off this machine.

- **A parent is declared before its children.** The order in `layers` is the
  order the builder adds nodes, and a parent that appears later would be a node
  added under one that does not exist yet. Absent means the model's root.

- **Every error names the layer it is about.** A placement comes out of a
  pipeline step nobody watched, so the one thing an error has to carry is which
  of the layers to go and look at.

- **Reading validates, writing does not.** [`read_layers`] refuses a placement
  whose files are not there; [`write_layers`] is the pipeline's own side of the
  handoff and writes what it is given, because the step that writes the
  placement may not have written the PNGs yet.
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

# The one version of the placement document this module reads and writes.
LAYERS_VERSION = 1

# The key the version travels under, which is also what tells a placement from
# any other JSON object somebody points this at.
_VERSION_KEY = "catchlight_layers"


class LayersError(ValueError):
    """A placement this module will not read. The message names the layer."""


@dataclass(frozen=True)
class Layer:
    """One part, or one group when it has no `file`."""

    name: str
    z: float = 0.0
    file: str | None = None
    parent: str | None = None
    offset: tuple[float, float] = (0.0, 0.0)
    opacity: float = 1.0
    mesh: AutoMesh | None = None

    @property
    def is_group(self) -> bool:
        """A layer with no bytes is a group; it draws nothing."""
        return self.file is None

    def to_wire(self) -> dict[str, Any]:
        """This layer as the placement carries it, leaving defaults off."""
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
    """Every layer in the order to add it, and the directory its files are in."""

    root: Path
    layers: tuple[Layer, ...]

    def path_of(self, layer: Layer) -> Path:
        """Where `layer`'s image is. A group has none, and asking is an error."""
        if layer.file is None:
            raise LayersError(f"layer {layer.name!r} is a group and has no file")
        return self.root / PurePosixPath(layer.file)


def read_layers(path: str | os.PathLike[str]) -> Placement:
    """Read a placement, refusing one the builder could not walk.

    Files resolve against `path`'s own directory, which is the placement's
    root.
    """
    source = Path(os.fspath(path))
    try:
        document = json.loads(source.read_bytes())
    except json.JSONDecodeError as bad:
        raise LayersError(f"{source.name} is not JSON: {bad}") from bad
    if not isinstance(document, Mapping):
        raise LayersError(f"{source.name} is not a placement object")

    version = document.get(_VERSION_KEY)
    if version != LAYERS_VERSION:
        raise LayersError(
            f"{source.name} carries {_VERSION_KEY} {version!r}, not {LAYERS_VERSION}"
        )
    raw = document.get("layers")
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
    """Write a placement and return where it went. The root is not written.

    The root is where the file *is*, so writing it would be a second answer to
    a question the file's own location already settles.
    """
    target = Path(os.fspath(path))
    document = {
        _VERSION_KEY: LAYERS_VERSION,
        "layers": [layer.to_wire() for layer in placement.layers],
    }
    target.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    return target


def _layer(entry: Any, index: int) -> Layer:
    """One entry of `layers`, checked. `index` names it until its name is read."""
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
    """A JSON number as a float. `True` is not one, however much Python agrees."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise LayersError(f"{where} has a {field} that is not a number: {value!r}")
    return float(value)
