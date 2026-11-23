/** The editor page a local `catchlight-editor` serves; the backend is this origin. */
import { ConnectedBackend, Editor } from "@catchlight/core";
import { CatchlightEditor } from "@catchlight/editor-ui";
import "@catchlight/editor-ui/theme.css";
// wasm-bindgen's bundler target initializes itself on import.
import * as catchlight from "@catchlight/wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";

/**
 * The launcher opens `/#token=…`. The fragment is dropped from the address bar
 * once read; session storage keeps it so a reload still connects.
 */
function takeToken(): string | undefined {
  const fromHash = new URLSearchParams(location.hash.slice(1)).get("token");
  if (fromHash) {
    history.replaceState(null, "", location.pathname + location.search);
    try {
      sessionStorage.setItem(TOKEN_KEY, fromHash);
    } catch {
      // Storage refused: this load still connects; a reload will not.
    }
    return fromHash;
  }
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

const TOKEN_KEY = "catchlight-token";

async function start(): Promise<void> {
  const token = takeToken();
  if (token === undefined) {
    throw new Error("Open the editor with `catchlight-editor`; it opens this page with its access key.");
  }
  const editor = await Editor.create(catchlight, await ConnectedBackend.connect(location.origin, { token }));
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
