/** Boots a selected demo. Face tracking uses an ephemeral in-tab model. */

import {
  ConnectedBackend,
  Editor,
  InTabBackend,
  MemoryStorage,
  OpfsStorage,
} from "@catchlight/core";
import type { Backend, Session, Storage } from "@catchlight/core";
import { CatchlightEditor } from "@catchlight/editor-ui";
import { EditorProvider, fitCamera } from "@catchlight/react";
import "@catchlight/editor-ui/theme.css";
// wasm-bindgen's bundler target initializes itself on import.
import * as catchlight from "@catchlight/wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { downloadModel, showLoading, showProcessing } from "./loading.js";
import { FaceDemo } from "./face-demo.js";
import lanUrl from "../../../tests/models/lan.clm?url";

/** A bundled asset URL or development route, never a filesystem path. */
type Example = { file: string; name: string; size?: number };
declare const __CATCHLIGHT_LAN_SIZE__: number;
const lan: Example = { file: lanUrl, name: "Lan.clm", size: __CATCHLIGHT_LAN_SIZE__ };
declare const __CATCHLIGHT_EXAMPLE__: Example | null;
const example: Example = __CATCHLIGHT_EXAMPLE__ ?? lan;

export async function start(mode: "editor" | "face"): Promise<void> {
  try {
    const server = mode === "face" ? null : new URLSearchParams(globalThis.location.search).get("server");
    showLoading("Starting editor…", "Preparing your workspace");
    const editor = await Editor.create(catchlight, mode === "face" ? new InTabBackend(new catchlight.CatchlightEditor(), new MemoryStorage()) : await pick(server));

    const session = server === null
      ? await openStarter(editor, mode === "face" ? lan : example).catch(report)
      : undefined;

    mount(editor, mode, session);
    probe(editor);
  } catch (cause) {
    showLoading("Could not open Catchlight", "Reload the page to try again.");
    document.getElementById("loading-progress")?.setAttribute("hidden", "");
    report(cause);
  }
}

async function pick(server: string | null): Promise<Backend> {
  if (server !== null) return ConnectedBackend.connect(server);
  return new InTabBackend(new catchlight.CatchlightEditor(), await store());
}

/** Private windows lack OPFS; fall back rather than refuse to start. */
async function store(): Promise<Storage> {
  if (!OpfsStorage.available()) return new MemoryStorage();
  try {
    return await OpfsStorage.open();
  } catch (cause) {
    report(cause);
    return new MemoryStorage();
  }
}

/** Reported rather than thrown: a failed starter should not withhold a working editor. */
async function openStarter(editor: Editor, example: Example): Promise<Session> {
  // `BASE_URL` ends in a slash and is `/<repo>/` on Pages.
  const url = new URL(example.file, new URL(import.meta.env.BASE_URL, location.href)).href;
  const bytes = await downloadModel(url, example.name, example.size);
  await showProcessing(example.name);
  return editor.openFile(bytes, example.name);
}

function mount(editor: Editor, mode: "editor" | "face", session: Session | void): void {
  const host = document.getElementById("editor");
  if (!host) throw new Error("index.html has no #editor to mount into");
  createRoot(host).render(
    <StrictMode>
      <div className="demo-shell">
        <nav className="demo-nav"><a href={import.meta.env.BASE_URL}>← All demos</a><span>{mode === "face" ? "Face tracking" : "Editor demo"}</span></nav>
        {mode === "face" ? (session ? <EditorProvider editor={editor}><FaceDemo session={session} /></EditorProvider> : null) :
          <CatchlightEditor editor={editor} onOpenStarter={() => openStarter(editor, example)} />}
      </div>
    </StrictMode>,
  );
}

/** `?probe=1` exposes the editor and `fitCamera` to the browser smoke test (`e2e/drive.ts`). */
function probe(editor: Editor): void {
  if (!new URLSearchParams(globalThis.location.search).has("probe")) return;
  (globalThis as unknown as Record<string, unknown>).__catchlightProbe = {
    editor,
    fitCamera,
  };
}

function report(cause: unknown): void {
  const pre = document.getElementById("failure");
  console.error(cause);
  if (!pre) return;
  pre.hidden = false;
  pre.textContent = `${pre.textContent ?? ""}${describe(cause)}\n`;
}

function describe(cause: unknown): string {
  if (cause instanceof Error) {
    return cause.cause === undefined ? cause.message : `${cause.message}: ${describe(cause.cause)}`;
  }
  return String(cause);
}
