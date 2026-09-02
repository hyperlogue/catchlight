/**
 * The canvas: that attach and detach are inverses, that a pointer arrives in
 * world units, and that the two camera gestures are the component's own.
 */

import "./test/setup.js";

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { Camera } from "@catchlight/core";
import { StrictMode } from "react";

import { EditorProvider, Viewport } from "./index.js";
import type { ViewportPointerEvent } from "./index.js";
import { fire, harness, mount, pointer, settle, stubLayout, wheel } from "./test/harness.js";

/** An 800x600 canvas at the top-left of the page. */
let restore: () => void;
beforeEach(() => {
  restore = stubLayout(800, 600);
});
afterEach(() => restore());

describe("attaching", () => {
  test("StrictMode's double mount leaves exactly one live viewport", async () => {
    const { editor, viewports } = await harness();
    const session = await editor.newDocument();

    const view = await mount(
      <StrictMode>
        <EditorProvider editor={editor}>
          <Viewport.Root session={session} />
        </EditorProvider>
      </StrictMode>,
    );
    // `attach` is asynchronous, so the first mount's renderer can arrive after
    // its own cleanup ran; it is disposed on arrival rather than kept.
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
    const session = await editor.newDocument();

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
    const session = await editor.newDocument();
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

describe("pointers", () => {
  test("a pointer arrives in world units, Y-up", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
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
    // Screen Y grows downward and world Y grows upward.
    expect(seen[0]?.world[1]).toBeCloseTo(-(200 - 300) * (2 / 600), 12);
    await view.unmount();
  });

  test("the camera the host set is the one the pointer is read against", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
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

    // One world unit per pixel now.
    await fire(canvas, pointer("pointerdown", { clientX: 500, clientY: 400, button: 0 }));

    expect(seen[0]?.world[0]).toBeCloseTo(110, 12);
    expect(seen[0]?.world[1]).toBeCloseTo(-95, 12);
    await view.unmount();
  });
});

describe("the camera gestures", () => {
  test("a middle drag pans, and the host hears about it instead of the pointer props", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
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
    // The world under the pointer follows it: 100 px right and 50 px down at
    // 1/300 of a unit per pixel.
    expect(cameras[0]?.center[0]).toBeCloseTo(-100 / 300, 12);
    expect(cameras[0]?.center[1]).toBeCloseTo(50 / 300, 12);
    expect(cameras[0]?.height).toBe(2);
    // A pan is the component's gesture, not something the host has to filter.
    expect(pointers).toEqual([]);
    await view.unmount();
  });

  test("a primary drag pans while Space is held", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
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

    // Space released: the same drag is the host's again.
    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    expect(pointers).toEqual(["down"]);
    await view.unmount();
  });

  test("a wheel zooms about the cursor", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
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

    // Dead centre: one notch out changes the height and nothing else.
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
    // 100 px right of centre, so that world point has to stay 100 px right of
    // centre after the zoom: the centre moves by the change in world-per-pixel.
    await fire(canvas, wheel(100, 500, 300));
    expect(cameras[1]?.height).toBeCloseTo(660, 12);
    expect(cameras[1]?.center[0]).toBeCloseTo(100 - 100 * 1.1, 12);
    expect(cameras[1]?.center[1]).toBeCloseTo(0, 12);
    await view.unmount();
  });

  test("a controlled camera moves only when the host says so", async () => {
    const { editor, viewports } = await harness();
    const session = await editor.newDocument();
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

    // The host was told, and ignored it. The renderer must not have moved.
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
