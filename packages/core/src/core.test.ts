/**
 * Units for the seams, against a fake wasm editor.
 *
 * Deliberately not against the real module: these assert the *contract* — that
 * a drag does not bump the revision, that opening stages before it commands,
 * that saving drains staging — and a fake makes each of those a two-line
 * setup instead of 2 MiB of WebAssembly and a fixture model. The real module
 * gets its own thin integration suite; that one proves the wiring, this one
 * proves the rules.
 */

import { describe, expect, test } from "bun:test";

import { Editor } from "./client.js";
import { Session } from "./session.js";
import { MemoryStorage, NotFoundError } from "./storage.js";
import { LocalTransport, ProtocolError } from "./transport.js";
import { Viewport, devicePixelSize } from "./viewport.js";
import type { WasmViewport } from "./viewport.js";
import type { ResponseBody } from "./protocol.gen.js";
import type { WasmEditor } from "./transport.js";

/** A wasm editor that answers the handful of commands these tests send. */
class FakeWasm implements WasmEditor {
  staged = new Map<string, Uint8Array>();
  requests: Array<Record<string, unknown>> = [];
  freed = false;
  /** Commands to answer with an error instead, keyed by `cmd`. */
  refuse = new Map<string, { code: string; message: string }>();

  #nextSession = 1;

  handle(requestJson: string): string {
    const request = JSON.parse(requestJson) as Record<string, unknown>;
    this.requests.push(request);
    const id = request.id as number;
    const cmd = request.cmd as string;

    const refusal = this.refuse.get(cmd);
    if (refusal) {
      return JSON.stringify({ reply: "err", id, ...refusal });
    }

    switch (cmd) {
      case "session_new":
        return this.#ok(id, { result: "session", session: this.#nextSession++ });
      case "session_open": {
        const key = request.path as string;
        if (!this.staged.has(key)) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "io",
            message: `${JSON.stringify(key)} was not staged`,
          });
        }
        return this.#ok(id, { result: "session", session: this.#nextSession++ });
      }
      case "save": {
        const key = (request.path as string | undefined) ?? "previous.clm";
        this.staged.set(key, new TextEncoder().encode(`bytes of ${key}`));
        return this.#ok(id, { result: "saved", path: key });
      }
      case "session_list":
        return this.#ok(id, { result: "sessions", sessions: [] });
      default:
        return this.#ok(id, { result: "empty" });
    }
  }

  #ok(id: number, body: Record<string, unknown>): string {
    return JSON.stringify({ reply: "ok", id, body });
  }

  putBytes(key: string, bytes: Uint8Array): void {
    this.staged.set(key, bytes);
  }

  takeBytes(key: string): Uint8Array | undefined {
    const bytes = this.staged.get(key);
    this.staged.delete(key);
    return bytes;
  }

  stagedKeys(): string[] {
    return [...this.staged.keys()].sort();
  }

  attach(_canvas: HTMLCanvasElement, _session: number): Promise<WasmViewport> {
    return Promise.resolve(new FakeViewport());
  }

  free(): void {
    this.freed = true;
  }
}

/** A renderer that counts what it was told, and draws nothing. */
class FakeViewport implements WasmViewport {
  started = 0;
  stopped = 0;
  invalidated = 0;
  freed = 0;
  size: [number, number] | undefined;

  start(): void {
    this.started += 1;
  }
  stop(): void {
    this.stopped += 1;
  }
  invalidate(): void {
    this.invalidated += 1;
  }
  resize(width: number, height: number): void {
    this.size = [width, height];
  }
  setCamera(): void {}
  maxSize(): number {
    return 4096;
  }
  free(): void {
    this.freed += 1;
  }
}

/** Just enough of a `ResizeObserverEntry` for the size arithmetic. */
function entry(parts: {
  devicePixelContentBoxSize?: ResizeObserverSize[];
  contentBoxSize?: ResizeObserverSize[];
  contentRect?: { width: number; height: number };
}): ResizeObserverEntry {
  return { contentRect: { width: 0, height: 0 }, ...parts } as unknown as ResizeObserverEntry;
}

describe("LocalTransport", () => {
  test("gives every request its own correlation id", async () => {
    const wasm = new FakeWasm();
    const transport = new LocalTransport(wasm);
    await transport.send({ cmd: "session_new", name: null });
    await transport.send({ cmd: "session_new", name: null });
    expect(wasm.requests.map((r) => r.id)).toEqual([1, 2]);
  });

  test("rejects a refusal as a ProtocolError carrying its code", async () => {
    const wasm = new FakeWasm();
    wasm.refuse.set("session_open", { code: "no_session", message: "nope" });
    const transport = new LocalTransport(wasm);

    const failure = transport.send({ cmd: "session_open", path: "x.clm" });
    await expect(failure).rejects.toBeInstanceOf(ProtocolError);
    await expect(failure).rejects.toMatchObject({ code: "no_session", message: "nope" });
  });

  test("a closed transport rejects rather than throwing synchronously", async () => {
    const wasm = new FakeWasm();
    const transport = new LocalTransport(wasm);
    transport.close();
    expect(wasm.freed).toBe(true);
    await expect(transport.send({ cmd: "session_new", name: null })).rejects.toMatchObject({
      code: "closed",
    });
  });
});

describe("Session revisions", () => {
  test("a document command bumps the revision and notifies", async () => {
    const wasm = new FakeWasm();
    const session = new Session(new LocalTransport(wasm), 1);
    let notified = 0;
    session.subscribe(() => {
      notified += 1;
    });

    expect(session.getRevision()).toBe(0);
    await session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    expect(session.getRevision()).toBe(1);
    expect(notified).toBe(1);
  });

  test("the presence path bumps nothing — a drag never reaches React", async () => {
    const wasm = new FakeWasm();
    const session = new Session(new LocalTransport(wasm), 1);
    let notified = 0;
    session.subscribe(() => {
      notified += 1;
    });

    for (let i = 0; i < 100; i++) {
      await session.sendPresence({ cmd: "scratch_deform", node: "hair", offsets: [] });
    }

    expect(session.getRevision()).toBe(0);
    expect(notified).toBe(0);
    // The commands did happen; they just are not revisions.
    expect(wasm.requests).toHaveLength(100);
  });

  test("unsubscribing during a notification does not skip a listener", async () => {
    const wasm = new FakeWasm();
    const session = new Session(new LocalTransport(wasm), 1);
    const seen: string[] = [];
    const off = session.subscribe(() => {
      seen.push("first");
      off();
    });
    session.subscribe(() => seen.push("second"));

    await session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    expect(seen).toEqual(["first", "second"]);
  });
});

describe("Generated wire types", () => {
  test("a session command cannot address a session, and a bare one cannot omit it", () => {
    const session = new Session(new LocalTransport(new FakeWasm()), 1);

    // @ts-expect-error — the session fills this in; a caller that could pass
    // it could address the wrong document.
    void (() => session.query({ cmd: "presence_get", session: 2 }));

    // @ts-expect-error — `session_new` names no session, so it belongs on the
    // editor rather than on one.
    void (() => session.send({ cmd: "session_new", name: null }));

    // @ts-expect-error — `nod_add` is not a command. The union is the spelling
    // check the old `{ cmd: string }` placeholder could not do.
    void (() => session.send({ cmd: "nod_add", parent: "root" }));

    expect(session.id).toBe(1);
  });

  test("a command cannot be sent by the wrong method", () => {
    const session = new Session(new LocalTransport(new FakeWasm()), 1);

    // @ts-expect-error — `scratch_deform` is a presence command. Sending it
    // through `send` would bump the revision and re-render every panel on
    // every pointer move, which is the bug this split exists to prevent.
    void (() => session.send({ cmd: "scratch_deform", node: "hair", offsets: [] }));

    // @ts-expect-error — `node_set` changes the document, so it cannot go out
    // as presence: the panels would never learn the edit happened.
    void (() => session.sendPresence({ cmd: "node_set", node: "hair", patch: {} }));

    // @ts-expect-error — `status` is a read; `sendPresence` would repaint a
    // canvas that nothing changed.
    void (() => session.sendPresence({ cmd: "status" }));

    // @ts-expect-error — and a document command is not a query.
    void (() => session.query({ cmd: "undo" }));

    expect(session.id).toBe(1);
  });

  test("a reply narrows on its result tag", () => {
    const body: ResponseBody = { result: "saved", path: "out.clm" };
    // `path` is reachable only after the tag is checked, which is the whole
    // point of generating the union rather than hand-writing an index type.
    expect(body.result === "saved" ? body.path : null).toBe("out.clm");
  });
});

describe("Viewport", () => {
  test("a drag repaints without bumping the revision", async () => {
    const session = new Session(new LocalTransport(new FakeWasm()), 1);
    const view = new FakeViewport();
    new Viewport(view, {} as HTMLCanvasElement, session);
    let revisions = 0;
    session.subscribe(() => {
      revisions += 1;
    });

    for (let i = 0; i < 20; i++) {
      await session.sendPresence({ cmd: "scratch_deform", node: "hair", offsets: [] });
    }

    // Twenty repaints asked for, no revision: React saw nothing, the canvas
    // saw everything. That split is the whole reason there are two channels.
    expect(view.invalidated).toBe(20);
    expect(revisions).toBe(0);
  });

  test("a document command repaints too", async () => {
    const session = new Session(new LocalTransport(new FakeWasm()), 1);
    const view = new FakeViewport();
    new Viewport(view, {} as HTMLCanvasElement, session);

    await session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    expect(view.invalidated).toBe(1);
  });

  test("disposing stops the renderer and lets go of the session", async () => {
    const session = new Session(new LocalTransport(new FakeWasm()), 1);
    const view = new FakeViewport();
    const viewport = new Viewport(view, {} as HTMLCanvasElement, session);

    viewport.dispose();
    await session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });

    expect(view.stopped).toBe(1);
    expect(view.freed).toBe(1);
    // A disposed viewport that stayed subscribed would keep the whole GPU
    // state alive behind the session for as long as the document is open.
    expect(view.invalidated).toBe(0);
  });

  test("an editor with no renderer in this page says so", async () => {
    const editor = new Editor({
      transport: new LocalTransport(new FakeWasm()),
      storage: new MemoryStorage(),
    });
    const session = await editor.newDocument();
    await expect(editor.attach(session, {} as HTMLCanvasElement)).rejects.toThrow(
      /no renderer in this page/,
    );
  });

  test("device pixels are taken from the browser, not multiplied out", () => {
    // The observed box is already device pixels: 393.6 CSS px at 1.25 zoom is
    // 492 real pixels, which no rounding of 393.6 × 1.25 has to reproduce.
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

describe("Editor documents", () => {
  test("opening reads the store, stages the bytes, then names the key", async () => {
    const wasm = new FakeWasm();
    const storage = new MemoryStorage();
    const bytes = new TextEncoder().encode("a model");
    await storage.write("project/akari.clm", bytes);

    const editor = Editor.local(wasm, storage);
    const session = await editor.openDocument("project/akari.clm");

    expect(session).toBeInstanceOf(Session);
    // The staged bytes had to be there before the command ran, or the fake
    // would have refused it.
    const open = wasm.requests.find((r) => r.cmd === "session_open");
    expect(open?.path).toBe("project/akari.clm");
  });

  test("opening a key the store does not have fails before any command", async () => {
    const wasm = new FakeWasm();
    const editor = Editor.local(wasm, new MemoryStorage());

    await expect(editor.openDocument("missing.clm")).rejects.toBeInstanceOf(NotFoundError);
    expect(wasm.requests).toHaveLength(0);
  });

  test("saving drains staging into the store rather than copying it", async () => {
    const wasm = new FakeWasm();
    const storage = new MemoryStorage();
    const editor = Editor.local(wasm, storage);
    const session = await editor.newDocument();

    const key = await editor.saveDocument(session, "out/akari.clm");

    expect(key).toBe("out/akari.clm");
    expect(await storage.read("out/akari.clm")).toEqual(
      new TextEncoder().encode("bytes of out/akari.clm"),
    );
    // Drained: nothing is held twice.
    expect(wasm.stagedKeys()).toEqual([]);
  });

  test("saving with no key reuses the one the reply names", async () => {
    const wasm = new FakeWasm();
    const storage = new MemoryStorage();
    const editor = Editor.local(wasm, storage);
    const session = await editor.newDocument();

    expect(await editor.saveDocument(session)).toBe("previous.clm");
    expect(storage.keys()).toEqual(["previous.clm"]);
  });

  test("a save the editor did not stage is an error, not a silent no-op", async () => {
    const wasm = new FakeWasm();
    // Report success but stage nothing — the shape of a server-side bug.
    wasm.handle = ((requestJson: string) => {
      const { id, cmd } = JSON.parse(requestJson) as { id: number; cmd: string };
      const body =
        cmd === "save" ? { result: "saved", path: "ghost.clm" } : { result: "session", session: 1 };
      return JSON.stringify({ reply: "ok", id, body });
    }) as WasmEditor["handle"];

    const editor = Editor.local(wasm, new MemoryStorage());
    const session = await editor.newDocument();
    await expect(editor.saveDocument(session)).rejects.toMatchObject({ code: "bad_reply" });
  });
});

/**
 * A canvas and the two observers the viewport reaches for, with the callbacks
 * exposed so a test can fire them. Restores whatever was there when it is
 * done, so one test's fakes never leak into the next.
 */
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

describe("Viewport visibility", () => {
  test("a viewport scrolled off screen stops, and starts again when it returns", () => {
    const browser = stubBrowser(800, 600, 1);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();
    expect(view.started).toBe(1);

    browser.scroll(false);
    expect(view.stopped).toBe(1);
    // Still started as far as the host is concerned — nothing was disposed.
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

    // The observer is disconnected, but even a stale callback must not revive a
    // viewport the host stopped: `#started` gates it, not the observer.
    browser.scroll(true);
    expect(view.started).toBe(startedBefore);

    browser.restore();
  });

  test("the backing store is the CSS box times the device pixel ratio on a 2x screen", () => {
    const browser = stubBrowser(800, 600, 2);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();

    // 800x600 CSS pixels on a retina screen is a 1600x1200 backing store, and
    // the canvas attributes and the surface configuration have to agree — a
    // canvas sized in CSS pixels is the classic blurry-viewport bug.
    expect(view.size).toEqual([1600, 1200]);
    expect(browser.canvas.width).toBe(1600);
    expect(browser.canvas.height).toBe(1200);

    viewport.stop();
    browser.restore();
  });
});

describe("Viewport backing store", () => {
  test("a canvas larger than the adapter allows is clamped to what it reported", () => {
    // 5000 CSS pixels at 2x is a 10000-pixel backing store; the fake renderer
    // reports a 4096 limit, and a surface configured past it fails outright.
    const browser = stubBrowser(5000, 5000, 2);
    const view = new FakeViewport();
    const viewport = new Viewport(view, browser.canvas);

    viewport.start();
    expect(view.size).toEqual([4096, 4096]);

    viewport.stop();
    browser.restore();
  });
});
