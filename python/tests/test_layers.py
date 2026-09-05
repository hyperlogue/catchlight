"""The placement file, read and written.

The round trip is the contract with the pipeline step upstream: whatever it
writes with `write_layers` is what `read_layers` hands the builder, mesh modes
and all. Everything else here is a way for that step to be wrong, and the check
is always that the message names the layer to go and look at.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from catchlight import (
    AutoMeshGrid,
    Layer,
    LayersError,
    Placement,
    read_layers,
    write_layers,
)
from support import write_png


def _placement(root: Path) -> Placement:
    write_png(root / "face.png")
    write_png(root / "hair.png")
    return Placement(
        root=root,
        layers=(
            Layer(name="Head", z=1.0),
            Layer(
                name="Face",
                file="face.png",
                parent="Head",
                z=2.0,
                offset=(0.25, -0.5),
                opacity=0.75,
                mesh=AutoMeshGrid(cols=4, rows=3),
            ),
            Layer(name="Hair", file="hair.png", parent="Head", z=3.0),
        ),
    )


def test_a_placement_survives_being_written_and_read(tmp_path: Path) -> None:
    written = _placement(tmp_path)
    where = write_layers(written, tmp_path / "layers.json")

    read = read_layers(where)
    assert read.layers == written.layers
    assert read.root == tmp_path
    assert read.path_of(read.layers[1]) == tmp_path / "face.png"


def test_a_layer_with_no_file_is_a_group_and_has_no_path(tmp_path: Path) -> None:
    read = read_layers(write_layers(_placement(tmp_path), tmp_path / "layers.json"))
    head = read.layers[0]
    assert head.is_group
    with pytest.raises(LayersError) as raised:
        read.path_of(head)
    assert "'Head'" in str(raised.value)


def test_defaults_are_left_out_and_read_back_in(tmp_path: Path) -> None:
    """A layer that took every default writes two keys, and reads as itself."""
    write_png(tmp_path / "hair.png")
    plain = Layer(name="Hair", file="hair.png")
    where = write_layers(Placement(root=tmp_path, layers=(plain,)), tmp_path / "l.json")

    placement = json.loads(where.read_text())
    assert placement["layers"] == [{"name": "Hair", "z": 0.0, "file": "hair.png"}]
    assert read_layers(where).layers == (plain,)


def _write(tmp_path: Path, *layers: dict[str, object], version: object = 1) -> Path:
    where = tmp_path / "layers.json"
    where.write_text(json.dumps({"catchlight_layers": version, "layers": list(layers)}))
    return where


def test_a_placement_of_another_version_is_refused(tmp_path: Path) -> None:
    with pytest.raises(LayersError, match="catchlight_layers 2"):
        read_layers(_write(tmp_path, {"name": "Head"}, version=2))
    with pytest.raises(LayersError, match="catchlight_layers None"):
        read_layers(_write(tmp_path, {"name": "Head"}, version=None))


def test_a_name_declared_twice_is_refused(tmp_path: Path) -> None:
    with pytest.raises(LayersError, match="'Head' is declared twice"):
        read_layers(_write(tmp_path, {"name": "Head"}, {"name": "Head"}))


def test_a_parent_declared_after_its_child_is_refused(tmp_path: Path) -> None:
    with pytest.raises(LayersError) as raised:
        read_layers(_write(tmp_path, {"name": "Face", "parent": "Head"}, {"name": "Head"}))
    assert "'Face'" in str(raised.value) and "'Head'" in str(raised.value)


def test_a_file_that_is_not_there_is_refused_by_name(tmp_path: Path) -> None:
    with pytest.raises(LayersError) as raised:
        read_layers(_write(tmp_path, {"name": "Face", "file": "face.png"}))
    assert "'Face'" in str(raised.value) and "face.png" in str(raised.value)


def test_a_file_outside_the_placement_is_refused(tmp_path: Path) -> None:
    """A placement travels with its directory, so it may not name a path off it."""
    for escape in ("/etc/passwd", "../face.png"):
        with pytest.raises(LayersError, match="not below the placement"):
            read_layers(_write(tmp_path, {"name": "Face", "file": escape}))


def test_a_layer_with_no_name_is_refused_by_its_position(tmp_path: Path) -> None:
    with pytest.raises(LayersError, match="layer 1 has no name"):
        read_layers(_write(tmp_path, {"name": "Head"}, {"z": 1}))


def test_a_field_that_is_not_a_number_is_refused_by_name(tmp_path: Path) -> None:
    with pytest.raises(LayersError, match="'Head' has a z that is not a number"):
        read_layers(_write(tmp_path, {"name": "Head", "z": "front"}))
    with pytest.raises(LayersError, match="'Head' has an offset"):
        read_layers(_write(tmp_path, {"name": "Head", "offset": [1.0]}))
    with pytest.raises(LayersError, match="'Head' has a opacity"):
        read_layers(_write(tmp_path, {"name": "Head", "opacity": True}))


def test_a_mesh_mode_the_editor_does_not_know_is_refused_by_name(tmp_path: Path) -> None:
    with pytest.raises(LayersError) as raised:
        read_layers(_write(tmp_path, {"name": "Head", "mesh": {"mode": "voronoi"}}))
    assert "'Head'" in str(raised.value) and "voronoi" in str(raised.value)


def test_something_that_is_not_a_placement_is_refused(tmp_path: Path) -> None:
    where = tmp_path / "layers.json"
    where.write_text("not json at all")
    with pytest.raises(LayersError, match="is not JSON"):
        read_layers(where)
    where.write_text("[]")
    with pytest.raises(LayersError, match="is not a placement object"):
        read_layers(where)
    where.write_text('{"catchlight_layers": 1}')
    with pytest.raises(LayersError, match="no layers list"):
        read_layers(where)
