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


def test_an_edit_returns_its_body_and_moves_the_revision(client: Client) -> None:
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


def test_reads_refresh_the_captured_revision_and_stale_writes_are_not_retried(
    server,
) -> None:
    from catchlight import EditOpNodeSet, UnixSocketTransport

    first = server.client()
    second = Client(UnixSocketTransport(server.socket_path))
    session = first.new()
    second.send(Status(session=session))
    first.add_part(session, name="Body")
    with pytest.raises(ProtocolError) as conflict:
        second.apply(session, [EditOpNodeSet(node="root", name="stale")])
    assert conflict.value.code is ErrorCode.REVISION_CONFLICT
    assert second.revision(session) == 0

    second.send(NodeTree(session=session))
    assert second.revision(session) == first.revision(session) == 1
    result = second.apply(session, [EditOpNodeSet(node="root", name="fresh")])
    assert result.changed
    assert second.revision(session) == 2
    second.close()


def test_forks_and_branch_navigation_keep_their_own_revisions(client: Client) -> None:
    from catchlight import EditOpNodeSet

    source = client.new()
    client.add_part(source, name="Body")
    fork = client.fork(source, name="Candidate")
    assert client.revision(source) == 1
    assert client.revision(fork) == 0
    client.apply(fork, [EditOpNodeSet(node="root", name="A")])
    a = client.revision(fork)
    client.undo(fork)
    client.apply(fork, [EditOpNodeSet(node="root", name="B")])
    history = client.history(fork)
    assert [entry.revision for entry in history.entries] == [0, 1, 3]
    client.goto(fork, a)
    assert client.revision(fork) == 4
    assert client.history(fork).current == a
    client.goto(fork, a)
    assert client.revision(fork) == 4
    assert client.revision(source) == 1


def test_atomic_failures_keep_operation_and_limit_metadata(client: Client) -> None:
    from catchlight import EditOpNodeSet

    session = client.new()
    with pytest.raises(ProtocolError) as failure:
        client.apply(session, [
            EditOpNodeSet(node="root", name="never installed"),
            EditOpNodeSet(node="missing", name="bad"),
        ])
    assert failure.value.op_index == 1
    assert failure.value.code is ErrorCode.NO_NODE
    assert client.revision(session) == 0
    assert len(client.history(session).entries) == 1

    with pytest.raises(ProtocolError) as limit:
        client.apply(session, [EditOpNodeSet(node="root", name="many")] * 1025)
    assert limit.value.code is ErrorCode.LIMIT_EXCEEDED
    assert limit.value.limit is not None
    assert limit.value.limit.limit == 1024
    assert limit.value.limit.requested == 1025


def test_validation_returns_results_without_publishing(client: Client) -> None:
    from catchlight import EditOpNodeSet

    session = client.new()
    result = client.apply(session, [EditOpNodeSet(node="root", name="Candidate")], validate=True)
    assert result.changed
    assert client.revision(session) == 0
    assert len(client.history(session).entries) == 1
