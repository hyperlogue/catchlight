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

  test("bytes go with the command that uses them, under the name it declares", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    await backend.send({ cmd: "session_new", name: null });
    const bytes = new TextEncoder().encode("a model");

    const reply = await backend.sendWith({ cmd: "import_file", session: 1, parent: null }, [
      ["model", bytes],
    ]);

    expect(reply.body).toMatchObject({ result: "session" });
    expect(editor.attached.at(-1)).toEqual({ model: bytes });
    // Nothing was parked under a key on the way through.
    expect(editor.writtenKeys()).toEqual([]);
  });

  test("a byte-bearing command sent through `send` never reaches the editor", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());
    await backend.send({ cmd: "session_new", name: null });
    const before = editor.requests.length;

    await expect(
      backend.send({ cmd: "import_file", session: 1, parent: null }),
    ).rejects.toBeInstanceOf(ProtocolError);

    // Refused here, where the mistake is, rather than a round trip away.
    expect(editor.requests).toHaveLength(before);
  });

  test("a key the editor's store does not have is its own refusal", async () => {
    const editor = new FakeEditor();
    const backend = new InTabBackend(editor, new MemoryStorage());

    await expect(backend.send({ cmd: "session_open", path: "missing.clm" })).rejects.toBeInstanceOf(
      ProtocolError,
    );
  });

  test("saving drains what the editor wrote into the store", async () => {
    const editor = new FakeEditor();
    const storage = new MemoryStorage();
    const backend = new InTabBackend(editor, storage);
    await backend.send({ cmd: "session_new", name: null });

    const reply = await backend.send({ cmd: "save", session: 1, path: "out/akari.clm" });

    expect(reply.body).toEqual({ result: "saved", path: "out/akari.clm" });
    expect(await storage.list()).toEqual(["out/akari.clm"]);
    // Drained: a map that is never emptied holds every texture twice.
    expect(editor.writtenKeys()).toEqual([]);
  });

  test("an export drains the files it wrote beside the one it named", async () => {
    const editor = new FakeEditor();
    const storage = new MemoryStorage();
    const backend = new InTabBackend(editor, storage);
    await backend.send({ cmd: "session_new", name: null });

    await backend.send({ cmd: "export_manifest", session: 1, path: "out/model.json" });

    expect(await storage.list()).toEqual(["out/model.json", "tex0.png"]);
    expect(editor.writtenKeys()).toEqual([]);
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

  test("a feed fetches a byte extension only when its hash moved", async () => {
    const doc = emptyDoc("akari");
    doc.extensions = [
      { key: "molan.thumb", kind: "bytes", hash: "first" },
      { key: "molan.other", kind: "bytes", hash: "held" },
      { key: "molan.caster", kind: "json", value: { v: 1 } },
    ];
    const { backend, http } = await connect({
      "/sessions/1/structure": () =>
        httpResponse(structureBytes(doc), { headers: { "X-Catchlight-Rev": "3" } }),
      "/sessions/1/extensions/molan.thumb": () =>
        httpResponse(new TextEncoder().encode("first")),
    });
    const replica = new FakeReplica();
    // One held at the hash the structure names, one held at an older hash.
    replica.heldExtensions.set("molan.other", "held");
    replica.heldExtensions.set("molan.thumb", "stale");

    expect(await backend.feed(replica, 1, 3)).toBe(3);

    const fetched = http.calls.map((call) => call.url).filter((url) => url.includes("/extensions/"));
    // Only the one whose hash moved: an unchanged marker costs nothing, and a
    // json extension is already in the structure.
    expect(fetched).toEqual(["http://editor.local/sessions/1/extensions/molan.thumb"]);
    expect(replica.heldExtensions.get("molan.thumb")).toBe("first");
  });

  test("an unrelated edit fetches no extension at all", async () => {
    const doc = emptyDoc("akari");
    doc.extensions = [{ key: "molan.thumb", kind: "bytes", hash: "held" }];
    const { backend, http } = await connect({
      "/sessions/1/structure": () =>
        httpResponse(structureBytes(doc), { headers: { "X-Catchlight-Rev": "9" } }),
    });
    const replica = new FakeReplica();
    replica.heldExtensions.set("molan.thumb", "held");

    expect(await backend.feed(replica, 1, 9)).toBe(9);
    expect(http.calls.filter((call) => call.url.includes("/extensions/"))).toHaveLength(0);
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

  test("a byte-bearing command is one multipart post, with the bearer", async () => {
    const { backend, http } = await connect({
      "/request": () =>
        httpResponse({ reply: "ok", id: 1, rev: 4, body: { result: "session", session: 1 } }),
    });

    const reply = await backend.sendWith({ cmd: "import_file", session: 1, parent: null }, [
      ["model", new TextEncoder().encode("a model")],
    ]);

    expect(reply.body).toEqual({ result: "session", session: 1 });
    expect(reply.rev).toBe(4);
    const post = http.calls.find((call) => call.url.endsWith("/request"));
    expect(post?.init?.method).toBe("POST");
    expect(post?.init?.headers?.Authorization).toBe("Bearer abc123");
    const form = post?.init?.body as FormData;
    expect(form.get("request")).toContain('"cmd":"import_file"');
    expect(form.get("model")).toBeInstanceOf(Blob);
  });

  test("a reply that carries bytes hands them back beside it", async () => {
    const png = new Uint8Array([137, 80, 78, 71]);
    const { backend } = await connect({
      "/request": () =>
        httpResponse(png, {
          headers: {
            "X-Catchlight-Reply": JSON.stringify({
              reply: "ok",
              id: 1,
              rev: 2,
              body: { result: "preview", preview: { width: 8, height: 8 } },
            }),
          },
        }),
    });

    const reply = await backend.sendWith(
      { cmd: "preview", session: 1, pose: [], size: [8, 8], camera: null },
      [],
    );

    expect(reply.body).toEqual({ result: "preview", preview: { width: 8, height: 8 } });
    expect(reply.payload).toEqual(png);
  });

  test("a byte-bearing command never goes near the socket", async () => {
    const { backend, socket } = await connect();
    const before = socket.sent.length;

    await expect(
      backend.send({ cmd: "import_file", session: 1, parent: null }),
    ).rejects.toBeInstanceOf(ProtocolError);

    expect(socket.sent).toHaveLength(before);
  });

  test("reading a key back is nothing: the file is already where it was asked to go", async () => {
    const { backend } = await connect();
    expect(await backend.readDocument("project/akari.clm")).toBeUndefined();
  });
});
