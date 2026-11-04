/** Assert browser presentation separately from the renderer's readback. */
import type { Page } from "playwright-core";
import { hex, uniformity } from "./frame.ts";

const MAX_FLAT_SHARE = 0.995;
const DECORATIONS = [
  "[data-catchlight-scene-label]",
  "[data-catchlight-canvas-tools]",
  "[data-catchlight-zoom-tools]",
  "[data-catchlight-canvas-hint]",
].join(",");

/** Capture the resting canvas before a readback can trigger another draw. */
export async function assertPresentedCanvas(
  page: Page,
  what: string,
  path: string,
): Promise<void> {
  const deadline = Date.now() + 2000;
  for (;;) {
    const region = await page.evaluate((decorations) => {
      const canvas = document.querySelector("canvas[data-catchlight-viewport]");
      if (!canvas) throw new Error("the model canvas is missing");
      const stage = canvas.closest("[data-catchlight-stage]");
      if (stage?.querySelector(
        "[data-catchlight-grid-overlay], [data-catchlight-selection-overlay]",
      )) {
        throw new Error("presentation must be checked before grid or selection overlays");
      }
      const box = canvas.getBoundingClientRect();
      // Exclude the canvas's inward keyboard-focus outline.
      const x = Math.ceil(Math.max(0, box.left) + 4);
      const y = Math.ceil(Math.max(0, box.top) + 4);
      const right = Math.floor(Math.min(innerWidth, box.right) - 4);
      const bottom = Math.floor(Math.min(innerHeight, box.bottom) - 4);
      if (right <= x || bottom <= y) throw new Error("the model canvas is not on screen");
      const clip = { x, y, width: right - x, height: bottom - y };
      const masks = [...document.querySelectorAll(decorations)].map((element) => {
        const rect = element.getBoundingClientRect();
        // Include decoration shadows, which also appear in the screenshot.
        return { left: rect.left - 12, top: rect.top - 12,
          right: rect.right + 12, bottom: rect.bottom + 12 };
      });
      return { clip, masks };
    }, DECORATIONS);

    const png = await page.screenshot({ path, clip: region.clip, scale: "css" });
    const sampled = await page.evaluate(async ({ png, clip, masks }) => {
      const bytes = Uint8Array.from(atob(png), (character) => character.charCodeAt(0));
      const bitmap = await createImageBitmap(new Blob([bytes], { type: "image/png" }));
      const scratch = new OffscreenCanvas(bitmap.width, bitmap.height);
      const context = scratch.getContext("2d")!;
      context.drawImage(bitmap, 0, 0);
      bitmap.close();
      const { width, height } = scratch;
      const rgba = context.getImageData(0, 0, width, height).data;
      const pixels = new Uint8Array(rgba.length);
      let length = 0;
      for (let y = 0; y < height; y++) {
        const pageY = clip.y + (y + 0.5) * clip.height / height;
        for (let x = 0; x < width; x++) {
          const pageX = clip.x + (x + 0.5) * clip.width / width;
          if (masks.some((mask) => pageX >= mask.left && pageX <= mask.right
            && pageY >= mask.top && pageY <= mask.bottom)) continue;
          const at = (y * width + x) * 4;
          pixels.set(rgba.subarray(at, at + 4), length);
          length += 4;
        }
      }
      const chunks: string[] = [];
      for (let at = 0; at < length; at += 16384) {
        chunks.push(String.fromCharCode(...pixels.subarray(at, Math.min(at + 16384, length))));
      }
      return { width, height, rgba: btoa(chunks.join("")) };
    }, { png: png.toString("base64"), ...region });
    const pixels = Buffer.from(sampled.rgba, "base64");
    const count = pixels.length / 4;
    if (count < sampled.width * sampled.height / 2) {
      throw new Error(`${what}: decorations obscure most of the model canvas`);
    }
    const { share, colour } = uniformity({
      width: count, height: 1, channels: 4, data: pixels,
    });
    if (share <= MAX_FLAT_SHARE) return;
    if (Date.now() >= deadline) {
      throw new Error(
        `${what}: presented canvas is ${(share * 100).toFixed(2)}% ${hex(colour, 4)}; ` +
        `the model is not visible (screenshot: ${path})`,
      );
    }
    await page.waitForTimeout(150);
  }
}
