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
 * **A failure is shown in the page.** No WebGPU and no WebGL2, a server that
 * refuses the token, a sample that will not fetch: each of those is a blank
 * screen with a console message unless someone writes it down, and the console
 * is not where a person looks.
 */

import {
  ConnectedBackend,
  Editor,
  InTabBackend,
  MemoryStorage,
  OpfsStorage,
} from "@catchlight/core";
import type { Backend, Storage, WasmModule } from "@catchlight/core";
import { CatchlightEditor } from "@catchlight/editor";
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

/**
 * The generated module, as `@catchlight/core` declares it.
 *
 * The assertion is one mismatch and not a shrug: wasm-bindgen gives every
 * exported class a `[Symbol.dispose]`, which `WasmGpu` does not declare, so
 * `new Replica(gpu)` fails the contravariant check on its parameter. The
 * methods core actually calls all line up. `WasmGpu` gaining `[Symbol.dispose]`
 * would remove this.
 */
const wasm = catchlight as unknown as WasmModule;

void start();

async function start(): Promise<void> {
  try {
    const server = new URLSearchParams(globalThis.location.search).get("server");
    const editor = await Editor.create(wasm, await pick(server));

    // Before the mount, so the editor comes up showing something: the panel
    // takes the first document the editor lists as the current one.
    if (server === null) await sample(editor);

    mount(editor);
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
