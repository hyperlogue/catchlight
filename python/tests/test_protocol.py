"""The generated protocol module checked against hand-written wire shapes.

The same shapes are asserted against serde in the xtask generator tests.
"""

from __future__ import annotations

import dataclasses

import pytest

from catchlight import (
    CLEAR,
    COMMAND_KINDS,
    AutoMeshGrid,
    BindingList,
    Camera,
    Command,
    CommandKind,
    ErrorCode,
    EventModelChanged,
    MeshAuto,
    MeshSet,
    MotionResponseSet,
    MotionResponse,
    ResponseInput,
    ResponseSourceParent,
    NodeAdd,
    NodeKindArg,
    NodeSet,
    ParamPose,
    PresenceSet,
    RenameId,
    RenameParam,
    ReplyErr,
    ReplyEvent,
    ReplyOk,
    ResponseBodySession,
    ResponseBodyTree,
    Status,
    TreeNode,
    parse_event,
    parse_reply,
)
from catchlight import protocol_gen


def command_classes() -> list[type]:
    import typing

    return list(typing.get_args(Command))


def test_every_command_class_is_classified() -> None:
    classes = command_classes()
    assert classes, "the Command union is empty"
    tags = {cls.CMD for cls in classes}
    assert tags == set(COMMAND_KINDS), "COMMAND_KINDS and the Command union disagree"
    for cls in classes:
        assert cls.KIND is COMMAND_KINDS[cls.CMD]
        assert isinstance(cls.KIND, CommandKind)
        assert cls.TAG == cls.CMD
        assert cls.TAG_FIELD == "cmd"


def test_every_command_is_reachable_through_its_kind() -> None:
    import typing

    reached: set[str] = set()
    for kind in CommandKind:
        alias = {
            CommandKind.EDIT: protocol_gen.EditCommand,
            CommandKind.PRESENCE: protocol_gen.PresenceCommand,
            CommandKind.REVIEW: protocol_gen.ReviewCommand,
            CommandKind.VERSION: protocol_gen.VersionCommand,
            CommandKind.REPLICA_QUERY: protocol_gen.ReplicaQueryCommand,
            CommandKind.SERVER_QUERY: protocol_gen.ServerQueryCommand,
        }[kind]
        members = typing.get_args(alias) or (alias,)
        for cls in members:
            assert cls.KIND is kind
            reached.add(cls.CMD)
    assert reached == set(COMMAND_KINDS)


# One command of each kind, with the JSON the editor expects to read.
ROUND_TRIPS: list[tuple[CommandKind, object, dict]] = [
    (
        CommandKind.EDIT,
        NodeAdd(session=1, parent="root", kind=NodeKindArg.PART, name="Body"),
        {
            "cmd": "node_add",
            "session": 1,
            "parent": "root",
            "kind": "part",
            "name": "Body",
        },
    ),
    (
        CommandKind.PRESENCE,
        PresenceSet(
            session=2,
            pose=[ParamPose(param="head.x", value=0.25)],
            camera=Camera(center=(0.0, 1.0), height=2.0),
        ),
        {
            "cmd": "presence_set",
            "session": 2,
            "pose": [{"param": "head.x", "value": 0.25}],
            "camera": {"center": [0.0, 1.0], "height": 2.0},
        },
    ),
    (
        CommandKind.REPLICA_QUERY,
        BindingList(session=4, node="root/part-1"),
        {"cmd": "binding_list", "session": 4, "node": "root/part-1"},
    ),
    (
        CommandKind.SERVER_QUERY,
        Status(session=5),
        {"cmd": "session_get", "session": 5},
    ),
]


@pytest.mark.parametrize(("kind", "command", "expected"), ROUND_TRIPS)
def test_a_command_of_each_kind_encodes_to_the_wire(kind, command, expected) -> None:
    assert type(command).KIND is kind
    assert command.to_wire() == expected


def test_an_absent_option_is_left_off_rather_than_sent_as_null() -> None:
    assert NodeAdd(session=1, parent="root", kind=NodeKindArg.GROUP).to_wire() == {
        "cmd": "node_add",
        "session": 1,
        "parent": "root",
        "kind": "group",
    }


def test_a_flattened_struct_is_flat_on_the_wire() -> None:
    assert NodeSet(session=1, node="hair", opacity=0.5, texture="tex-1").to_wire() == {
        "cmd": "node_set",
        "session": 1,
        "node": "hair",
        "opacity": 0.5,
        "texture": "tex-1",
    }


def test_a_merge_patch_field_says_which_of_its_three_states_it_means() -> None:
    """`None` leaves the texture, `CLEAR` removes it, a `TexId` replaces it."""
    absent = NodeSet(session=1, node="hair").to_wire()
    assert "texture" not in absent

    assert NodeSet(session=1, node="hair", texture=CLEAR).to_wire() == {
        "cmd": "node_set",
        "session": 1,
        "node": "hair",
        "texture": None,
    }

    assert NodeSet(session=1, node="hair", texture="tex-1").to_wire() == {
        "cmd": "node_set",
        "session": 1,
        "node": "hair",
        "texture": "tex-1",
    }


def test_a_mesh_travels_as_lists_of_points() -> None:
    assert MeshSet(
        session=1,
        node="hair",
        verts=[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        uvs=[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        indices=[(0, 1, 2)],
        origin=(0.0, 0.0),
    ).to_wire() == {
        "cmd": "mesh_set",
        "session": 1,
        "node": "hair",
        "verts": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        "indices": [[0, 1, 2]],
        "origin": [0.0, 0.0],
    }


def test_a_response_carries_nested_sources() -> None:
    response = MotionResponse(
        inputs=[ResponseInput(source=ResponseSourceParent(), gain=0.75)],
        period_s=0.5,
        damping_ratio=1.0,
        output_param="bend",
    )
    assert MotionResponseSet(session=1, node="tip", response=response).to_wire() == {
        "cmd": "motion_response_set",
        "session": 1,
        "node": "tip",
        "response": {
            "inputs": [{"source": {"kind": "parent"}, "gain": 0.75}],
            "period_s": 0.5,
            "damping_ratio": 1.0,
            "output_param": "bend",
        },
    }


def test_a_python_keyword_still_travels_under_its_real_name() -> None:
    assert RenameId(session=1, rename=RenameParam(from_="old", to="new")).to_wire() == {
        "cmd": "id_rename",
        "session": 1,
        "rename": {"kind": "param", "from": "old", "to": "new"},
    }


def test_a_nested_tagged_enum_carries_its_own_tag() -> None:
    assert MeshAuto(session=1, node="hair", mode=AutoMeshGrid(cols=4, rows=3)).to_wire() == {
        "cmd": "mesh_generate",
        "session": 1,
        "node": "hair",
        "mode": {"mode": "grid", "cols": 4, "rows": 3},
    }


def test_an_ok_reply_parses() -> None:
    reply = parse_reply({"reply": "ok", "id": 7, "rev": 4, "body": {"result": "session", "session": 3}})
    assert isinstance(reply, ReplyOk)
    assert reply.id == 7
    assert reply.rev == 4
    assert reply.body == ResponseBodySession(session=3)


def test_an_ok_reply_that_names_no_session_carries_no_revision() -> None:
    reply = parse_reply({"reply": "ok", "id": 1, "body": {"result": "empty"}})
    assert isinstance(reply, ReplyOk)
    assert reply.rev is None


def test_an_err_reply_parses_to_a_code_a_client_can_branch_on() -> None:
    reply = parse_reply(
        {"reply": "err", "id": 3, "code": "unknown_slot", "message": "part carries no such slot"}
    )
    assert isinstance(reply, ReplyErr)
    assert reply.code is ErrorCode.UNKNOWN_SLOT
    assert reply.message == "part carries no such slot"


def test_an_event_arrives_flattened_into_its_reply() -> None:
    """An event's fields sit beside the `reply` tag (internally tagged enum)."""
    line = {"reply": "event", "event": "model_changed", "session": 3, "rev": 9}
    reply = parse_reply(line)
    assert isinstance(reply, ReplyEvent)
    assert reply.event == EventModelChanged(session=3, rev=9)
    assert reply.to_wire() == line
    assert parse_event(line) == EventModelChanged(session=3, rev=9)


def test_a_tree_reply_decodes_all_the_way_down() -> None:
    reply = parse_reply(
        {
            "reply": "ok",
            "id": 1,
            "rev": 2,
            "body": {
                "result": "tree",
                "root": {
                    "id": "root",
                    "name": "Root",
                    "kind": "group",
                    "z_order": 0.0,
                    "enabled": True,
                    "children": [
                        {
                            "id": "root/part-1",
                            "name": "Body",
                            "kind": "part",
                            "z_order": 1.0,
                            "enabled": False,
                            "children": [],
                        }
                    ],
                },
            },
        }
    )
    assert isinstance(reply, ReplyOk)
    assert isinstance(reply.body, ResponseBodyTree)
    root: TreeNode = reply.body.root
    assert [child.id for child in root.children] == ["root/part-1"]
    assert root.children[0].enabled is False


def test_a_reply_missing_a_required_field_says_which() -> None:
    with pytest.raises(ValueError, match="'body'"):
        parse_reply({"reply": "ok", "id": 1})


def test_an_unknown_tag_is_refused() -> None:
    with pytest.raises(ValueError, match="reply"):
        parse_reply({"reply": "nonsense", "id": 1})


def test_a_command_is_frozen_and_keyword_only() -> None:
    """Keyword-only because several string fields could silently swap."""
    command = Status(session=1)
    with pytest.raises(dataclasses.FrozenInstanceError):
        command.session = 2  # type: ignore[misc]
    with pytest.raises(TypeError):
        Status(1)  # type: ignore[misc]


@pytest.mark.parametrize("annotation,value,wire", [
    (protocol_gen.BindingCellValue,
     protocol_gen.BindingCellValueScalar(scalar=0.25), {"scalar": 0.25}),
    (protocol_gen.BindingCellValue,
     protocol_gen.BindingCellValueOffsets(offsets=[(2.0, -3.0)]),
     {"offsets": [[2.0, -3.0]]}),
    (protocol_gen.BindingIdentity,
     protocol_gen.BindingIdentityScalar(scalar=1.0), {"scalar": 1.0}),
    (protocol_gen.BindingIdentity,
     protocol_gen.BindingIdentityDeform(offset=(0.0, 0.0), vertex_count=12),
     {"offset": [0.0, 0.0], "vertex_count": 12}),
])
def test_exact_cell_values_and_compact_identities_round_trip(annotation, value, wire) -> None:
    assert protocol_gen._wire(value) == wire
    assert protocol_gen._decode(annotation, wire) == value


def test_exact_cell_writes_keep_payload_tags_and_holes_distinct() -> None:
    command = protocol_gen.BindingCellsSet(
        session=1, if_rev=8, node="panel", param="drive",
        target=protocol_gen.BindingTarget.DEFORM,
        cells=[protocol_gen.BindingCellWrite(
            cell=(1, 0), value=protocol_gen.BindingCellValueOffsets(offsets=[(0.0, 0.0)]),
        )],
    )
    assert command.to_wire()["cells"] == [
        {"cell": [1, 0], "value": {"offsets": [[0.0, 0.0]]}},
    ]
    wire = {
        "reply": "ok", "id": 1, "rev": 8,
        "body": {"result": "binding_cells", "node": "panel", "param": "drive",
                 "target": "deform", "width": 2, "height": 1,
                 "interpolate": "linear", "vertex_count": 1,
                 "cells": [{"cell": [0, 0], "authored": False},
                           {"cell": [1, 0], "authored": True,
                            "value": {"offsets": [[0.0, 0.0]]}}]},
    }
    reply = parse_reply(wire)
    assert reply.body.cells[0].value is None
    assert reply.body.cells[1].value == protocol_gen.BindingCellValueOffsets(offsets=[(0.0, 0.0)])
    assert reply.to_wire() == wire
