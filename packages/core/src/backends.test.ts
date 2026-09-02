/**
 * The two backends against the same contract: what `send` guarantees, when a
 * key gets staged, and what a feed fetches.
 */

import { describe, expect, test } from "bun:test";

import { ProtocolError } from "./backend.js";
import { ConnectedBackend } from "./connected.js";
import type { Event } from "./protocol.gen.js";
import {
  emptyDoc,
  FakeEditor,
  FakeReplica,
  FakeSocket,
  fakeFetch,
  httpResponse,
  structureBytes,
} from "./fakes.js";
import { InTabBackend } from "./in-tab.js";
import { MemoryStorage, NotFoundError } from "./storage.js";

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe("in-tab", () => {
  test("dispatches an event before the send that produced it resolves", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    const order: string[] = [];
    backend.onEvent((event: Event) => order.push(event.event));

    const reply = await backend.send({ cmd: "session_new", name: "akari" });
    order.push("resolved");

    // A caller about to wait on the reply's revision needs the feed that this
    // event starts to already be in flight.
    expect(order).toEqual(["sessions_changed", "resolved"]);
    expect(reply.body).toEqual({ result: "session", session: 1 });
    expect(reply.rev).toBe(1);
  });

  test("stages the key a command names, out of the store, and then drops it", async () => {
    const editor = new FakeEditor();
    const storage = new MemoryStorage();
    await storage.write("project/akari.clm", new TextEncoder().encode("a model"));
    const backend = new InTabBackend(editor, storage);

    await backend.send({ cmd: "session_open", path: "project/akari.clm" });

    // The fake refuses an unstaged key, so reaching a `session` reply is the
    // proof that the bytes were there first — and staging is empty afterwards,
    // because the model the open built owns its own copy.
    expect(editor.requests.map((r) => r.cmd)).toEqual(["session_open"]);
    expect(editor.stagedKeys()).toEqual([]);

    // And the key is still openable: the second one stages out of the store
    // again rather than finding a hole where the bytes used to be.
    await backend.send({ cmd: "session_open", path: "project/akari.clm" });
    expect(editor.docs.size).toBe(2);
  });

  test("a command that failed keeps its bytes staged", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    editor.refuse.set("session_open", { code: "io", message: "not a model" });
    await backend.putBytes("akari.clm", new TextEncoder().encode("a model"));

    await expect(backend.send({ cmd: "session_open", path: "akari.clm" })).rejects.toBeInstanceOf(
      ProtocolError,
    );

    // Nothing read them, and the caller may retry without producing them twice.
    expect(editor.stagedKeys()).toEqual(["akari.clm"]);
  });

  test("a key the store does not have fails before any command", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());

    await expect(backend.send({ cmd: "session_open", path: "missing.clm" })).rejects.toBeInstanceOf(
      NotFoundError,
    );
    expect(editor.requests).toHaveLength(0);
  });

  test("bytes the caller staged are not looked up in the store", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());

    await backend.putBytes("dropped.clm", new TextEncoder().encode("a model"));
    const reply = await backend.send({ cmd: "session_open", path: "dropped.clm" });

    // An empty store and a document all the same: a file the page holds never
    // has to be written down first.
    expect(reply.body).toMatchObject({ result: "session" });
  });

  test("saving drains staging into the store rather than copying it", async () => {
    const editor = new FakeEditor();
    const storage = new MemoryStorage();
    const backend = new InTabBackend(editor, storage);
    await backend.send({ cmd: "session_new", name: null });

    const reply = await backend.send({ cmd: "save", session: 1, path: "out/akari.clm" });

    expect(reply.body).toEqual({ result: "saved", path: "out/akari.clm" });
    expect(await storage.list()).toEqual(["out/akari.clm"]);
    // Drained: a staging map that is never emptied holds every texture twice.
    expect(editor.stagedKeys()).toEqual([]);
  });

  test("an export drains the files it wrote beside the one it named", async () => {
    const editor = new FakeEditor();
    const storage = new MemoryStorage();
    const backend = new InTabBackend(editor, storage);
    await backend.send({ cmd: "session_new", name: null });

    await backend.send({ cmd: "export_manifest", session: 1, path: "out/model.json" });

    expect(await storage.list()).toEqual(["out/model.json", "tex0.png"]);
    expect(editor.stagedKeys()).toEqual([]);
  });

  test("a save that staged no bytes is an error, not a silent no-op", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    await backend.send({ cmd: "session_new", name: null });
    // The shape of an editor-side bug: success reported, nothing written.
    editor.handle = ((requestJson: string) => {
      const { id } = JSON.parse(requestJson) as { id: number };
      return JSON.stringify({ reply: "ok", id, rev: 1, body: { result: "saved", path: "ghost.clm" } });
    }) as FakeEditor["handle"];

    await expect(backend.send({ cmd: "save", session: 1, path: null })).rejects.toMatchObject({
      code: "bad_reply",
    });
  });

  test("a refusal rejects as a ProtocolError carrying its code", async () => {
    const editor = new FakeEditor();
    editor.refuse.set("undo", { code: "nothing_to_undo", message: "nothing to undo" });
    const backend = new InTabBackend(editor, new MemoryStorage());

    const failure = backend.send({ cmd: "undo", session: 1 });
    await expect(failure).rejects.toBeInstanceOf(ProtocolError);
    await expect(failure).rejects.toMatchObject({ code: "nothing_to_undo" });
  });

  test("a feed hands the editor's own model to the replica", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    const replica = new FakeReplica();
    await backend.send({ cmd: "session_new", name: null });
    await backend.send({ cmd: "node_add", session: 1, parent: "root", kind: "group", name: null });

    const rev = await backend.feed(replica, 1, 2);

    expect(rev).toBe(2);
    expect(replica.syncs).toEqual([1]);
    expect(replica.doc?.root.children).toHaveLength(1);
  });

  test("closing frees the editor and refuses everything after", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    backend.close();

    expect(editor.freed).toBe(true);
    await expect(backend.send({ cmd: "session_list" })).rejects.toMatchObject({ code: "closed" });
  });
});

/** A connected backend over a fake socket and a fake origin. */
async function connect(routes: Record<string, () => ReturnType<typeof httpResponse>> = {}) {
  FakeSocket.opened = [];
  const http = fakeFetch({
    "/token": () => httpResponse({ token: "abc123" }),
    ...routes,
  });
  const backend = await ConnectedBackend.connect("http://editor.local/", {
    fetch: http.fetch,
    socket: (url) => new FakeSocket(url),
  });
  const socket = FakeSocket.opened[0];
  if (!socket) throw new Error("no socket was opened");
  return { backend, socket, http };
}

describe("connected", () => {
  test("takes a token over HTTP and carries it into the socket URL", async () => {
    const { socket, http } = await connect();
    expect(http.calls[0]?.url).toBe("http://editor.local/token");
    expect(socket.url).toBe("ws://editor.local/ws?token=abc123");
  });

  test("correlates replies by id, whatever order they arrive in", async () => {
    const { backend, socket } = await connect();

    const first = backend.send({ cmd: "session_list" });
    const second = backend.send({ cmd: "status", session: 1 });
    const firstId = socket.request(0).id;
    const secondId = socket.request(1).id;
    expect(firstId).not.toBe(secondId);

    socket.deliver({ reply: "ok", id: secondId, rev: 4, body: { result: "empty" } });
    socket.deliver({ reply: "ok", id: firstId, body: { result: "sessions", sessions: [] } });

    expect(await second).toEqual({ body: { result: "empty" }, rev: 4 });
    expect(await first).toEqual({ body: { result: "sessions", sessions: [] } });
  });

  test("an err frame rejects its own request and nobody else's", async () => {
    const { backend, socket } = await connect();
    const failing = backend.send({ cmd: "undo", session: 1 });
    const waiting = backend.send({ cmd: "redo", session: 1 });

    socket.deliver({
      reply: "err",
      id: socket.request(0).id,
      code: "nothing_to_undo",
      message: "nothing to undo",
    });

    await expect(failing).rejects.toMatchObject({ code: "nothing_to_undo" });
    socket.deliver({ reply: "ok", id: socket.request(1).id, rev: 2, body: { result: "empty" } });
    expect(await waiting).toMatchObject({ rev: 2 });
  });

  test("a closed socket rejects everything in flight", async () => {
    const { backend, socket } = await connect();
    const first = backend.send({ cmd: "session_list" });
    const second = backend.send({ cmd: "status", session: 1 });

    socket.close();

    await expect(first).rejects.toMatchObject({ code: "closed" });
    await expect(second).rejects.toMatchObject({ code: "closed" });
    // And an editor that cannot be reached is not a slow editor.
    await expect(backend.send({ cmd: "session_list" })).rejects.toMatchObject({ code: "closed" });
  });

  test("an event reaches the listeners", async () => {
    const { backend, socket } = await connect();
    const seen: Event[] = [];
    backend.onEvent((event) => seen.push(event));

    socket.deliver({ reply: "event", event: "document_changed", session: 3, rev: 9 });
    socket.deliver({ reply: "event", event: "sessions_changed" });

    expect(seen).toEqual([
      { event: "document_changed", session: 3, rev: 9 },
      { event: "sessions_changed" },
    ]);
  });

  test("a feed fetches exactly the textures the structure named, and applies at the header's revision", async () => {
    const doc = emptyDoc("akari");
    doc.textures = [
      { id: "tex-1", width: 8, height: 8 },
      { id: "tex-2", width: 8, height: 8 },
    ];
    const { backend, http } = await connect({
      "/sessions/1/structure": () =>
        httpResponse(structureBytes(doc), { headers: { "X-Catchlight-Rev": "12" } }),
      "/sessions/1/textures/tex-1": () => httpResponse(new Uint8Array([1])),
      "/sessions/1/textures/tex-2": () => httpResponse(new Uint8Array([2])),
    });
    const replica = new FakeReplica();
    replica.held.add("tex-1");

    // Asked for 7; the editor had moved on, and the bytes are what they are.
    const rev = await backend.feed(replica, 1, 7);

    expect(rev).toBe(12);
    expect(replica.applied.map((a) => a.rev)).toEqual([12]);
    const fetched = http.calls.map((call) => call.url).filter((url) => url.includes("/textures/"));
    // tex-1 was already held; refetching it is a texture's worth of bandwidth
    // per edit.
    expect(fetched).toEqual(["http://editor.local/sessions/1/textures/tex-2"]);
  });

  test("a feed that cannot be applied rejects rather than leaving the replica half fed", async () => {
    const doc = emptyDoc("akari");
    doc.textures = [{ id: "tex-1", width: 8, height: 8 }];
    const { backend } = await connect({
      "/sessions/1/structure": () =>
        httpResponse(structureBytes(doc), { headers: { "X-Catchlight-Rev": "3" } }),
      "/sessions/1/textures/tex-1": () => httpResponse({ error: "gone" }, { status: 500 }),
    });

    await expect(backend.feed(new FakeReplica(), 1, 3)).rejects.toMatchObject({ code: "feed" });
  });

  test("a structure with no revision header is refused", async () => {
    const { backend } = await connect({
      "/sessions/1/structure": () => httpResponse(structureBytes(emptyDoc("akari"))),
    });
    await expect(backend.feed(new FakeReplica(), 1, 1)).rejects.toMatchObject({ code: "feed" });
  });

  test("feeds for one session never overlap, and coalesce to the newest", async () => {
    let served = 0;
    const doc = emptyDoc("akari");
    const { backend, http } = await connect({
      "/sessions/1/structure": () => {
        served += 1;
        return httpResponse(structureBytes(doc), {
          headers: { "X-Catchlight-Rev": String(served + 1) },
        });
      },
    });
    const replica = new FakeReplica();

    const feeds = [backend.feed(replica, 1, 2), backend.feed(replica, 1, 3), backend.feed(replica, 1, 4)];
    await Promise.all(feeds);
    await tick();

    // Three events, two fetches: the two behind the one in flight collapse.
    expect(http.calls.filter((call) => call.url.endsWith("/structure"))).toHaveLength(2);
    expect(replica.rev()).toBe(3);
  });

  test("putting bytes writes to the editor's store, with the bearer", async () => {
    const { backend, http } = await connect({
      "/files/project%2Fakari.clm": () => httpResponse({ ok: true }),
    });

    await backend.putBytes("project/akari.clm", new TextEncoder().encode("a model"));

    const put = http.calls.find((call) => call.url.includes("/files/"));
    expect(put?.url).toBe("http://editor.local/files/project%2Fakari.clm");
    expect(put?.init?.method).toBe("PUT");
    expect(put?.init?.headers?.Authorization).toBe("Bearer abc123");
  });

  test("staging a key is nothing: the editor already reads its own store", async () => {
    const { backend, http } = await connect();
    await backend.stageKey("project/akari.clm");
    expect(http.calls.filter((call) => call.url.includes("/files/"))).toHaveLength(0);
  });

  test("reading a key back is nothing either: the file is already where it was asked to go", async () => {
    const { backend, http } = await connect();
    expect(await backend.readBytes("project/akari.clm")).toBeUndefined();
    expect(http.calls.filter((call) => call.url.includes("/files/"))).toHaveLength(0);
  });
});
