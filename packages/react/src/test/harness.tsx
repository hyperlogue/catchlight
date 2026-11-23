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

export function fakeReplica(session: Session): FakeReplica {
  return session.replica as FakeReplica;
}

export interface Mounted {
  container: HTMLElement;
  render(ui: ReactNode): Promise<void>;
  unmount(): Promise<void>;
}

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

export async function run<T>(action: () => Promise<T> | T): Promise<T> {
  let result: T | undefined;
  await act(async () => {
    result = await action();
  });
  return result as T;
}

export async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

// Fixed CSS size and position for observed elements; happy-dom reports zero.
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
  // happy-dom drops `pointerId` from the constructor init.
  Object.defineProperty(event, "pointerId", { value: parts.pointerId ?? 1 });
  return event;
}

export function wheel(deltaY: number, clientX: number, clientY: number): WheelEvent {
  const event = new WheelEvent("wheel", { deltaY, bubbles: true, cancelable: true });
  // happy-dom's `WheelEvent` drops the mouse half of the init dictionary.
  Object.defineProperty(event, "clientX", { value: clientX });
  Object.defineProperty(event, "clientY", { value: clientY });
  return event;
}

export async function fire(target: EventTarget, event: Event): Promise<void> {
  await act(async () => {
    target.dispatchEvent(event);
  });
}
