import "./test/setup.js";

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { Editor, InTabBackend, MemoryStorage } from "@catchlight/core";
import type { Camera } from "@catchlight/core";
import { fakeWasm } from "@catchlight/core/fakes";
import type { FakeEditor } from "@catchlight/core/fakes";
import { StrictMode } from "react";
import type { ReactNode } from "react";

import { DEFAULT_CAMERA, EditorProvider, Viewport, useViewportCamera } from "./index.js";
import type { ViewportCamera, ViewportPointerEvent } from "./index.js";
import {
  fakeReplica,
  fire,
  harness,
  mount,
  pointer,
  run,
  settle,
  stubLayout,
  wheel,
} from "./test/harness.js";

let restore: () => void;
beforeEach(() => {
  restore = stubLayout(800, 600);
});
afterEach(() => restore());

describe("attaching", () => {
  test("StrictMode's double mount leaves exactly one live viewport", async () => {
    const { editor, viewports } = await harness();
    const session = await editor.newSession();

    const view = await mount(
      <StrictMode>
        <EditorProvider editor={editor}>
          <Viewport.Root session={session} />
        </EditorProvider>
      </StrictMode>,
    );
    await settle();

    expect(viewports.length).toBeGreaterThan(0);
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(1);
    expect(viewports.filter((viewport) => viewport.freed === 0)[0]?.started).toBeGreaterThan(0);

    await view.unmount();
    await settle();

    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(0);
  });

  test("the canvas says what it is and claims its own touch gestures", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root session={session} className="fills" data-testid="stage" />
      </EditorProvider>,
    );
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    expect(canvas.getAttribute("data-catchlight-viewport")).toBe("");
    expect(canvas.className).toBe("fills");
    expect(canvas.getAttribute("data-testid")).toBe("stage");
    expect(canvas.style.touchAction).toBe("none");
    expect(canvas.style.display).toBe("block");
    await view.unmount();
  });

  test("a ref the host passed still gets the element", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    let seen: HTMLCanvasElement | null = null;

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          ref={(canvas) => {
            seen = canvas;
          }}
        />
      </EditorProvider>,
    );
    expect(seen).not.toBeNull();

    await view.unmount();
    expect(seen).toBeNull();
  });
});

describe("an attach that cannot happen", () => {
  test("the reason goes to the host, which is the only place a person reads it", async () => {
    const editor = await editorWithoutADevice(NO_DEVICE);
    const session = await editor.newSession();
    const seen: unknown[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root session={session} onError={(cause) => seen.push(cause)} />
      </EditorProvider>,
    );
    await settle();
    await settle();

    expect(seen).toHaveLength(1);
    expect((seen[0] as Error).message).toBe(NO_DEVICE);
    await view.unmount();
  });

  test("with no handler it falls back to the console, and that is all it is", async () => {
    const editor = await editorWithoutADevice(NO_DEVICE);
    const session = await editor.newSession();
    const warned = stubWarn();

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root session={session} />
      </EditorProvider>,
    );
    await settle();
    await settle();

    expect(warned.calls).toHaveLength(1);
    const cause = warned.calls[0]?.find((arg) => arg instanceof Error) as Error | undefined;
    expect(cause?.message).toBe(NO_DEVICE);

    warned.restore();
    await view.unmount();
  });
});

describe("a stage with nothing to draw", () => {
  test("waits with its canvas, and attaches on that same element when a session arrives", async () => {
    const { editor, viewports } = await harness();
    const session = await editor.newSession();
    const stage = (drawn: typeof session | undefined) => (
      <EditorProvider editor={editor}>
        <Viewport.Root session={drawn} />
      </EditorProvider>
    );

    const view = await mount(stage(undefined));
    await settle();
    const canvas = view.container.querySelector("canvas");
    expect(canvas).not.toBeNull();
    expect(viewports).toHaveLength(0);

    await view.render(stage(session));
    await settle();
    expect(view.container.querySelector("canvas")).toBe(canvas);
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(1);

    await view.render(stage(undefined));
    await settle();
    expect(view.container.querySelector("canvas")).toBe(canvas);
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(0);

    await view.unmount();
  });
});

describe("pointers", () => {
  test("a pointer arrives in world units, Y-up", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const seen: ViewportPointerEvent[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          camera={{ center: [0, 0], height: 2 }}
          onPointerMove={(event) => seen.push(event)}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    // 800x600 framing 2 world units of height: one pixel is 1/300 of a unit.
    await fire(canvas, pointer("pointermove", { clientX: 600, clientY: 200 }));

    expect(seen).toHaveLength(1);
    expect(seen[0]?.screen).toEqual([600, 200]);
    expect(seen[0]?.world[0]).toBeCloseTo((600 - 400) * (2 / 600), 12);
    expect(seen[0]?.world[1]).toBeCloseTo(-(200 - 300) * (2 / 600), 12);
    await view.unmount();
  });

  test("the camera the host set is the one the pointer is read against", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const seen: ViewportPointerEvent[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          camera={{ center: [10, 5], height: 600 }}
          onPointerDown={(event) => seen.push(event)}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 500, clientY: 400, button: 0 }));

    expect(seen[0]?.world[0]).toBeCloseTo(110, 12);
    expect(seen[0]?.world[1]).toBeCloseTo(-95, 12);
    await view.unmount();
  });
});

describe("the camera gestures", () => {
  test("a middle drag pans, and the host hears about it instead of the pointer props", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];
    const pointers: string[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          defaultCamera={{ center: [0, 0], height: 2 }}
          onCameraChange={(camera) => cameras.push(camera)}
          onPointerDown={() => pointers.push("down")}
          onPointerMove={() => pointers.push("move")}
          onPointerUp={() => pointers.push("up")}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 1 }));
    await fire(canvas, pointer("pointermove", { clientX: 500, clientY: 350, button: 1 }));
    await fire(canvas, pointer("pointerup", { clientX: 500, clientY: 350, button: 1 }));

    expect(cameras).toHaveLength(1);
    // 100 px right and 50 px down at 1/300 of a unit per pixel.
    expect(cameras[0]?.center[0]).toBeCloseTo(-100 / 300, 12);
    expect(cameras[0]?.center[1]).toBeCloseTo(50 / 300, 12);
    expect(cameras[0]?.height).toBe(2);
    expect(pointers).toEqual([]);
    await view.unmount();
  });

  test("a primary drag pans while Space is held", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];
    const pointers: string[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          defaultCamera={{ center: [0, 0], height: 600 }}
          onCameraChange={(camera) => cameras.push(camera)}
          onPointerDown={() => pointers.push("down")}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(globalThis, new KeyboardEvent("keydown", { code: "Space" }));
    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointermove", { clientX: 420, clientY: 300 }));
    expect(pointers).toEqual([]);
    expect(cameras[0]?.center[0]).toBeCloseTo(-20, 12);

    await fire(canvas, pointer("pointerup", { clientX: 420, clientY: 300 }));
    await fire(globalThis, new KeyboardEvent("keyup", { code: "Space" }));

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    expect(pointers).toEqual(["down"]);
    await view.unmount();
  });

  test("a wheel zooms about the cursor", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          defaultCamera={{ center: [0, 0], height: 600 }}
          onCameraChange={(camera) => cameras.push(camera)}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, wheel(100, 400, 300));
    expect(cameras[0]?.height).toBeCloseTo(660, 12);
    expect(cameras[0]?.center).toEqual([0, 0]);

    await view.render(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          camera={{ center: [0, 0], height: 600 }}
          onCameraChange={(camera) => cameras.push(camera)}
        />
      </EditorProvider>,
    );
    // The world point 100 px right of centre must stay there after the zoom.
    await fire(canvas, wheel(100, 500, 300));
    expect(cameras[1]?.height).toBeCloseTo(660, 12);
    expect(cameras[1]?.center[0]).toBeCloseTo(100 - 100 * 1.1, 12);
    expect(cameras[1]?.center[1]).toBeCloseTo(0, 12);
    await view.unmount();
  });

  test("a controlled camera moves only when the host says so", async () => {
    const { editor, viewports } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          camera={{ center: [0, 0], height: 2 }}
          onCameraChange={(camera) => cameras.push(camera)}
        />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;
    const live = viewports.filter((viewport) => viewport.freed === 0)[0];
    expect(live?.camera).toEqual([0, 0, 2]);

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 1 }));
    await fire(canvas, pointer("pointermove", { clientX: 500, clientY: 300, button: 1 }));

    expect(cameras).toHaveLength(1);
    expect(live?.camera).toEqual([0, 0, 2]);

    await view.render(
      <EditorProvider editor={editor}>
        <Viewport.Root session={session} camera={{ center: [1, 2], height: 3 }} />
      </EditorProvider>,
    );
    expect(live?.camera).toEqual([1, 2, 3]);
    await view.unmount();
  });
});

describe("framing the model", () => {
  test("a session is framed on the first frame that has something to frame", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];
    const frames = stubFrames();

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root session={session} onCameraChange={(camera) => cameras.push(camera)} />
      </EditorProvider>,
    );
    await settle();

    await frames.flush();
    expect(cameras).toEqual([]);

    fakeReplica(session).box = [-8, -1, 8, 1];
    await frames.flush();

    expect(cameras).toHaveLength(1);
    expect(cameras[0]?.center).toEqual([0, 0]);
    expect(cameras[0]?.height).toBeCloseTo(16.2, 9);

    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));
    await frames.flush();
    await frames.flush();
    expect(cameras).toHaveLength(1);

    await view.unmount();
    frames.restore();
  });

  test("a host that named a starting camera keeps it", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const cameras: Camera[] = [];
    const frames = stubFrames();

    const view = await mount(
      <EditorProvider editor={editor}>
        <Viewport.Root
          session={session}
          defaultCamera={{ center: [1, 2], height: 40 }}
          onCameraChange={(camera) => cameras.push(camera)}
        />
      </EditorProvider>,
    );
    await settle();

    fakeReplica(session).box = [-8, -1, 8, 1];
    await frames.flush();
    await frames.flush();

    expect(cameras).toEqual([]);
    await view.unmount();
    frames.restore();
  });

  test("the camera hook frames on demand, against the size the canvas reported", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    let api: ViewportCamera | undefined;

    function Host(): ReactNode {
      const camera = useViewportCamera();
      api = camera;
      return (
        <Viewport.Root
          session={session}
          camera={camera.camera}
          onCameraChange={camera.onCameraChange}
          onResize={camera.onResize}
          defaultCamera={DEFAULT_CAMERA}
        />
      );
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Host />
      </EditorProvider>,
    );
    await settle();

    expect(api?.fit(session)).toBe(false);
    expect(api?.camera).toEqual(DEFAULT_CAMERA);

    fakeReplica(session).box = [-8, -1, 8, 1];
    await run(() => api?.fit(session));

    // 800x600: covering 16 world units across needs a height of 12, plus margin.
    expect(api?.camera.center).toEqual([0, 0]);
    expect(api?.camera.height).toBeCloseTo(16.2, 9);
    await run(() => api?.onResize({ width: 300, height: 600 }));
    expect(api?.camera.height).toBeCloseTo(43.2, 9);
    expect(api?.zoom).toBeCloseTo(1, 9);
    await run(() => api?.setCamera({ center: [3, 4], height: 43.2 }));
    await run(() => api?.onResize({ width: 800, height: 600 }));
    expect(api?.camera.center).toEqual([3, 4]);
    expect(api?.camera.height).toBeCloseTo(43.2, 9);
    await view.unmount();
  });

  test("the zoom is measured against the fit, whichever side framed it", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    const frames = stubFrames();
    let api: ViewportCamera | undefined;

    function Host(): ReactNode {
      const camera = useViewportCamera();
      api = camera;
      return (
        <Viewport.Root
          session={session}
          camera={camera.camera}
          onCameraChange={camera.onCameraChange}
          onFit={camera.onFit}
          onResize={camera.onResize}
        />
      );
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Host />
      </EditorProvider>,
    );
    await settle();
    expect(api?.zoom).toBeUndefined();

    fakeReplica(session).box = [-8, -1, 8, 1];
    await run(() => frames.flush());
    expect(api?.camera.height).toBeCloseTo(16.2, 9);
    expect(api?.zoom).toBeCloseTo(1, 9);

    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;
    await fire(canvas, wheel(-100, 400, 300));
    expect(api?.zoom).toBeCloseTo(1.1, 9);

    await run(() => api?.fit(session));
    expect(api?.zoom).toBeCloseTo(1, 9);

    await view.unmount();
    frames.restore();
  });
});

const NO_DEVICE =
  "the catchlight editor needs WebGPU or WebGL2 to draw and this browser offered neither";

// The real device-acquisition failure path, not a stubbed `attach`.
async function editorWithoutADevice(message: string): Promise<Editor> {
  const module = fakeWasm();
  module.failNextAcquire = message;
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  return Editor.create(module.module, new InTabBackend(wasm, new MemoryStorage()));
}

function stubWarn(): { calls: unknown[][]; restore(): void } {
  const calls: unknown[][] = [];
  const saved = console.warn;
  console.warn = (...args: unknown[]): void => {
    calls.push(args);
  };
  return {
    calls,
    restore: () => {
      console.warn = saved;
    },
  };
}

function stubFrames(): { flush(): Promise<void>; restore(): void } {
  const queued = new Map<number, () => void>();
  let next = 1;
  const saved = {
    request: globalThis.requestAnimationFrame,
    cancel: globalThis.cancelAnimationFrame,
  };
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    const id = next++;
    queued.set(id, () => callback(0));
    return id;
  }) as typeof requestAnimationFrame;
  globalThis.cancelAnimationFrame = ((id: number) => {
    queued.delete(id);
  }) as typeof cancelAnimationFrame;

  return {
    flush: async () => {
      const running = [...queued.values()];
      queued.clear();
      await run(() => {
        for (const frame of running) frame();
      });
    },
    restore: () => {
      globalThis.requestAnimationFrame = saved.request;
      globalThis.cancelAnimationFrame = saved.cancel;
    },
  };
}
