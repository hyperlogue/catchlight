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
  is there to catch and that [`Builder.mesh_size`] reads back as an empty
  mesh.

- **A binding's keys are param values, not cell indices.** The wire keys a
  binding by the index of its own normalized key positions. [`Builder.bind`]
  inserts positions only on that binding, then writes exact cell values in
  one guarded atomic edit. Other bindings driven by the same input keep their
  grids. A refused edit leaves no half-created binding.

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
from collections.abc import Mapping, Sequence

from .client import Client, ProtocolError
from .layers import Layer, Placement, read_layers
from .protocol_gen import (
    AutoMesh,
    BindingInfo,
    BindingList,
    BindingTarget,
    BindingCellWrite,
    BindingCellValueScalar,
    BindingCellValueOffsets,
    EditOp,
    EditOpBindingAdd,
    EditOpBindingKeyInsert,
    EditOpBindingCellsSet,
    ErrorCode,
    ResponseBodyBindings,
    ChainArg,
    Check,
    CommandNodeInfo,
    MeshAuto,
    NodeAdd,
    NodeId,
    NodeInfo,
    NodeKindArg,
    NodeSet,
    ParamAdd,
    ParamId,
    ParamInfo,
    ParamList,
    ResponseBodyNode,
    ResponseBodyNodeInfo,
    ResponseBodyParam,
    ResponseBodyParams,
    ResponseBodyWarnings,
    ResponseBodySpineFit,
    ScalarTarget,
    SessionId,
    SpineAdd,
    SpineFit,
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

    # -- spines

    def spine(
        self,
        parent: NodeId,
        joints: Sequence[tuple[float, float]] | Sequence[list[float]],
        *,
        name: str | None = None,
        targets: Sequence[ParamId | None] | None = None,
        chain: ChainArg | None = None,
        node: NodeId | None = None,
    ) -> NodeId:
        """Add a spine under `parent` and return its Id.

        `joints` is the far end of each link in the node's own space, root to
        tip, so the first one ends the link that starts at the node itself.
        `targets` names the param each link's bend is read from, in link
        order, and has to be exactly as long as `joints` — `None` where a link
        is rigid. Absent, every link is rigid. `chain` hangs a particle chain
        on the spine, whose links are the joints and whose rest shape is the
        drawing.
        """
        made = self.client.send(
            SpineAdd(
                session=self.session,
                parent=parent,
                name=name,
                joints=[[float(j[0]), float(j[1])] for j in joints],
                targets=None if targets is None else list(targets),
                chain=chain,
                node=node,
            )
        )
        if not isinstance(made, ResponseBodyNode):
            raise BuilderError(f"spine_add answered {made!r}")
        return made.node

    def fit_spine(
        self,
        part: NodeId,
        links: int,
        *,
        axis: tuple[float, float] | None = None,
        name: str | None = None,
        chain: ChainArg | None = None,
        node: NodeId | None = None,
    ) -> ResponseBodySpineFit:
        """Rig `part`'s art to a spine of `links` links, in one edit.

        The editor measures the strand, divides it into links, and authors a
        spine between the part and its parent plus a bend param per link. The
        part keeps the world placement it had. No bindings are written: a
        spine turns the art by composing its joints.

        `axis` is the direction the strand hangs in the part's own frame,
        absent being straight down. `chain` hangs a particle chain on the
        spine. Re-fitting a part that already hangs from a spine re-measures
        that spine rather than nesting another. The reply names the spine, the
        params in link order, and anything the physics cannot hold.
        """
        body = self.client.send(
            SpineFit(
                session=self.session,
                part=part,
                links=links,
                axis=axis,
                name=name,
                chain=chain,
                node=node,
            )
        )
        if not isinstance(body, ResponseBodySpineFit):
            raise BuilderError(f"spine_fit answered {body!r}")
        return body

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
        target: ScalarTarget,
        keys: Sequence[tuple[float, float]] = (),
    ) -> None:
        """Author scalar keys in one atomic edit on this binding's own grid.

        Key positions are values in the input parameter's range. Existing
        cells are preserved; near-equal positions share a cell and the last
        supplied value wins. Sibling bindings never change their grids.
        """
        info = self._param(param)
        captured = self.client.require_revision(self.session)
        target = BindingTarget(target)
        binding = self._binding(param, node, target)
        if self.client.revision(self.session) != captured:
            raise ProtocolError(
                ErrorCode.REVISION_CONFLICT,
                "model changed between parameter and binding reads; read again",
            )
        axis = list(binding.key_positions[0]) if binding else [0.0, 1.0]
        positions = [self._normalize(info, at) for at, _ in keys]
        edits: list[EditOp] = []
        if binding is None:
            edits.append(EditOpBindingAdd(
                node=node, param=param, target=target, key_positions=[axis.copy()],
            ))
        for position in positions:
            if not any(abs(position - held) <= _KEY_EPSILON for held in axis):
                edits.append(EditOpBindingKeyInsert(
                    node=node, param=param, target=target, axis=param, value=position,
                ))
                axis.append(position)
                axis.sort()
        # Duplicate writes are rejected by the exact-cell primitive, so fold
        # caller duplicates before composing the one atomic operation.
        values = {
            self._cell(axis, position): value
            for position, (_, value) in zip(positions, keys, strict=True)
        }
        if values:
            edits.append(EditOpBindingCellsSet(
                node=node, param=param, target=target,
                cells=[BindingCellWrite(
                    cell=(index, 0), value=BindingCellValueScalar(scalar=value),
                ) for index, value in values.items()],
            ))
        if edits:
            self.client.apply(self.session, edits, if_rev=captured)

    def bind_deform(
        self,
        param: ParamId,
        node: NodeId,
        cells: Mapping[tuple[int, int] | int, Sequence[tuple[float, float]]],
        param_y: ParamId | None = None,
        *,
        key_positions: Sequence[Sequence[float]] | None = None,
    ) -> None:
        """Create or update a deform binding and exact cells atomically.

        `key_positions` belongs to this binding: one normalized axis per
        driving input. A new binding defaults to endpoints [0, 1]. On an
        existing binding, omitted positions preserve its grid; supplied
        positions must match it. Use binding-key operations to reshape it.

        Cell coordinates index that grid. Values are offsets from rest, one
        `(dx, dy)` per vertex; an explicit zero cell is authored rest, while
        omitted cells remain untouched or un-authored.
        """
        binding = self._binding(param, node, BindingTarget.DEFORM, param_y)
        captured = self.client.require_revision(self.session)
        positions = None if key_positions is None else [list(axis) for axis in key_positions]
        if binding is not None and positions is not None:
            if len(positions) != len(binding.key_positions) or any(
                len(wanted) != len(held) or any(
                    abs(a - b) > _KEY_EPSILON for a, b in zip(wanted, held)
                ) for wanted, held in zip(positions, binding.key_positions)
            ):
                raise BuilderError("binding grid already exists; use binding-key operations to reshape it")
        edits: list[EditOp] = []
        if binding is None:
            edits.append(EditOpBindingAdd(
                node=node, param=param, param_y=param_y,
                target=BindingTarget.DEFORM,
                key_positions=positions if positions is not None else [[0.0, 1.0] for _ in range(1 if param_y is None else 2)],
            ))
        values = {
            (cell, 0) if isinstance(cell, int) else cell: list(map(tuple, offsets))
            for cell, offsets in cells.items()
        }
        if values:
            edits.append(EditOpBindingCellsSet(
                node=node, param=param, param_y=param_y, target=BindingTarget.DEFORM,
                cells=[BindingCellWrite(
                    cell=at, value=BindingCellValueOffsets(offsets=offsets),
                ) for at, offsets in values.items()],
            ))
        if edits:
            self.client.apply(self.session, edits, if_rev=captured)

    def _binding(
        self, param: ParamId, node: NodeId, target: BindingTarget,
        param_y: ParamId | None = None,
    ) -> BindingInfo | None:
        body = self.client.send(BindingList(session=self.session, node=node))
        if not isinstance(body, ResponseBodyBindings):
            raise BuilderError(f"binding_list answered {body!r}")
        return next((binding for binding in body.bindings if
            (binding.param, binding.param_y, binding.target) == (param, param_y, target)
        ), None)

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
    def _cell(axis: Sequence[float], position: float) -> int:
        for index, held in enumerate(axis):
            if abs(position - held) <= _KEY_EPSILON:
                return index
        raise BuilderError(f"binding grid has no key at {position}")

    # -- reading and writing

    def info(self, node: NodeId) -> NodeInfo:
        """Everything the editor reports about one node."""
        body = self.client.send(CommandNodeInfo(session=self.session, node=node))
        if not isinstance(body, ResponseBodyNodeInfo):
            raise BuilderError(f"node_info answered {body!r}")
        return body.node

    def mesh_size(self, node: NodeId) -> tuple[int, int]:
        """How many vertices and triangles `node`'s mesh holds.

        A part is meshed when this is not `(0, 0)`, which is the question
        `check`'s "textured but its mesh has no triangles" lint answers in
        prose. A node of a kind that holds no mesh at all — a group, a
        composite, a physics driver — raises, because the answer for one of
        those is not a count.
        """
        info = self.info(node)
        if info.vertex_count is None or info.triangle_count is None:
            raise BuilderError(f"node {node!r} is a {info.kind}, which holds no mesh")
        return info.vertex_count, info.triangle_count

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
