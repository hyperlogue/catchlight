/**
 * That the assembled screen is wired to one session, and to the right one.
 *
 * There is no logic here to test — every behaviour belongs to a part or a hook
 * in `@catchlight/react`, which has its own suites. What can only break at this
 * layer is the wiring: which document the panels are showing, whether a part
 * that needs the selection is under the provider that supplies it, and whether
 * the toolbar reaches the editor at all. So this mounts the whole thing over
 * the fake wasm module and reads the DOM.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import { Editor, InTabBackend, MemoryStorage } from "@catchlight/core";
import { fakeWasm } from "@catchlight/core/fakes";
import type {
  FakeEditor,
  FakeGpu,
  FakeModule,
  FakeReplica,
  FakeViewport,
} from "@catchlight/core/fakes";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";

import { CatchlightEditor } from "./CatchlightEditor.js";

describe("the assembled editor", () => {
  test("comes up empty and then shows the document the editor lists", async () => {
    const stop = stubObservers();
    const editor = await fakeEditor();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    expect(view.querySelector(".catchlight")).not.toBeNull();
    expect(view.querySelector("[data-catchlight-toolbar]")).not.toBeNull();
    expect(save(view).disabled).toBe(true);
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();

    // Not opened through this component: the editor said the set changed, and
    // the first document it lists is the one the panels take.
    await run(() => editor.newDocument("akari"));
    await settle();

    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(view.querySelector("[data-catchlight-viewport]")).not.toBeNull();
    expect(view.querySelector("[data-catchlight-node-tree]")).not.toBeNull();
    expect(save(view).disabled).toBe(false);
    expect(text(view, "[data-catchlight-status]")).toContain("akari");

    await unmount();
    editor.close();
    stop();
  });

  test("a node is selected in the tree and a param is a named slider", async () => {
    const stop = stubObservers();
    const editor = await fakeEditor();
    const session = await editor.newDocument("akari");
    await session.send({ cmd: "node_add", parent: "root", kind: "part", name: "body" });
    await session.send({
      cmd: "param_add",
      name: "head:yaw",
      min: -1,
      max: 1,
      default: 0,
      key_positions: [-1, 0, 1],
    });

    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    const label = view.querySelector<HTMLButtonElement>(
      '[data-catchlight-node][data-node$="part-1"] [data-catchlight-node-label]',
    );
    expect(label?.textContent).toBe("body");
    await run(() => label?.click());
    await settle();

    expect(view.querySelector("[data-catchlight-node][data-selected]")).not.toBeNull();
    expect(text(view, "[data-catchlight-status]")).toContain("selected");

    // The default row is a bare slider, so the fields around it are this
    // package's doing — and the name is one a person can edit, not a label.
    const name = view.querySelector<HTMLInputElement>("[data-catchlight-param-rename]");
    expect(name?.value).toBe("head:yaw");
    const slider = view.querySelector<HTMLInputElement>("[data-catchlight-param-slider]");
    expect(slider?.min).toBe("-1");
    expect(slider?.max).toBe("1");

    await unmount();
    editor.close();
    stop();
  });

  test("the toolbar frames the model, and the status line says where this is", async () => {
    const stop = stubObservers();
    const { editor, viewports } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    // Nothing to frame without a document, the same rule the Save button is on.
    expect(fit(view).disabled).toBe(true);

    const session = await run(() => editor.newDocument("akari"));
    await settle();
    expect(fit(view).disabled).toBe(false);

    // A fact a person cannot get by looking at the picture, and one that
    // changes what a bug report means.
    expect(text(view, "[data-catchlight-backend]")).toBe("in-tab");

    // A box off the origin, so a camera that landed on it could not have been
    // the default one.
    (session.replica as FakeReplica).box = [10, 20, 12, 24];
    await run(() => fit(view).click());
    await settle();

    const drawn = viewports[viewports.length - 1];
    expect(drawn?.camera?.[0]).toBe(11);
    expect(drawn?.camera?.[1]).toBe(22);
    expect(drawn?.camera?.[2]).toBeGreaterThan(0);

    await unmount();
    editor.close();
    stop();
  });

  test("New opens a document, Save As downloads it, and closing the last one empties the screen", async () => {
    const stop = stubObservers();
    const download = stubDownload();
    const { editor, wasm } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    await run(() => button(view, "[data-catchlight-new]").click());
    await settle();
    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(text(view, "[data-catchlight-status]")).toContain("untitled");
    expect(text(view, "[data-catchlight-file]")).toBe("not saved yet");

    const input = view.querySelector<HTMLInputElement>("[data-catchlight-save-as]");
    const form = view.querySelector<HTMLFormElement>("[data-catchlight-file-save]");
    if (!input || !form) throw new Error("no save-as form in the toolbar");
    input.value = "copy";
    await run(() => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
    await settle();
    await settle();

    // The command named the key, the bytes came back out as a file of that
    // name, and the status line says so.
    expect(wasm.requests.find((request) => request.cmd === "save")).toMatchObject({
      path: "copy.clm",
    });
    expect(download.names).toEqual(["copy.clm"]);
    expect(text(view, "[data-catchlight-notice]")).toBe("downloaded copy.clm");

    await run(() => button(view, "[data-catchlight-session-close]").click());
    await settle();
    await settle();

    expect(wasm.requests.map((request) => request.cmd)).toContain("session_close");
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();
    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(0);
    expect(save(view).disabled).toBe(true);

    await unmount();
    editor.close();
    download.restore();
    stop();
  });

  test("closing the current document switches to another; closing another leaves it", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    const first = await editor.newDocument("akari");
    await editor.newDocument("beni");
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    expect(text(view, "[data-catchlight-status]")).toContain("akari");
    const current = "[data-catchlight-session][data-current]";
    expect(view.querySelector(current)?.getAttribute("data-session")).toBe(String(first.id));

    await run(() => button(view, `${current} [data-catchlight-session-close]`).click());
    await settle();
    await settle();

    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(text(view, "[data-catchlight-status]")).toContain("beni");
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).toBeNull();
    // The closed one's replica is gone, and the screen never read it again.
    expect((first.replica as FakeReplica).freed).toBe(true);

    await run(() => editor.newDocument("chika"));
    await settle();
    const rows = view.querySelectorAll<HTMLElement>("[data-catchlight-session]");
    expect(rows).toHaveLength(2);
    await run(() =>
      rows[1]?.querySelector<HTMLButtonElement>("[data-catchlight-session-close]")?.click(),
    );
    await settle();
    await settle();

    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(text(view, "[data-catchlight-status]")).toContain("beni");

    await unmount();
    editor.close();
    stop();
  });

  test("one canvas element outlives every document: empty, open, close all, open again", async () => {
    const stop = stubObservers();
    const { editor, gpu, viewports } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    // Empty, and the canvas is already there, drawn on by nothing.
    const canvas = view.querySelector("canvas");
    if (!canvas) throw new Error("no canvas in the empty state");
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();
    expect(viewports).toHaveLength(0);

    await run(() => editor.newDocument("akari"));
    await settle();
    expect(view.querySelector("canvas")).toBe(canvas);
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).toBeNull();
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(1);

    await run(() => button(view, "[data-catchlight-session-close]").click());
    await settle();
    await settle();
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();
    expect(view.querySelector("canvas")).toBe(canvas);
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(0);

    await run(() => button(view, "[data-catchlight-new]").click());
    await settle();
    expect(view.querySelector("canvas")).toBe(canvas);
    expect(viewports.filter((viewport) => viewport.freed === 0)).toHaveLength(1);
    // One device for the editor, acquired once, whatever came and went above
    // it: the canvas outlived two documents and the device outlived both.
    expect(gpu.acquires).toBe(1);

    await unmount();
    editor.close();
    stop();
  });

  test("a canvas that cannot be attached says so, instead of coming up blank", async () => {
    const stop = stubObservers();
    const { editor, module } = await fakeStack();
    // What a browser with no WebGPU does: the device the first attach asks for
    // never arrives, and the stage is the one cell with nothing else to say.
    module.failNextAcquire = "this browser has no WebGPU, which the catchlight editor requires";
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    await run(() => editor.newDocument("akari"));
    await settle();
    await settle();

    expect(text(view, "[data-catchlight-problem]")).toContain("no WebGPU");

    await unmount();
    editor.close();
    stop();
  });

  test("the zoom readout follows the camera, relative to the fit", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    const session = await run(() => editor.newDocument("akari"));
    await settle();

    // No fit yet, so nothing to be relative to.
    expect(text(view, "[data-catchlight-zoom]")).toBe("–");

    (session.replica as FakeReplica).box = [10, 20, 12, 24];
    await run(() => fit(view).click());
    expect(text(view, "[data-catchlight-zoom]")).toBe("100%");

    // One notch in on the canvas, the wheel gesture the viewport owns.
    const canvas = view.querySelector("canvas");
    if (!canvas) throw new Error("no canvas");
    await run(() =>
      canvas.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true, cancelable: true })),
    );
    expect(text(view, "[data-catchlight-zoom]")).toBe("110%");

    await run(() => button(view, "[data-catchlight-camera-reset]").click());
    expect(text(view, "[data-catchlight-zoom]")).toBe("100%");

    await unmount();
    editor.close();
    stop();
  });
});

/** A real editor over the fake wasm module: the layer under this one, working. */
async function fakeEditor(): Promise<Editor> {
  return (await fakeStack()).editor;
}

/** The same, plus the fakes underneath, for a test that reads what was drawn. */
async function fakeStack(): Promise<{
  editor: Editor;
  wasm: FakeEditor;
  gpu: FakeGpu;
  viewports: FakeViewport[];
  module: FakeModule;
}> {
  const module = fakeWasm();
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  const editor = await Editor.create(module.module, new InTabBackend(wasm, new MemoryStorage()));
  return { editor, wasm, gpu: module.gpu, viewports: module.viewports, module };
}

/** Catches the name a download went out under; happy-dom would navigate to it otherwise. */
function stubDownload(): { names: string[]; restore(): void } {
  const names: string[] = [];
  const saved = {
    create: URL.createObjectURL,
    revoke: URL.revokeObjectURL,
    click: HTMLAnchorElement.prototype.click,
  };
  URL.createObjectURL = (): string => "blob:test";
  URL.revokeObjectURL = (): void => {};
  HTMLAnchorElement.prototype.click = function click(this: HTMLAnchorElement): void {
    if (this.hasAttribute("data-catchlight-download")) names.push(this.download);
  };
  return {
    names,
    restore: () => {
      URL.createObjectURL = saved.create;
      URL.revokeObjectURL = saved.revoke;
      HTMLAnchorElement.prototype.click = saved.click;
    },
  };
}

interface Mounted {
  container: HTMLElement;
  unmount(): Promise<void>;
}

async function mount(ui: ReactNode): Promise<Mounted> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(ui);
  });
  return {
    container,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
      container.remove();
    },
  };
}

/** Runs something that moves the editor, letting React commit what it caused. */
async function run<T>(action: () => Promise<T> | T): Promise<T> {
  let result: T | undefined;
  await act(async () => {
    result = await action();
  });
  return result as T;
}

/** Lets the round trips a render started settle before the DOM is read. */
async function settle(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 4; i += 1) await Promise.resolve();
  });
}

function save(view: HTMLElement): HTMLButtonElement {
  return button(view, "[data-catchlight-save]");
}

function fit(view: HTMLElement): HTMLButtonElement {
  return button(view, "[data-catchlight-fit]");
}

function button(view: HTMLElement, selector: string): HTMLButtonElement {
  const found = view.querySelector<HTMLButtonElement>(selector);
  if (!found) throw new Error(`no ${selector} button`);
  return found;
}

function text(view: HTMLElement, selector: string): string {
  return view.querySelector(selector)?.textContent ?? "";
}

/**
 * The two observers the viewport builds. happy-dom lays nothing out, so a real
 * one would report zeroes; what this suite reads is the DOM, not a frame.
 */
function stubObservers(): () => void {
  const saved = {
    ResizeObserver: globalThis.ResizeObserver,
    IntersectionObserver: globalThis.IntersectionObserver,
  };
  class Noop {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  Object.assign(globalThis, { ResizeObserver: Noop, IntersectionObserver: Noop });
  return () => Object.assign(globalThis, saved);
}
