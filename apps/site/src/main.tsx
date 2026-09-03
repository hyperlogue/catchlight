/**
 * The host: choose a backend, build one editor, mount it.
 *
 * This is the whole of the site. Everything a person sees is
 * `@catchlight/editor`; what is left here is the two decisions only a host can
 * make — where the document is served from, and what to do when the page
 * cannot start at all.
 *
 * **The backend comes from the URL.** `?server=<http base>` points the tab at a
 * `catchlight-editor-server` running on the machine, which is how an agent and
 * a person drive the same document. Without it the editor runs in this tab,
 * over the wasm module, against the origin private file system — so the plain
 * URL is a self-contained editor with no process behind it.
 *
 * **A failure is shown in the page.** A server that refuses the token, a
 * sample that will not fetch, a wasm module that will not start: each of those
 * is a blank screen with a console message unless someone writes it down, and
 * the console is not where a person looks. A browser with neither WebGPU nor
 * WebGL2 is the same kind of failure, and it surfaces one step later — the
 * device is acquired at the first attach, so the editor mounts and the
 * viewport reports what it could not have. Which of the two tiers did answer
 * is in the editor's own status line, because the two draw the same picture at
 * different costs and a bug report wants to say which one it saw.
 */

import {
  ConnectedBackend,
  Editor,
  InTabBackend,
  MemoryStorage,
  OpfsStorage,
} from "@catchlight/core";
import type { Backend, Storage } from "@catchlight/core";
import { CatchlightEditor } from "@catchlight/editor";
// Only the probe door below uses this, and only when the URL asks for it.
import { fitCamera } from "@catchlight/react";
import "@catchlight/editor/theme.css";
// The generated module initializes itself on import: `cargo xtask wasm` emits
// wasm-bindgen's bundler target, whose `.wasm` is an ESM import rather than a
// fetch this page would have to sequence.
import * as catchlight from "@catchlight/wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./site.css";

/** Copied into `public/` from `tests/models/`; see the `sample` script. */
const SAMPLE = "sample.clm";

void start();

async function start(): Promise<void> {
  try {
    const server = new URLSearchParams(globalThis.location.search).get("server");
    const editor = await Editor.create(catchlight, await pick(server));

    // Before the mount, so the editor comes up showing something: the panel
    // takes the first document the editor lists as the current one.
    if (server === null) await sample(editor);

    mount(editor);
    probe(editor);
  } catch (cause) {
    report(cause);
  }
}

/** A local editor process when the URL names one, this tab otherwise. */
async function pick(server: string | null): Promise<Backend> {
  if (server !== null) return ConnectedBackend.connect(server);
  return new InTabBackend(new catchlight.CatchlightEditor(), await store());
}

/**
 * Where an in-tab editor's bytes live.
 *
 * OPFS survives a reload and a private window does not have it, so the
 * fallback keeps the editor usable rather than refusing to start.
 */
async function store(): Promise<Storage> {
  if (!OpfsStorage.available()) return new MemoryStorage();
  try {
    return await OpfsStorage.open();
  } catch (cause) {
    report(cause);
    return new MemoryStorage();
  }
}

/**
 * Opens the model shipped with the site.
 *
 * Reported rather than thrown: a sample that will not load is worth saying out
 * loud, and is no reason to withhold an editor that works.
 */
async function sample(editor: Editor): Promise<void> {
  try {
    // `BASE_URL` ends in a slash and is `/<repo>/` on Pages.
    const url = `${import.meta.env.BASE_URL}${SAMPLE}`;
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`fetching ${url} failed: ${response.status} ${response.statusText}`);
    }
    await editor.openFile(new Uint8Array(await response.arrayBuffer()), SAMPLE);
  } catch (cause) {
    report(cause);
  }
}

function mount(editor: Editor): void {
  const host = document.getElementById("editor");
  if (!host) throw new Error("index.html has no #editor to mount into");
  createRoot(host).render(
    <StrictMode>
      <CatchlightEditor editor={editor} />
    </StrictMode>,
  );
}

/**
 * A door for the browser smoke test, shut unless the URL asks for it.
 *
 * `?probe=1` puts the editor on `globalThis` so a driver in the page can
 * attach a second viewport. That is the one thing about the WebGL2 tier no
 * unit test can show — an extra canvas borrows the main one's surface, and
 * whether the main canvas survives that is a question only a real browser
 * answers. Nothing reaches this on an ordinary load, and nothing in the editor
 * reads it.
 *
 * `fitCamera` rides along because the test's second viewport has to be framed
 * the way the editor frames the first. It is the same function the React
 * viewport calls when it opens a document, so a driver that calls it with the
 * same bounds and the same size gets the same camera, and the two canvases can
 * then be compared pixel for pixel.
 */
function probe(editor: Editor): void {
  if (!new URLSearchParams(globalThis.location.search).has("probe")) return;
  (globalThis as unknown as Record<string, unknown>).__catchlightProbe = { editor, fitCamera };
}

/** One `<pre>`, appended to. Several things can go wrong on one load. */
function report(cause: unknown): void {
  const pre = document.getElementById("failure");
  console.error(cause);
  if (!pre) return;
  pre.hidden = false;
  pre.textContent = `${pre.textContent ?? ""}${describe(cause)}\n`;
}

function describe(cause: unknown): string {
  if (cause instanceof Error) {
    return cause.cause === undefined
      ? cause.message
      : `${cause.message}: ${describe(cause.cause)}`;
  }
  return String(cause);
}
