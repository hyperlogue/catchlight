/** The editor page a local `catchlight-editor` serves; the backend is this origin. */
import { ConnectedBackend, Editor } from "@catchlight/core";
import { AgentPrompt, CatchlightEditor } from "@catchlight/editor-ui";
import "@catchlight/editor-ui/theme.css";
// wasm-bindgen's bundler target initializes itself on import.
import * as catchlight from "@catchlight/wasm";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";

/**
 * The launcher opens `/#code=…` with a single-use code, which this trades for
 * the editor's token. The fragment is dropped from the address bar once read;
 * session storage keeps the token so a reload still connects.
 */
async function takeToken(): Promise<string | undefined> {
  const code = new URLSearchParams(location.hash.slice(1)).get("code");
  if (code) {
    history.replaceState(null, "", location.pathname + location.search);
    const token = await redeem(code);
    try {
      sessionStorage.setItem(TOKEN_KEY, token);
    } catch {
      // Storage refused: this load still connects; a reload will not.
    }
    return token;
  }
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

async function redeem(code: string): Promise<string> {
  const response = await fetch("/token", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code }),
  });
  if (!response.ok) {
    throw new Error("This link was already used. Run `catchlight-editor` again to open the editor.");
  }
  const { token } = (await response.json()) as { token: string };
  return token;
}

const TOKEN_KEY = "catchlight-token";

async function start(): Promise<void> {
  const token = await takeToken();
  if (token === undefined) {
    throw new Error("Open the editor with `catchlight-editor`; it opens this page with its access key.");
  }
  const editor = await Editor.create(catchlight, await ConnectedBackend.connect(location.origin, { token }));
  const host = document.getElementById("editor");
  if (!host) throw new Error("index.html has no #editor to mount into");
  createRoot(host).render(
    <StrictMode>
      <CatchlightEditor
        editor={editor}
        agentConnect={(session) => (
          <AgentPrompt
            prompt={`Join catchlight session ${session.id} and work through my review comments: run \`catchlight-editor guide\`, then \`catchlight-editor review listen --session ${session.id}\`.`}
          >
            <p>Bring your agent into this session by telling it:</p>
          </AgentPrompt>
        )}
      />
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
