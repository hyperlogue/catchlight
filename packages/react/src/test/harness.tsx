/**
 * What every suite here needs: a real `Editor` over the fake wasm module, a
 * React root to mount into, and the two browser measurements happy-dom does
 * not make.
 *
 * The editor is the real one — the point of these tests is the layer above it,
 * so the only thing faked is the wasm module underneath.
 */

import { Editor, InTabBackend, MemoryStorage } from "@catchlight/core";
import type { Session } from "@catchlight/core";
import { FakeEditor, fakeWasm } from "@catchlight/core/fakes";
import type { FakeReplica, FakeViewport } from "@catchlight/core/fakes";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot } from "react-dom/client";

export interface Harness {
  editor: Editor;
  wasm: FakeEditor;
  replicas: FakeReplica[];
  viewports: FakeViewport[];
}

export async function harness(): Promise<Harness> {
  const module = fakeWasm();
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  const editor = await Editor.create(module.module, new InTabBackend(wasm, new MemoryStorage()));
  return { editor, wasm, replicas: module.replicas, viewports: module.viewports };
}

/** The replica behind a session, as the fake implements it. */
export function fakeReplica(session: Session): FakeReplica {
  return session.replica as FakeReplica;
}

export interface Mounted {
  container: HTMLElement;
  render(ui: ReactNode): Promise<void>;
  unmount(): Promise<void>;
}

/** Mounts into a detached container and gives back the two things a test drives. */
export async function mount(ui: ReactNode): Promise<Mounted> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const render = async (next: ReactNode): Promise<void> => {
    await act(async () => {
      root.render(next);
    });
  };
  await render(ui);
  return {
    container,
    render,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
      container.remove();
    },
  };
}

/** Runs an action that moves the session, letting React commit what it caused. */
export async function run<T>(action: () => Promise<T> | T): Promise<T> {
  let result: T | undefined;
  await act(async () => {
    result = await action();
  });
  return result as T;
}

/** Lets whatever a handler started settle before a test looks at the result. */
export async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

/**
 * Gives every observed element a fixed CSS size and screen position — the two
 * things a layout engine would have decided and happy-dom reports as zero.
 * Returns a restore function.
 */
export function stubLayout(width: number, height: number, left = 0, top = 0): () => void {
  const saved = {
    ResizeObserver: globalThis.ResizeObserver,
    IntersectionObserver: globalThis.IntersectionObserver,
    rect: Element.prototype.getBoundingClientRect,
  };

  globalThis.ResizeObserver = class {
    #callback: ResizeObserverCallback;
    constructor(callback: ResizeObserverCallback) {
      this.#callback = callback;
    }
    observe(): void {
      const box = { inlineSize: width, blockSize: height };
      const entry = {
        contentBoxSize: [box],
        devicePixelContentBoxSize: [box],
        contentRect: { width, height },
      } as unknown as ResizeObserverEntry;
      this.#callback([entry], this as unknown as ResizeObserver);
    }
    unobserve(): void {}
    disconnect(): void {}
  } as unknown as typeof ResizeObserver;

  globalThis.IntersectionObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
    takeRecords(): IntersectionObserverEntry[] {
      return [];
    }
  } as unknown as typeof IntersectionObserver;

  Element.prototype.getBoundingClientRect = function rect(): DOMRect {
    return {
      x: left,
      y: top,
      left,
      top,
      width,
      height,
      right: left + width,
      bottom: top + height,
      toJSON: () => ({}),
    } as DOMRect;
  };

  return () => {
    globalThis.ResizeObserver = saved.ResizeObserver;
    globalThis.IntersectionObserver = saved.IntersectionObserver;
    Element.prototype.getBoundingClientRect = saved.rect;
  };
}

/** A pointer event with the fields the viewport reads. */
export function pointer(
  type: string,
  parts: { clientX?: number; clientY?: number; button?: number; pointerId?: number },
): PointerEvent {
  const event = new PointerEvent(type, {
    bubbles: true,
    cancelable: true,
    clientX: parts.clientX ?? 0,
    clientY: parts.clientY ?? 0,
    button: parts.button ?? 0,
  });
  // happy-dom does not carry `pointerId` through the constructor, and the
  // viewport matches moves to the pointer that started the gesture by it.
  Object.defineProperty(event, "pointerId", { value: parts.pointerId ?? 1 });
  return event;
}

/** A wheel event with the fields the zoom reads. */
export function wheel(deltaY: number, clientX: number, clientY: number): WheelEvent {
  const event = new WheelEvent("wheel", { deltaY, bubbles: true, cancelable: true });
  // happy-dom's `WheelEvent` drops the mouse half of the init dictionary.
  Object.defineProperty(event, "clientX", { value: clientX });
  Object.defineProperty(event, "clientY", { value: clientY });
  return event;
}

/** Dispatches `event` on `target` inside `act`, so React commits before the test looks. */
export async function fire(target: EventTarget, event: Event): Promise<void> {
  await act(async () => {
    target.dispatchEvent(event);
  });
}
