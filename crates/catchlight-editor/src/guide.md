# Working in the Catchlight editor

Catchlight models are `.clm` files: 2.5D characters built from 2D parts that
deform as params move. `catchlight-editor` runs the editor in the user's browser
and lets you drive the same session from the command line. Every edit you make
appears in their tab as it lands, and theirs appear in your reads.

## 1. Find or start the editor

```sh
catchlight-editor session list
```

If that reports that no editor is running, start one as a background process
(it serves until stopped) and give the user the URL it prints:

```sh
catchlight-editor /absolute/path/model.clm      # or with no model
```

Add `--no-open` only when the user does not want a browser tab. Running it
again while one is up reuses that editor instead of starting a second. Stop an
editor only if you started it, with Ctrl-C (SIGINT), which also removes its
connection record.

## 2. Pick the session

```sh
catchlight-editor session list                  # what is open, yours and theirs
catchlight-editor session open /absolute/path/model.clm
catchlight-editor session use <id>              # target for later commands
```

`session open` takes a path the editor process can read; pass it absolute.
`--session <id>` overrides the remembered session for one command.

## 3. Read before writing

```sh
catchlight-editor status
catchlight-editor node tree
catchlight-editor node info <node>
catchlight-editor param list
catchlight-editor check                         # untextured parts, bad meshes, unfilled slots
catchlight-editor preview --out view.png        # what the model looks like now
```

Nodes, params, textures and slots are named by their Id, the string the
`.clm` stores, so an Id from `node tree` stays valid across sessions of the
same file. Add `--json` to any command for the exact protocol reply.

## 4. Edit

Prefer the typed commands (`node set`, `node add`, `param …`, `binding …`,
`mesh …`, `weld …`, `deform …`); `catchlight-editor help <command>` shows each
one. For several changes that belong together, write a JSON array of edit
operations and apply it as one undo step, validating first:

```sh
catchlight-editor edit edits.json --validate
catchlight-editor edit edits.json
```

```json
[
  {"op": "node_set", "node": "panel", "opacity": 0.8},
  {"op": "node_reorder", "node": "panel", "index": 0}
]
```

Operation names follow the commands. A shape the editor does not accept is
refused before anything changes, with the reason. `request -` sends any
protocol command as JSON on stdin when no typed command fits.

## 5. Share the session safely

The user may edit while you work. Each reply that changes the model carries
the session's new `rev` (visible with `--json`). Pass `--if-rev <rev>` from your
last read to make an edit fail instead of overwriting a change you have not
seen; on that failure, read again and redo the edit against what is there now.
Never retry a refused edit blindly.

- `undo` / `redo` step through history, the user's edits included.
- `presence get` shows the pose and selection the user's tab last published:
  how to tell which part they mean by "this one".
- `save` writes the model back to its file. Save only when the user asks, or
  confirm first: it overwrites their file. `export <path>` writes a snapshot
  without touching it.

## 6. Report

Tell the user what you changed in their terms (which parts, params or
motions), mention anything `check` still reports, and offer a `preview` image
when a visual result matters.
