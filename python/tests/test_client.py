"""The client against a real editor: one send, routed by kind.

The round trip at the end is the point of the whole package — a model authored
from Python, written by the editor, and read back by the editor's own reader,
which is the only validation a client that never links Rust can have.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    Camera,
    Check,
    Client,
    ErrorCode,
    MeshAuto,
    NodeAdd,
    NodeKindArg,
    NodeTree,
    ParamPose,
    PresenceSet,
    ProtocolError,
    Status,
)
from catchlight.protocol_gen import (
    ResponseBodyNode,
    ResponseBodyStatus,
    ResponseBodyTree,
    TreeNode,
)
from support import write_png


def test_a_document_command_returns_its_body_and_moves_the_revision(client: Client) -> None:
    session = client.new()
    opened_at = client.revision(session)
    assert opened_at is not None

    body = client.send(
        NodeAdd(session=session, parent="root", kind=NodeKindArg.PART, name="Body")
    )
    assert isinstance(body, ResponseBodyNode)
    assert body.node.startswith("root/")
    after = client.revision(session)
    assert after is not None and after > opened_at


def test_a_query_command_leaves_the_revision_alone(client: Client) -> None:
    session = client.new()
    client.add_part(session, name="Body")
    before = client.revision(session)

    body = client.send(Status(session=session))
    assert isinstance(body, ResponseBodyStatus)
    assert body.status.node_count == 2  # root and the part
    assert client.revision(session) == before


def test_a_presence_command_is_legal_and_moves_no_revision(client: Client) -> None:
    """Presence publishes view state; a panel never repaints for it."""
    session = client.new()
    before = client.revision(session)
    client.send(
        PresenceSet(
            session=session,
            pose=[ParamPose(param="none", value=0.0)],
            camera=Camera(center=(0.0, 0.0), height=2.0),
        )
    )
    assert client.revision(session) == before


def test_a_refused_command_raises_with_the_code_to_branch_on(client: Client) -> None:
    with pytest.raises(ProtocolError) as raised:
        client.send(Status(session=9999))
    assert raised.value.code is ErrorCode.NO_SESSION

    session = client.new()
    with pytest.raises(ProtocolError) as missing_node:
        client.send(
            NodeAdd(session=session, parent="root/nope-1", kind=NodeKindArg.PART)
        )
    assert missing_node.value.code is ErrorCode.NO_NODE


def test_closing_a_session_forgets_the_revision_it_had(client: Client) -> None:
    session = client.new()
    assert client.revision(session) is not None
    client.close_session(session)
    assert client.revision(session) is None
    with pytest.raises(ProtocolError) as raised:
        client.send(Check(session=session))
    assert raised.value.code is ErrorCode.NO_SESSION


def test_a_model_authored_here_is_read_back_by_the_editors_own_reader(
    client: Client, tmp_path: Path
) -> None:
    session = client.new("Round trip")
    part = client.add_part(session, name="Body")
    texture = client.add_texture(session, part, write_png(tmp_path / "body.png"))
    assert texture

    client.send(MeshAuto(session=session, node=part))
    saved = client.save_to(session, tmp_path / "round-trip.clm")
    assert Path(saved).is_file()

    reopened = client.open(saved)
    assert reopened != session
    body = client.send(NodeTree(session=reopened))
    assert isinstance(body, ResponseBodyTree)
    assert _names(body.root) == ["Body"]


def _names(node: TreeNode) -> list[str]:
    """Every name below the root, which is named for the file rather than by
    the author."""
    return [child.name for child in node.children] + [
        name for child in node.children for name in _names(child)
    ]
