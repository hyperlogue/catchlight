"""Imperative model authoring over one editing session.

`check` reports lints and never raises: a lint is a state the model can be
in, not an error.
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

# The wire carries `f32`, so a key position sent as 0.3 comes back slightly off.
_KEY_EPSILON = 1e-5


class BuilderError(RuntimeError):
    """The script asked for something the builder cannot turn into commands."""


class Builder:

    def __init__(self, client: Client, session: SessionId) -> None:
        self.client = client
        self.session = session
        self.problems: list[str] = []

    @classmethod
    def new(cls, client: Client, name: str | None = None) -> Builder:
        """`name` is a title only."""
        return cls(client, client.new(name))

    @property
    def revision(self) -> int | None:
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
        """`mesh` absent traces the image's alpha with the editor's default."""
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
        node: NodeId | None = None,
    ) -> NodeId:
        """`joints` are link end points in the node's own space, root to tip.

        `targets` gives each link's bend param, exactly as long as `joints`;
        `None` (or absent) means rigid.
        """
        made = self.client.send(
            SpineAdd(
                session=self.session,
                parent=parent,
                name=name,
                joints=[[float(j[0]), float(j[1])] for j in joints],
                targets=None if targets is None else list(targets),
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
        node: NodeId | None = None,
    ) -> ResponseBodySpineFit:
        """Authors a spine and one bend param per link; writes no bindings.

        `axis` is the hang direction in the part's own frame (default: down).
        Refitting preserves the existing spine and bend params.
        """
        body = self.client.send(
            SpineFit(
                session=self.session,
                part=part,
                links=links,
                axis=axis,
                name=name,
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

        Key positions are values in the param's range. Near-equal positions
        share a cell and the last value wins.
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
        axis = list(binding.key_positions[0]) if binding else list(info.initial_positions)
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
        # The exact-cell primitive rejects duplicate writes, so fold them here.
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

        `key_positions`: one normalized axis per driving input. On an existing
        binding, supplied positions must match its grid.

        Cell values are offsets from rest, one `(dx, dy)` per vertex.
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
                key_positions=positions,
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
        body = self.client.send(CommandNodeInfo(session=self.session, node=node))
        if not isinstance(body, ResponseBodyNodeInfo):
            raise BuilderError(f"node_info answered {body!r}")
        return body.node

    def mesh_size(self, node: NodeId) -> tuple[int, int]:
        """`(vertices, triangles)`; raises for kinds that hold no mesh."""
        info = self.info(node)
        if info.vertex_count is None or info.triangle_count is None:
            raise BuilderError(f"node {node!r} is a {info.kind}, which holds no mesh")
        return info.vertex_count, info.triangle_count

    def check(self) -> list[str]:
        body = self.client.send(Check(session=self.session))
        if not isinstance(body, ResponseBodyWarnings):
            raise BuilderError(f"check answered {body!r}")
        self.problems = list(body.warnings)
        return self.problems

    def save(self, key: str | None = None) -> str:
        return self.client.save(self.session, key)

    def save_to(self, path: str | os.PathLike[str]) -> str:
        return self.client.save_to(self.session, path)


def build_from_layers(
    client: Client,
    placement: Placement | str | os.PathLike[str],
    *,
    name: str | None = None,
) -> Builder:
    """Build a model from a placement; lints are left on `problems`."""
    placed = placement if isinstance(placement, Placement) else read_layers(placement)
    builder = Builder.new(client, name)
    made: dict[str, NodeId] = {}
    for layer in placed.layers:
        parent = "root" if layer.parent is None else made[layer.parent]
        made[layer.name] = _one(builder, placed, layer, parent)
    builder.check()
    return builder


def _one(builder: Builder, placed: Placement, layer: Layer, parent: NodeId) -> NodeId:
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
