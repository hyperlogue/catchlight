# catchlight (Python)

A blocking, dependency-free Python client for `catchlight-editor-server`.
Generated dataclasses expose every typed command; the server validates model
invariants and owns revisions, atomic edits, and branching history.

```python
import catchlight

with catchlight.launch() as server:
    client = server.client()
    built = catchlight.Builder.new(client, "Doll")
    face = built.part("Face", "face.png")
    tilt = built.param("Tilt", min=-1.0, max=1.0)
    built.bind(tilt, face, "ty", [(-1.0, -10.0), (0.0, 0.0), (1.0, 10.0)])
    built.save_to("doll.clm")
```

For a complete synthetic cut, assembly, candidate-rendering and guarded replay
workflow, see the [rig review example](examples/README.md).

## Modules

| Module | Responsibility |
| --- | --- |
| `protocol_gen.py` | Generated wire types, command classifications, byte contracts |
| `transport.py` | Unix socket and HTTP request framing |
| `server.py` | `launch()` owns a backend process; `connect(url, token)` attaches |
| `client.py` | Typed sends, source factories, guarded edits, exports, history |
| `builder.py` | Convenience composition for nodes, parameters, and bindings |
| `layers.py` | Placement files for cut-out image layers |

Rust wire types are authoritative. Run `cargo xtask generate` after changing
`crates/catchlight-editor-protocol`; never edit `protocol_gen.py` by hand.
`cargo test -p xtask -- --skip fixtures::` checks generation and classification.

## Sessions and bytes

`client.new(name)` creates an empty session. These factories validate a complete
source and publish one clean revision-zero session only on success:

```python
session = client.from_file("doll.clm")
session = client.from_json(structure, {"face": "face.png"})
session = client.from_manifest("model.json")
```

Each factory is one request with its source bytes attached. CLM sources preserve
binary extensions; JSON carries no binary extension payloads. Imported sessions
have no save path. `client.open(path)` instead opens a file in the server's store
and remembers its path. `client.import_json(session, structure, textures,
parent="root")` installs roots into an existing model as one guarded edit;
colliding IDs are refused.

`launch()` uses a Unix socket inside a fresh private temporary directory and
cleans up the owned process on exit. `connect(url, token)` uses a loopback HTTP
server and never stops it. Attachments travel as temporary files over the socket
or multipart bodies over HTTP; both expose the same Python calls.

`client.save(session, key)` writes into the server's store and marks the captured
state saved. `client.save_to(session, path)` writes locally: a store save over
the socket, or a guarded export over HTTP. Exporting does not mark the server's
session saved. `client.export_model(session)` returns CLM bytes with the same
behavior on either transport.

## Revisions, atomic edits, and history

Every successful captured model reply refreshes `client.revision(session)`,
including reads. The blocking client does not subscribe to browser events.
Convenience methods use the last captured revision as their guard; an unknown
session is read once first. Pass `if_rev=` to use a specific revision. A stale
guard raises `ProtocolError` and is never silently refreshed or retried.

```python
from catchlight import EditOpNodeSet, EditOpBindingCellsSet

revision = client.revision(session)
edits = [EditOpNodeSet(node=face, opacity=0.8)]
trial = client.apply(session, edits, if_rev=revision, validate=True)
result = client.apply(session, edits, if_rev=revision)
assert result.changed

candidate = client.fork(session, name="Candidate")
client.undo(session)
client.redo(session)
history = client.history(session)
client.goto(session, history.root)
```

`apply` publishes the whole edit list once or publishes nothing. Validation
returns the same per-operation results without modifying the session. An
operation failure exposes `ProtocolError.op_index`; bounded requests also expose
`ProtocolError.limit` with resource, requested, and allowed counts. Content
no-ops keep the revision and history unchanged.

A fork contains the captured model in a new clean session with independent
history and no save path. Undo, redo, and goto preserve retained branches and
publish a fresh live revision when they navigate. History entries expose their
creation revision, activation aliases, parent, and preferred redo child;
`pruned` reports when retention has removed earlier states.

## Exact bindings and discovery

Parameters are scalar inputs. Each binding owns its normalized key positions,
so two bindings driven by the same input can use different grids.
`Builder.bind` accepts parameter values, inserts positions only on its selected
binding, and writes exact scalar cells in one atomic edit. Existing cells and
sibling grids are preserved. `Builder.bind_deform` accepts grid indices and
per-vertex offsets; `key_positions=` selects a new binding's normalized axes.
Binding creation and cell writes roll back together if any cell is invalid.

All generated commands remain available through `client.send`:

```python
from catchlight import BindingList, BindingCellsGet, BindingTarget

bindings = client.send(BindingList(session=session, node=face))
cells = client.send(BindingCellsGet(
    session=session, if_rev=client.revision(session), node=face,
    param=tilt, target=BindingTarget.TY, cells=[(0, 0), (1, 0)],
    include_derived=True,
))
```

Cell values distinguish scalar values from deform offsets. `authored=False`
means a hole; it differs from an authored identity value. Derived values are
optional read results, never implicit writes. Deform arrays support bounded
vertex pages. Node, mesh, geometry, and binding reads are available as
generated command classes for callers composing their own inspection tools.

A dataclass field left `None` is omitted. Fields typed `T | Clear | None` also
accept `CLEAR` for explicit JSON null: `NodeSet(..., texture=CLEAR)` removes a
part's texture while omission preserves it. `ProtocolError.code` distinguishes
server refusal from a transport failure (`TransportError`).

## Layer placements

A placement describes image files and their hierarchy. `name` and `z` are
required; a layer without `file` is a group. Parents precede children, and image
filenames are relative to the placement directory.

```json
{
  "catchlight_layers": 1,
  "layers": [
    {"name": "Head", "z": 1},
    {"name": "Face", "file": "face.png", "parent": "Head", "z": 2,
     "offset": [0, 40], "mesh": {"mode": "grid", "cols": 4, "rows": 3}}
  ]
}
```

```python
built = catchlight.build_from_layers(client, "layers.json")
print(built.problems)  # Model lints are reported, not raised.
built.save_to("doll.clm")
```

## Tests

```sh
cargo build -p catchlight-editor-server
cd python
uv run pytest -q
```

Tests launch isolated backends and cover both socket and HTTP transports.
`launch()` locates its binary from an explicit `binary` argument, then
`$CATCHLIGHT_EDITOR_SERVER`, `PATH`, or the workspace's `target/debug` build.
Use `-m 'not slow'` to skip the real 30-second idle timeout test. Unix-only
because the launcher's local transport is a Unix socket.
