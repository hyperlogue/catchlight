import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright-core";
import { assertPresentedCanvas } from "./presentation.ts";

const executablePath = process.env.CHROMIUM ?? Bun.which("chromium");

test.skipIf(!executablePath)("presentation rejects a blank canvas despite healthy readback and DOM decorations", async () => {
  const directory = await mkdtemp(join(tmpdir(), "catchlight-presentation-"));
  const browser = await chromium.launch({
    executablePath: executablePath!,
    headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
  });
  try {
    const page = await browser.newPage({
      viewport: { width: 640, height: 480 }, deviceScaleFactor: 2,
    });
    await page.setContent(`
      <style>
        body { margin: 0; background: white; }
        [data-catchlight-stage] { position: relative; margin: 20px; width: 600px; height: 400px; }
        canvas { width: 600px; height: 400px; opacity: 0; outline-offset: -2px; }
        canvas:focus-visible { outline: 2px solid #ed6c4c; }
        [data-catchlight-scene-label] { position: absolute; left: 12px; top: 10px; color: blue; }
        [data-catchlight-zoom-tools] { position: absolute; right: 12px; bottom: 12px;
          color: white; background: black; padding: 12px; box-shadow: 0 0 6px black; }
      </style>
      <div data-catchlight-stage>
        <canvas data-catchlight-viewport width="600" height="400" tabindex="0"></canvas>
        <div data-catchlight-scene-label>SCENE / Public test artwork</div>
        <div data-catchlight-zoom-tools>− &nbsp; 100% &nbsp; +</div>
      </div>
    `);
    const readback = await page.evaluate(() => {
      const canvas = document.querySelector("canvas")!;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#171d22";
      context.fillRect(0, 0, 600, 400);
      context.fillStyle = "#ed6c4c";
      context.fillRect(200, 80, 100, 240);
      context.fillStyle = "#56aee8";
      context.fillRect(300, 80, 100, 240);
      const read = () => ({ width: 600, height: 400,
        rgba: context.getImageData(0, 0, 600, 400).data });
      Object.defineProperty(canvas, "__catchlightReadback", { value: read });
      const { rgba } = read();
      return { left: [...rgba.slice((200 * 600 + 250) * 4, (200 * 600 + 250) * 4 + 4)],
        right: [...rgba.slice((200 * 600 + 350) * 4, (200 * 600 + 350) * 4 + 4)] };
    });
    expect(readback.left).toEqual([237, 108, 76, 255]);
    expect(readback.right).toEqual([86, 174, 232, 255]);
    await expect(assertPresentedCanvas(page, "hidden artwork", join(directory, "blank.png")))
      .rejects.toThrow("presented canvas is 100.00%");

    await page.locator("canvas").evaluate((canvas) => { canvas.style.opacity = "1"; });
    await assertPresentedCanvas(page, "visible artwork", join(directory, "visible.png"));

    await page.locator("canvas").evaluate((element) => {
      const canvas = element as HTMLCanvasElement;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "white";
      context.fillRect(0, 0, canvas.width, canvas.height);
    });
    await page.keyboard.press("Tab");
    expect(await page.locator("canvas").evaluate((canvas) =>
      canvas.matches(":focus-visible"))).toBe(true);
    await expect(assertPresentedCanvas(page, "focused blank canvas", join(directory, "focused.png")))
      .rejects.toThrow("presented canvas is 100.00%");

    await page.locator("[data-catchlight-stage]").evaluate((stage) => {
      const grid = document.createElement("div");
      grid.setAttribute("data-catchlight-grid-overlay", "");
      stage.append(grid);
    });
    await expect(assertPresentedCanvas(page, "decorated artwork", join(directory, "grid.png")))
      .rejects.toThrow("before grid or selection overlays");
  } finally {
    await browser.close();
    await rm(directory, { recursive: true, force: true });
  }
}, 15000);
