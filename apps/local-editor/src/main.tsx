/**
 * The editor page a local `catchlight-editor` serves.
 *
 * The page and the editor share an origin, so the backend is always that
 * origin: the token comes from its `GET /token` and the model lives in the
 * process that served this page. Sessions an agent opens over the CLI appear
 * in the session list as they are opened.
 */
import { ConnectedBackend, Editor } from "@catchlight/core";
import { CatchlightEditor } from "@catchlight/editor";
import "@catchlight/editor/theme.css";
// wasm-bindgen's bundler target initializes itself on import.
import * as catchlight from "@catchlight/wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";

async function start(): Promise<void> {
  const editor = await Editor.create(catchlight, await ConnectedBackend.connect(location.origin));
  const host = document.getElementById("editor");
  if (!host) throw new Error("index.html has no #editor to mount into");
  createRoot(host).render(
    <StrictMode>
      <CatchlightEditor editor={editor} />
    </StrictMode>,
  );
}

start().catch((cause: unknown) => {
  console.error(cause);
  const loading = document.getElementById("loading");
  if (loading) loading.textContent = "Could not reach the editor. Is catchlight-editor still running?";
  const failure = document.getElementById("failure");
  if (failure) {
    failure.hidden = false;
    failure.textContent = describe(cause);
  }
});

function describe(cause: unknown): string {
  if (cause instanceof Error) {
    return cause.cause === undefined ? cause.message : `${cause.message}: ${describe(cause.cause)}`;
  }
  return String(cause);
}
