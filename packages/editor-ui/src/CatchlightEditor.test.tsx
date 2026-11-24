
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
  test("comes up empty and then shows the model the editor lists", async () => {
    const stop = stubObservers();
    const editor = await fakeEditor();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    expect(view.querySelector(".catchlight")).not.toBeNull();
    expect(view.querySelector("[data-catchlight-toolbar]")).not.toBeNull();
    expect(save(view).disabled).toBe(true);
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();

    await run(() => editor.newSession("akari"));
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

  test("C opens the review tab aimed at the selection", async () => {
    const stop = stubObservers();
    const editor = await fakeEditor();
    const session = await editor.newSession("akari");
    await session.send({ cmd: "node_add", parent: "root", kind: "part", name: "body" });
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();
    expect(view.querySelector("[data-catchlight-review]")).toBeNull();

    await run(() =>
      view
        .querySelector<HTMLButtonElement>(
          '[data-catchlight-node][data-node$="part-1"] [data-catchlight-node-label]',
        )
        ?.click(),
    );
    await run(() => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "c", bubbles: true })));
    await settle();

    expect(view.querySelector("[data-catchlight-review-tab][data-active]")).not.toBeNull();
    expect(
      view.querySelector("[data-catchlight-review-composer] textarea")?.getAttribute("aria-label"),
    ).toBe("Comment on body");

    await unmount();
    editor.close();
    stop();
  });

  test("a node is selected in the tree and a param is a named slider", async () => {
    const stop = stubObservers();
    const editor = await fakeEditor();
    const session = await editor.newSession("akari");
    await session.send({
      cmd: "node_add",
      parent: "root",
      kind: "part",
      name: "body",
    });
    await session.send({
      cmd: "param_add",
      name: "head:yaw",
      min: -1,
      max: 1,
      default: 0,
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

    expect(text(view, "[data-catchlight-param-name]")).toContain("head:yaw");
    await run(() => button(view, '[aria-label="Edit head:yaw"]').click());
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

    expect(fit(view).disabled).toBe(true);

    const session = await run(() => editor.newSession("akari"));
    await settle();
    expect(fit(view).disabled).toBe(false);

    expect(text(view, "[data-catchlight-backend]")).toBe("in-tab");
    expect(text(view, "[data-catchlight-tier]")).toBe("webgpu");

    // Off the origin, so the default camera cannot pass.
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

  test("New opens a model, Save As downloads it, and closing the last one empties the screen", async () => {
    const stop = stubObservers();
    const download = stubDownload();
    const { editor, wasm } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    await run(() => button(view, "[data-catchlight-new]").click());
    await settle();
    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(text(view, "[data-catchlight-status]")).toContain("untitled");
    expect(text(view, "[data-catchlight-save-state]")).toBe("All changes saved");
    await run(() =>
      Array.from(view.querySelectorAll<HTMLButtonElement>("button"))
        .find((b) => b.textContent === "Save as…")
        ?.click(),
    );

    const input = view.querySelector<HTMLInputElement>("[data-catchlight-save-as]");
    const form = view.querySelector<HTMLFormElement>("[data-catchlight-file-save]");
    if (!input || !form) throw new Error("no save-as form in the dialog");
    input.value = "copy";
    await run(() => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
    await settle();
    await settle();

    expect(wasm.requests.find((request) => request.cmd === "session_save")).toMatchObject({
      path: "copy.clm",
    });
    expect(download.names).toEqual(["copy.clm"]);
    expect(text(view, "[data-catchlight-notice]")).toBe("Downloaded copy.clm");

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

  test("closing the current model switches to another; closing another leaves it", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    const first = await editor.newSession("akari");
    await editor.newSession("beni");
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
    expect((first.replica as FakeReplica).freed).toBe(true);

    await run(() => editor.newSession("chika"));
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

  test("a deferred close uses the current model when its metadata arrives", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    const first = await editor.newSession("akari");
    const second = await editor.newSession("beni");
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();
    const listSessions = editor.listSessions.bind(editor);
    const listed = await listSessions();
    let release!: (sessions: typeof listed) => void;
    const pending = new Promise<typeof listed>((resolve) => { release = resolve; });
    editor.listSessions = () => {
      editor.listSessions = listSessions;
      return pending;
    };
    const secondRow = `[data-catchlight-session][data-session="${second.id}"]`;
    const current = "[data-catchlight-session][data-current]";

    await run(() => button(view, `${secondRow} [data-catchlight-session-close]`).click());
    await run(() => button(view, `${secondRow} > button`).click());
    await settle();
    expect(view.querySelector(current)?.getAttribute("data-session")).toBe(String(second.id));

    await run(() => release(listed));
    await settle();
    await settle();
    expect(view.querySelector(current)?.getAttribute("data-session")).toBe(String(first.id));
    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(1);
    expect(view.querySelector("[data-catchlight-problem]")).toBeNull();
    expect((second.replica as FakeReplica).freed).toBe(true);
    expect((first.replica as FakeReplica).freed).toBe(false);

    await unmount();
    editor.close();
    stop();
  });

  test("closing a model never switches to a closed entry from a delayed list", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    await editor.newSession("akari");
    const second = await editor.newSession("beni");
    const listSessions = editor.listSessions.bind(editor);
    const stale = await listSessions();
    let release!: (sessions: typeof stale) => void;
    const pending = new Promise<typeof stale>((resolve) => { release = resolve; });
    let hold = true;
    editor.listSessions = () => hold ? pending : listSessions();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    hold = false;
    await run(() => editor.closeSession(second.id));
    await settle();
    await run(() => release(stale));
    await settle();
    await run(() => button(view, "[data-catchlight-session][data-current] [data-catchlight-session-close]").click());
    await settle();
    await settle();

    expect(view.querySelector("[data-catchlight-problem]")?.textContent ?? null).toBeNull();
    expect(view.querySelectorAll("[data-catchlight-session]")).toHaveLength(0);
    await unmount();
    editor.close();
    stop();
  });

  test("one canvas element outlives every model: empty, open, close all, open again", async () => {
    const stop = stubObservers();
    const { editor, gpu, viewports } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    const canvas = view.querySelector("canvas");
    if (!canvas) throw new Error("no canvas in the empty state");
    expect(view.querySelector("[data-catchlight-stage][data-empty]")).not.toBeNull();
    expect(viewports).toHaveLength(0);

    await run(() => editor.newSession("akari"));
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
    // On the WebGL2 tier the device can only present into the canvas it was
    // acquired from, so the stage keeps one canvas across models.
    expect(gpu.acquiredFrom).toEqual([canvas]);

    await unmount();
    editor.close();
    stop();
  });

  test("a canvas that cannot be attached says so, instead of coming up blank", async () => {
    const stop = stubObservers();
    const { editor, module } = await fakeStack();
    module.failNextAcquire =
      "the catchlight editor needs WebGPU or WebGL2 to draw and this browser offered neither";
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    await settle();

    await run(() => editor.newSession("akari"));
    await settle();
    await settle();

    expect(text(view, "[data-catchlight-problem]")).toContain("needs WebGPU or WebGL2");

    await unmount();
    editor.close();
    stop();
  });

  test("the zoom readout follows the camera, relative to the fit", async () => {
    const stop = stubObservers();
    const { editor } = await fakeStack();
    const { container: view, unmount } = await mount(<CatchlightEditor editor={editor} />);
    const session = await run(() => editor.newSession("akari"));
    await settle();

    expect(text(view, "[data-catchlight-zoom]")).toBe("100%");

    (session.replica as FakeReplica).box = [10, 20, 12, 24];
    await run(() => fit(view).click());
    expect(text(view, "[data-catchlight-zoom]")).toBe("100%");

    const canvas = view.querySelector("canvas");
    if (!canvas) throw new Error("no canvas");
    await run(() =>
      canvas.dispatchEvent(
        new WheelEvent("wheel", {
          deltaY: -100,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(text(view, "[data-catchlight-zoom]")).toBe("110%");

    await run(() => button(view, "[data-catchlight-fit]").click());
    expect(text(view, "[data-catchlight-zoom]")).toBe("100%");

    await unmount();
    editor.close();
    stop();
  });
});

async function fakeEditor(): Promise<Editor> {
  return (await fakeStack()).editor;
}

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

/** happy-dom would navigate to a download otherwise. */
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

async function run<T>(action: () => Promise<T> | T): Promise<T> {
  let result: T | undefined;
  await act(async () => {
    result = await action();
  });
  return result as T;
}

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

/** happy-dom lays nothing out, so a real observer would report zeroes. */
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
  Object.assign(globalThis, {
    ResizeObserver: Noop,
    IntersectionObserver: Noop,
  });
  return () => Object.assign(globalThis, saved);
}
