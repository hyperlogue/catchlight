"""The generated protocol module, held to the wire it describes.

`protocol_gen.py` is written by `cargo xtask generate python`, so what is worth
testing is not that the generator ran but that what it produced says the same
thing the Rust types do. The expected dicts here are written by hand, from the
protocol crate's own definitions; the same shapes are asserted against `serde`
in `crates/xtask/src/generate/python.rs`, so the two languages are anchored to
one written-down answer rather than to each other.
"""

from __future__ import annotations

import dataclasses

import pytest

from catchlight import (
    COMMAND_KINDS,
    AutoMeshGrid,
    BindingList,
    Camera,
    Command,
    CommandKind,
    ErrorCode,
    EventDocumentChanged,
    MeshAuto,
    MeshCopy,
    NodeAdd,
    NodeKindArg,
    NodeSet,
    ParamPose,
    PhysicsSet,
    PresenceSet,
    RenameId,
    RenameParam,
    ReplyErr,
    ReplyEvent,
    ReplyOk,
    ResponseBodySession,
    ResponseBodyTree,
    ScratchDeform,
    Status,
    TreeNode,
    parse_event,
    parse_reply,
)
from catchlight import protocol_gen


def command_classes() -> list[type]:
    """Every class the `Command` union is made of."""
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
    """A client picks a send method by kind, so no command may sit outside one."""
    import typing

    reached: set[str] = set()
    for kind in CommandKind:
        alias = {
            CommandKind.DOCUMENT: protocol_gen.DocumentCommand,
            CommandKind.PRESENCE: protocol_gen.PresenceCommand,
            CommandKind.SCRATCH: protocol_gen.ScratchCommand,
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
        CommandKind.DOCUMENT,
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
        CommandKind.SCRATCH,
        ScratchDeform(session=3, node="root/part-1", offsets=[0.5, -0.5]),
        {
            "cmd": "scratch_deform",
            "session": 3,
            "node": "root/part-1",
            "offsets": [0.5, -0.5],
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
        {"cmd": "status", "session": 5},
    ),
]


@pytest.mark.parametrize(("kind", "command", "expected"), ROUND_TRIPS)
def test_a_command_of_each_kind_encodes_to_the_wire(kind, command, expected) -> None:
    assert type(command).KIND is kind
    assert command.to_wire() == expected


def test_an_absent_option_is_left_off_rather_than_sent_as_null() -> None:
    """Every optional field on this wire has a serde default, so absent says
    "unchanged" and is shorter than null."""
    assert NodeAdd(session=1, parent="root", kind=NodeKindArg.GROUP).to_wire() == {
        "cmd": "node_add",
        "session": 1,
        "parent": "root",
        "kind": "group",
    }


def test_a_flattened_struct_is_flat_on_the_wire() -> None:
    """`NodeSet` carries a `NodePatch` under `#[serde(flatten)]`, and
    `BindingKey` carries its params the same way, so both are one level deep."""
    assert NodeSet(session=1, node="hair", opacity=0.5, clear_texture=True).to_wire() == {
        "cmd": "node_set",
        "session": 1,
        "node": "hair",
        "opacity": 0.5,
        "clear_texture": True,
    }


def test_a_python_keyword_still_travels_under_its_real_name() -> None:
    assert MeshCopy(session=1, from_="a", to="b").to_wire() == {
        "cmd": "mesh_copy",
        "session": 1,
        "from": "a",
        "to": "b",
    }
    assert RenameId(session=1, rename=RenameParam(from_="old", to="new")).to_wire() == {
        "cmd": "rename_id",
        "session": 1,
        "rename": {"kind": "param", "from": "old", "to": "new"},
    }


def test_a_nested_tagged_enum_carries_its_own_tag() -> None:
    assert MeshAuto(session=1, node="hair", mode=AutoMeshGrid(cols=4, rows=3)).to_wire() == {
        "cmd": "mesh_auto",
        "session": 1,
        "node": "hair",
        "mode": {"mode": "grid", "cols": 4, "rows": 3},
    }


def test_a_null_inside_a_list_survives() -> None:
    """`target_params` is `[angle, length]`, and a null entry is an output
    nothing is bound to — so dropping nulls stops at the field itself."""
    assert PhysicsSet(session=1, node="hair", target_params=[None, "len"]).to_wire() == {
        "cmd": "physics_set",
        "session": 1,
        "node": "hair",
        "target_params": [None, "len"],
        "clear_target_params": False,
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
        {"reply": "err", "id": 3, "code": "unknown_slot", "message": "seam carries no such slot"}
    )
    assert isinstance(reply, ReplyErr)
    assert reply.code is ErrorCode.UNKNOWN_SLOT
    assert reply.message == "seam carries no such slot"


def test_an_event_arrives_flattened_into_its_reply() -> None:
    """`Reply::Event` is a newtype variant of an internally tagged enum, so the
    event's own fields sit beside the `reply` tag rather than under a key."""
    line = {"reply": "event", "event": "document_changed", "session": 3, "rev": 9}
    reply = parse_reply(line)
    assert isinstance(reply, ReplyEvent)
    assert reply.event == EventDocumentChanged(session=3, rev=9)
    assert reply.to_wire() == line
    assert parse_event(line) == EventDocumentChanged(session=3, rev=9)


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
    """Frozen because a command is a message, not a builder; keyword-only
    because `session`, `node`, `seam` and `slot` are all strings and positional
    arguments would let two of them swap silently."""
    command = Status(session=1)
    with pytest.raises(dataclasses.FrozenInstanceError):
        command.session = 2  # type: ignore[misc]
    with pytest.raises(TypeError):
        Status(1)  # type: ignore[misc]
