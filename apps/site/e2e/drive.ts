/**
 * One headless-browser pass over the web editor, against one backend.
 *
 * ```text
 * bun run apps/site/e2e/drive.ts <chromium-exe> <site-url> [server-base]
 *   OPEN_FILE=<path.clm>  opens a model through the file input
 *   AGENT_CMD=<shell>     (connected only) an edit over the Unix socket that must reach the tab
 *   AGENT_CWD=<dir>       where that command runs; this process's directory otherwise
 *   SHOTS=<dir>           where canvas screenshots land
 * ```
 *
 * `e2e/run.ts` is what starts the servers and calls this twice; run it by hand
 * only to drive something they do not set up.
 *
 * **The browser is a real one, and it draws on WebGL2.** Playwright's own
 * Chromium download does not run on NixOS, so the executable is an argument
 * and `nix/shell.nix` puts one on `PATH`. Headless Chromium has no WebGPU this
 * editor survives: `--enable-unsafe-webgpu` hands out a SwiftShader adapter
 * that loses its device within a second in plain JavaScript, and fails a
 * 144-byte `createBuffer` under `wgpu`. So the flags below select ANGLE's
 * software rasteriser instead and this exercises the WebGL2 tier, which the
 * editor supports rather than tolerates.
 *
 * **A step that cannot see the picture is not a passing step.** Canvas
 * assertions go through [`canvasShot`], which decodes the screenshot and, for
 * every pose but the deliberate extreme, refuses a frame that is one flat
 * colour: a dead device leaves the page running, the DOM correct and the
 * canvas the colour it was cleared to, and every other check here would still
 * report ok. For the same reason a panic, a trap or a lost device aborts the
 * run where it happens instead of being counted in the summary — after one of
 * those, the next step's timeout would report the wrong failure.
 */

import { chromium } from "playwright-core";

import { decodePng, fingerprint, hex, uniformity } from "./png.ts";

/**
 * How much of the canvas one colour may cover before the frame reads as blank.
 *
 * A canvas nothing drew on is its clear colour and scores exactly 1; the
 * margin below that is for antialiasing along a model's edge. What makes this
 * worth asserting is that a dead GPU device is invisible from everywhere else
 * in this file — the page runs, the DOM fills in, the status line moves, and
 * the picture never appears.
 */
const MAX_FLAT_SHARE = 0.995;

/** Lines that mean the page is no longer able to draw, whatever it still shows. */
const CONSOLE_FATAL = [/panicked at/, /device (?:is |was )?lost/i, /lost the device/i, /context lost/i];
/** A wasm trap reaches the page as an error and nothing else; hence the extra pattern. */
const PAGE_FATAL = [...CONSOLE_FATAL, /unreachable/i];

const [exe, site, server] = process.argv.slice(2);
if (!exe || !site) throw new Error("usage: drive.ts <chromium> <site-url> [server-base]");
const url = server ? `${site}?server=${encodeURIComponent(server)}` : site;
const shots = process.env.SHOTS ?? ".";
const tag = server ? "connected" : "intab";

const browser = await chromium.launch({
  executablePath: exe,
  headless: true,
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist", "--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

let failed = false;
let fatal: string | null = null;
let trip: (reason: string) => void = () => {};
// The promise every step races against. Nothing polls it: the console handler
// rejects it, and whichever step is waiting at that moment loses the race and
// reports the death rather than its own timeout.
const tripwire = new Promise<never>((_, reject) => {
  trip = (reason) => {
    if (fatal !== null) return;
    fatal = reason;
    reject(new Error(`the page can no longer draw: ${reason}`));
  };
});
tripwire.catch(() => {});

const logs: string[] = [];
const watch = (line: string, fatalPatterns: RegExp[]) => {
  logs.push(line);
  if (fatalPatterns.some((pattern) => pattern.test(line))) trip(line.slice(0, 300));
};
page.on("console", (m) => watch(`[${m.type()}] ${m.text()}`, CONSOLE_FATAL));
page.on("pageerror", (e) => watch(`[pageerror] ${e.message}`, PAGE_FATAL));
page.on("response", (r) => {
  if (r.status() >= 400) logs.push(`[http ${r.status()}] ${r.url()}`);
});

const step = async (name: string, body: () => Promise<void>) => {
  if (fatal !== null) throw new Error(`the page can no longer draw: ${fatal}`);
  try {
    const running = body();
    // It keeps running when the tripwire wins the race, and would then reject
    // with nobody left waiting on it.
    running.catch(() => {});
    await Promise.race([running, tripwire]);
    console.log(`ok   ${name}`);
  } catch (e) {
    failed = true;
    console.log(`FAIL ${name}: ${String(e).split("\n")[0]}`);
    await page.screenshot({ path: `${shots}/${tag}-fail-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    throw e;
  }
};

interface Shot {
  bytes: number;
  hash: number;
  share: number;
  colour: string;
}

let shotN = 0;
/**
 * The viewport canvas as the compositor has it, asserted to be a picture.
 *
 * `flat` is for the one shot where a single colour is the right answer: a
 * param at the end of its range can pose a fixture so a flat-coloured part
 * covers the whole frame. That shot still has to differ from the one before
 * it, which is what catches a canvas that stopped drawing at that moment.
 */
const canvasShot = async (what: string, flat: "must vary" | "may be flat" = "must vary"): Promise<Shot> => {
  const png = await page.locator("canvas[data-catchlight-viewport]").first().screenshot({
    path: `${shots}/${tag}-canvas-${shotN++}.png`,
  });
  const image = decodePng(png);
  const uniform = uniformity(image);
  const shot = {
    bytes: png.length,
    hash: fingerprint(image),
    share: uniform.share,
    colour: hex(uniform.colour, image.channels),
  };
  if (flat === "must vary" && shot.share > MAX_FLAT_SHARE) {
    throw new Error(
      `${what}: the canvas is ${(shot.share * 100).toFixed(2)}% ${shot.colour} ` +
        `over ${image.width}x${image.height} — nothing was drawn`,
    );
  }
  return shot;
};

try {
  await step("load page", async () => {
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(1500);
    const pre = page.locator("pre#failure");
    const hidden = await pre.evaluate((el) => (el as HTMLElement).hidden).catch(() => true);
    if (!hidden) throw new Error(`page reported: ${(await pre.textContent())?.trim()}`);
  });
  await step("document open", async () => {
    await page.locator("canvas[data-catchlight-viewport]").first().waitFor({ timeout: 20000 });
    await page.locator("[data-catchlight-node]").first().waitFor({ timeout: 20000 });
    await page.waitForTimeout(1500);
  });
  if (server && process.env.AGENT_CMD) {
    await step("an agent's edit over the socket reaches the tab", async () => {
      const nodesBefore = await page.locator("[data-catchlight-node]").count();
      const statusBefore = (await page.locator("[data-catchlight-status]").textContent())?.trim();
      const proc = Bun.spawn(["bash", "-lc", process.env.AGENT_CMD!], { cwd: process.env.AGENT_CWD ?? process.cwd(), stdout: "pipe", stderr: "pipe" });
      const out = await new Response(proc.stdout).text();
      const err = await new Response(proc.stderr).text();
      if ((await proc.exited) !== 0) throw new Error(`agent command failed: ${err.trim() || out.trim()}`);
      console.log("     agent said:", out.trim().split("\n")[0]);
      await page.waitForFunction((n) => document.querySelectorAll("[data-catchlight-node]").length > n, nodesBefore, { timeout: 10000 });
      await page.waitForTimeout(500);
      const statusAfter = (await page.locator("[data-catchlight-status]").textContent())?.trim();
      console.log("     status before/after:", statusBefore?.slice(0, 60), "|", statusAfter?.slice(0, 60));
      if (statusBefore === statusAfter) throw new Error("status line did not move");
    });
  }
  if (process.env.OPEN_FILE) {
    await step("open a .clm through the file input", async () => {
      await page.locator("input[type=file][data-catchlight-file-open]").first().setInputFiles(process.env.OPEN_FILE!);
      const stem = process.env.OPEN_FILE!.split("/").pop()!.replace(/\.clm$/, "");
      await page.waitForFunction((s) => (document.querySelector("[data-catchlight-status]")?.textContent ?? "").includes(s), stem, { timeout: 15000 });
      await page.locator("[data-catchlight-param-slider]").first().waitFor({ timeout: 15000 });
      await page.waitForTimeout(1000);
      console.log("     status:", (await page.locator("[data-catchlight-status]").textContent())?.trim().slice(0, 80));
    });
  }
  let before: Shot = { bytes: 0, hash: 0, share: 0, colour: "" };
  await step("canvas draws something", async () => {
    before = await canvasShot("at rest");
    console.log("     canvas png bytes:", before.bytes, "| flattest colour", before.colour, "covers", `${(before.share * 100).toFixed(1)}%`);
  });
  await step("slider poses the puppet", async () => {
    const slider = page.locator("[data-catchlight-param-slider]").first();
    if (!(await slider.count())) { console.log("     (no params in this model)"); return; }
    const max = await slider.getAttribute("max");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v; input.dispatchEvent(new Event("input", { bubbles: true }));
    }, max ?? "1");
    await page.waitForTimeout(800);
    const after = await canvasShot("with the param at its maximum", "may be flat");
    if (after.hash === before.hash) throw new Error("canvas unchanged after slider");
    // Back to the rest pose, so the drag below moves a picture with structure in it.
    const min = await slider.getAttribute("min");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v; input.dispatchEvent(new Event("input", { bubbles: true }));
    }, min ?? "0");
    await page.waitForTimeout(500);
    before = await canvasShot("back at the rest pose");
  });
  await step("select a node and drag it", async () => {
    const items = page.locator("[data-catchlight-node]");
    const n = await items.count();
    const leaf = items.nth(n - 1);
    const clickable = leaf.locator("button, [role=button], span, label").first();
    if (await clickable.count()) await clickable.click(); else await leaf.click();
    await page.waitForTimeout(300);
    const status = await page.locator("[data-catchlight-status]").textContent();
    console.log("     status:", status?.trim().slice(0, 120));
    const box = (await page.locator("canvas[data-catchlight-viewport]").first().boundingBox())!;
    const cx = box.x + box.width / 2, cy = box.y + box.height / 2;
    const b0 = await canvasShot("before the drag");
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 1; i <= 10; i++) { await page.mouse.move(cx + i * 15, cy + i * 8); await page.waitForTimeout(40); }
    await page.waitForTimeout(200);
    const mid = await canvasShot("mid-drag");
    await page.mouse.up();
    await page.waitForTimeout(1500);
    const b1 = await canvasShot("after the commit");
    const rev = await page.locator("[data-catchlight-status]").textContent();
    console.log("     hashes before/mid/after:", b0.hash, mid.hash, b1.hash, "| status:", rev?.trim().slice(0, 120));
    if (mid.hash === b0.hash) throw new Error("no live preview during drag");
    if (b1.hash === b0.hash) throw new Error("canvas unchanged after commit");
  });
  await page.screenshot({ path: `${shots}/${tag}-final.png` });
} catch {
  // reported by step
} finally {
  if (fatal !== null) console.log(`fatal: ${fatal}`);
  const bad = logs.filter((l) => /error|pageerror|warn|http 4|http 5/i.test(l));
  console.log(`console lines: ${logs.length}, errors/warnings: ${bad.length}`);
  for (const l of bad.slice(0, 15)) console.log("   ", l.slice(0, 300));
  await browser.close();
  process.exit(failed ? 1 : 0);
}
