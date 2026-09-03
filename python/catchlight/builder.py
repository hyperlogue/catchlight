"""Authoring a model a step at a time, over one editing session.

A [`Builder`] is the imperative shape of the protocol: a script says "a group
here, a part with this image under it, a param, a binding" and gets back the
Ids the editor minted, in the order it made them. It is a convenience over
[`Client`](catchlight.client.Client) and nothing more — every method is one or
a few commands, and a script that needs a command this class does not wrap
sends it through `builder.client` without leaving the session.

Invariants this module enforces:

- **The editor names things, and the builder hands the name back.** Every add
  returns the Id from the reply rather than one composed here, because the
  editor is what mints a free Id and what refuses a taken one. A script that
  wants to choose an Id passes it, and gets the same Id back.

- **A part is complete when `part` returns.** The node, its texture, its place
  and its mesh are four commands and this makes all four, so a model built here
  never has the half-built part — textured, unmeshed — that [`Builder.check`]
  is there to catch.

- **A binding's keys are param values, not cell indices.** The wire keys a
  binding by the index of a param's key position, and the position a script
  means is the param value it read off a reference. So [`Builder.bind`] inserts
  the key positions the values need and then indexes them, and a caller never
  computes a cell.

- **`check` reports and never raises.** The editor's own lints are what a model
  is graded against, and every one of them is a state the model can be in —
  a part still to be meshed is a step not taken, not an error. So they come
  back as a list, and [`build_from_layers`] leaves them on
  [`Builder.problems`] rather than throwing away a model over them.

- **Nothing here validates a model.** This package links no Rust. The editor
  refuses what it refuses, and the answer to "is this a model" is that the
  editor's own reader opened it.
"""

from __future__ import annotations

import os
from collections.abc import Sequence

from .client import Client, ProtocolError
from .layers import Layer, Placement, read_layers
from .protocol_gen import (
    AutoMesh,
    BindingAdd,
    BindingKey,
    Check,
    MeshAuto,
    NodeAdd,
    NodeId,
    NodeKindArg,
    NodeSet,
    ParamAdd,
    ParamId,
    ParamInfo,
    ParamKeyInsert,
    ParamList,
    ResponseBodyNode,
    ResponseBodyParam,
    ResponseBodyParams,
    ResponseBodyWarnings,
    SessionId,
)

__all__ = ["Builder", "BuilderError", "build_from_layers"]

# How close two normalized key positions have to be to be the same one. The
# wire carries `f32`, so a position sent as 0.3 comes back a little else.
_KEY_EPSILON = 1e-5


class BuilderError(RuntimeError):
    """The script asked for something the builder cannot turn into commands."""


class Builder:
    """One session, built up one call at a time."""

    def __init__(self, client: Client, session: SessionId) -> None:
        self.client = client
        self.session = session
        # What the last `check` found. `build_from_layers` leaves its own here.
        self.problems: list[str] = []

    @classmethod
    def new(cls, client: Client, name: str | None = None) -> Builder:
        """Create a session and build in it. `name` is a title only."""
        return cls(client, client.new(name))

    @property
    def revision(self) -> int | None:
        """The revision this session's last reply reported."""
        return self.client.revision(self.session)

    # -- nodes

    def group(
        self,
        name: str,
        parent: NodeId = "root",
        z: float | None = None,
        *,
        node: NodeId | None = None,
    ) -> NodeId:
        """Add a group under `parent` and return its Id."""
        made = self._add(name, NodeKindArg.GROUP, parent, node)
        if z is not None:
            self.client.send(NodeSet(session=self.session, node=made, z_order=z))
        return made

    def part(
        self,
        name: str,
        image: str | os.PathLike[str],
        *,
        parent: NodeId = "root",
        z: float | None = None,
        offset: tuple[float, float] | None = None,
        opacity: float | None = None,
        mesh: AutoMesh | None = None,
        node: NodeId | None = None,
    ) -> NodeId:
        """Add a textured, placed, meshed part under `parent` and return its Id.

        `mesh` absent is the editor's default trace of the image's own alpha.
        """
        made = self._add(name, NodeKindArg.PART, parent, node)
        self.client.add_texture(self.session, made, image)
        if z is not None or offset is not None or opacity is not None:
            self.client.send(
                NodeSet(
                    session=self.session,
                    node=made,
                    z_order=z,
                    translate=None if offset is None else (offset[0], offset[1], 0.0),
                    opacity=opacity,
                )
            )
        self.mesh(made, mesh)
        return made

    def mesh(self, node: NodeId, mode: AutoMesh | None = None) -> None:
        """Derive `node`'s mesh from its texture's alpha, replacing what it had."""
        self.client.send(MeshAuto(session=self.session, node=node, mode=mode))

    def _add(
        self, name: str, kind: NodeKindArg, parent: NodeId, node: NodeId | None
    ) -> NodeId:
        body = self.client.send(
            NodeAdd(
                session=self.session, parent=parent, kind=kind, name=name, node=node
            )
        )
        if not isinstance(body, ResponseBodyNode):
            raise BuilderError(f"node_add answered {body!r}")
        return body.node

    # -- params and bindings

    def param(
        self,
        name: str,
        *,
        min: float = 0.0,
        max: float = 1.0,
        default: float = 0.0,
        param: ParamId | None = None,
    ) -> ParamId:
        """Add a scalar param over `[min, max]` and return its Id."""
        body = self.client.send(
            ParamAdd(
                session=self.session,
                name=name,
                min=min,
                max=max,
                default=default,
                param=param,
            )
        )
        if not isinstance(body, ResponseBodyParam):
            raise BuilderError(f"param_add answered {body!r}")
        return body.param

    def bind(
        self,
        param: ParamId,
        node: NodeId,
        target: str,
        keys: Sequence[tuple[float, float]] = (),
    ) -> None:
        """Make `param` drive `target` on `node`, keyed at `keys`.

        `target` is the property's wire name: `tx`, `ty`, `sx`, `sy`, `rx`,
        `ry`, `rz`, `z_order`, `opacity`, `tint{r,g,b}`, `screentint{r,g,b}`
        or `outputscale{x,y}`.

        Each key is a param value and the value the target takes there. The
        param values are in the param's own range and this inserts the key
        positions they need, so two keys closer together than the wire's `f32`
        can tell apart are one key, and the later one wins.
        """
        if not keys:
            self.client.send(
                BindingAdd(session=self.session, param=param, node=node, target=target)
            )
            return

        info = self._param(param)
        positions = [self._normalize(info, at) for at, _ in keys]
        for position in positions:
            near = (abs(position - held) <= _KEY_EPSILON for held in info.key_positions)
            if not any(near):
                self.client.send(
                    ParamKeyInsert(session=self.session, param=param, value=position)
                )
                info = self._param(param)
        for position, (_, value) in zip(positions, keys, strict=True):
            self.client.send(
                BindingKey(
                    session=self.session,
                    param=param,
                    node=node,
                    target=target,
                    cell=(self._cell(info, position), 0),
                    value=value,
                )
            )

    def _param(self, param: ParamId) -> ParamInfo:
        body = self.client.send(ParamList(session=self.session))
        if not isinstance(body, ResponseBodyParams):
            raise BuilderError(f"param_list answered {body!r}")
        for info in body.params:
            if info.id == param:
                return info
        raise BuilderError(f"this session has no param {param!r}")

    @staticmethod
    def _normalize(info: ParamInfo, value: float) -> float:
        """A param value as the 0..1 position the wire keys by."""
        span = info.max - info.min
        if span == 0.0:
            raise BuilderError(
                f"param {info.id!r} has an empty range, so it has one key"
            )
        position = (value - info.min) / span
        if not 0.0 <= position <= 1.0:
            raise BuilderError(
                f"param {info.id!r} is {info.min}..{info.max}, "
                f"which does not reach {value}"
            )
        return position

    @staticmethod
    def _cell(info: ParamInfo, position: float) -> int:
        """The index of the key position at `position`, which must be there."""
        for index, held in enumerate(info.key_positions):
            if abs(position - held) <= _KEY_EPSILON:
                return index
        raise BuilderError(
            f"param {info.id!r} has no key position at {position}, "
            f"only {info.key_positions}"
        )

    # -- reading and writing

    def check(self) -> list[str]:
        """The editor's own lints on this model, and what `problems` becomes."""
        body = self.client.send(Check(session=self.session))
        if not isinstance(body, ResponseBodyWarnings):
            raise BuilderError(f"check answered {body!r}")
        self.problems = list(body.warnings)
        return self.problems

    def save(self, key: str | None = None) -> str:
        """Write into the editor's store and return the key it wrote."""
        return self.client.save(self.session, key)

    def save_to(self, path: str | os.PathLike[str]) -> str:
        """Write to a local file and return its path."""
        return self.client.save_to(self.session, path)


def build_from_layers(
    client: Client,
    placement: Placement | str | os.PathLike[str],
    *,
    name: str | None = None,
) -> Builder:
    """Build a whole model from a placement, in the order it declares.

    One node per layer, parents before children because the placement already
    put them that way, each part textured, placed and meshed. It ends with a
    [`Builder.check`], whose problems are left on [`Builder.problems`] for the
    caller to read: a model with a lint is still a model, and throwing it away
    would cost the caller every part that is fine.
    """
    placed = placement if isinstance(placement, Placement) else read_layers(placement)
    builder = Builder.new(client, name)
    made: dict[str, NodeId] = {}
    for layer in placed.layers:
        parent = "root" if layer.parent is None else made[layer.parent]
        made[layer.name] = _one(builder, placed, layer, parent)
    builder.check()
    return builder


def _one(builder: Builder, placed: Placement, layer: Layer, parent: NodeId) -> NodeId:
    """One layer as the node it describes, with what the editor refused named."""
    try:
        if layer.is_group:
            return builder.group(layer.name, parent, layer.z)
        return builder.part(
            layer.name,
            placed.path_of(layer),
            parent=parent,
            z=layer.z,
            offset=layer.offset,
            opacity=layer.opacity,
            mesh=layer.mesh,
        )
    except ProtocolError as refused:
        raise BuilderError(f"layer {layer.name!r}: {refused}") from refused
