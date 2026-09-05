/**
 * The surface a host actually calls: opening documents, and what is true about
 * a `Session` the moment it is handed over.
 */

import { describe, expect, test } from "bun:test";

import { Editor, fileKey } from "./editor.js";
import { FakeEditor, fakeWasm, readStructure } from "./fakes.js";
import { InTabBackend } from "./in-tab.js";
import { MemoryStorage } from "./storage.js";

async function inTab(): Promise<{
  editor: Editor;
  wasm: FakeEditor;
  storage: MemoryStorage;
  module: ReturnType<typeof fakeWasm>;
}> {
  const module = fakeWasm();
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  const storage = new MemoryStorage();
  const editor = await Editor.create(module.module, new InTabBackend(wasm, storage));
  return { editor, wasm, storage, module };
}

describe("opening documents", () => {
  test("a new session is already readable when it is handed over", async () => {
    const { editor } = await inTab();

    const session = await editor.newDocument("akari");

    // Fed before the caller ever saw it: the first thing a panel does is a
    // read, and a read that answers "nothing loaded yet" is a bug a host
    // would have to work around forever.
    expect(session.tree().id).toBe("root");
    expect(session.getRevision()).toBe(1);
    expect(editor.session(session.id)).toBe(session);
  });

  test("an edit is readable the moment its promise settles", async () => {
    const { editor } = await inTab();
    const session = await editor.newDocument();

    const body = await session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" });
    const node = body.result === "node" ? body.node : "";

    // The whole reason `send` waits on the reply's revision: the Id it minted
    // resolves in the very next line, with no poll and no second round trip.
    const children = session.tree().children.map((child) => child.id);
    expect(children).toEqual([node]);
    expect(session.getRevision()).toBe(2);
  });

  test("opening reads the store", async () => {
    const { editor, storage, wasm } = await inTab();
    await storage.write("project/akari.clm", new TextEncoder().encode("a model"));

    // The store here is the editor's, so the key names a file it can read.
    wasm.written.set("project/akari.clm", new TextEncoder().encode("a model"));
    const session = await editor.openDocument("project/akari.clm");

    expect(session.id).toBe(1);
    expect(wasm.requests.map((request) => request.cmd)).toEqual(["session_open"]);
  });

  test("a file the page holds is a fresh session and an import into it", async () => {
    const { editor, storage, wasm } = await inTab();
    const bytes = new TextEncoder().encode("a model");

    const session = await editor.openFile(bytes, "Akari Final.clm");

    expect(session.id).toBe(1);
    expect(wasm.requests.map((r) => r.cmd)).toEqual(["session_new", "import_file"]);
    expect(wasm.requests[0]).toMatchObject({ cmd: "session_new", name: "Akari_Final.clm" });
    // The bytes went with the command that reads them, not under a key.
    expect(wasm.attached[1]).toEqual({ model: bytes });
    expect(wasm.writtenKeys()).toEqual([]);
    // A dropped file is not written to the store until somebody saves it.
    expect(await storage.list()).toEqual([]);
  });

  test("importing reads the manifest here and attaches every texture it names", async () => {
    const { editor, storage, wasm } = await inTab();
    const manifest = new TextEncoder().encode(
      JSON.stringify({ textures: [{ id: "a", path: "tex0.png" }, { id: "b", path: "tex1.png" }] }),
    );
    const tex0 = new TextEncoder().encode("a texture");
    const tex1 = new TextEncoder().encode("another");
    await storage.write("rig/model.json", manifest);
    await storage.write("rig/tex0.png", tex0);
    await storage.write("rig/tex1.png", tex1);

    const session = await editor.importManifest("rig/model.json");

    expect(session.id).toBe(1);
    // No round trip to ask what it needs: the manifest says, and reading it is
    // a pure function.
    expect(wasm.requests.map((r) => r.cmd)).toEqual(["session_new", "import_manifest"]);
    // Each reference resolved against the manifest's own key, and attached
    // under the name the import matches it by.
    expect(wasm.attached[1]).toEqual({
      manifest,
      "texture:tex0.png": tex0,
      "texture:tex1.png": tex1,
    });
    expect(wasm.writtenKeys()).toEqual([]);
  });

  test("opening a file twice is two documents", async () => {
    const { editor, wasm } = await inTab();

    await editor.openFile(new TextEncoder().encode("a model"), "akari.clm");
    expect(wasm.writtenKeys()).toEqual([]);

    // The same name again is a second document, not a failure: nothing here
    // depends on the first open's bytes still being around.
    const second = await editor.openFile(new TextEncoder().encode("a newer model"), "akari.clm");
    expect(second.id).toBe(2);
    expect(wasm.writtenKeys()).toEqual([]);
  });

  test("saving reports the key and leaves the bytes in the store", async () => {
    const { editor, storage } = await inTab();
    const session = await editor.newDocument();

    expect(await editor.saveDocument(session, "out/akari.clm")).toBe("out/akari.clm");
    expect(await storage.list()).toEqual(["out/akari.clm"]);

    // With no key it saves where it saved last.
    expect(await editor.saveDocument(session)).toBe("out/akari.clm");
  });

  test("a saved document reads back as the bytes the editor wrote", async () => {
    const { editor, wasm } = await inTab();
    const session = await editor.newDocument("akari");

    const key = await editor.saveDocument(session, "akari.clm");
    const bytes = await editor.readDocument(key);

    // What a download hands the browser: the store's copy, which is what the
    // editor wrote for the save — not a re-serialization here.
    if (!bytes) throw new Error("the store handed back nothing");
    expect(readStructure(bytes).title).toBe(wasm.snapshot(session.id).title);
    // And a key nobody wrote is an error, never an empty download.
    await expect(editor.readDocument("nowhere.clm")).rejects.toThrow("nowhere.clm");
  });

  test("closing a document frees its replica and tells the list", async () => {
    const { editor, wasm, module } = await inTab();
    const session = await editor.newDocument("akari");
    let changes = 0;
    const off = editor.onSessionsChanged(() => {
      changes += 1;
    });

    await editor.closeDocument(session.id);

    expect(wasm.requests.map((request) => request.cmd)).toEqual(["session_new", "session_close"]);
    expect(await editor.listSessions()).toEqual([]);
    expect(editor.session(session.id)).toBeUndefined();
    expect(module.replicas[0]?.freed).toBe(true);
    expect(changes).toBe(1);
    off();

    // A session this tab never attached — one an agent opened — closes too.
    wasm.handle(JSON.stringify({ id: 99, cmd: "session_new", name: "from an agent" }));
    wasm.drainEvents();
    const [info] = await editor.listSessions();
    if (!info) throw new Error("the editor listed no sessions");
    await editor.closeDocument(info.session);
    expect(await editor.listSessions()).toEqual([]);
  });

  test("a document that moved reaches onSessionsChanged too", async () => {
    const { editor } = await inTab();
    const session = await editor.newDocument();
    let changes = 0;
    const off = editor.onSessionsChanged(() => {
      changes += 1;
    });

    // The list carries the revision, the node count and the dirty flag, and
    // an edit moves all three — a list that only refreshed on open and close
    // would show a document as clean forever.
    await session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" });
    expect(changes).toBe(1);
    off();
  });

  test("a session an agent opened can be followed by id", async () => {
    const { editor, wasm } = await inTab();
    // Something else opened it: the editor knows, this tab does not.
    wasm.handle(JSON.stringify({ id: 99, cmd: "session_new", name: "from an agent" }));
    wasm.drainEvents();

    const [info] = await editor.listSessions();
    if (!info) throw new Error("the editor listed no sessions");
    const session = await editor.attachSession(info);

    expect(session.id).toBe(info.session);
    expect(session.tree().id).toBe("root");
    // Twice is the same session, not a second replica of it.
    expect(await editor.attachSession(info)).toBe(session);
  });

  test("a document opened anywhere reaches onSessionsChanged", async () => {
    const { editor } = await inTab();
    let changes = 0;
    const off = editor.onSessionsChanged(() => {
      changes += 1;
    });

    await editor.newDocument();
    expect(changes).toBe(1);

    off();
    await editor.newDocument();
    expect(changes).toBe(1);
  });
});

describe("viewports and lifetime", () => {
  test("a viewport draws the session it was attached to", async () => {
    const { editor, module } = await inTab();
    const session = await editor.newDocument();

    await editor.attach(session, {} as HTMLCanvasElement);
    session.setParam("param-1", 0.5);

    const view = module.viewports[0];
    expect(module.viewports).toHaveLength(1);
    expect(view?.invalidated).toBe(1);
  });

  test("a document opens with no device at all", async () => {
    const { editor, module } = await inTab();
    const session = await editor.newDocument();

    // Reading a tree never needed a GPU, and a tab that only lists documents
    // should not be holding a device.
    expect(module.gpu.acquires).toBe(0);
    expect(session.tree().id).toBe("root");
  });

  test("the first canvas acquires the device, and the second shares it", async () => {
    const { editor, module } = await inTab();
    const session = await editor.newDocument();
    const first = { id: "first" } as unknown as HTMLCanvasElement;
    const second = { id: "second" } as unknown as HTMLCanvasElement;

    const [a, b] = await Promise.all([
      editor.attach(session, first),
      editor.attach(session, second),
    ]);

    // Two viewports mounting in one tick is a React remount, not a reason for
    // two devices; the second waits on the first one's acquisition. Which
    // canvas the device came from matters on the fallback tier, where it is
    // the one the device can present into: it is the first.
    expect(module.gpu.acquires).toBe(1);
    expect(module.gpu.acquiredFrom).toEqual([first]);
    expect(module.viewports).toHaveLength(2);
    expect(a).not.toBe(b);
  });

  test("an acquisition that failed is tried again, not remembered", async () => {
    const { editor, module } = await inTab();
    const session = await editor.newDocument();
    module.failNextAcquire = "this browser needs WebGPU or WebGL2";

    await expect(editor.attach(session, {} as HTMLCanvasElement)).rejects.toThrow(
      "this browser needs WebGPU or WebGL2",
    );

    // The next mount is a new chance — StrictMode's second one, or a canvas
    // that can be acquired from where the first could not.
    const canvas = { id: "second" } as unknown as HTMLCanvasElement;
    await editor.attach(session, canvas);
    expect(module.gpu.acquiredFrom).toEqual([canvas]);
    expect(module.viewports).toHaveLength(1);
  });

  test("closing frees every replica, the device and the backend", async () => {
    const { editor, module, wasm } = await inTab();
    const session = await editor.newDocument();
    await editor.newDocument();
    await editor.attach(session, {} as HTMLCanvasElement);

    editor.close();

    expect(module.replicas.map((replica) => replica.freed)).toEqual([true, true]);
    expect(module.gpu.freed).toBe(true);
    expect(wasm.freed).toBe(true);
  });

  test("closing an editor that never drew anything frees no device", async () => {
    const { editor, module, wasm } = await inTab();
    await editor.newDocument();

    editor.close();

    expect(module.gpu.freed).toBe(false);
    expect(wasm.freed).toBe(true);
  });
});

describe("what the editor says about itself", () => {
  test("the backend kind is what a status line reads", async () => {
    const { editor } = await inTab();
    expect(editor.backendKind()).toBe("in-tab");
  });

  test("there is no graphics tier until a canvas has asked for a device", async () => {
    const { editor, module } = await inTab();
    const session = await editor.newDocument();
    module.gpu.tierName = "webgl2";

    // Opening a document, reading it and running a drag need no device, so
    // there is nothing truthful to say until an attach happens.
    expect(editor.gpuTier()).toBeUndefined();

    let told = 0;
    const off = editor.onGpuChanged(() => (told += 1));
    await editor.attach(session, {} as HTMLCanvasElement);

    expect(editor.gpuTier()).toBe("webgl2");
    expect(told).toBe(1);

    // One device per editor, acquired once: a second canvas says nothing new.
    await editor.attach(session, {} as HTMLCanvasElement);
    expect(told).toBe(1);
    off();
  });
});

describe("file keys", () => {
  test("keeps the last segment and the extension, and nothing else", () => {
    expect(fileKey("models/Akari Final.clm")).toBe("Akari_Final.clm");
    expect(fileKey("a/b/c/rig.inx")).toBe("rig.inx");
    expect(fileKey("...")).toBe("untitled.clm");
    expect(fileKey("")).toBe("untitled.clm");
  });
});
