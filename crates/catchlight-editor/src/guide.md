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

## 6. Review threads

The user can comment on a node, param, texture, slot or the whole model in
their tab, then send those comments to you. Listen for them once:

```sh
catchlight-editor review listen
```

It returns at once. From then on a background worker shows you as listening
in their tab, and each time they press Send a short notice arrives in this
session naming the editor session, the model and the exact `review fetch`
command to run. Nothing waits in your turn: do not poll, watch or announce
that you are waiting; carry on or end your turn. It works from Claude Code,
Codex and Antigravity (`agy`). Running `listen` again replaces your
registration, and `review unlisten` ends it. Exit 5 means this harness or
platform cannot receive a notice; then wait instead with:

```sh
catchlight-editor review watch                  # run it in the background
```

which shows you as listening and returns when they send.

`review fetch`, which a notice asks for, and `watch` print each thread with
its Id, what it is about, the pose the user was looking at,
and the conversation so far, the new comments marked `(new)`. Only once that
is printed does it acknowledge them, which marks them delivered and claims
their threads for you, so the tab shows you working. Its exit code says what
happened:

- `0`: the comments were delivered; act on them.
- `3`: `watch --timeout <seconds>` passed with nothing sent.
- `4`: they changed before the acknowledgment (deleted, resolved, or taken by
  another agent); disregard the output and run `review fetch` or `watch` again.
- `1`: they could not be saved, printed or acknowledged; they stay pending,
  so fix the cause and fetch again, expecting the same comments.

Then, for every thread printed:

1. Read the target (`node info <id>`, `preview --param …` at the thread's
   pose) and make the change, or decide what to ask.
2. Answer on the thread, saying what you changed in the user's terms:

   ```sh
   catchlight-editor review reply <thread> -m "Raised the ear's z order above the hair." --key ear-1
   ```

   The reply ends your claim on the thread. If a reply or `review open` fails
   or you cannot tell whether it landed, run it again with the same `--key`:
   a repeat writes nothing twice, and a key reused for other content is
   refused.
3. If you wait with `watch` rather than listen, run `review watch` again.

Reply to every thread you were given, even when the answer is a question or
"not done, because …": the thread shows as yours until you do. Never resolve
a thread; the user does that when they are satisfied. `review list` shows the
open threads, who is working on each and which agents are around, and
`review open --node <id> -m "…"` (or `--param`, `--texture`, `--slot <node>
<slot>`, `--model`) starts one of your own when you need to ask about
something.

Several agents can attend one review. Each is its harness session
(`claude:<session>`, `codex:<thread>`, `antigravity:<conversation>`), or
`CATCHLIGHT_AGENT_SESSION` when set; with neither, every agent is the same
one, `agent`. `--name` (or `CATCHLIGHT_AGENT`) is only the name the user
sees, by default the harness's. `review claim <thread>...` takes a 60-minute
claim, renewed by claiming again before it lapses, and is refused for a thread
another agent holds; `review release <thread>...` gives yours up. New comments
on a thread another agent holds wait for that agent. When several agents
listen on one session, each is notified of a Send; whoever fetches a thread
first claims it, and the others see it held.

One worker serves every listening agent of the user; it exits when none is
left, and drops an agent whose harness process has exited or whose session
closed. `review worker status` shows it, who listens, and a notice that could
not be delivered: that agent stops showing as listening until it runs
`listen` again. `review worker stop` stops the worker and keeps the
registrations for the next `listen` or `review worker start`.

Threads of a saved model are kept for you on this machine and are there when
it is next opened; those of a model never saved last only as long as its
session. Moving or renaming the model file leaves its threads behind; an
editor started with `--reviews beside` keeps them next to the file
(`<model>.review/`) instead. `catchlight-editor reviews gc --dry-run` lists
the kept threads of models that no longer exist; without `--dry-run` it
removes them.

A comment can carry images: a screenshot, or one the user drew on to point at
the problem. Such a comment lists `image <id> (…)` and may have no text at
all, so always ask for the files:

```sh
catchlight-editor review watch --attachments-dir review-images
```

Each image is then saved and printed under its comment as `image: <path>`
(`review fetch` and `review list` take the same flag). Open every one of those
files with a tool that shows you the picture before you answer; a reply written
without looking misses what the user pointed at. Show your own evidence the
same way, up to four images a comment:

```sh
catchlight-editor preview --out after.png
catchlight-editor review reply <thread> -m "Ear now draws above the hair." --attach after.png
```

## 7. Report

Tell the user what you changed in their terms (which parts, params or
motions), mention anything `check` still reports, and offer a `preview` image
when a visual result matters.
