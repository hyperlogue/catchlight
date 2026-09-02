import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `packages/wasm` sits outside this app, and the dev server serves only what it
// has been allowed.
const workspaceRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  // GitHub Pages serves under /<repo>/; a dev server and `preview` serve under /.
  base: process.env.VITE_BASE ?? "/",
  plugins: [react()],
  server: {
    fs: { allow: [workspaceRoot] },
  },
  build: {
    // The wasm bundle is ~28 MiB in debug and megabytes in release; the warning
    // says nothing this build does not already know.
    chunkSizeWarningLimit: 8192,
  },
});
