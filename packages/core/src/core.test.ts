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

  free(): void {
    this.freed = true;
  }
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
      await session.sendQuiet({ cmd: "scratch_deform", node: "hair", offsets: [] });
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
    void (() => session.sendQuiet({ cmd: "presence_get", session: 2 }));

    // @ts-expect-error — `session_new` names no session, so it belongs on the
    // editor rather than on one.
    void (() => session.send({ cmd: "session_new", name: null }));

    // @ts-expect-error — `nod_add` is not a command. The union is the spelling
    // check the old `{ cmd: string }` placeholder could not do.
    void (() => session.send({ cmd: "nod_add", parent: "root" }));

    expect(session.id).toBe(1);
  });

  test("a reply narrows on its result tag", () => {
    const body: ResponseBody = { result: "saved", path: "out.clm" };
    // `path` is reachable only after the tag is checked, which is the whole
    // point of generating the union rather than hand-writing an index type.
    expect(body.result === "saved" ? body.path : null).toBe("out.clm");
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
