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
 * **The browser is a real one, and it draws on WebGPU.** Playwright's own
 * Chromium download does not run on NixOS, so the executable is an argument
 * and the e2e dev shell puts one on `PATH`. The editor is WebGPU-only, and
 * headless Chromium reaches a real device only with the whole flag set below:
 * `--enable-unsafe-webgpu` to have `navigator.gpu` at all, and
 * `--use-angle=vulkan` beside `--use-vulkan=native` so it lands on Mesa's
 * llvmpipe rather than on SwiftShader, whose device dies at the first canvas
 * configure. The ICD comes from the shell's `XDG_DATA_DIRS`, the same one the
 * Rust GPU tests find lavapipe through. Dropping any of those flags is the
 * negative case: the tab reports that it needs WebGPU and this run fails
 * saying so.
 *
 * **The picture comes from the renderer, not from a screenshot.** That
 * configuration never composites the canvas — a screenshot of it is blank
 * however good the frame is — so [`canvasFrame`] asks the viewport to copy
 * its own surface back through `Viewport.readback`. The screenshots this
 * still writes are of the *page*, kept because a human reading a failure
 * wants to see the DOM: the canvas inside them is blank by construction and
 * says nothing.
 *
 * **A step that cannot see the picture is not a passing step.** For every pose
 * but the deliberate extreme, a frame that is one flat colour is a failure: a
 * dead device leaves the page running, the DOM correct and the canvas the
 * colour it was cleared to, and every other check here would still report ok.
 * For the same reason a panic, a trap, a lost device or a tab saying it has no
 * WebGPU aborts the run where it happens instead of being counted in the
 * summary — after one of those, the next step's timeout would report the wrong
 * failure.
 *
 * **The tripwire watches the page, not only its console.** A console line and
 * an uncaught error are two of the three places such a death shows up. The
 * third is the editor's own problem line, and it is the one the negative case
 * lands in: a viewport that cannot attach hands the reason to its host, which
 * writes it into the status line and logs nothing. So [`checkProblem`] reads
 * that element wherever a step is about to depend on the picture.
 */

import { READBACK } from "@catchlight/core";
import { chromium } from "playwright-core";

import { fingerprint, hex, uniformity } from "./frame.ts";
import type { Image } from "./frame.ts";

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

/**
 * Lines that mean the page is no longer able to draw, whatever it still shows.
 *
 * The WebGPU one is the whole negative case: launched without the flags above,
 * `Gpu::acquire` rejects with that string and the editor mounts over a canvas
 * nothing will ever reach. Catching it is what makes the run say why rather
 * than time out on a missing viewport — from the console when nothing handled
 * the rejection, and from the problem line when the editor did.
 */
const CONSOLE_FATAL = [
  /panicked at/,
  /device (?:is |was )?lost/i,
  /lost the device/i,
  /context lost/i,
  /has no WebGPU/i,
];
/** A wasm trap reaches the page as an error and nothing else; hence the extra pattern. */
const PAGE_FATAL = [...CONSOLE_FATAL, /unreachable/i];

/**
 * What it takes to hold a WebGPU device in a headless Chromium on llvmpipe.
 *
 * Every one of the four graphics flags is load-bearing; see the header.
 * `CHROMIUM_ARGS` replaces the set outright, which is how the negative case is
 * run by hand: the old WebGL flags select a browser with no `navigator.gpu`
 * and this must then fail rather than pass.
 */
const DEFAULT_ARGS = [
  "--headless=new",
  "--no-sandbox",
  "--no-first-run",
  "--disable-dev-shm-usage",
  "--enable-unsafe-webgpu",
  "--enable-features=Vulkan",
  "--use-vulkan=native",
  "--use-angle=vulkan",
  "--ignore-gpu-blocklist",
].join(" ");

const [exe, site, server] = process.argv.slice(2);
if (!exe || !site) throw new Error("usage: drive.ts <chromium> <site-url> [server-base]");
const url = server ? `${site}?server=${encodeURIComponent(server)}` : site;
const shots = process.env.SHOTS ?? ".";
const tag = server ? "connected" : "intab";

// `headless: false` with `--headless=new` in the arguments: Playwright's own
// headless switch is not the mode a WebGPU device comes up in.
const browser = await chromium.launch({
  executablePath: exe,
  headless: false,
  args: (process.env.CHROMIUM_ARGS ?? DEFAULT_ARGS).split(/\s+/).filter(Boolean),
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

/**
 * The editor's own problem line, read for a reason to stop.
 *
 * A failure the editor *handles* reaches neither of the tripwire's other two
 * sources: the viewport hands a failed attach to its host, and the host puts
 * the sentence in this one element. Without this read the negative case dies
 * on the next step's fifteen-second timeout and never says WebGPU.
 *
 * `evaluate` rather than a locator, because the element is absent on a healthy
 * page and a locator would wait thirty seconds to discover that.
 */
const checkProblem = async (): Promise<void> => {
  const said = await page
    .evaluate(() => document.querySelector("[data-catchlight-problem]")?.textContent?.trim() ?? "")
    .catch(() => "");
  if (!said) return;
  const line = `[problem] ${said}`;
  if (!PAGE_FATAL.some((pattern) => pattern.test(line))) return;
  watch(line, PAGE_FATAL);
  // The tripwire has already rejected; this only stops the caller walking into
  // a wait whose answer no longer matters.
  throw new Error(`the page can no longer draw: ${line}`);
};

interface Shot {
  size: string;
  hash: number;
  share: number;
  colour: string;
}

/**
 * The frame the viewport is showing, asserted to be a picture.
 *
 * The pixels are the renderer's own copy of its surface, reached through the
 * property a live viewport hangs off its canvas — the element is all a driver
 * in the page has a handle to. Nothing here goes near the compositor, which in
 * this browser has never seen the canvas.
 *
 * `flat` is for the one shot where a single colour is the right answer: a
 * param at the end of its range can pose a fixture so a flat-coloured part
 * covers the whole frame. That shot still has to differ from the one before
 * it, which is what catches a canvas that stopped drawing at that moment.
 */
const canvasFrame = async (what: string, flat: "must vary" | "may be flat" = "must vary"): Promise<Shot> => {
  // Before the wait below, so a viewport that will never arrive is reported as
  // the reason it did not rather than as this step timing out.
  await checkProblem();
  // A document swap rebuilds the viewport, and the rebuild is asynchronous.
  // Waiting is not the assertion — a canvas that never gets one fails here
  // with a timeout naming this step, which is the same verdict.
  await page.waitForFunction(
    (property: string) =>
      typeof (document.querySelector("canvas[data-catchlight-viewport]") as unknown as
        | Record<string, unknown>
        | null)?.[property] === "function",
    READBACK,
    { timeout: 15000 },
  );
  const read = await page.evaluate(async (property: string) => {
    const canvas = document.querySelector("canvas[data-catchlight-viewport]");
    const readback = canvas ? (canvas as unknown as Record<string, unknown>)[property] : undefined;
    if (typeof readback !== "function") {
      throw new Error("the canvas carries no viewport; nothing is drawing it");
    }
    const frame = (await readback()) as { width: number; height: number; rgba: Uint8Array };
    // Through the CDP boundary as an ordinary array of bytes: a typed array is
    // not part of what `evaluate` is guaranteed to carry across.
    return { width: frame.width, height: frame.height, rgba: [...frame.rgba] };
  }, READBACK);

  const image: Image = {
    width: read.width,
    height: read.height,
    channels: 4,
    data: Uint8Array.from(read.rgba),
  };
  const uniform = uniformity(image);
  const shot = {
    size: `${image.width}x${image.height}`,
    hash: fingerprint(image),
    share: uniform.share,
    colour: hex(uniform.colour, image.channels),
  };
  if (flat === "must vary" && shot.share > MAX_FLAT_SHARE) {
    throw new Error(
      `${what}: the frame is ${(shot.share * 100).toFixed(2)}% ${shot.colour} ` +
        `over ${shot.size} — nothing was drawn`,
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
    // The device is acquired at the first attach, so this is the earliest step
    // a browser without WebGPU can be told apart from a slow one.
    await checkProblem();
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
  let before: Shot = { size: "", hash: 0, share: 0, colour: "" };
  await step("canvas draws something", async () => {
    before = await canvasFrame("at rest");
    console.log("     frame:", before.size, "| flattest colour", before.colour, "covers", `${(before.share * 100).toFixed(1)}%`);
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
    const after = await canvasFrame("with the param at its maximum", "may be flat");
    if (after.hash === before.hash) throw new Error("canvas unchanged after slider");
    // Back to the rest pose, so the drag below moves a picture with structure in it.
    const min = await slider.getAttribute("min");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v; input.dispatchEvent(new Event("input", { bubbles: true }));
    }, min ?? "0");
    await page.waitForTimeout(500);
    before = await canvasFrame("back at the rest pose");
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
    const b0 = await canvasFrame("before the drag");
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 1; i <= 10; i++) { await page.mouse.move(cx + i * 15, cy + i * 8); await page.waitForTimeout(40); }
    await page.waitForTimeout(200);
    const mid = await canvasFrame("mid-drag");
    await page.mouse.up();
    await page.waitForTimeout(1500);
    const b1 = await canvasFrame("after the commit");
    const rev = await page.locator("[data-catchlight-status]").textContent();
    console.log("     hashes before/mid/after:", b0.hash, mid.hash, b1.hash, "| status:", rev?.trim().slice(0, 120));
    if (mid.hash === b0.hash) throw new Error("no live preview during drag");
    if (b1.hash === b0.hash) throw new Error("canvas unchanged after commit");
  });
  // The page, for a human reading the run afterwards. The canvas in it is
  // blank: this browser never composites one.
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
