"""A public synthetic rig review, composed from ordinary protocol operations.

The client chooses topology, correspondence and acceptance poses. Catchlight
preserves authored fields and owns revision guards, transactions and rendering.
Run from the repository as documented in the adjacent README.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import struct
import subprocess
import zlib

from catchlight import Client, ProtocolError, launch
from catchlight import protocol_gen as p


# A/B/C becomes A/D/E/C plus D/B/E, with D and E on the old triangle's edges.
OLD = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0)]
LEFT = [[(0, 1.0)], [(0, 0.5), (1, 0.5)], [(1, 0.5), (2, 0.5)], [(2, 1.0)]]
RIGHT = [LEFT[1], [(1, 1.0)], LEFT[2]]
POSES = [(0.0, 0.0), (0.3, 0.6), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]


def texture_png() -> bytes:
    """Generate a small opaque gradient with only the Python standard library."""
    def chunk(kind: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    pixels = b"".join(
        b"\0" + bytes(v for x in range(16) for v in (40 + x * 12, 50 + y * 11, 180, 255))
        for y in range(16)
    )
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def mapped(values, rows):
    """Client-chosen convex correspondence, used for positions, UVs and offsets."""
    return [tuple(sum(values[i][axis] * weight for i, weight in row)
                  for axis in range(len(values[0]))) for row in rows]


def cells(node: str, param: str, values) -> p.EditOpBindingCellsSet:
    return p.EditOpBindingCellsSet(
        node=node, param=param, target=p.BindingTarget.DEFORM,
        cells=[p.BindingCellWrite(cell=address, value=p.BindingCellValueOffsets(offsets=offsets))
               for address, offsets in values],
    )


def synthetic_model(client: Client) -> p.SessionId:
    """Two independent input fields and an existing slot/weld record."""
    session = client.new("Synthetic rig review")
    for param in ["shape", "turn"]:
        client.send(p.ParamAdd(session=session, param=param, name=param))
    client.apply(session, [
        p.EditOpNodeAdd(node="panel-a", parent="root", kind=p.NodeKindArg.PART),
        p.EditOpMeshSet(node="panel-a", verts=OLD, uvs=[(0, 0), (1, 0), (0, 1)],
                        indices=[(0, 1, 2)], origin=(0, 0)),
        p.EditOpNodeAdd(node="anchor", parent="root", kind=p.NodeKindArg.PART),
        p.EditOpMeshSet(node="anchor", verts=[(0, 0)], uvs=[], indices=[], origin=(0, 0)),
        p.EditOpSlotAdd(node="panel-a", slot="pin"),
        p.EditOpSlotFill(node="panel-a", slot="pin", vertex=0),
        p.EditOpSlotAdd(node="panel-a", slot="top"),
        p.EditOpSlotFill(node="panel-a", slot="top", vertex=2),
        p.EditOpSlotAdd(node="anchor", slot="pin"),
        p.EditOpSlotFill(node="anchor", slot="pin", vertex=0),
        p.EditOpWeldSet(a="panel-a", b="anchor",
                       pairs=[p.SlotPair(a="pin", b="pin", weight=0.3)]),
    ])
    client.send_with(p.TextureAdd(session=session, node="panel-a", texture="art"),
                     {"texture": texture_png()})
    for param, positions, end in [
        ("shape", [0.0, 0.5, 1.0], [(0.0, y * 0.1) for _, y in OLD]),
        ("turn", [0.0, 0.25, 1.0], [(x * 0.5, 0.0) for x, _ in OLD]),
    ]:
        client.apply(session, [
            p.EditOpBindingAdd(node="panel-a", param=param, target=p.BindingTarget.DEFORM,
                               key_positions=[positions]),
            cells("panel-a", param, [((0, 0), [(0, 0)] * 3), ((2, 0), end)]),
        ])
    return session


def cut_edits(client: Client, session: p.SessionId, revision: int) -> list[p.EditOp]:
    """Read exact authored cells, map the new Part, and replace the retained mesh."""
    mesh = client.send(p.MeshGet(session=session, node="panel-a", if_rev=revision)).mesh
    if list(mesh.verts) != OLD:
        raise ValueError("this recipe expects the synthetic triangle's vertex order")
    bindings = client.send(p.BindingList(session=session, node="panel-a")).bindings
    if client.revision(session) != revision:
        raise ValueError("binding metadata came from another revision")
    edits: list[p.EditOp] = [
        p.EditOpNodeAdd(node="panel-b", parent="root", kind=p.NodeKindArg.PART),
        p.EditOpNodeSet(node="panel-b", texture="art"),
        p.EditOpMeshSet(node="panel-b", verts=mapped(mesh.verts, RIGHT),
                        uvs=mapped(mesh.uvs, RIGHT), indices=[(0, 1, 2)], origin=mesh.origin),
    ]
    for binding in bindings:
        addresses = [(x, y) for y, row in enumerate(binding.authored)
                     for x, authored in enumerate(row) if authored]
        edits.append(p.EditOpBindingAdd(
            node="panel-b", param=binding.param, target=binding.target,
            key_positions=binding.key_positions,
        ))
        if addresses:
            read = client.send(p.BindingCellsGet(
                session=session, if_rev=revision, node="panel-a", param=binding.param,
                target=binding.target, cells=addresses,
            ))
            edits.append(cells("panel-b", binding.param,
                               [(c.cell, mapped(c.value.offsets, RIGHT)) for c in read.cells]))
        edits.append(p.EditOpBindingInterpolationSet(
            node="panel-b", param=binding.param, target=binding.target, mode=binding.interpolate,
        ))
    edits += [
        p.EditOpMeshSet(node="panel-a", verts=mapped(mesh.verts, LEFT),
                        uvs=mapped(mesh.uvs, LEFT), indices=[(0, 1, 2), (0, 2, 3)],
                        origin=mesh.origin,
                        deform_mapping=[[p.VertexWeight(vertex=i, weight=w) for i, w in row]
                                        for row in LEFT]),
        # Topology replacement clears slots. Refill intentionally chosen indices
        # in the same transaction; the existing weld record stays in place.
        p.EditOpSlotFill(node="panel-a", slot="pin", vertex=0),
        p.EditOpSlotFill(node="panel-a", slot="top", vertex=3),
    ]
    for node, seam in [("panel-a", [1, 2]), ("panel-b", [0, 2])]:
        for slot, vertex in zip(["lower-seam", "upper-seam"], seam):
            edits += [p.EditOpSlotAdd(node=node, slot=slot),
                      p.EditOpSlotFill(node=node, slot=slot, vertex=vertex)]
    edits.append(p.EditOpWeldSet(a="panel-a", b="panel-b", pairs=[
        p.SlotPair(a=slot, b=slot, weight=weight)
        for slot, weight in [("lower-seam", 0.25), ("upper-seam", 0.75)]
    ]))
    return edits


def assembly_edits(client: Client, session: p.SessionId, revision: int) -> list[p.EditOp]:
    """Lift this fixture's common affine X field into a shared MeshGroup.

    Shape moves only Y, so H(x,y)=(0.5x,0) commutes with it. This is the exact
    representable case; arbitrary independently authored rigs need caller fitting.
    """
    profile = None
    for node in ["panel-a", "panel-b"]:
        bindings = client.send(p.BindingList(session=session, node=node)).bindings
        if client.revision(session) != revision:
            raise ValueError("binding metadata came from another revision")
        binding = next(b for b in bindings if b.param == "turn")
        current = (binding.key_positions, binding.interpolate)
        if profile is not None and current != profile:
            raise ValueError("turn bindings use different grids or interpolation")
        profile = current
        read = client.send(p.BindingCellsGet(
            session=session, if_rev=revision, node=node, param="turn",
            target=p.BindingTarget.DEFORM, cells=[(0, 0), (1, 0), (2, 0)],
        ))
        mesh = client.send(p.MeshGet(session=session, if_rev=revision, node=node)).mesh
        expected = [(x * 0.5, 0.0) for x, _ in mesh.verts]
        if (read.cells[1].authored or list(read.cells[2].value.offsets) != expected
                or any(any(v) for v in read.cells[0].value.offsets)):
            raise ValueError("turn is not the synthetic affine field with an authored hole")
    assert profile is not None
    positions, interpolation = profile
    lattice = [(0, 0), (3, 0), (3, 3), (0, 3)]
    edits: list[p.EditOp] = [
        p.EditOpNodeAdd(node="placement", parent="root", kind=p.NodeKindArg.MESH_GROUP),
        p.EditOpMeshSet(node="placement", verts=lattice, uvs=[],
                        indices=[(0, 1, 2), (0, 2, 3)], origin=(0, 0)),
        p.EditOpBindingAdd(node="placement", param="turn", target=p.BindingTarget.DEFORM,
                           key_positions=positions),
        p.EditOpBindingInterpolationSet(node="placement", param="turn",
                                        target=p.BindingTarget.DEFORM, mode=interpolation),
        cells("placement", "turn", [((0, 0), [(0, 0)] * 4),
                                    ((2, 0), [(x * 0.5, 0) for x, _ in lattice])]),
    ]
    for node in ["panel-a", "panel-b"]:
        edits += [p.EditOpBindingDelete(node=node, param="turn", target=p.BindingTarget.DEFORM),
                  p.EditOpNodeReparent(node=node, to="placement")]
    return edits


def worlds(client: Client, session: p.SessionId, nodes: list[str]) -> list[dict]:
    revision = client.require_revision(session)
    return [{node.node: node.world for node in client.send(p.GeometryGet(
        session=session, if_rev=revision, nodes=nodes, fields=[p.GeometryField.WORLD],
        pose=[p.ParamPose(param="shape", value=shape), p.ParamPose(param="turn", value=turn)],
    )).nodes} for shape, turn in POSES]


def assert_close(actual, expected) -> None:
    if len(actual) != len(expected) or any(
        abs(a - b) > 1e-5 for point, reference in zip(actual, expected)
        for a, b in zip(point, reference)
    ):
        raise AssertionError(f"geometry differs: {actual!r} != {expected!r}")


def run(client: Client, out: Path, cli: Path) -> dict:
    out.mkdir(parents=True, exist_ok=False)
    source = synthetic_model(client)
    base_revision = client.require_revision(source)
    original = worlds(client, source, ["panel-a"])
    candidate = client.fork(source, if_rev=base_revision)
    baseline = client.export_model(candidate, if_rev=0)
    cut = cut_edits(client, candidate, 0)
    client.apply(candidate, cut, if_rev=0, validate=True)
    client.apply(candidate, cut, if_rev=0)
    cut_snapshot = client.export_model(candidate)
    split = worlds(client, candidate, ["panel-a", "panel-b"])
    for before, after in zip(original, split):
        assert_close(after["panel-a"], mapped(before["panel-a"], LEFT))
        assert_close(after["panel-b"], mapped(before["panel-a"], RIGHT))
    assembly = assembly_edits(client, candidate, client.require_revision(candidate))
    client.apply(candidate, assembly)
    assembled = worlds(client, candidate, ["panel-a", "panel-b"])
    for before, after in zip(split, assembled):
        for node in before:
            assert_close(after[node], before[node])
    candidate_bytes = client.export_model(candidate)
    edits = cut + assembly
    (out / "edits.json").write_text(json.dumps([e.to_wire() for e in edits], indent=2) + "\n")
    spec = {"schema": 1, "requests": {
        f"pose-{i}": {"rect": [-0.25, -0.25, 3.5, 3], "scale": 128,
                      "physics": {"mode": "off"}, "pose": {"shape": shape, "turn": turn},
                      "geometry": ["panel-a"]}
        for i, (shape, turn) in enumerate(POSES)
    }}
    spec_path = out / "render.json"
    spec_path.write_text(json.dumps(spec, indent=2) + "\n")
    for name, data in [("baseline", baseline), ("cut", cut_snapshot), ("assembled", candidate_bytes)]:
        path = out / f"{name}.clm"
        path.write_bytes(data)
        subprocess.run([str(cli), "render", str(path), "--spec", str(spec_path),
                        "--out-dir", str(out / name)], check=True, capture_output=True)
        if not json.loads((out / name / "run.json").read_text())["complete"]:
            raise AssertionError(f"render did not complete: {name}")
    # Publication stays a separate caller decision after geometry and renders pass.
    client.apply(source, edits, if_rev=base_revision)
    if client.export_model(source) != candidate_bytes:
        raise AssertionError("replay differs from the reviewed candidate")
    try:
        client.apply(source, edits, if_rev=base_revision)
    except ProtocolError as error:
        if error.code != p.ErrorCode.REVISION_CONFLICT:
            raise
    else:
        raise AssertionError("stale replay unexpectedly succeeded")
    report = {"accepted": True, "base_revision": base_revision,
              "published_revision": client.require_revision(source),
              "poses_checked": len(POSES), "stale_replay_refused": True}
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    client.close_session(candidate)
    client.close_session(source)
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True, help="catchlight-cli executable")
    parser.add_argument("--server", type=Path, help="catchlight-editor-server executable")
    parser.add_argument("--out", type=Path, required=True, help="new review artifact directory")
    args = parser.parse_args()
    with launch(binary=args.server) as running:
        print(json.dumps(run(running.client(), args.out, args.cli.resolve()), indent=2))
