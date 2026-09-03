/**
 * One headless-browser pass over the web editor, against one backend.
 *
 * ```text
 * bun run apps/site/e2e/drive.ts <chromium-exe> <site-url> [server-base]
 *   TIER=webgpu|webgl2|none  which tier this run is for; webgpu by default
 *   OPEN_FILE=<path.clm>  opens a model through the file input
 *   AGENT_CMD=<shell>     (connected only) an edit over the Unix socket that must reach the tab
 *   AGENT_CWD=<dir>       where that command runs; this process's directory otherwise
 *   SHOTS=<dir>           where canvas screenshots land
 *   CHROMIUM_ARGS=<args>  replaces the flag set `TIER` would choose
 * ```
 *
 * `e2e/run.ts` is what starts the servers and calls this once per pass; run it
 * by hand only to drive something they do not set up.
 *
 * **The browser is a real one, and `TIER` says which tier it may draw on.**
 * Playwright's own Chromium download does not run on NixOS, so the executable
 * is an argument and the dev shell puts one on `PATH`. The editor draws on
 * WebGPU where a browser has it and on WebGL2 where it does not, and both are
 * worth a pass because the fallback is what an iOS device below Safari 26
 * gets. So this launches Chromium with the flag set for the tier under test
 * ([`ARGS`]) and then asserts that the tab reports *that* tier: a WebGPU run
 * that quietly fell back would otherwise pass as a WebGPU run, which is the
 * one failure this file exists to catch. `TIER=none` is the negative case —
 * both tiers switched off — and it passes when the tab says what it needs.
 *
 * **The picture comes from the renderer, not from a screenshot.** A headless
 * Chromium holding a WebGPU device never composites the canvas — a screenshot
 * of it is blank however good the frame is — so [`canvasFrame`] asks the
 * viewport for its own copy through `Viewport.readback`, which answers the
 * same shape on either tier. The screenshots this still writes are of the
 * *page*, kept because a human reading a failure wants to see the DOM.
 *
 * **A step that cannot see the picture is not a passing step.** For every pose
 * but the deliberate extreme, a frame that is one flat colour is a failure: a
 * dead device leaves the page running, the DOM correct and the canvas the
 * colour it was cleared to, and every other check here would still report ok.
 * For the same reason a panic, a trap, a lost device or a tab saying it has no
 * tier to draw on aborts the run where it happens instead of being counted in
 * the summary — after one of those, the next step's timeout would report the
 * wrong failure.
 *
 * **The tripwire watches the page, not only its console.** A console line and
 * an uncaught error are two of the three places such a death shows up. The
 * third is the editor's own problem line, and it is the one a failed attach
 * lands in: a viewport that cannot attach hands the reason to its host, which
 * writes it into the status line and logs nothing. So [`checkProblem`] reads
 * that element wherever a step is about to depend on the picture — and the
 * negative run waits for that same element to fill in.
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
 * What the tab says when it found neither tier, verbatim enough to match.
 *
 * The expected end of a `TIER=none` run and a fatality in every other one:
 * `Gpu::acquire` rejects with this and the editor mounts over a canvas nothing
 * will ever reach.
 */
const NO_DEVICE = /needs WebGPU or WebGL2/i;

/**
 * Lines that mean the page is no longer able to draw, whatever it still shows.
 *
 * Catching them is what makes a run say why rather than time out on a missing
 * viewport — from the console when nothing handled the rejection, and from the
 * problem line when the editor did. [`NO_DEVICE`] is on the list for every run
 * but the negative one, where it is the answer being asked for.
 */
const CONSOLE_FATAL = [
  /panicked at/,
  /device (?:is |was )?lost/i,
  /lost the device/i,
  /context lost/i,
  ...(process.env.TIER === "none" ? [] : [NO_DEVICE]),
];
/** A wasm trap reaches the page as an error and nothing else; hence the extra pattern. */
const PAGE_FATAL = [...CONSOLE_FATAL, /unreachable/i];

/** Headless, and none of it about graphics. */
const BASE_ARGS = ["--headless=new", "--no-sandbox", "--no-first-run", "--disable-dev-shm-usage"];

/** What puts both tiers on Mesa's llvmpipe rather than on SwiftShader. */
const REAL_GPU = [
  "--enable-features=Vulkan",
  "--use-vulkan=native",
  "--use-angle=vulkan",
  "--ignore-gpu-blocklist",
];

/**
 * What a headless Chromium needs to reach a real GPU of either kind, and what
 * it takes to withhold one.
 *
 * `--use-angle=vulkan` beside `--use-vulkan=native` is what lands both tiers on
 * Mesa's llvmpipe rather than on SwiftShader, whose WebGPU device dies at the
 * first canvas configure; the ICD comes from the shell's `XDG_DATA_DIRS`, the
 * same one the Rust GPU tests find lavapipe through. On top of that the tiers
 * differ in one switch each: `--enable-unsafe-webgpu` is what gives the tab a
 * `navigator.gpu` at all, and `--disable-features=WebGPU` is what takes it
 * away, which is how the fallback is reached in a browser that would otherwise
 * have preferred WebGPU. `none` takes WebGL away as well, and is the only
 * configuration in which the editor is supposed to refuse to draw.
 *
 * `CHROMIUM_ARGS` replaces whichever set `TIER` chose, for driving a browser
 * these do not describe.
 */
const ARGS: Record<string, string[]> = {
  webgpu: [...BASE_ARGS, ...REAL_GPU, "--enable-unsafe-webgpu"],
  webgl2: [...BASE_ARGS, ...REAL_GPU, "--disable-features=WebGPU"],
  none: [...BASE_ARGS, "--disable-features=WebGPU", "--disable-webgl"],
};

const [exe, site, server] = process.argv.slice(2);
if (!exe || !site) throw new Error("usage: drive.ts <chromium> <site-url> [server-base]");
/** The probe door is asked for only where a second viewport is under test. */
const probe = process.env.TIER === "webgl2" ? "probe=1" : "";
const query = [server ? `server=${encodeURIComponent(server)}` : "", probe].filter(Boolean).join("&");
const url = query ? `${site}?${query}` : site;
const shots = process.env.SHOTS ?? ".";
/** Which tier this pass is for: what the browser is launched as, and what the tab must report. */
const tier = process.env.TIER ?? "webgpu";
const chosen = ARGS[tier];
if (!chosen) throw new Error(`TIER=${tier} is not one of ${Object.keys(ARGS).join(", ")}`);
const args = process.env.CHROMIUM_ARGS?.split(/\s+/).filter(Boolean) ?? chosen;
const tag = `${server ? "connected" : "intab"}-${tier}`;

// `headless: false` with `--headless=new` in the arguments: Playwright's own
// headless switch is not the mode a WebGPU device comes up in.
const browser = await chromium.launch({ executablePath: exe, headless: false, args });
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

/** What the status line says the device came up on, or "" before there is one. */
const tierSaid = async (): Promise<string> =>
  await page
    .evaluate(() => document.querySelector("[data-catchlight-tier]")?.textContent?.trim() ?? "")
    .catch(() => "");

interface Shot {
  size: string;
  hash: number;
  share: number;
  colour: string;
  /** The pixels behind the numbers, for a step that compares two canvases. */
  image: Image;
}

/**
 * How far two canvases drawing the same thing may differ, per channel.
 *
 * Zero is what a correct blit gives, and what the WebGL2 run has been
 * observed to give. The margin is for an sRGB surface rounding a channel by
 * one on the way through, which is a rounding difference and not a drawing
 * one; anything larger is a different picture.
 */
const MAX_CHANNEL_DRIFT = 2;

/** The largest absolute per-channel difference between two frames. */
const maxChannelDifference = (a: Image, b: Image): number => {
  let worst = 0;
  const length = Math.min(a.data.length, b.data.length);
  for (let at = 0; at < length; at++) {
    const difference = Math.abs(a.data[at]! - b.data[at]!);
    if (difference > worst) worst = difference;
  }
  return worst;
};

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
const canvasFrame = async (
  what: string,
  flat: "must vary" | "may be flat" = "must vary",
  selector = "canvas[data-catchlight-viewport]",
): Promise<Shot> => {
  // Before the wait below, so a viewport that will never arrive is reported as
  // the reason it did not rather than as this step timing out.
  await checkProblem();
  // A document swap rebuilds the viewport, and the rebuild is asynchronous.
  // Waiting is not the assertion — a canvas that never gets one fails here
  // with a timeout naming this step, which is the same verdict.
  await page.waitForFunction(
    ([property, which]: string[]) =>
      typeof (document.querySelector(which!) as unknown as Record<string, unknown> | null)?.[
        property!
      ] === "function",
    [READBACK, selector],
    { timeout: 15000 },
  );
  const read = await page.evaluate(async ([property, which]: string[]) => {
    const canvas = document.querySelector(which!);
    const readback = canvas ? (canvas as unknown as Record<string, unknown>)[property!] : undefined;
    if (typeof readback !== "function") {
      throw new Error("the canvas carries no viewport; nothing is drawing it");
    }
    const frame = (await readback()) as { width: number; height: number; rgba: Uint8Array };
    // Through the CDP boundary as an ordinary array of bytes: a typed array is
    // not part of what `evaluate` is guaranteed to carry across.
    return { width: frame.width, height: frame.height, rgba: [...frame.rgba] };
  }, [READBACK, selector]);

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
    image,
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
  if (tier === "none") {
    await step("the tab says what it needs", async () => {
      // The device is asked for at the first attach, so the sentence appears
      // once the editor has mounted and its viewport has tried. A wait rather
      // than a read: the mount, the sample and the attach are three awaits
      // deep and none of them is this file's to sequence.
      await page.waitForFunction(
        (want: string) =>
          new RegExp(want, "i").test(
            document.querySelector("[data-catchlight-problem]")?.textContent ?? "",
          ),
        NO_DEVICE.source,
        { timeout: 30000 },
      );
      const said = await page.locator("[data-catchlight-problem]").textContent();
      console.log("     problem:", said?.trim().slice(0, 160));
      console.log("     tier:", await tierSaid());
    });
    await page.screenshot({ path: `${shots}/${tag}-final.png` });
  } else {
    await draws();
  }
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

/** Everything a run that is supposed to draw does once the page has loaded. */
async function draws(): Promise<void> {
  await step(`the tab draws on ${tier}`, async () => {
    // Before anything looks at a picture: a WebGPU run that quietly fell
    // back to WebGL2 draws a perfectly good frame, and every other check
    // here would pass while the tier under test went untested.
    await page
      .waitForFunction(
        () => (document.querySelector("[data-catchlight-tier]")?.textContent ?? "") !== "no device",
        undefined,
        { timeout: 20000 },
      )
      .catch(async (cause: unknown) => {
        // A device that never arrived says so in the problem line, and that
        // sentence is a better failure than this wait running out.
        await checkProblem();
        throw cause;
      });
    const said = await tierSaid();
    console.log("     tier:", said);
    if (said !== tier) throw new Error(`the tab draws on ${said}, not on ${tier}`);
  });

  await step("document open", async () => {
    await page.locator("canvas[data-catchlight-viewport]").first().waitFor({ timeout: 20000 });
    await page.locator("[data-catchlight-node]").first().waitFor({ timeout: 20000 });
    await page.waitForTimeout(1500);
    // The device is acquired at the first attach, so this is the earliest step
    // a browser with no tier at all can be told apart from a slow one.
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
  let before: Shot = {
    size: "",
    hash: 0,
    share: 0,
    colour: "",
    image: { width: 0, height: 0, channels: 4, data: new Uint8Array() },
  };
  await step("canvas draws something", async () => {
    before = await canvasFrame("at rest");
    console.log("     frame:", before.size, "| flattest colour", before.colour, "covers", `${(before.share * 100).toFixed(1)}%`);
  });
  if (tier === "webgl2") {
    await step("a second canvas draws the same picture, and the first is untouched", async () => {
      // The whole of what the fallback tier does differently: an extra
      // viewport has no surface of its own, so it borrows the main canvas's,
      // presents through it and copies the rectangle out before the main view
      // takes the surface back. Two things can go wrong and only a browser can
      // say so. The extra's picture could come out upside down, mis-scaled or
      // in the wrong colours, which is why it is compared to the main one
      // rather than merely checked for not being flat. And the borrowing could
      // leave a mark on the main canvas, which is why that hash has to be the
      // one it had before the extra existed.
      const before = await canvasFrame("the main canvas before a second one exists");
      const failure = await page.evaluate(async () => {
        const door = (globalThis as unknown as Record<string, any>).__catchlightProbe;
        if (!door) return "the page has no probe door";
        const { editor, fitCamera } = door;
        const main = document.querySelector("canvas[data-catchlight-viewport]");
        if (!main) return "no main canvas to match";
        // Two different measurements of the same element, and both are
        // needed. The layout box is fractional and is what the resize
        // observer turns into a backing store, and the rounded pair is what
        // the React viewport passed to `fitCamera` when it framed this
        // document, so it is what reproduces the camera.
        const box = main.getBoundingClientRect();
        const framing = { width: main.clientWidth, height: main.clientHeight };
        const open = await editor.listSessions();
        const info = open[open.length - 1];
        if (!info) return "no document to draw twice";
        const session = await editor.attachSession(info);

        const canvas = document.createElement("canvas");
        canvas.setAttribute("data-e2e-extra", "");
        // Laid over the main canvas, at its position and its size, and
        // invisible. The position is not decoration: a fractional CSS box
        // snaps to a different number of device rows depending on where it
        // starts, so the same 732.42 css pixels are 733 device rows here and
        // 732 in the corner of the page — a difference the comparison below
        // would rightly call a different picture. `opacity` keeps it out of
        // the screenshots and out of nobody's way; the readback goes to the
        // renderer and never to the compositor.
        canvas.style.cssText =
          `position:fixed;left:${box.left}px;top:${box.top}px;` +
          `pointer-events:none;opacity:0;` +
          `width:${box.width}px;height:${box.height}px`;
        document.body.appendChild(canvas);
        const view = await editor.attach(session, canvas);

        // The camera the React viewport computed when it opened this
        // document: same function, same bounds, same size, same padding.
        const framed = fitCamera(session.bounds(), framing);
        if (!framed) return "the document has no bounds to frame";
        view.setCamera(framed.center[0], framed.center[1], framed.height);
        view.start();
        (globalThis as unknown as Record<string, unknown>).__catchlightProbeView = view;
        return "";
      });
      if (failure) throw new Error(failure);
      await page.waitForTimeout(1200);

      const extra = await canvasFrame("the second canvas", "must vary", "canvas[data-e2e-extra]");
      const after = await canvasFrame("the main canvas while a second one draws");
      if (extra.size !== after.size) {
        throw new Error(`the second canvas is ${extra.size} where the first is ${after.size}`);
      }
      const worst = maxChannelDifference(extra.image, after.image);
      console.log(
        "     second:",
        extra.size,
        worst === 0
          ? "| identical to the first, byte for byte"
          : `| at most ${worst} per channel off the first`,
        "| hashes", extra.hash, "and", after.hash,
        "| main hash", before.hash, "->", after.hash,
      );
      if (worst > MAX_CHANNEL_DRIFT) {
        throw new Error(
          `the second canvas differs from the first by ${worst} per channel; ` +
            "it is not drawing the same picture",
        );
      }
      if (after.hash !== before.hash) {
        throw new Error("the second viewport changed what the main canvas shows");
      }

      // Taken down before the steps below, so what they measure is the editor
      // and not this.
      await page.evaluate(() => {
        const held = globalThis as unknown as Record<string, any>;
        held.__catchlightProbeView?.dispose?.();
        held.__catchlightProbeView = undefined;
        document.querySelector("[data-e2e-extra]")?.remove();
      });
      await page.waitForTimeout(300);
    });
  }
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
  // The page, for a human reading the run afterwards. On the WebGPU tier the
  // canvas in it is blank, because that browser never composites one.
  await page.screenshot({ path: `${shots}/${tag}-final.png` });
}
