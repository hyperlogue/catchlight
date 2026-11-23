/** `cargo xtask bundle-ui` builds this and copies `dist/` into `catchlight-editor`. */
import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const workspaceRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  base: "/",
  plugins: [react()],
  server: { fs: { allow: [workspaceRoot] } },
  build: {
    chunkSizeWarningLimit: 8192,
    // The server marks hashed names under `assets/` immutable.
    assetsDir: "assets",
  },
});
