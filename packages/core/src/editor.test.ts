/**
 * The surface a host actually calls: opening documents, and what is true about
 * a `Session` the moment it is handed over.
 */

import { describe, expect, test } from "bun:test";

import { Editor, fileKey } from "./editor.js";
import { FakeEditor, fakeWasm } from "./fakes.js";
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

    const session = await editor.openDocument("project/akari.clm");

    expect(session.id).toBe(1);
    expect(wasm.requests.map((request) => request.cmd)).toEqual(["session_open"]);
  });

  test("opening a file the page holds stages it, then opens it", async () => {
    const { editor, storage, wasm } = await inTab();

    const session = await editor.openFile(new TextEncoder().encode("a model"), "Akari Final.clm");

    expect(session.id).toBe(1);
    expect(wasm.requests[0]).toMatchObject({ cmd: "session_open", path: "Akari_Final.clm" });
    // Staging is not storing: a dropped file is not written to the store until
    // somebody saves it.
    expect(await storage.list()).toEqual([]);
  });

  test("importing asks what the manifest needs, then stages exactly that", async () => {
    const { editor, storage, wasm } = await inTab();
    await storage.write("rig/model.json", new TextEncoder().encode("{}"));
    await storage.write("rig/tex0.png", new TextEncoder().encode("a texture"));
    await storage.write("rig/tex1.png", new TextEncoder().encode("another"));
    wasm.requirements = ["rig/tex0.png", "rig/tex1.png"];

    const session = await editor.importManifest("rig/model.json");

    expect(session.id).toBe(1);
    // The requirements come first, because an in-tab editor cannot go looking
    // for a file that is not staged yet.
    expect(wasm.requests.map((request) => request.cmd)).toEqual([
      "manifest_requirements",
      "session_import",
    ]);
    expect(wasm.stagedKeys()).toEqual(["rig/model.json", "rig/tex0.png", "rig/tex1.png"]);
  });

  test("saving reports the key and leaves the bytes in the store", async () => {
    const { editor, storage } = await inTab();
    const session = await editor.newDocument();

    expect(await editor.saveDocument(session, "out/akari.clm")).toBe("out/akari.clm");
    expect(await storage.list()).toEqual(["out/akari.clm"]);

    // With no key it saves where it saved last.
    expect(await editor.saveDocument(session)).toBe("out/akari.clm");
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

  test("closing frees every replica, the device and the backend", async () => {
    const { editor, module, wasm } = await inTab();
    await editor.newDocument();
    await editor.newDocument();

    editor.close();

    expect(module.replicas.map((replica) => replica.freed)).toEqual([true, true]);
    expect(module.gpu.freed).toBe(true);
    expect(wasm.freed).toBe(true);
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
