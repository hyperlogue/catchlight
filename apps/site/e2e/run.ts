/**
 * The web editor's smoke test: serve what Pages would ship, and drive it.
 *
 * Run it from the dev shell: `bun run --filter catchlight-site e2e`. The
 * Chromium it drives is in that shell.
 *
 * ```text
 * bun run --filter catchlight-site e2e
 *   CHROMIUM=<exe>        overrides the `chromium` on PATH
 *   E2E_BUILD=1           rebuild the site even when `dist/` is already there
 *   E2E_SITE_PORT=4173    where `vite preview` listens; a free port if taken
 *   E2E_SERVER_PORT=9377  where `catchlight-editor-server` listens; likewise
 * ```
 *
 * **The built site, not the dev server.** `vite preview` serves `dist/`, so
 * this exercises the bundle and the asset URLs a visitor gets rather than
 * Vite's module graph — the two differ in exactly the places a browser test is
 * for. CI builds the site in the step before this one and this reuses it.
 *
 * **Both backends, one run.** The tab is the same editor either way, but the
 * seam underneath is not: in-tab it drives the wasm module against OPFS, and
 * connected it drives `catchlight-editor-server` over a WebSocket and HTTP.
 * The connected pass also makes an agent edit the document over the Unix
 * socket, which is the one thing no unit test can show: a command from outside
 * the browser landing in the tab's replica.
 *
 * **Both graphics tiers, and the browser that has neither.** The editor draws
 * on WebGPU where a browser offers it and on WebGL2 where it does not, and the
 * fallback is not a detail: it is what an iOS device that cannot run Safari 26
 * gets. So a third pass launches the same in-tab tab with WebGPU switched off
 * and asserts the picture on the tier underneath, and a fourth switches both
 * off and asserts the sentence the tab shows instead of a blank canvas. Which
 * tier a pass ran on is asserted in the tab, not assumed from the flags: see
 * `drive.ts`.
 *
 * **The socket is private to the run.** `default_socket_path` is derived from
 * `XDG_RUNTIME_DIR`, so pointing that at a temporary directory keeps the
 * server and the CLI talking to each other and not to whatever editor the
 * developer already has open. `XDG_CACHE_HOME` moves for the same reason: the
 * CLI remembers a current session there.
 */

import { openSync, readFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** Opened by the server, edited by the agent, and what the tab shows first. */
const SERVER_MODEL = "tests/models/quad_over_bg.clm";
/**
 * Opened through the file input, in every pass that draws.
 *
 * Not the default sample: `quad_over_bg` is a red disk on a blue ground, and
 * the viewport's default camera frames it so the disk covers the canvas edge
 * to edge — a drag across the middle of that moves no pixel a screenshot can
 * see. Two welded quads leave a seam in frame for the drag to move.
 */
const OPEN_MODEL = "tests/models/welded_seam.clm";
/** What the agent calls the node it adds; the drive only needs the tree to grow. */
const AGENT_NODE = "agent-added";

const root = fileURLToPath(new URL("../../..", import.meta.url));
const at = (...parts: string[]) => join(root, ...parts);

const sitePort = await pick("the site", process.env.E2E_SITE_PORT, 4173);
const serverPort = await pick("the editor", process.env.E2E_SERVER_PORT, 9377);
const origin = `http://127.0.0.1:${sitePort}`;
const serverBase = `http://127.0.0.1:${serverPort}`;
// Under `target/` because it is already ignored, already the directory build
// leavings go in, and `*.png` is a Git LFS pattern everywhere else.
const shots = at("target", "e2e");

const chromium = process.env.CHROMIUM ?? Bun.which("chromium");
if (!chromium) {
  fail("no chromium: run this inside `nix develop`, or set CHROMIUM=<exe>");
}

await mkdir(shots, { recursive: true });
const runtime = await mkdtemp(join(tmpdir(), "catchlight-e2e-"));
const env = { ...process.env, XDG_RUNTIME_DIR: runtime, XDG_CACHE_HOME: join(runtime, "cache") };

for (const model of [SERVER_MODEL, OPEN_MODEL, "apps/site/public/sample.clm"]) {
  const head = readFileSync(at(model)).subarray(0, 40).toString("latin1");
  if (head.startsWith("version https://git-lfs")) {
    fail(`${model} is a Git LFS pointer; fetch the objects with \`git lfs pull\``);
  }
}

for (const bin of ["catchlight-editor-server", "catchlight-editor-cli"]) {
  if (!(await Bun.file(at("target", "debug", bin)).exists())) {
    await must("cargo", ["build", "-p", "catchlight-editor-server", "-p", "catchlight-editor-cli"]);
    break;
  }
}
if (process.env.E2E_BUILD === "1" || !(await Bun.file(at("apps", "site", "dist", "index.html")).exists())) {
  await must("bun", ["run", "--filter", "catchlight-site", "build"]);
}

const preview = serve("site", at("apps", "site", "node_modules", ".bin", "vite"), [
  "preview", "--host", "127.0.0.1", "--port", String(sitePort), "--strictPort",
], at("apps", "site"));
const server = serve("server", at("target", "debug", "catchlight-editor-server"), [
  "--http", `127.0.0.1:${serverPort}`, "--allow-origin", origin, SERVER_MODEL,
], root);

let code = 1;
try {
  await reachable(`${origin}/`, "site", preview);
  await reachable(`${serverBase}/token`, "server", server);

  // In order: the two backends on WebGPU, then the fallback tier, then the
  // browser that has neither. Sequential because they share one site, one
  // editor server and one machine's GPU.
  const passes = [
    await drive("in-tab, WebGPU", [chromium, `${origin}/`], { TIER: "webgpu" }),
    await drive("connected, WebGPU", [chromium, `${origin}/`, serverBase], {
      TIER: "webgpu",
      AGENT_CMD:
        `target/debug/catchlight-editor-cli node add --session 1 --parent root --kind group --name ${AGENT_NODE}`,
    }),
    await drive("in-tab, WebGL2", [chromium, `${origin}/`], { TIER: "webgl2" }),
    await drive("in-tab, neither tier", [chromium, `${origin}/`], { TIER: "none" }),
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
  console.log(code === 0 ? "\ne2e: both backends and both tiers passed" : "\ne2e: FAILED");
  process.exit(code);
}

/** One pass of `drive.ts`, with the shared environment already in place. */
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

/** A long-lived child whose output goes to a log this can print on failure. */
function serve(name: string, exe: string, args: string[], cwd: string) {
  const log = openSync(join(shots, `${name}.log`), "w");
  return Bun.spawn([exe, ...args], { cwd, env, stdout: log, stderr: log });
}

/**
 * Waits for `url` to answer, so a drive never races a server's startup.
 *
 * The child is watched as well as the port: a server that failed to bind is
 * the case where waiting on the URL alone is actively wrong, because whatever
 * already holds that port answers happily and the run then tests it instead.
 */
async function reachable(url: string, name: string, child: Bun.Subprocess): Promise<void> {
  const deadline = Date.now() + 60_000;
  for (;;) {
    if (child.exitCode !== null) {
      await tail(name);
      fail(`${name} exited with ${child.exitCode} before it served ${url}`);
    }
    const ok = await fetch(url).then((r) => r.ok, () => false);
    if (ok) return;
    if (Date.now() > deadline) {
      await tail(name);
      fail(`${name} never answered at ${url}`);
    }
    await Bun.sleep(250);
  }
}

/**
 * A port to listen on: the familiar one when it is free, any free one when it
 * is not.
 *
 * A developer running the editor server, or a second copy of this, holds 9377
 * — and a run that quietly drove *that* server would report a passing tab
 * against a document nobody set up. Binding here first is the cheap way to
 * find out, and moving is better than refusing to run.
 */
async function pick(name: string, override: string | undefined, preferred: number): Promise<number> {
  if (override !== undefined) return Number(override);
  for (const candidate of [preferred, 0]) {
    let chosen: number | undefined;
    try {
      const probe = Bun.serve({ port: candidate, hostname: "127.0.0.1", fetch: () => new Response() });
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
  const child = Bun.spawn([exe, ...args], { cwd: root, env, stdout: "inherit", stderr: "inherit" });
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
