import { describe, expect, test } from "bun:test";

import { FakeReplica, FakeViewport, ScriptedBackend } from "./fakes.js";
import { Session } from "./session.js";
import { READBACK, Viewport, devicePixelSize } from "./viewport.js";

function entry(parts: {
  devicePixelContentBoxSize?: ResizeObserverSize[];
  contentBoxSize?: ResizeObserverSize[];
  contentRect?: { width: number; height: number };
}): ResizeObserverEntry {
  return { contentRect: { width: 0, height: 0 }, ...parts } as unknown as ResizeObserverEntry;
}

/** Fake canvas and observers with exposed callbacks; restores globals when done. */
function stubBrowser(cssWidth: number, cssHeight: number, ratio: number) {
  const saved = {
    ResizeObserver: globalThis.ResizeObserver,
    IntersectionObserver: globalThis.IntersectionObserver,
    devicePixelRatio: globalThis.window?.devicePixelRatio,
  };
  let onScreen: ((entries: IntersectionObserverEntry[]) => void) | undefined;

  globalThis.ResizeObserver = class {
    observe(): void {}
    disconnect(): void {}
    unobserve(): void {}
  } as unknown as typeof ResizeObserver;

  globalThis.IntersectionObserver = class {
    constructor(callback: (entries: IntersectionObserverEntry[]) => void) {
      onScreen = callback;
    }
    observe(): void {}
    disconnect(): void {}
    unobserve(): void {}
    takeRecords(): IntersectionObserverEntry[] {
      return [];
    }
  } as unknown as typeof IntersectionObserver;

  globalThis.window = { devicePixelRatio: ratio } as unknown as Window & typeof globalThis;

  const canvas = {
    clientWidth: cssWidth,
    clientHeight: cssHeight,
    width: 0,
    height: 0,
  } as unknown as HTMLCanvasElement;

  return {
    canvas,
    scroll(visible: boolean) {
      onScreen?.([{ isIntersecting: visible } as IntersectionObserverEntry]);
    },
    restore() {
      globalThis.ResizeObserver = saved.ResizeObserver;
      globalThis.IntersectionObserver = saved.IntersectionObserver;
      if (saved.devicePixelRatio === undefined) {
        delete (globalThis as { window?: unknown }).window;
      }
    },
  };
}

function session(): Session {
  return new Session(new ScriptedBackend(), 1, new FakeReplica());
}

describe("device pixels", () => {
  test("are taken from the browser, not multiplied out", () => {
    // The observed box is already device pixels; no rounding to reproduce.
    expect(
      devicePixelSize(
        entry({
          devicePixelContentBoxSize: [{ inlineSize: 492, blockSize: 800 }],
          contentBoxSize: [{ inlineSize: 393.6, blockSize: 640 }],
        }),
        1.25,
      ),
    ).toEqual({ width: 492, height: 800 });
  });

  test("without the device-pixel box, the CSS box is scaled and rounded", () => {
    expect(
      devicePixelSize(entry({ contentBoxSize: [{ inlineSize: 393.6, blockSize: 640 }] }), 1.25),
    ).toEqual({ width: 492, height: 800 });
  });

  test("with neither box, the content rect still gives a size", () => {
    expect(devicePixelSize(entry({ contentRect: { width: 100, height: 50 } }), 2)).toEqual({
      width: 200,
      height: 100,
    });
  });
});

describe("the picture and the model", () => {
  test("a scratch edit repaints without bumping the revision", () => {
    const open = session();
    const view = new FakeViewport();
    new Viewport(view, {} as HTMLCanvasElement, open);
    let revisions = 0;
    open.subscribe(() => {
      revisions += 1;
    });

    for (let i = 0; i < 20; i++) open.setParam("param-1", i / 20);

    // Twenty repaints, no revision: the reason there are two channels.
    expect(view.invalidated).toBe(20);
    expect(revisions).toBe(0);
  });

  test("disposing stops the renderer and lets go of the session", () => {
    const open = session();
    const view = new FakeViewport();
    const viewport = new Viewport(view, {} as HTMLCanvasElement, open);

    viewport.dispose();
    open.setParam("param-1", 1);

    expect(view.stopped).toBe(1);
    expect(view.freed).toBe(1);
    expect(view.invalidated).toBe(0);
  });
});

describe("visibility", () => {
  test("a viewport scrolled off screen stops, and starts again when it returns", () => {
    const browser = stubBrowser(800, 600, 1);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();
    expect(view.started).toBe(1);

    browser.scroll(false);
    expect(view.stopped).toBe(1);
    browser.scroll(true);
    expect(view.started).toBe(2);

    viewport.stop();
    browser.restore();
  });

  test("a hidden viewport that the host stops does not start when it reappears", () => {
    const browser = stubBrowser(800, 600, 1);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();
    browser.scroll(false);
    viewport.stop();
    const startedBefore = view.started;

    // A stale callback must not revive a stopped viewport.
    browser.scroll(true);
    expect(view.started).toBe(startedBefore);

    browser.restore();
  });
});

describe("the backing store", () => {
  test("is the CSS box times the device pixel ratio on a 2x screen", () => {
    const browser = stubBrowser(800, 600, 2);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas, undefined, 8192);

    viewport.start();

    expect(view.size).toEqual([1600, 1200]);
    expect(browser.canvas.width).toBe(1600);
    expect(browser.canvas.height).toBe(1200);

    viewport.stop();
    browser.restore();
  });

  test("never exceeds what the device reported", () => {
    // The device allows 4096; configuring past it fails.
    const browser = stubBrowser(5000, 5000, 2);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas, undefined, 4096);

    viewport.start();
    expect(view.size).toEqual([4096, 4096]);

    viewport.stop();
    browser.restore();
  });

  test("falls back to the conservative floor when nobody asked a device", () => {
    const browser = stubBrowser(5000, 5000, 2);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();
    expect(view.size).toEqual([8192, 8192]);

    viewport.stop();
    browser.restore();
  });
});

describe("reading a frame back", () => {
  test("the canvas carries it while a viewport draws, and not afterwards", async () => {
    const view = new FakeViewport();
    view.frame = { width: 2, height: 1, rgba: new Uint8Array(8) };
    const canvas = {} as HTMLCanvasElement;
    const viewport = new Viewport(view, canvas);

    const read = (canvas as unknown as Record<string, () => Promise<unknown>>)[READBACK];
    expect(typeof read).toBe("function");
    expect(await read?.()).toEqual(view.frame);

    // Gone before the renderer is freed: calling a freed viewport traps.
    viewport.dispose();
    expect((canvas as unknown as Record<string, unknown>)[READBACK]).toBeUndefined();
  });

  test("a late dispose does not take a newer viewport's hook away", async () => {
    const canvas = {} as HTMLCanvasElement;
    const outgoing = new Viewport(new FakeViewport(), canvas);
    const live = new FakeViewport();
    live.frame = { width: 3, height: 1, rgba: new Uint8Array(12) };
    new Viewport(live, canvas);
    outgoing.dispose();

    const read = (canvas as unknown as Record<string, () => Promise<unknown>>)[READBACK];
    expect(await read?.()).toEqual(live.frame);
  });
});
