"""Rigging a strand of hair to a particle chain, through the editor.

`fit_chain` is one command that authors three kinds of thing at once — the
bend params, the chain node, and the deform bindings under them — so the test
that matters is the one that reads all three back out of a *reopened* file
rather than out of the session that wrote them. Everything here follows
`test_builder.py` in that: build, save, open, ask.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    BindingList,
    Builder,
    ChainLinkArg,
    Check,
    Client,
    CommandNodeInfo,
    ParamList,
    SessionId,
)
from catchlight.client import ProtocolError
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


def test_a_fitted_chain_reopens_as_what_was_fitted(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client, "Doll")
    part = built.part("Hair", _strand(tmp_path))

    fit = built.fit_chain(part, 3)
    assert len(fit.params) == 3
    assert fit.bound == part, "absent `on` binds the part itself"
    assert fit.replaced == [], "nothing stood under these params before"
    assert built.check() == [], "a chain driving its links is not a lint"

    saved = built.save_to(tmp_path / "doll.clm")
    reopened = client.open(saved)

    # Three params, named after the part, keyed at the seven bends a link's
    # deform is authored at.
    params = _params(client, reopened).params
    assert [p.id for p in params] == fit.params
    assert [p.name for p in params] == [
        "Hair bend 1",
        "Hair bend 2",
        "Hair bend 3",
    ]
    for param in params:
        assert (param.min, param.max, param.default) == (-0.5, 0.5, 0.0)
        assert len(param.key_positions) == 7
        assert param.key_positions[0] == pytest.approx(0.0)
        assert param.key_positions[3] == pytest.approx(0.5)
        assert param.key_positions[6] == pytest.approx(1.0)
        assert param.bindings == 1

    # One cubic deform binding per param, on the part, every cell authored.
    bindings = _bindings(client, reopened, part).bindings
    assert [b.param for b in bindings] == fit.params
    for binding in bindings:
        assert binding.target == "deform"
        assert binding.param_y is None
        assert binding.interpolate == "cubic"
        assert (binding.width, binding.height) == (7, 1)
        assert binding.authored == [[True] * 7]

    # And the chain, hanging beside the part, driving those three params.
    chain = _info(client, reopened, fit.node)
    assert chain.kind == "particle_chain"
    assert chain.name == "Hair chain"
    assert chain.parent == _info(client, reopened, part).parent
    assert chain.chain is not None
    assert chain.chain.outputs == fit.params
    assert len(chain.chain.links) == 3
    lengths = [link.length for link in chain.chain.links]
    assert all(length == pytest.approx(lengths[0]) for length in lengths)
    assert lengths[0] > 0.0, "the links divide the art's height between them"


def test_a_refit_keeps_the_params_and_says_what_it_replaced(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client)
    part = built.part("Hair", _strand(tmp_path))
    first = built.fit_chain(part, 3)

    again = built.fit_chain(part, 3, chain=first.node)
    assert again.node == first.node
    assert again.params == first.params
    assert again.replaced == first.params, "every binding under them rewritten"
    assert len(_params(built.client, built.session).params) == 3, "no second set"


def test_a_chain_added_by_hand_drives_the_params_it_names(
    client: Client, tmp_path: Path
) -> None:
    """`chain` is the plain half: the links and their outputs, nothing else."""
    built = Builder.new(client)
    sway = built.param("Sway", min=-1.0, max=1.0)
    node = built.chain(
        "root",
        [ChainLinkArg(length=40.0), ChainLinkArg(length=30.0)],
        name="Ponytail",
        gravity=4.5,
        weight=0.5,
        outputs=[sway, None],
    )

    held = built.info(node).chain
    assert held is not None
    assert held.gravity == pytest.approx(4.5)
    assert held.weight == pytest.approx(0.5)
    assert [link.length for link in held.links] == [40.0, 30.0]
    assert held.outputs == [sway, None]

    # A count instead of a list is the same chain at the editor's defaults,
    # and an add may name the Id it makes, as every other add here may.
    plain = built.chain("root", 2, node="root/tail")
    assert plain == "root/tail"
    defaults = built.info(plain).chain
    assert defaults is not None
    # An omitted weight is a chain that decides the params it names.
    assert defaults.weight == pytest.approx(1.0)
    assert len(defaults.links) == 2
    assert defaults.outputs == [None, None]
    assert all(link.length is not None for link in defaults.links)


def test_a_chain_that_drives_nothing_is_a_lint(client: Client) -> None:
    built = Builder.new(client)
    built.chain("root", 2, name="Loose")
    assert any("drives no output param" in problem for problem in built.check())


def test_a_fit_on_a_node_with_no_mesh_is_refused(
    client: Client, tmp_path: Path
) -> None:
    built = Builder.new(client)
    part = built.part("Hair", _strand(tmp_path))
    group = built.group("Head")
    with pytest.raises(ProtocolError):
        built.fit_chain(part, 3, on=group)
    # The refused fit left nothing behind.
    assert _params(built.client, built.session).params == []
