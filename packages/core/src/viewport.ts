/**
 * The canvas half of the viewport: sizing, lifecycle, and what makes a frame
 * happen.
 *
 * The division of labour with Rust is the whole design. The renderer schedules
 * its own `requestAnimationFrame` and decides whether a frame is needed; this
 * side owns the element, because only the page knows how big it is in CSS and
 * only the browser can say what that is in device pixels. Nothing here draws,
 * measures a frame time, or holds a copy of the scene.
 *
 * **The backing store is measured in device pixels, exactly once, by the
 * browser.** `ResizeObserver` with `box: "device-pixel-content-box"` reports
 * the number of real pixels the element occupies — already correct across
 * fractional zoom, per-monitor DPI and a page moved between screens.
 * Multiplying a CSS size by `devicePixelRatio` is the same number rounded
 * twice, and the rounding error is a blurry canvas or a one-pixel seam. That
 * multiplication is the fallback, not the plan.
 *
 * **A repaint is asked for, never performed.** Every path that could change
 * the picture calls `invalidate`, including a pointer move that will call it
 * again in four milliseconds. The renderer coalesces: one animation frame
 * draws once no matter how many times it was told.
 */

import type { Session } from "./session.js";
import type { Unsubscribe } from "./session.js";

/**
 * The renderer's surface, as this package needs it — `Viewport` in
 * `@catchlight/wasm`. Declared structurally, like `WasmEditor`, so the
 * lifecycle can be tested without a GPU.
 */
export interface WasmViewport {
  start(): void;
  stop(): void;
  invalidate(): void;
  resize(width: number, height: number): void;
  setCamera(centerX: number, centerY: number, height: number): void;
  free(): void;
}

/**
 * A backing store no GPU will configure a surface for. Nothing reports the
 * adapter's real limit before a device exists, and a canvas this size is a
 * layout bug rather than an intent — an unclamped one fails surface
 * configuration and the viewport goes black with no other symptom.
 */
const MAX_BACKING_STORE = 8192;

/** One canvas drawing one session. */
export class Viewport {
  #wasm: WasmViewport;
  #canvas: HTMLCanvasElement;
  #observer: ResizeObserver | undefined;
  #unsubscribe: Unsubscribe | undefined;
  #size = { width: 0, height: 0 };

  constructor(wasm: WasmViewport, canvas: HTMLCanvasElement, session?: Session) {
    this.#wasm = wasm;
    this.#canvas = canvas;
    if (session) this.#unsubscribe = session.onInvalidate(() => this.invalidate());
  }

  /**
   * Starts drawing, and starts following the element's size.
   *
   * Safe to call on a viewport that is already started, and safe to call again
   * after `stop` — which is what a React effect does on every remount, twice
   * over in StrictMode.
   */
  start(): void {
    this.#observe();
    this.#wasm.start();
  }

  /** Stops drawing and stops observing. The canvas and the GPU state stay. */
  stop(): void {
    this.#observer?.disconnect();
    this.#observer = undefined;
    this.#wasm.stop();
  }

  /** Asks for one more frame. Cheap enough to call per pointer move. */
  invalidate(): void {
    this.#wasm.invalidate();
  }

  /** Frames `height` world units vertically, centred on `(x, y)`. */
  setCamera(x: number, y: number, height: number): void {
    this.#wasm.setCamera(x, y, height);
  }

  /** Stops everything and releases the GPU resources. */
  dispose(): void {
    this.stop();
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    this.#wasm.free();
  }

  #observe(): void {
    if (this.#observer) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[entries.length - 1];
      if (entry) this.#resize(devicePixelSize(entry, window.devicePixelRatio));
    });
    try {
      observer.observe(this.#canvas, { box: "device-pixel-content-box" });
    } catch {
      // Safari only learned the box in 16.4. Observing the default box still
      // fires on every change; `devicePixelSize` falls back to the multiply.
      observer.observe(this.#canvas);
    }
    this.#observer = observer;
    // The observer fires once on observe, but not before the next frame — and
    // the renderer would spend that frame configured for whatever the canvas
    // was constructed at. Measure now.
    this.#resize({
      width: this.#canvas.clientWidth * window.devicePixelRatio,
      height: this.#canvas.clientHeight * window.devicePixelRatio,
    });
  }

  #resize(size: { width: number; height: number }): void {
    const width = clampBackingStore(size.width);
    const height = clampBackingStore(size.height);
    if (width === this.#size.width && height === this.#size.height) return;
    this.#size = { width, height };
    // Assigning these clears the canvas, so it happens only on a real change —
    // and it has to happen before the surface is reconfigured to match.
    this.#canvas.width = width;
    this.#canvas.height = height;
    this.#wasm.resize(width, height);
  }
}

/**
 * The device-pixel size of an observed element, preferring what the browser
 * measured over what a multiplication would guess.
 *
 * Exported for its own test: this is the one piece of arithmetic in the
 * viewport, and it is the piece that makes a canvas blurry when it is wrong.
 */
export function devicePixelSize(
  entry: ResizeObserverEntry,
  ratio: number,
): { width: number; height: number } {
  const devicePixels = entry.devicePixelContentBoxSize?.[0];
  if (devicePixels) {
    // Already device pixels, already integral. Nothing to round.
    return { width: devicePixels.inlineSize, height: devicePixels.blockSize };
  }
  const cssPixels = entry.contentBoxSize?.[0];
  if (cssPixels) {
    return {
      width: Math.round(cssPixels.inlineSize * ratio),
      height: Math.round(cssPixels.blockSize * ratio),
    };
  }
  return {
    width: Math.round(entry.contentRect.width * ratio),
    height: Math.round(entry.contentRect.height * ratio),
  };
}

/** At least one pixel, at most one a surface can be configured for. */
function clampBackingStore(value: number): number {
  if (!Number.isFinite(value)) return 1;
  return Math.min(MAX_BACKING_STORE, Math.max(1, Math.round(value)));
}
