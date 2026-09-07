"""Authoring a spine through the editor, and reopening the file it wrote.

A spine is the one deforming node that carries no cells: a polyline of joints
and one bend param per link. What the test grades is that both survive a save
and a reopen under the names `spine_set` takes them by, and that a spine the
file would refuse never reaches the model.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import Builder, Client, CommandNodeInfo, ParamAdd, SessionId
from catchlight.client import ProtocolError
from catchlight.protocol_gen import NodeInfo, ResponseBodyNodeInfo, ResponseBodyParam


def _info(client: Client, session: SessionId, node: str) -> NodeInfo:
    body = client.send(CommandNodeInfo(session=session, node=node))
    assert isinstance(body, ResponseBodyNodeInfo)
    return body.node


def _param(client: Client, session: SessionId, name: str) -> str:
    body = client.send(
        ParamAdd(
            session=session,
            name=name,
            min=-1.0,
            max=1.0,
            default=0.0,
            key_positions=[],
        )
    )
    assert isinstance(body, ResponseBodyParam)
    return body.param


def test_a_spine_reopens_as_what_was_authored(client: Client, tmp_path: Path) -> None:
    built = Builder.new(client, "Doll")
    bend = _param(client, built.session, "tail bend")
    joints = [(0.0, -50.0), (8.0, -95.0)]
    node = built.spine("root", joints, targets=[bend, None])

    saved = built.save_to(tmp_path / "doll.clm")
    reopened = client.open(saved)

    info = _info(client, reopened, node)
    assert info.kind == "spine"
    assert info.chain is None, "a spine is not a particle chain"
    assert info.physics is None, "a spine is not a pendulum"
    spine = info.spine
    assert spine is not None
    assert list(spine.joints) == [(0.0, -50.0), (8.0, -95.0)]
    assert spine.targets == [bend, None]


def test_an_absent_target_list_leaves_one_rigid_slot_per_link(client: Client) -> None:
    built = Builder.new(client, "Doll")
    node = built.spine("root", [(0.0, -40.0), (0.0, -80.0)])
    spine = _info(client, built.session, node).spine
    assert spine is not None
    assert spine.targets == [None, None], "the length is the model's, not the file's"


@pytest.mark.parametrize(
    "joints",
    [
        pytest.param([], id="no joints"),
        pytest.param([(0.0, 0.0)], id="first link has no length"),
        pytest.param([(0.0, -40.0), (0.0, -40.0)], id="a repeated joint"),
    ],
)
def test_a_spine_the_file_would_refuse_is_refused(
    client: Client, joints: list[tuple[float, float]]
) -> None:
    built = Builder.new(client, "Doll")
    with pytest.raises(ProtocolError):
        built.spine("root", joints)
