"""The builder against a real editor, graded by the editor's own reader.

Every assertion here is made on a *reopened* file rather than on the session
that wrote it. A builder that only ever agreed with itself would pass a suite
that never left the session, so each test saves, opens the saved file, and asks
the editor what it reads back.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    AutoMeshGrid,
    BindingList,
    Builder,
    BuilderError,
    Check,
    Client,
    CommandNodeInfo,
    Layer,
    LayersError,
    NodeAdd,
    NodeKindArg,
    NodeTree,
    ParamList,
    Placement,
    SessionId,
    build_from_layers,
    write_layers,
)
from catchlight.protocol_gen import (
    NodeInfo,
    ResponseBodyBindings,
    ResponseBodyNode,
    ResponseBodyNodeInfo,
    ResponseBodyParams,
    ResponseBodyTree,
    ResponseBodyWarnings,
    TreeNode,
)
from support import write_png


def _tree(client: Client, session: SessionId) -> TreeNode:
    body = client.send(NodeTree(session=session))
    assert isinstance(body, ResponseBodyTree)
    return body.root


def _info(client: Client, session: SessionId, node: str) -> NodeInfo:
    body = client.send(CommandNodeInfo(session=session, node=node))
    assert isinstance(body, ResponseBodyNodeInfo)
    return body.node


def _warnings(client: Client, session: SessionId) -> list[str]:
    body = client.send(Check(session=session))
    assert isinstance(body, ResponseBodyWarnings)
    return body.warnings


def test_a_built_model_reopens_as_what_was_built(client: Client, tmp_path: Path) -> None:
    body = write_png(tmp_path / "body.png")
    hair = write_png(tmp_path / "hair.png")

    built = Builder.new(client, "Doll")
    head = built.group("Head", z=1.0)
    back = built.part("Body", body, parent=head, z=1.0, offset=(0.0, -0.25))
    front = built.part(
        "Hair", hair, parent=head, z=5.0, opacity=0.5, mesh=AutoMeshGrid(cols=3, rows=2)
    )
    assert head.startswith("root/") and back.startswith(head + "/")
    assert built.revision is not None

    tilt = built.param("Tilt", min=-1.0, max=1.0, default=0.0)
    built.bind(tilt, front, "ty", [(-1.0, -0.2), (0.0, 0.0), (1.0, 0.2)])
    assert built.check() == []

    saved = built.save_to(tmp_path / "doll.clm")
    reopened = client.open(saved)

    (group,) = _tree(client, reopened).children
    assert group.name == "Head" and group.kind == "group" and group.z_order == 1.0
    assert [(c.name, c.z_order) for c in group.children] == [("Body", 1.0), ("Hair", 5.0)]

    # Every part carries a mesh: the editor's own lint is what says so, and it
    # is the same lint that fires below on a part that was never meshed.
    assert _warnings(client, reopened) == []

    behind = _info(client, reopened, back)
    assert behind.translate == (0.0, -0.25, 0.0) and behind.opacity == 1.0
    assert behind.texture is not None
    assert _info(client, reopened, front).opacity == 0.5

    params = client.send(ParamList(session=reopened))
    assert isinstance(params, ResponseBodyParams)
    (param,) = params.params
    assert (param.id, param.name) == (tilt, "Tilt")
    assert (param.min, param.max, param.default) == (-1.0, 1.0, 0.0)
    assert param.key_positions == [0.0, 0.5, 1.0]
    assert param.bindings == 1

    bindings = client.send(BindingList(session=reopened, node=front))
    assert isinstance(bindings, ResponseBodyBindings)
    (binding,) = bindings.bindings
    assert (binding.target, binding.param, binding.param_y) == ("ty", tilt, None)
    assert (binding.width, binding.height) == (3, 1)
    assert binding.keys[0] == pytest.approx([-0.2, 0.0, 0.2])
    assert binding.authored == [[True, True, True]]


def test_the_lint_that_stands_for_a_mesh_fires_on_a_part_without_one(
    client: Client, tmp_path: Path
) -> None:
    """What the empty warning list above is worth.

    `check` is the editor's prose about a half-built part, and a caller that
    reads it has to see it fire for a part the builder did not mesh. The count
    behind the lint is asserted separately, on the node itself.
    """
    session = client.new()
    body = client.send(
        NodeAdd(session=session, parent="root", kind=NodeKindArg.PART, name="Bare")
    )
    assert isinstance(body, ResponseBodyNode)
    node = body.node
    client.add_texture(session, node, write_png(tmp_path / "bare.png"))
    assert any("no triangles" in warning for warning in _warnings(client, session))

    Builder(client, session).mesh(node)
    assert _warnings(client, session) == []


def test_a_part_reports_the_mesh_it_was_built_with(
    client: Client, tmp_path: Path
) -> None:
    """The count behind the lint, read off the node rather than off `check`."""
    built = Builder.new(client)
    part = built.part("Body", write_png(tmp_path / "body.png"))
    verts, tris = built.mesh_size(part)
    assert verts > 0 and tris > 0

    # A part nobody meshed carries an empty mesh, which is the state the
    # "no triangles" lint is about — and the same read says so.
    body = client.send(
        NodeAdd(
            session=built.session, parent="root", kind=NodeKindArg.PART, name="Bare"
        )
    )
    assert isinstance(body, ResponseBodyNode)
    assert built.mesh_size(body.node) == (0, 0)

    # A group holds no mesh at all, which is not a count of zero.
    group = built.group("Head")
    with pytest.raises(BuilderError, match="no mesh"):
        built.mesh_size(group)
    assert built.info(group).vertex_count is None


def test_a_binding_with_no_keys_is_still_a_binding(client: Client, tmp_path: Path) -> None:
    built = Builder.new(client)
    part = built.part("Body", write_png(tmp_path / "body.png"))
    param = built.param("Wink", min=0.0, max=1.0, default=0.0)
    built.bind(param, part, "opacity")

    bindings = built.client.send(BindingList(session=built.session, node=part))
    assert isinstance(bindings, ResponseBodyBindings)
    (binding,) = bindings.bindings
    assert binding.target == "opacity"
    assert binding.authored == [[False, False]]


def test_a_key_outside_the_params_range_names_the_param(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client)
    part = built.part("Body", write_png(tmp_path / "body.png"))
    param = built.param("Tilt", min=-1.0, max=1.0, default=0.0)
    with pytest.raises(BuilderError) as raised:
        built.bind(param, part, "ty", [(2.0, 0.5)])
    assert param in str(raised.value) and "does not reach 2.0" in str(raised.value)


def _placement(root: Path) -> Placement:
    for name in ("body.png", "hair.png", "face.png"):
        write_png(root / name)
    return Placement(
        root=root,
        layers=(
            Layer(name="Head", z=1.0),
            Layer(name="Body", file="body.png", z=0.0, offset=(0.0, -0.5)),
            Layer(name="Face", file="face.png", parent="Head", z=2.0, opacity=0.5),
            Layer(
                name="Hair",
                file="hair.png",
                parent="Head",
                z=3.0,
                mesh=AutoMeshGrid(cols=3, rows=2),
            ),
        ),
    )


def test_a_placement_becomes_a_model_the_editor_reads_back(
    client: Client, tmp_path: Path
) -> None:
    where = write_layers(_placement(tmp_path), tmp_path / "layers.json")

    built = build_from_layers(client, where, name="From layers")
    assert built.problems == []

    reopened = client.open(built.save_to(tmp_path / "from-layers.clm"))
    root = _tree(client, reopened)
    assert [(c.name, c.kind, c.z_order) for c in root.children] == [
        ("Head", "group", 1.0),
        ("Body", "part", 0.0),
    ]
    head, _ = root.children
    assert [(c.name, c.z_order) for c in head.children] == [("Face", 2.0), ("Hair", 3.0)]
    assert _warnings(client, reopened) == []


def test_a_placement_can_be_built_from_the_object_as_well_as_the_file(
    client: Client, tmp_path: Path
) -> None:
    built = build_from_layers(client, _placement(tmp_path))
    assert [c.name for c in _tree(client, built.session).children] == ["Head", "Body"]

    # `save` writes into the editor's store, not here, and answers with the key
    # rather than a path — the fixture's store is this test's directory, which
    # is the only reason the two can be compared at all.
    assert (tmp_path / built.save("from-layers.clm")).is_file()


def test_a_placement_whose_image_is_missing_fails_naming_the_layer(
    client: Client, tmp_path: Path
) -> None:
    where = write_layers(_placement(tmp_path), tmp_path / "layers.json")
    (tmp_path / "hair.png").unlink()

    with pytest.raises(LayersError) as raised:
        build_from_layers(client, where)
    assert "'Hair'" in str(raised.value) and "hair.png" in str(raised.value)
