/**
 * The canvas half of the viewport: sizing and lifecycle. The renderer owns the
 * frame loop; this side owns the element. The backing store comes from
 * `device-pixel-content-box`, because multiplying a CSS size by
 * `devicePixelRatio` rounds twice and blurs. A viewport runs only while
 * started, on screen, and the page is visible.
 */

import type { Unsubscribe } from "./backend.js";
import type { Session } from "./session.js";
import type { WasmFrame, WasmViewport } from "./wasm.js";

/**
 * Fallback backing-store ceiling: the floor of `max_texture_dimension_2d`
 * across browser tiers, used only without a device to ask. Exceeding the real
 * limit fails surface configuration and the viewport goes black silently.
 */
const FALLBACK_MAX_BACKING_STORE = 8192;

/**
 * Canvas property resolving to the current frame, for test drivers that run in
 * the page and cannot import this bundle.
 */
export const READBACK = "__catchlightReadback";

export class Viewport {
  #wasm: WasmViewport;
  #canvas: HTMLCanvasElement;
  #readback: () => Promise<WasmFrame>;
  #observer: ResizeObserver | undefined;
  #onScreen: IntersectionObserver | undefined;
  #unsubscribe: Unsubscribe | undefined;
  #size = { width: 0, height: 0 };
  #maxSize: number;
  #started = false;
  /** Assumed true until observed. */
  #visible = true;
  #pageVisible = true;
  #onPageVisibility = (): void => {
    this.#pageVisible = pageIsVisible();
    this.#sync();
  };

  constructor(
    wasm: WasmViewport,
    canvas: HTMLCanvasElement,
    session?: Session,
    maxSize?: number,
  ) {
    this.#wasm = wasm;
    this.#canvas = canvas;
    this.#maxSize =
      typeof maxSize === "number" && maxSize > 0 ? maxSize : FALLBACK_MAX_BACKING_STORE;
    if (session) this.#unsubscribe = session.onInvalidate(() => this.invalidate());
    this.#readback = (): Promise<WasmFrame> => this.readback();
    Object.defineProperty(canvas, READBACK, { value: this.#readback, configurable: true });
  }

  /** Device pixels, RGBA. For tests; redraws once and advances nothing. */
  readback(): Promise<WasmFrame> {
    return this.#wasm.readback();
  }

  /** Idempotent, and restartable after `stop` (React StrictMode remounts). */
  start(): void {
    this.#started = true;
    this.#observe();
    this.#watchVisibility();
    this.#sync();
  }

  /** The canvas and GPU state stay, so `start` resumes. */
  stop(): void {
    this.#started = false;
    this.#observer?.disconnect();
    this.#observer = undefined;
    this.#onScreen?.disconnect();
    this.#onScreen = undefined;
    globalThis.document?.removeEventListener("visibilitychange", this.#onPageVisibility);
    this.#wasm.stop();
  }

  #sync(): void {
    if (this.#started && this.#visible && this.#pageVisible) {
      this.#wasm.start();
    } else {
      this.#wasm.stop();
    }
  }

  /** Zero threshold: one visible pixel owes the user a correct picture. */
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

  /** Cheap enough to call per pointer move. */
  invalidate(): void {
    this.#wasm.invalidate();
  }

  /** Frames `height` world units vertically, centred on `(x, y)`. */
  setCamera(x: number, y: number, height: number): void {
    this.#wasm.setCamera(x, y, height);
  }

  dispose(): void {
    this.stop();
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    // Before the free, and only if still ours: a host swapping models on one
    // canvas may already have installed the next viewport's hook.
    const element = this.#canvas as unknown as Record<string, unknown>;
    if (element[READBACK] === this.#readback) delete element[READBACK];
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
      // Safari < 16.4 lacks the box; `devicePixelSize` falls back to the multiply.
      observer.observe(this.#canvas);
    }
    this.#observer = observer;
    // The observer's first report waits a frame; measure now.
    this.#resize({
      width: this.#canvas.clientWidth * window.devicePixelRatio,
      height: this.#canvas.clientHeight * window.devicePixelRatio,
    });
  }

  #resize(size: { width: number; height: number }): void {
    const width = clampBackingStore(size.width, this.#maxSize);
    const height = clampBackingStore(size.height, this.#maxSize);
    if (width === this.#size.width && height === this.#size.height) return;
    this.#size = { width, height };
    // Assigning clears the canvas, so only on change, and before reconfiguring.
    this.#canvas.width = width;
    this.#canvas.height = height;
    this.#wasm.resize(width, height);
  }
}

/** Prefers the browser-measured device pixels over `devicePixelRatio` math. */
export function devicePixelSize(
  entry: ResizeObserverEntry,
  ratio: number,
): { width: number; height: number } {
  const devicePixels = entry.devicePixelContentBoxSize?.[0];
  if (devicePixels) {
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

/** Absent `model` counts as visible: only a provably hidden viewport stops. */
function pageIsVisible(): boolean {
  return globalThis.document?.visibilityState !== "hidden";
}

function clampBackingStore(value: number, max: number): number {
  if (!Number.isFinite(value)) return 1;
  return Math.min(max, Math.max(1, Math.round(value)));
}
