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
 *
 * **A viewport nobody can see does not run.** Started is what the host asked
 * for; running is that *and* the canvas being on screen *and* the page being
 * visible. A background tab already stops firing `requestAnimationFrame`, but
 * an editor scrolled off screen, behind a full-screen dialog, or in a collapsed
 * panel does not — it keeps ticking physics on a GPU nobody is looking at. The
 * two observers below are what close that gap; the renderer itself is told
 * nothing more than `start` and `stop`, which it already had to make
 * repeatable.
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
  /** The adapter's `max_texture_dimension_2d`, in device pixels. */
  maxSize(): number;
  free(): void;
}

/**
 * The backing-store ceiling to use when the renderer will not name one.
 *
 * The real limit is the adapter's `max_texture_dimension_2d`, which
 * [`WasmViewport.maxSize`] reports once a device exists — a canvas above it
 * fails surface configuration and the viewport goes black with no other
 * symptom. This is the floor of that limit across the WebGPU tiers, used only
 * if `maxSize` is unavailable or nonsensical, so the clamp is never a guess
 * when a fact is on hand.
 */
const FALLBACK_MAX_BACKING_STORE = 8192;

/** One canvas drawing one session. */
export class Viewport {
  #wasm: WasmViewport;
  #canvas: HTMLCanvasElement;
  #observer: ResizeObserver | undefined;
  #onScreen: IntersectionObserver | undefined;
  #unsubscribe: Unsubscribe | undefined;
  #size = { width: 0, height: 0 };
  /** What the host asked for. */
  #started = false;
  /** Whether any part of the canvas is on screen. Assumed true until observed. */
  #visible = true;
  /** Whether the page itself is visible. */
  #pageVisible = true;
  #onPageVisibility = (): void => {
    this.#pageVisible = pageIsVisible();
    this.#sync();
  };

  constructor(wasm: WasmViewport, canvas: HTMLCanvasElement, session?: Session) {
    this.#wasm = wasm;
    this.#canvas = canvas;
    if (session) this.#unsubscribe = session.onInvalidate(() => this.invalidate());
  }

  /**
   * Starts drawing, and starts following the element's size and visibility.
   *
   * Safe to call on a viewport that is already started, and safe to call again
   * after `stop` — which is what a React effect does on every remount, twice
   * over in StrictMode.
   */
  start(): void {
    this.#started = true;
    this.#observe();
    this.#watchVisibility();
    this.#sync();
  }

  /**
   * Stops drawing and stops observing. The canvas and the GPU state stay, so
   * `start` picks up where this left off.
   */
  stop(): void {
    this.#started = false;
    this.#observer?.disconnect();
    this.#observer = undefined;
    this.#onScreen?.disconnect();
    this.#onScreen = undefined;
    globalThis.document?.removeEventListener("visibilitychange", this.#onPageVisibility);
    this.#wasm.stop();
  }

  /**
   * Runs the renderer exactly when the host wants it *and* someone could see
   * it. Idempotent on both sides, which is why the renderer's `start`/`stop`
   * had to be.
   */
  #sync(): void {
    if (this.#started && this.#visible && this.#pageVisible) {
      this.#wasm.start();
    } else {
      this.#wasm.stop();
    }
  }

  /**
   * Follows whether the canvas is on screen at all.
   *
   * A zero threshold is deliberate: one visible pixel is enough to owe the user
   * a correct picture, and a partially scrolled viewport must not stutter.
   * Where `IntersectionObserver` is missing the viewport simply always runs,
   * which is what it did before.
   */
  #watchVisibility(): void {
    this.#pageVisible = pageIsVisible();
    globalThis.document?.addEventListener("visibilitychange", this.#onPageVisibility);
    if (this.#onScreen || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      const entry = entries[entries.length - 1];
      if (!entry) return;
      this.#visible = entry.isIntersecting;
      this.#sync();
    });
    observer.observe(this.#canvas);
    this.#onScreen = observer;
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
    const max = this.#maxBackingStore();
    const width = clampBackingStore(size.width, max);
    const height = clampBackingStore(size.height, max);
    if (width === this.#size.width && height === this.#size.height) return;
    this.#size = { width, height };
    // Assigning these clears the canvas, so it happens only on a real change —
    // and it has to happen before the surface is reconfigured to match.
    this.#canvas.width = width;
    this.#canvas.height = height;
    this.#wasm.resize(width, height);
  }

  /** The adapter's limit if it reported one, else the conservative floor. */
  #maxBackingStore(): number {
    const reported = this.#wasm.maxSize?.();
    return typeof reported === "number" && reported > 0
      ? reported
      : FALLBACK_MAX_BACKING_STORE;
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

/**
 * Whether the page is on screen. Absent `document` — a non-browser host, or a
 * unit test — counts as visible: the visibility rule may only ever stop a
 * viewport that is provably hidden.
 */
function pageIsVisible(): boolean {
  return globalThis.document?.visibilityState !== "hidden";
}

/** At least one pixel, at most one a surface can be configured for. */
function clampBackingStore(value: number, max: number): number {
  if (!Number.isFinite(value)) return 1;
  return Math.min(max, Math.max(1, Math.round(value)));
}
