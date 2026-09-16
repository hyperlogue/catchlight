"""Rigging a strand of hair to a spine, through the editor.

`fit_spine` is one command that authors two kinds of thing at once — the bend
params and the spine node between the part and its parent — so the test that
matters is the one that reads both back out of a *reopened* file rather than
out of the session that wrote them. Everything here follows `test_builder.py`
in that: build, save, open, ask.

It authors no bindings, which is the point of the kind: a spine composes its
joints itself, so there is nothing to key.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    BindingList,
    Builder,
    Check,
    ChainArg,
    Client,
    CommandNodeInfo,
    LinkFeelArg,
    ParamList,
    SessionId,
)
from catchlight.protocol_gen import (
    NodeInfo,
    ResponseBodyBindings,
    ResponseBodyNodeInfo,
    ResponseBodyParams,
    ResponseBodyWarnings,
)
from support import write_png


def _info(client: Client, session: SessionId, node: str) -> NodeInfo:
    body = client.send(CommandNodeInfo(session=session, node=node))
    assert isinstance(body, ResponseBodyNodeInfo)
    return body.node


def _params(client: Client, session: SessionId) -> ResponseBodyParams:
    body = client.send(ParamList(session=session))
    assert isinstance(body, ResponseBodyParams)
    return body


def _bindings(client: Client, session: SessionId, node: str) -> ResponseBodyBindings:
    body = client.send(BindingList(session=session, node=node))
    assert isinstance(body, ResponseBodyBindings)
    return body


def _warnings(client: Client, session: SessionId) -> list[str]:
    body = client.send(Check(session=session))
    assert isinstance(body, ResponseBodyWarnings)
    return body.warnings


def _strand(tmp_path: Path) -> Path:
    """A tall, narrow image: art a strand can be fitted to."""
    return write_png(tmp_path / "hair.png", width=16, height=64)


def test_a_fitted_spine_reopens_as_what_was_fitted(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client, "Doll")
    part = built.part("Hair", _strand(tmp_path))
    parent_before = _info(client, built.session, part).parent

    fit = built.fit_spine(part, 3, chain=ChainArg())
    assert len(fit.params) == 3
    assert fit.warnings == [], "a strand drawn along gravity rests as drawn"
    assert built.check() == [], "a spine reading its links is not a lint"

    saved = built.save_to(tmp_path / "doll.clm")
    reopened = client.open(saved)

    # Three input params, named after the part, with half-turn ranges.
    params = _params(client, reopened).params
    assert [p.id for p in params] == fit.params
    assert [p.name for p in params] == ["Hair bend 1", "Hair bend 2", "Hair bend 3"]
    for param in params:
        assert (param.min, param.max, param.default) == (-1.0, 1.0, 0.0)
        assert param.bindings == 0, "a spine keys nothing"

    # No bindings at all on the part.
    assert _bindings(client, reopened, part).bindings == []

    # The spine, between the part and the parent the part used to have.
    spine = _info(client, reopened, fit.node)
    assert spine.kind == "spine"
    assert spine.name == "Hair spine"
    assert spine.parent == parent_before
    assert _info(client, reopened, part).parent == fit.node
    assert spine.spine is not None
    assert spine.spine.targets == fit.params
    assert len(spine.spine.joints) == 3
    assert spine.spine.chain is not None
    assert spine.spine.chain.links is not None
    assert len(spine.spine.chain.links) == 3


def test_a_refit_reuses_the_spine_and_keeps_its_params(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client)
    part = built.part("Hair", _strand(tmp_path))
    first = built.fit_spine(part, 3)

    again = built.fit_spine(part, 3)
    assert again.node == first.node, "a re-fit reuses the spine, it does not nest"
    assert again.params == first.params
    assert len(_params(built.client, built.session).params) == 3, "no second set"


def test_a_spine_added_by_hand_reads_the_params_it_names(
    client: Client, tmp_path: Path
) -> None:
    """`spine` is the plain half: the joints, their targets and a chain."""
    built = Builder.new(client)
    sway = built.param("Sway", min=-1.0, max=1.0)
    node = built.spine(
        "root",
        [(0.0, -40.0), (0.0, -70.0)],
        name="Ponytail",
        targets=[sway, None],
        chain=ChainArg(
            gravity=4.5,
            weight=0.5,
            links=[LinkFeelArg(stiffness=2.0), LinkFeelArg(damping=0.25)],
        ),
    )

    held = built.info(node).spine
    assert held is not None
    assert held.targets == [sway, None]
    assert held.chain is not None
    assert held.chain.gravity == pytest.approx(4.5)
    assert held.chain.weight == pytest.approx(0.5)
    assert held.chain.links is not None
    assert held.chain.links[0].stiffness == pytest.approx(2.0)
    assert held.chain.links[1].damping == pytest.approx(0.25)

    # An add may name the Id it makes, as every other add here may.
    plain = built.spine("root", [(0.0, -20.0)], node="root/tail")
    assert plain == "root/tail"
