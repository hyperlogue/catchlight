"""Public synthetic recipes preserve evaluated fields, sparse authorship and guards."""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

from catchlight import Client, ProtocolError
from catchlight import protocol_gen as p

_spec = importlib.util.spec_from_file_location(
    "rig_workflow", Path(__file__).parents[1] / "examples" / "rig_workflow.py"
)
assert _spec is not None and _spec.loader is not None
recipe = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(recipe)


def test_cut_preserves_authored_holes_slots_welds_geometry_and_undo(client: Client) -> None:
    source = recipe.synthetic_model(client)
    revision = client.require_revision(source)
    before = client.export_model(source)
    reference = recipe.worlds(client, source, ["panel-a"])
    source_bindings = client.send(p.BindingList(session=source, node="panel-a")).bindings
    original_welds = client.send(p.Welds(session=source)).welds
    fork = client.fork(source, if_rev=revision)
    edits = recipe.cut_edits(client, fork, 0)
    validated = client.apply(fork, edits, if_rev=0, validate=True)
    assert validated.changed
    assert client.revision(fork) == 0
    assert client.export_model(fork) == before
    client.apply(fork, edits, if_rev=0)
    assert client.revision(fork) == 1
    assert client.export_model(source, if_rev=revision) == before
    for node in ["panel-a", "panel-b"]:
        bindings = client.send(p.BindingList(session=fork, node=node)).bindings
        assert [b.key_positions for b in bindings] == [b.key_positions for b in source_bindings]
        assert [b.interpolate for b in bindings] == [b.interpolate for b in source_bindings]
        assert all(b.authored == [[True, False, True]] for b in bindings)
    slots = client.send(p.Slots(session=fork, node="panel-a")).slots
    assert {s.id: s.vertex for s in slots} == {
        "pin": 0, "top": 3, "lower-seam": 1, "upper-seam": 2,
    }
    welds = client.send(p.Welds(session=fork)).welds
    assert welds[:1] == original_welds
    assert (welds[1].a, welds[1].b) == ("panel-a", "panel-b")
    assert [pair.weight for pair in welds[1].pairs] == [0.25, 0.75]
    for old, new in zip(reference, recipe.worlds(client, fork, ["panel-a", "panel-b"])):
        recipe.assert_close(new["panel-a"], recipe.mapped(old["panel-a"], recipe.LEFT))
        recipe.assert_close(new["panel-b"], recipe.mapped(old["panel-a"], recipe.RIGHT))
    edited = client.export_model(fork)
    client.undo(fork)
    assert client.export_model(fork) == before
    client.redo(fork)
    assert client.export_model(fork) == edited


def test_affine_assembly_keeps_local_shape_and_all_attachment_records(client: Client) -> None:
    session = recipe.synthetic_model(client)
    client.apply(session, recipe.cut_edits(client, session, client.require_revision(session)))
    before = client.export_model(session)
    reference = recipe.worlds(client, session, ["panel-a", "panel-b"])
    welds = client.send(p.Welds(session=session)).welds
    slots = {node: client.send(p.Slots(session=session, node=node)).slots
             for node in ["panel-a", "panel-b"]}
    edits = recipe.assembly_edits(client, session, client.require_revision(session))
    client.apply(session, edits)
    for old, new in zip(reference, recipe.worlds(client, session, ["panel-a", "panel-b"])):
        for node in old:
            recipe.assert_close(new[node], old[node])
    for node in slots:
        assert client.send(p.Slots(session=session, node=node)).slots == slots[node]
        bindings = client.send(p.BindingList(session=session, node=node)).bindings
        assert [b.param for b in bindings] == ["shape"]
        assert bindings[0].key_positions == [[0, 0.5, 1]]
        assert bindings[0].authored == [[True, False, True]]
    group = client.send(p.BindingList(session=session, node="placement")).bindings
    assert group[0].key_positions == [[0, 0.25, 1]]
    assert group[0].authored == [[True, False, True]]
    assert client.send(p.Welds(session=session)).welds == welds
    client.undo(session)
    assert client.export_model(session) == before


def test_reviewed_edits_replay_atomically_and_reject_changed_source(client: Client) -> None:
    source = recipe.synthetic_model(client)
    base = client.require_revision(source)
    baseline = client.export_model(source)
    candidate = client.fork(source, if_rev=base)
    cut = recipe.cut_edits(client, candidate, 0)
    client.apply(candidate, cut, if_rev=0)
    assembly = recipe.assembly_edits(client, candidate, 1)
    client.apply(candidate, assembly, if_rev=1)
    reviewed = client.export_model(candidate, if_rev=2)
    edits = cut + assembly
    with pytest.raises(ProtocolError) as failed:
        client.apply(source, edits + [p.EditOpNodeReparent(node="missing", to="root")], if_rev=base)
    assert failed.value.op_index == len(edits)
    assert client.export_model(source, if_rev=base) == baseline
    client.apply(source, edits, if_rev=base)
    assert client.revision(source) == base + 1
    assert client.export_model(source) == reviewed
    with pytest.raises(ProtocolError) as stale:
        client.apply(source, edits, if_rev=base)
    assert stale.value.code == p.ErrorCode.REVISION_CONFLICT
    assert client.export_model(source) == reviewed
    client.undo(source)
    assert client.export_model(source) == baseline


def test_assembly_refuses_independent_grid_profiles_without_mutation(client: Client) -> None:
    session = recipe.synthetic_model(client)
    client.apply(session, recipe.cut_edits(client, session, client.require_revision(session)))
    client.apply(session, [p.EditOpBindingKeyMove(
        node="panel-b", param="turn", target=p.BindingTarget.DEFORM,
        axis="turn", index=1, value=0.4,
    )])
    before = client.export_model(session)
    with pytest.raises(ValueError, match="different grids or interpolation"):
        recipe.assembly_edits(client, session, client.require_revision(session))
    assert client.export_model(session) == before
