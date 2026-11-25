---
name: edit-catchlight-model
description: Open, inspect, edit, rig or preview Catchlight `.clm` character models in the Catchlight editor, sharing the session with the user's browser tab. Use when the user mentions Catchlight, a `.clm` file, or asks you to work on a character in the editor.
---

Run `catchlight-editor guide` once per session and follow it: it covers
starting or finding the editor, choosing a session, reading the model, editing
it while the user watches, and saving. It comes from the installed binary, so it
always matches the commands available.

If `catchlight-editor` is not on `PATH`, run the latest release with npm and use
that same prefix for every later command:

```sh
npx --yes @catchlight/editor@latest guide
```

With Bun:

```sh
bunx @catchlight/editor@latest guide
```

Or install the binary (Linux and macOS; Windows uses `install.ps1` from the same
release with `irm … | iex`):

```sh
curl -fsSL https://github.com/hyperlogue/catchlight/releases/latest/download/install.sh | sh
```

Once you are on a session, run `catchlight-editor review listen`. It returns
at once; each time the user sends comments from their tab, a notice arrives in
your session with the `review fetch --attachments-dir <dir>` command to run.
Do not wait or poll in between. If `listen` exits 5, your harness cannot
receive notices: run `catchlight-editor review watch --attachments-dir <dir>`
in the background instead; it returns with the comments. Act only on exit 0
of `fetch` or `watch`; on 4 or 1 run `review fetch` again, as the guide's
review section explains. Open every image it saved before acting. Change the
model, answer each thread with `catchlight-editor review reply <thread> -m "…"
--key <k>`, adding `--attach <image>` when a `preview --out` render shows the
result, and, if you used `watch`, watch again. Repeat a failed reply with the
same `--key`. Leave resolving to the user.

Use `catchlight-editor help <command>` for a command's flags. Give the user the
editor URL when you start one, so they can watch your edits land.
