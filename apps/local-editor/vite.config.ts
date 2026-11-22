/**
 * The page `catchlight-editor` serves at its own origin. `cargo xtask
 * bundle-ui` builds it and copies `dist/` into that crate.
 */
import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `packages/wasm` sits outside this app, and the dev server serves only what it
// has been allowed.
const workspaceRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  base: "/",
  plugins: [react()],
  server: { fs: { allow: [workspaceRoot] } },
  build: {
    // The wasm runtime is one large asset by design.
    chunkSizeWarningLimit: 8192,
    // Hashed names under `assets/` are what lets the server mark them immutable.
    assetsDir: "assets",
  },
});
