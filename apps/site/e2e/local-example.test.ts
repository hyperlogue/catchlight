import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AddressInfo } from "node:net";
import { build, createServer, preview } from "vite";
import type { InlineConfig } from "vite";
import siteConfig from "../vite.config.js";

const temporary: string[] = [];
const privateBytes = new TextEncoder().encode("private-model-fixture-not-for-public-builds");

afterEach(async () => {
  await Promise.all(temporary.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function fixture() {
  const directory = await mkdtemp(join(tmpdir(), "catchlight-local-example-"));
  temporary.push(directory);
  const root = join(directory, "site");
  const model = join(directory, "private", "Local.clm");
  await mkdir(root);
  await mkdir(join(directory, "private"));
  await writeFile(model, privateBytes);
  await writeFile(join(root, "index.html"), '<script type="module" src="./entry.ts"></script>');
  await writeFile(join(root, "entry.ts"), "document.body.textContent = JSON.stringify(__CATCHLIGHT_EXAMPLE__);");
  return { root, model };
}

async function withModel<T>(path: string, work: () => Promise<T>): Promise<T> {
  const previous = process.env.CATCHLIGHT_LOCAL_MODEL;
  const nodeEnv = process.env.NODE_ENV;
  process.env.CATCHLIGHT_LOCAL_MODEL = path;
  try {
    return await work();
  } finally {
    if (previous === undefined) delete process.env.CATCHLIGHT_LOCAL_MODEL;
    else process.env.CATCHLIGHT_LOCAL_MODEL = previous;
    if (nodeEnv === undefined) delete process.env.NODE_ENV;
    else process.env.NODE_ENV = nodeEnv;
  }
}

async function config(root: string, command: "serve" | "build", isPreview = false): Promise<InlineConfig> {
  const resolved = await (typeof siteConfig === "function"
    ? siteConfig({ command, mode: "test", isPreview })
    : siteConfig);
  return {
    ...resolved,
    root,
    base: "/",
    configFile: false,
    envDir: false,
    publicDir: false,
    logLevel: "silent",
    cacheDir: join(root, ".vite"),
    optimizeDeps: { noDiscovery: true, include: [] },
    server: {
      ...resolved.server,
      port: 0,
      strictPort: true,
      fs: { ...resolved.server?.fs, allow: [...(resolved.server?.fs?.allow ?? []), root] },
    },
  };
}

function origin(server: { address(): AddressInfo | string | null }): string {
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("expected a TCP listener");
  return `http://127.0.0.1:${address.port}`;
}

async function emittedText(directory: string): Promise<string> {
  const contents = await Promise.all((await readdir(directory, { withFileTypes: true })).map((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? emittedText(path) : readFile(path, "utf8");
  }));
  return contents.join("\n");
}

describe("private local example", () => {
  test("serves one configured file and keeps its source outside the filesystem allowlist", async () => {
    const { root, model } = await fixture();
    await withModel(model, async () => {
      const server = await createServer(await config(root, "serve"));
      try {
        await server.listen();
        const base = origin(server.httpServer!);
        const file = await fetch(`${base}/__catchlight_local_example.clm`);
        expect(file.status).toBe(200);
        expect(file.headers.get("cache-control")).toContain("no-store");
        expect(new Uint8Array(await file.arrayBuffer())).toEqual(privateBytes);
        const head = await fetch(`${base}/__catchlight_local_example.clm`, { method: "HEAD" });
        expect(head.status).toBe(200);
        expect(await head.text()).toBe("");
        const definitions = await (await fetch(`${base}/@vite/env`)).text();
        expect(definitions).toContain("Local.clm");
        expect(definitions).toContain("__catchlight_local_example.clm");
        expect(definitions).not.toContain(model);
        const direct = await fetch(`${base}/@fs${model}`);
        expect(direct.status).toBe(403);
        expect(await direct.text()).not.toContain(new TextDecoder().decode(privateBytes));
      } finally {
        await server.close();
      }
    });
  }, 30_000);

  test("respects the base path and refuses cross-origin access", async () => {
    const { root, model } = await fixture();
    await withModel(model, async () => {
      const server = await createServer({ ...await config(root, "serve"), base: "/editor/" });
      try {
        await server.listen();
        const base = origin(server.httpServer!);
        const url = `${base}/editor/__catchlight_local_example.clm`;
        const local = await fetch(url, { headers: { Origin: base, "Sec-Fetch-Site": "same-origin" } });
        expect(new Uint8Array(await local.arrayBuffer())).toEqual(privateBytes);
        for (const headers of [
          { Origin: "https://unrelated.invalid" },
          { "Sec-Fetch-Site": "cross-site" },
          { Host: "unrelated.invalid" },
        ]) {
          const response = await fetch(url, { headers });
          expect(response.status).toBe(403);
          expect(await response.text()).not.toContain(new TextDecoder().decode(privateBytes));
        }
        const wrongPath = await fetch(`${base}/__catchlight_local_example.clm`);
        expect(new Uint8Array(await wrongPath.arrayBuffer())).not.toEqual(privateBytes);
      } finally {
        await server.close();
      }
    });
  }, 30_000);

  test("fails clearly for a missing file or a non-loopback listener", async () => {
    const { root, model } = await fixture();
    await withModel(`${model}.missing.clm`, async () => {
      await expect(config(root, "serve")).rejects.toThrow();
    });
    await withModel(model, async () => {
      const local = await config(root, "serve");
      let server: Awaited<ReturnType<typeof createServer>> | undefined;
      try {
        await expect(createServer({ ...local, server: { ...local.server, host: "0.0.0.0" } })
          .then((created) => { server = created; })).rejects.toThrow(/loopback/i);
      } finally {
        await server?.close();
      }
    });
  }, 30_000);

  test("unconfigured development has no starter", async () => {
    const { root } = await fixture();
    await withModel("", async () => {
      expect((await config(root, "serve")).define?.__CATCHLIGHT_EXAMPLE__).toBe("null");
    });
  });

  test("production output and preview ignore the private override", async () => {
    const { root, model } = await fixture();
    await withModel(model, async () => {
      await build(await config(root, "build"));
      const output = await emittedText(join(root, "dist"));
      expect(output).toContain("null");
      expect(output).not.toContain("Local.clm");
      expect(output).not.toContain("__catchlight_local_example.clm");
      expect(output).not.toContain(model);
      expect(output).not.toContain(new TextDecoder().decode(privateBytes));
      const server = await preview({
        ...await config(root, "serve", true),
        preview: { host: "127.0.0.1", port: 0, strictPort: true },
      });
      try {
        const response = await fetch(`${origin(server.httpServer)}/__catchlight_local_example.clm`);
        expect(new Uint8Array(await response.arrayBuffer())).not.toEqual(privateBytes);
      } finally {
        await new Promise<void>((resolve, reject) => server.httpServer.close((error) => error ? reject(error) : resolve()));
      }
    });
    await withModel(`${model}.missing.clm`, async () => {
      await expect(config(root, "build")).resolves.toBeDefined();
      await expect(config(root, "serve", true)).resolves.toBeDefined();
    });
  }, 30_000);
});
