# Editor session CLI

`catchlight-editor-cli` drives an existing editor session over its Unix socket.
`catchlight-cli` is the separate tool for model files and offline rendering.

```sh
catchlight-editor-cli session new --name Candidate
catchlight-editor-cli node add --parent root --kind part --name Panel
catchlight-editor-cli --session 1 status
```

The current session is remembered locally. `session use ID` selects another;
`--session ID` overrides it for one invocation. `--json` prints the protocol
reply instead of a friendly summary.

## Complete sources and snapshots

```sh
catchlight-editor-cli session new --clm model.clm --name Candidate
catchlight-editor-cli session fork --name Experiment
catchlight-editor-cli export candidate.clm
catchlight-editor-cli save candidate.clm
```

`new --clm` creates one finished, clean revision-zero session with no save path.
`session open` reads a store-owned file and remembers its path. A fork shares
immutable model assets but has independent edits, history, and save state.
Exporting captures CLM bytes without marking the session saved.

## Exact edits and history

```sh
catchlight-editor-cli binding add --node panel --param drive --target tx \
  --keys '[[0,0.5,1]]'
catchlight-editor-cli binding key --node panel --param drive --target tx \
  --cell 1,0 --value 12
catchlight-editor-cli binding key-insert --node panel --param drive --target tx \
  --axis drive --value 0.75
catchlight-editor-cli history
catchlight-editor-cli undo
catchlight-editor-cli redo
catchlight-editor-cli goto 3
```

Each binding owns its grid. Cell values are exact stored contributions; a cell
write creates an absent binding with default endpoint axes and no implicit rest
key. `binding unset` restores a hole; `binding reset` explicitly writes the identity reported by the binding read.
`binding copy-key` copies an authored value; `--derived` explicitly allows
copying an evaluated hole. `binding flip --axis PARAM` mirrors only that
binding's grid and sparse authored cells. `deform set` computes raw offsets
around the rest mesh origin, and `deform vertices` writes an offset array
verbatim. Neither command records a posed scene implicitly.

Shortcuts such as node move, mesh copy, binding flip/invert/reset, mask reorder,
and weld weight read model data and publish one `edit_apply`. All reads must
refer to the same captured revision. The final write uses that revision and
fails if someone else edited meanwhile. `--if-rev N` pins a guarded command to
a revision captured earlier. Failed edits are never retried automatically.

An explicit batch file is a JSON array of typed edit operations:

```json
[
  {"op":"node_set","node":"panel","opacity":0.8},
  {"op":"node_reorder","node":"panel","index":0}
]
```

```sh
catchlight-editor-cli --if-rev 4 edit edits.json --validate
catchlight-editor-cli --if-rev 4 edit edits.json
```

Validation returns operation results without publishing. A successful change
creates one history entry; final content no-ops leave the revision unchanged.
Navigation preserves retained branches and publishes a fresh live revision.

## Every protocol command

`request` accepts any typed command JSON, so new bounded reads, exact writes,
source formats, and exports do not need a separate convenience flag. Use `-`
to read stdin. `--attachment NAME=PATH` carries a declared attachment, and
`--out PATH` receives binary reply bytes.

```sh
printf '%s\n' '{"cmd":"mesh_get","session":1,"node":"panel","if_rev":4}' \
  | catchlight-editor-cli --json request -

printf '%s\n' '{"cmd":"session_create","source":{"format":"manifest"}}' \
  | catchlight-editor-cli request - --attachment manifest=model.json \
      --attachment texture:images/face.png=images/face.png
```

Commands use the authoritative types in `catchlight-editor-protocol`; unknown
commands and invalid field shapes are refused before sending. Paths in socket
attachment fields are made absolute at runtime. The command JSON is limited to
1 MiB, independently of attachment bytes. Larger independent work can be split
into explicit batches; no oversized atomic batch is split implicitly.

`slot unfilled` composes a tree read with per-Part slot reads. Its `--json`
output is a local `{ "rev": N, "slots": [...] }` report, carrying the revision
shared by every read.
