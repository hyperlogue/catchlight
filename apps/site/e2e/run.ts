/**
 * The web editor's smoke test: serve the built site and drive it.
 *
 * Run it from the dev shell, which supplies Chromium and Xvfb. On Linux
 * without DISPLAY, this re-launches under Xvfb so the WebGPU pass can verify
 * presentation. The Lan fixture is Git LFS; fetch it first.
 *
 * ```text
 * bun run --filter catchlight-demo e2e
 *   CHROMIUM=<exe>        overrides the `chromium` on PATH
 *   E2E_BUILD=1           rebuild the site even when `dist/` is already there
 *   E2E_SITE_PORT=4173    where `vite preview` listens; a free port if taken
 *   E2E_SERVER_PORT=9377  where `catchlight-editor-server` listens; likewise
 * ```
 *
 * `vite preview` serves `dist/`, so the run exercises the shipped bundle, not
 * Vite's module graph. `XDG_RUNTIME_DIR` and `XDG_CACHE_HOME` point at a
 * temporary directory so the server and CLI do not reach an editor the
 * developer already has open.
 */

import { closeSync, openSync, readFileSync, writeSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

import { LAN_MODEL } from "./model.ts";

const OPEN_MODEL = LAN_MODEL;

if (process.platform === "linux" && !process.env.DISPLAY && !process.env.CHROMIUM_ARGS) {
  const xvfb = Bun.which("xvfb-run");
  if (!xvfb) fail("no xvfb-run: run this inside `nix develop` for WebGPU presentation");
  const child = Bun.spawn([
    xvfb,
    "-a",
    "-s", "-screen 0 1920x1080x24 -nolisten tcp",
    process.execPath,
    ...process.argv.slice(1),
  ], { stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  process.exit(await child.exited);
}

const AGENT_NODE = "agent-added";

const root = fileURLToPath(new URL("../../..", import.meta.url));
const at = (...parts: string[]) => join(root, ...parts);

const sitePort = await pick("the site", process.env.E2E_SITE_PORT, 4173);
const serverPort = await pick("the editor", process.env.E2E_SERVER_PORT, 9377);
const origin = `http://127.0.0.1:${sitePort}`;
const serverBase = `http://127.0.0.1:${serverPort}`;
// Under `target/` because `*.png` is a Git LFS pattern everywhere else.
const shots = at("target", "e2e");

const chromium = process.env.CHROMIUM ?? Bun.which("chromium");
if (!chromium) {
  fail("no chromium: run this inside `nix develop`, or set CHROMIUM=<exe>");
}

await mkdir(shots, { recursive: true });
const runtime = await mkdtemp(join(tmpdir(), "catchlight-e2e-"));
await mkdir(join(runtime, "models"));
const env = {
  ...process.env,
  XDG_RUNTIME_DIR: runtime,
  XDG_CACHE_HOME: join(runtime, "cache"),
  CATCHLIGHT_KEEP: join(runtime, "kept"),
};

const modelBytes = readFileSync(OPEN_MODEL);
const modelHash = createHash("sha256").update(modelBytes).digest("hex");
if (modelBytes.subarray(0, 40).toString("latin1").startsWith("version https://git-lfs")) {
  fail("Lan is a Git LFS pointer; run `git lfs pull --include=tests/models/lan.clm`.");
}
const serverModel = join(runtime, "Lan.clm");
await copyFile(OPEN_MODEL, serverModel);

await must("cargo", ["build", "-p", "catchlight-editor-server", "-p", "catchlight-editor-cli"]);

if (
  process.env.E2E_BUILD === "1" ||
  !(await Bun.file(at("apps", "site", "dist", "index.html")).exists())
) {
  await must("bun", ["run", "--filter", "catchlight-demo", "build"]);
}

const preview = serve(
  "site",
  at("apps", "site", "node_modules", ".bin", "vite"),
  ["preview", "--host", "127.0.0.1", "--port", String(sitePort), "--strictPort"],
  at("apps", "site"),
);
const server = serve(
  "server",
  at("target", "debug", "catchlight-editor-server"),
  [
    "--http",
    `127.0.0.1:${serverPort}`,
    "--allow-origin",
    origin,
    "--store",
    join(runtime, "models"),
    serverModel,
  ],
  root,
);

let code = 1;
try {
  await reachable(`${origin}/`, "site", preview);
  await reachable(`${serverBase}/token`, "server", server);

  // Sequential: the passes share one site, one editor server and one GPU.
  const passes = [
    await drive("in-tab, WebGPU", [chromium, `${origin}/`], { TIER: "webgpu" }),
    await drive("connected, WebGPU", [chromium, `${origin}/`, serverBase], {
      TIER: "webgpu",
      AGENT_CMD: `target/debug/catchlight-editor-cli node add --session 1 --parent Puppet --kind group --name ${AGENT_NODE}`,
      REVIEW_CLI: "CATCHLIGHT_AGENT_SESSION=e2e target/debug/catchlight-editor-cli --session 1 review --name e2e-agent",
    }),
    await drive("in-tab, WebGL2", [chromium, `${origin}/`], { TIER: "webgl2" }),
    await drive("in-tab, neither tier", [chromium, `${origin}/`], {
      TIER: "none",
    }),
  ];
  code = passes.every((exit) => exit === 0) ? 0 : 1;
} finally {
  for (const child of [preview, server]) {
    child.kill();
    await child.exited;
  }
  if (code !== 0) {
    for (const name of ["site", "server"]) await tail(name);
  }
  if (createHash("sha256").update(readFileSync(OPEN_MODEL)).digest("hex") !== modelHash) {
    console.error("Lan fixture changed during the run");
    code = 1;
  }
  console.log(code === 0 ? "\ne2e: both backends and both tiers passed" : "\ne2e: FAILED");
  process.exit(code);
}

async function drive(name: string, args: string[], extra: Record<string, string>): Promise<number> {
  console.log(`\n=== ${name} ===`);
  const child = Bun.spawn(["bun", "run", "apps/site/e2e/drive.ts", ...args], {
    cwd: root,
    env: { ...env, ...extra, OPEN_FILE: OPEN_MODEL, SHOTS: "target/e2e" },
    stdout: "inherit",
    stderr: "inherit",
  });
  return await child.exited;
}

function serve(name: string, exe: string, args: string[], cwd: string) {
  const log = openSync(join(shots, `${name}.log`), "w");
  const child = Bun.spawn([exe, ...args], {
    cwd,
    env,
    stdout: "pipe",
    stderr: "pipe",
  });
  // Keep the launch credential and local paths out of the test artifact.
  const record = (line: string) =>
    writeSync(
      log,
      line
        .replace(/\btoken:\s*\S+/gi, "token: [redacted]")
        .replaceAll(root, "<repo>")
        .replaceAll(runtime, "<runtime>")
        .replace(/\/(?:home|Users)\/[^/\s]+/g, "~"),
    );
  const drain = async (stream: ReadableStream<Uint8Array>) => {
    const reader = stream.getReader();
    const decoder = new TextDecoder();
    let pending = "";
    for (;;) {
      const { value, done } = await reader.read();
      pending += decoder.decode(value, { stream: !done });
      const lines = pending.split("\n");
      pending = lines.pop()!;
      for (const line of lines) record(`${line}\n`);
      if (done) {
        if (pending) record(pending);
        return;
      }
    }
  };
  void Promise.all([drain(child.stdout), drain(child.stderr)]).finally(() => closeSync(log));
  return child;
}

/** The child is watched too: if it failed to bind, whatever holds the port would answer. */
async function reachable(url: string, name: string, child: Bun.Subprocess): Promise<void> {
  const deadline = Date.now() + 60_000;
  for (;;) {
    if (child.exitCode !== null) {
      await tail(name);
      fail(`${name} exited with ${child.exitCode} before it served ${url}`);
    }
    const ok = await fetch(url).then(
      (r) => r.ok,
      () => false,
    );
    if (ok) return;
    if (Date.now() > deadline) {
      await tail(name);
      fail(`${name} never answered at ${url}`);
    }
    await Bun.sleep(250);
  }
}

/** The preferred port when free, otherwise any, so a run never drives a developer's own server. */
async function pick(
  name: string,
  override: string | undefined,
  preferred: number,
): Promise<number> {
  if (override !== undefined) return Number(override);
  for (const candidate of [preferred, 0]) {
    let chosen: number | undefined;
    try {
      const probe = Bun.serve({
        port: candidate,
        hostname: "127.0.0.1",
        fetch: () => new Response(),
      });
      chosen = probe.port;
      probe.stop(true);
    } catch {
      continue;
    }
    if (chosen === undefined) continue;
    if (chosen !== preferred) console.log(`e2e: ${preferred} is taken, ${name} takes ${chosen}`);
    return chosen;
  }
  fail(`no free port for ${name}`);
}

async function must(exe: string, args: string[]): Promise<void> {
  console.log(`+ ${exe} ${args.join(" ")}`);
  const child = Bun.spawn([exe, ...args], {
    cwd: root,
    env,
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await child.exited) !== 0) fail(`${exe} ${args.join(" ")} failed`);
}

async function tail(name: string): Promise<void> {
  const text = await readFile(join(shots, `${name}.log`), "utf8").catch(() => "");
  console.log(`--- ${name}.log ---\n${text.split("\n").slice(-20).join("\n")}`);
}

function fail(message: string): never {
  console.error(`e2e: ${message}`);
  process.exit(1);
}
