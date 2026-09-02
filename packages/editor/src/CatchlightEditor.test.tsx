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
import type { FakeEditor } from "@catchlight/core/fakes";
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

    // The default row is a bare slider, so the name is this package's doing.
    expect(text(view, "[data-catchlight-param-name]")).toBe("head:yaw");
    const slider = view.querySelector<HTMLInputElement>("[data-catchlight-param-slider]");
    expect(slider?.min).toBe("-1");
    expect(slider?.max).toBe("1");

    await unmount();
    editor.close();
    stop();
  });
});

/** A real editor over the fake wasm module: the layer under this one, working. */
async function fakeEditor(): Promise<Editor> {
  const module = fakeWasm();
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  return Editor.create(module.module, new InTabBackend(wasm, new MemoryStorage()));
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
  const button = view.querySelector<HTMLButtonElement>("[data-catchlight-save]");
  if (!button) throw new Error("no save button");
  return button;
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
