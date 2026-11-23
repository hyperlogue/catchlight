/**
 * One automated browser pass over the web editor, against one backend.
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
 * `e2e/run.ts` starts the servers and calls this once per pass. Playwright's
 * Chromium download does not run on NixOS, so the executable is an argument;
 * the dev shell puts one on `PATH`. WebGPU needs an X11 display (`run.ts`
 * supplies Xvfb). The tab must report the tier under test, so a WebGPU run
 * that silently falls back to WebGL2 fails. `TIER=none` passes when the tab
 * says it needs WebGPU or WebGL2.
 */

import { READBACK } from "@catchlight/core";
import { openModel, selectNode } from "./model.ts";
import { studio } from "./studio.ts";
import { snapshots } from "./snapshots.ts";
import { assertPresentedCanvas } from "./presentation.ts";
import { workspaces } from "./workspaces.ts";
import { draftClose, drawerKeyboard } from "./draft-close.ts";
import { chromium } from "playwright-core";

import { fingerprint, hex, uniformity } from "./frame.ts";
import type { Image } from "./frame.ts";

/**
 * How much of the canvas one colour may cover before the frame reads as blank.
 * A dead GPU device leaves the DOM healthy, so this is the only place it shows;
 * the margin below 1 is for antialiasing along a model's edge.
 */
const MAX_FLAT_SHARE = 0.995;

/** What the tab says when it found neither tier: expected for `TIER=none`, fatal otherwise. */
const NO_DEVICE = /needs WebGPU or WebGL2/i;

const CONSOLE_FATAL = [
  /panicked at/,
  /device (?:is |was )?lost/i,
  /lost the device/i,
  /context lost/i,
  ...(process.env.TIER === "none" ? [] : [NO_DEVICE]),
];
/** A wasm trap reaches the page as an error and nothing else; hence the extra pattern. */
const PAGE_FATAL = [...CONSOLE_FATAL, /unreachable/i];

const BASE_ARGS = [
  "--no-sandbox",
  "--no-first-run",
  "--disable-dev-shm-usage",
];

/** WebGL2 on Mesa through ANGLE Vulkan. */
const WEBGL_GPU = [
  "--enable-features=Vulkan",
  "--use-vulkan=native",
  "--use-angle=vulkan",
  "--ignore-gpu-blocklist",
];

/**
 * Linux/Mesa defaults. WebGPU uses Graphite on X11 so frames reach the
 * compositor; WebGL2 can capture headless.
 */
const ARGS: Record<string, string[]> = {
  webgpu: [
    ...BASE_ARGS,
    "--ozone-platform=x11",
    "--enable-skia-graphite",
    "--skia-graphite-dawn-backend=vulkan",
    "--use-angle=vulkan",
    "--ignore-gpu-blocklist",
    "--enable-unsafe-webgpu",
  ],
  webgl2: [...BASE_ARGS, "--headless=new", ...WEBGL_GPU, "--disable-features=WebGPU"],
  none: [...BASE_ARGS, "--headless=new", "--disable-features=WebGPU", "--disable-webgl"],
};

const [exe, site, server] = process.argv.slice(2);
if (!exe || !site)
  throw new Error("usage: drive.ts <chromium> <site-url> [server-base]");
const probe = "probe=1";
const query = [server ? `server=${encodeURIComponent(server)}` : "", probe]
  .filter(Boolean)
  .join("&");
const url = query ? `${site}?${query}` : site;
const shots = process.env.SHOTS ?? ".";
const tier = process.env.TIER ?? "webgpu";
const chosen = ARGS[tier];
if (!chosen)
  throw new Error(`TIER=${tier} is not one of ${Object.keys(ARGS).join(", ")}`);
const args = process.env.CHROMIUM_ARGS?.split(/\s+/).filter(Boolean) ?? chosen;
const tag = `${server ? "connected" : "intab"}-${tier}`;

if (tier === "webgpu" && !process.env.CHROMIUM_ARGS && !process.env.DISPLAY) {
  throw new Error("WebGPU presentation needs X11; use e2e/run.ts or xvfb-run -a");
}
const browser = await chromium.launch({
  executablePath: exe,
  headless: false,
  args,
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

let failed = false;
let fatal: string | null = null;
let trip: (reason: string) => void = () => {};
// Every step races this, so a page death is reported instead of a later timeout.
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
  if (fatalPatterns.some((pattern) => pattern.test(line)))
    trip(line.slice(0, 300));
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
    running.catch(() => {});
    await Promise.race([running, tripwire]);
    console.log(`ok   ${name}`);
  } catch (e) {
    failed = true;
    console.log(`FAIL ${name}: ${e instanceof Error ? e.stack : String(e)}`);
    await page
      .screenshot({
        path: `${shots}/${tag}-fail-${name.replace(/\W+/g, "-")}.png`,
      })
      .catch(() => {});
    throw e;
  }
};

/**
 * A failed attach is handled by the editor and logs nothing; it only reaches
 * the problem line. `evaluate` rather than a locator, because the element is
 * absent on a healthy page and a locator would wait for it.
 */
const checkProblem = async (): Promise<void> => {
  const said = await page
    .evaluate(
      () =>
        document
          .querySelector("[data-catchlight-problem]")
          ?.textContent?.trim() ?? "",
    )
    .catch(() => "");
  if (!said) return;
  const line = `[problem] ${said}`;
  if (!PAGE_FATAL.some((pattern) => pattern.test(line))) return;
  watch(line, PAGE_FATAL);
  throw new Error(`the page can no longer draw: ${line}`);
};

const tierSaid = async (): Promise<string> =>
  await page
    .evaluate(
      () =>
        document.querySelector("[data-catchlight-tier]")?.textContent?.trim() ??
        "",
    )
    .catch(() => "");

interface Shot {
  size: string;
  hash: number;
  share: number;
  colour: string;
  image: Image;
}

/** Per-channel tolerance between two canvases: an sRGB surface may round by one. */
const MAX_CHANNEL_DRIFT = 2;

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
 * Redraw and read the renderer's surface; screenshots separately check that
 * the frame is presented. `"may be flat"` is for a param at the end of its
 * range, which can pose a flat-coloured part over the whole frame.
 */
const canvasFrame = async (
  what: string,
  flat: "must vary" | "may be flat" = "must vary",
  selector = "canvas[data-catchlight-viewport]",
): Promise<Shot> => {
  await checkProblem();
  // A model swap rebuilds the viewport asynchronously.
  await page.waitForFunction(
    ([property, which]: string[]) =>
      typeof (
        document.querySelector(which!) as unknown as Record<
          string,
          unknown
        > | null
      )?.[property!] === "function",
    [READBACK, selector],
    { timeout: 15000 },
  );
  const read = await page.evaluate(
    async ([property, which]: string[]) => {
      const canvas = document.querySelector(which!);
      const readback = canvas
        ? (canvas as unknown as Record<string, unknown>)[property!]
        : undefined;
      if (typeof readback !== "function") {
        throw new Error(
          "the canvas carries no viewport; nothing is drawing it",
        );
      }
      const frame = (await readback()) as {
        width: number;
        height: number;
        rgba: Uint8Array;
      };
      // `evaluate` is not guaranteed to carry a typed array across CDP.
      return {
        width: frame.width,
        height: frame.height,
        rgba: [...frame.rgba],
      };
    },
    [READBACK, selector],
  );

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
  page.setDefaultTimeout(90000);
  await step("load page", async () => {
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(1500);
    const pre = page.locator("pre#failure");
    const hidden = await pre
      .evaluate((el) => (el as HTMLElement).hidden)
      .catch(() => true);
    if (!hidden)
      throw new Error(`page reported: ${(await pre.textContent())?.trim()}`);
  });
  if (!server) {
    await step("the demo preloads Lan", async () => {
      await page.locator("[data-catchlight-session][data-current]").waitFor();
      const tip = await page.evaluate(() => {
        const { editor } = (globalThis as unknown as { __catchlightProbe: { editor: import("@catchlight/core").Editor } }).__catchlightProbe;
        const id = Number(document.querySelector("[data-catchlight-session][data-current]")!.getAttribute("data-session"));
        return editor.session(id)?.nodeInfo("motion/front_hair_chain_1_link_2");
      });
      if (tip?.kind !== "motion_response" || tip.parent !== "motion/front_hair_chain_1_link_1")
        throw new Error("expected the bundled Lan hair response at startup");
    });
  }

  if (tier === "none") {
    await step("the tab says what it needs", async () => {
      await page.waitForFunction(
        (want: string) =>
          new RegExp(want, "i").test(
            document.querySelector("[data-catchlight-problem]")?.textContent ??
              "",
          ),
        NO_DEVICE.source,
        { timeout: 30000 },
      );
      const said = await page
        .locator("[data-catchlight-problem]")
        .textContent();
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

async function draws(): Promise<void> {
  await step(`the tab draws on ${tier}`, async () => {
    await page.locator("[data-catchlight-tier]")
      .filter({ hasText: /^(webgpu|webgl2)$/ })
      .waitFor()
      .catch(async (cause: unknown) => {
        await checkProblem();
        throw cause;
      });
    const said = await tierSaid();
    console.log("     tier:", said);
    if (said !== tier)
      throw new Error(`the tab draws on ${said}, not on ${tier}`);
  });

  await step("model open", async () => {
    await page
      .locator("canvas[data-catchlight-viewport]")
      .first()
      .waitFor({ timeout: 20000 });
    await page
      .locator("[data-catchlight-node]")
      .first()
      .waitFor({ timeout: 20000 });
    await page.waitForTimeout(1500);
    await checkProblem();
  });
  await step("startup canvas is visible", () =>
    assertPresentedCanvas(page, "startup", `${shots}/${tag}-startup-canvas.png`));
  if (server && process.env.AGENT_CMD) {
    await step("an agent's edit over the socket reaches the tab", async () => {
      const nodesBefore = await page.locator("[data-catchlight-node]").count();
      const statusBefore = (
        await page.locator("[data-catchlight-status]").textContent()
      )?.trim();
      const proc = Bun.spawn(["bash", "-lc", process.env.AGENT_CMD!], {
        cwd: process.env.AGENT_CWD ?? process.cwd(),
        stdout: "pipe",
        stderr: "pipe",
      });
      const out = await new Response(proc.stdout).text();
      const err = await new Response(proc.stderr).text();
      if ((await proc.exited) !== 0)
        throw new Error(`agent command failed: ${err.trim() || out.trim()}`);
      console.log("     agent said:", out.trim().split("\n")[0]);
      await page.waitForFunction(
        (n) => document.querySelectorAll("[data-catchlight-node]").length > n,
        nodesBefore,
        { timeout: 10000 },
      );
      await page.waitForTimeout(500);
      const statusAfter = (
        await page.locator("[data-catchlight-status]").textContent()
      )?.trim();
      console.log(
        "     status before/after:",
        statusBefore?.slice(0, 60),
        "|",
        statusAfter?.slice(0, 60),
      );
    });
  }
  if (process.env.OPEN_FILE) {
    await step("open a .clm through the file input", async () => {
      await openModel(page);
      const stem = process.env
        .OPEN_FILE!.split("/")
        .pop()!
        .replace(/\.clm$/, "");
      await page.waitForFunction(
        (s) =>
          (
            document.querySelector("[data-catchlight-status]")?.textContent ??
            ""
          ).includes(s),
        stem,
        { timeout: 15000 },
      );
      await page
        .locator("[data-catchlight-param-slider]")
        .first()
        .waitFor({ timeout: 15000 });
      await page.waitForTimeout(1000);
      console.log(
        "     status:",
        (await page.locator("[data-catchlight-status]").textContent())
          ?.trim()
          .slice(0, 80),
      );
    });
    await step("opened model is visible", () =>
      assertPresentedCanvas(page, "opened model", `${shots}/${tag}-opened-canvas.png`));
  }
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await step("snapshot bytes match their receipt and forks edit independently", () => snapshots(page));
  let before: Shot = {
    size: "",
    hash: 0,
    share: 0,
    colour: "",
    image: { width: 0, height: 0, channels: 4, data: new Uint8Array() },
  };
  await step("canvas draws something", async () => {
    before = await canvasFrame("at rest");
    console.log(
      "     frame:",
      before.size,
      "| flattest colour",
      before.colour,
      "covers",
      `${(before.share * 100).toFixed(1)}%`,
    );
  });
  if (tier === "webgl2") {
    await step(
      "a second canvas draws the same picture, and the first is untouched",
      async () => {
        // On WebGL2 an extra viewport borrows the main canvas's surface. Its
        // picture must match the main one, and the main one must be unchanged.
        const before = await canvasFrame(
          "the main canvas before a second one exists",
        );
        const failure = await page.evaluate(async () => {
          const door = (globalThis as unknown as Record<string, any>)
            .__catchlightProbe;
          if (!door) return "the page has no probe door";
          const { editor, fitCamera } = door;
          const main = document.querySelector(
            "canvas[data-catchlight-viewport]",
          );
          if (!main) return "no main canvas to match";
          // The fractional box sizes the backing store; the rounded pair is
          // what the React viewport passed to `fitCamera`.
          const box = main.getBoundingClientRect();
          const framing = {
            width: main.clientWidth,
            height: main.clientHeight,
          };
          const open = await editor.listSessions();
          const info = open[open.length - 1];
          if (!info) return "no model to draw twice";
          const session = await editor.attachSession(info);

          const canvas = document.createElement("canvas");
          canvas.setAttribute("data-e2e-extra", "");
          // Same position as the main canvas: a fractional CSS box snaps to
          // a different number of device rows depending on where it starts.
          canvas.style.cssText =
            `position:fixed;left:${box.left}px;top:${box.top}px;` +
            `pointer-events:none;opacity:0;` +
            `width:${box.width}px;height:${box.height}px`;
          document.body.appendChild(canvas);
          const view = await editor.attach(session, canvas);

          const framed = fitCamera(session.bounds(), framing);
          if (!framed) return "the model has no bounds to frame";
          view.setCamera(framed.center[0], framed.center[1], framed.height);
          view.start();
          (
            globalThis as unknown as Record<string, unknown>
          ).__catchlightProbeView = view;
          return "";
        });
        if (failure) throw new Error(failure);
        await page.waitForTimeout(1200);

        const extra = await canvasFrame(
          "the second canvas",
          "must vary",
          "canvas[data-e2e-extra]",
        );
        const after = await canvasFrame(
          "the main canvas while a second one draws",
        );
        if (extra.size !== after.size) {
          throw new Error(
            `the second canvas is ${extra.size} where the first is ${after.size}`,
          );
        }
        const worst = maxChannelDifference(extra.image, after.image);
        console.log(
          "     second:",
          extra.size,
          worst === 0
            ? "| identical to the first, byte for byte"
            : `| at most ${worst} per channel off the first`,
          "| hashes",
          extra.hash,
          "and",
          after.hash,
          "| main hash",
          before.hash,
          "->",
          after.hash,
        );
        if (worst > MAX_CHANNEL_DRIFT) {
          throw new Error(
            `the second canvas differs from the first by ${worst} per channel; ` +
              "it is not drawing the same picture",
          );
        }
        if (after.hash !== before.hash) {
          throw new Error(
            "the second viewport changed what the main canvas shows",
          );
        }

        await page.evaluate(() => {
          const held = globalThis as unknown as Record<string, any>;
          held.__catchlightProbeView?.dispose?.();
          held.__catchlightProbeView = undefined;
          document.querySelector("[data-e2e-extra]")?.remove();
        });
        await page.waitForTimeout(300);
      },
    );
  }
  await page.getByRole("button", { name: "Arrange", exact: true }).click();
  await step("slider poses the puppet", async () => {
    before = await canvasFrame("before posing");
    const slider = page.locator("[data-catchlight-param-slider]").first();
    if (!(await slider.count())) {
      console.log("     (no params in this model)");
      return;
    }
    const max = await slider.getAttribute("max");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, max ?? "1");
    await page.waitForTimeout(800);
    const after = await canvasFrame(
      "with the param at its maximum",
      "may be flat",
    );
    if (after.hash === before.hash)
      throw new Error("canvas unchanged after slider");
    const min = await slider.getAttribute("min");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, min ?? "0");
    await page.waitForTimeout(500);
    before = await canvasFrame("back at the rest pose");
  });
  await step("select a node and drag it", async () => {
    await selectNode(page, "front-hair");
    const box = (await page.locator("[data-catchlight-selection-box]").boundingBox())!;
    const cx = box.x + box.width / 2, cy = box.y + box.height / 2;
    const b0 = await canvasFrame("before the drag");
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 1; i <= 10; i++) {
      await page.mouse.move(cx + i * 15, cy + i * 8);
      await page.waitForTimeout(40);
    }
    await page.waitForTimeout(200);
    const mid = await canvasFrame("mid-drag");
    await page.mouse.up();
    await page.waitForTimeout(1500);
    const b1 = await canvasFrame("after the commit");
    const rev = await page.locator("[data-catchlight-status]").textContent();
    console.log(
      "     hashes before/mid/after:",
      b0.hash,
      mid.hash,
      b1.hash,
      "| status:",
      rev?.trim().slice(0, 120),
    );
    if (mid.hash === b0.hash) throw new Error("no live preview during drag");
    if (b1.hash === b0.hash) throw new Error("canvas unchanged after commit");
  });
  await workspaces(page, step, shots, tag);
  await studio(page, step, shots, tag);
  await draftClose(page, step, shots, tag);
  await drawerKeyboard(page, step, shots, tag);
  await page.screenshot({ path: `${shots}/${tag}-final.png` });
}
