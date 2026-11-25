import { describe, expect, test } from "bun:test";

import { ProtocolError } from "./backend.js";
import { emptyDoc, FakeReplica, GuardedReplica, ScriptedBackend, structureBytes } from "./fakes.js";
import { Session } from "./session.js";

/** A re-feed timeout a suite can wait out, and no microtask can beat. */
const TIMEOUT = 5;

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function after(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function open(id = 1): { backend: ScriptedBackend; replica: FakeReplica; session: Session } {
  const backend = new ScriptedBackend();
  const replica = new FakeReplica();
  return { backend, replica, session: new Session(backend, id, replica) };
}

describe("an edit and its revision", () => {
  test("resolves only once the replica reached the revision the reply named", async () => {
    const { backend, replica, session } = open();
    backend.replies.set("node_add", { body: { result: "node", node: "root/group-7" }, rev: 5 });

    let settled = false;
    const sending = session
      .send({ cmd: "node_add", parent: "root", kind: "group", name: null })
      .then((body) => {
        settled = true;
        return body;
      });

    await tick();
    // Resolving here would hand back an Id no query can find.
    expect(settled).toBe(false);
    expect(replica.rev()).toBe(0);

    backend.changed(1, 5);
    const body = await sending;

    expect(settled).toBe(true);
    expect(replica.rev()).toBe(5);
    expect(session.getRevision()).toBe(5);
    expect(body).toEqual({ result: "node", node: "root/group-7" });
  });

  test("resolves when the event beat its own reply", async () => {
    const { backend, replica, session } = open();
    backend.replies.set("node_add", { body: { result: "node", node: "root/group-7" }, rev: 5 });

    const sending = session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    // In-tab, the feed is already running when `send` checks the revision.
    backend.changed(1, 5);

    await sending;
    expect(replica.rev()).toBe(5);
    expect(session.getRevision()).toBe(5);
  });

  test("notifies subscribers once, when the revision actually moves", async () => {
    const { backend, session } = open();
    backend.replies.set("node_add", { body: { result: "node", node: "n" }, rev: 3 });
    let notified = 0;
    session.subscribe(() => {
      notified += 1;
    });

    const sending = session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    backend.changed(1, 3);
    await sending;
    await tick();

    // React must not render twice for one edit.
    expect(notified).toBe(1);
    expect(session.getRevision()).toBe(3);
  });

  test("a feed that fails rejects the command instead of hanging", async () => {
    const { backend, session } = open();
    backend.replies.set("node_add", { body: { result: "node", node: "n" }, rev: 4 });
    const seen: ProtocolError[] = [];
    session.onError((error) => seen.push(error));

    backend.failFeed = "the structure was malformed";
    const sending = session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    backend.changed(1, 4);

    await expect(sending).rejects.toMatchObject({ code: "feed" });
    expect(seen.map((error) => error.message)).toEqual(["the structure was malformed"]);
  });

  test("two events for one session coalesce into one more feed, at the newer revision", async () => {
    const { backend, replica, session } = open();
    void session;
    let release!: () => void;
    backend.hold = new Promise<void>((resolve) => {
      release = resolve;
    });

    backend.changed(1, 2);
    backend.changed(1, 3);
    backend.changed(1, 4);
    await tick();

    expect(backend.runs).toEqual([2]);
    release();
    await tick();

    expect(backend.runs).toEqual([2, 4]);
    expect(replica.rev()).toBe(4);
    expect(replica.applied.map((a) => a.rev)).toEqual([2, 4]);
  });

  test("a lost frame becomes one re-feed, and the send still waits for the revision", async () => {
    const backend = new ScriptedBackend();
    const replica = new FakeReplica();
    const session = new Session(backend, 1, replica, TIMEOUT);
    backend.replies.set("node_add", { body: { result: "node", node: "root/group-7" }, rev: 5 });

    const sending = session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    await tick();
    expect(replica.rev()).toBe(0);
    expect(backend.feeds).toHaveLength(0);

    const body = await sending;

    expect(body).toEqual({ result: "node", node: "root/group-7" });
    // Resolved by a feed, not by giving up on the wait.
    expect(replica.rev()).toBe(5);
    expect(session.getRevision()).toBe(5);
    expect(backend.feeds).toEqual([{ session: 1, rev: 5 }]);
  });

  test("a send that caught up on its own event never re-feeds", async () => {
    const backend = new ScriptedBackend();
    const replica = new FakeReplica();
    const session = new Session(backend, 1, replica, TIMEOUT);
    backend.replies.set("node_add", { body: { result: "node", node: "n" }, rev: 5 });

    const sending = session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    backend.changed(1, 5);
    await sending;
    await after(TIMEOUT * 4);

    // A timer left armed behind a settled send would fetch again.
    expect(backend.runs).toEqual([5]);
    expect(replica.rev()).toBe(5);
  });

  test("a re-feed that fails rejects the command it was waiting for", async () => {
    const backend = new ScriptedBackend();
    const session = new Session(backend, 1, new FakeReplica(), TIMEOUT);
    backend.replies.set("node_add", { body: { result: "node", node: "n" }, rev: 5 });
    backend.failFeed = "the re-feed could not be read";

    let caught: unknown;
    try {
      await session.send({ cmd: "node_add", parent: "root", kind: "group", name: null });
    } catch (cause) {
      caught = cause;
    }

    // Not `expect().rejects`, which reports a timer-delivered rejection as its own error.
    expect(caught).toMatchObject({ code: "feed" });
  });

  test("an event for another session is not this one's business", async () => {
    const { backend, replica, session } = open(1);
    backend.changed(2, 9);
    await tick();
    expect(replica.rev()).toBe(0);
    expect(session.getRevision()).toBe(0);
  });
});

describe("the quiet paths", () => {
  test("presence bumps nothing and repaints nothing", async () => {
    const { backend, session } = open();
    let revisions = 0;
    let repaints = 0;
    session.subscribe(() => {
      revisions += 1;
    });
    session.onInvalidate(() => {
      repaints += 1;
    });

    for (let i = 0; i < 100; i++) await session.sendPresence({ cmd: "presence_set", pose: [] });

    expect(backend.sent).toHaveLength(100);
    expect(revisions).toBe(0);
    expect(repaints).toBe(0);
  });

  test("a drag repaints every viewport and is never a revision", () => {
    const { replica, session } = open();
    const repaints = [0, 0];
    session.onInvalidate(() => {
      repaints[0] = (repaints[0] ?? 0) + 1;
    });
    session.onInvalidate(() => {
      repaints[1] = (repaints[1] ?? 0) + 1;
    });
    let revisions = 0;
    session.subscribe(() => {
      revisions += 1;
    });

    session.setParam("param-1", 0.25);
    session.scratchDeform("root/part-1", new Float32Array([1, 2, 3, 4]));
    session.scratchTransform("root/part-1", { translate: [3, 4, 0], opacity: 0.5 });
    session.clearScratchDeform("root/part-1");
    session.clearAllScratch();

    expect(repaints).toEqual([5, 5]);
    expect(revisions).toBe(0);
    expect(session.getRevision()).toBe(0);
    expect(session.paramValue("param-1")).toBe(0.25);
    expect(replica.scratchDeforms.size).toBe(0);
  });

  test("an absent scratch field is NaN, which leaves what the fold produced", () => {
    const { replica, session } = open();
    session.scratchTransform("root/part-1", { scale: [2, 2] });

    const values = replica.scratchTransforms.get("root/part-1") ?? [];
    expect(values.slice(6, 8)).toEqual([2, 2]);
    expect(values.filter((value) => Number.isNaN(value))).toHaveLength(8);
  });
});

describe("preview inputs and playback", () => {
  function rigged() {
    const opened = open();
    const doc = emptyDoc("rigged");
    doc.humanoid = [
      { control: "mouth_open", param: "jaw", stops: [0, 0, 1] },
      { control: "mouth_smile", param: "cheek", stops: [-1, 0, 1] },
    ];
    doc.clips = [{ name: "wave", timestep: 0.1, length: 10, lead_in: null, lead_out: null, lanes: [] }];
    opened.replica.applyStructure(structureBytes(doc), 1);
    let repaints = 0;
    opened.session.onInvalidate(() => {
      repaints += 1;
    });
    let revisions = 0;
    opened.session.subscribe(() => {
      revisions += 1;
    });
    return { ...opened, repaints: () => repaints, revisions: () => revisions };
  }

  test("each input reaches the replica and repaints, and is never a revision", () => {
    const { replica, session, repaints, revisions } = rigged();
    session.setControl("mouth_open", 0.5);
    session.setExpressionWeight("happy", 0.25);
    session.setVisemeWeight("aa", 1);
    expect(repaints()).toBe(3);
    expect(replica.preview).toEqual({
      controls: { mouth_open: 0.5 },
      expressions: { happy: 0.25 },
      visemes: { aa: 1 },
    });
    expect(session.inputs()).toEqual(replica.preview);
    expect(session.inputs()).toBe(session.inputs());

    session.setExpressionWeight("happy", undefined);
    expect(session.inputs().expressions).toEqual({});
    session.clearInputs();
    expect(repaints()).toBe(5);
    expect(session.inputs()).toEqual({ controls: {}, expressions: {}, visemes: {} });
    expect(revisions()).toBe(0);
  });

  test("setting a param drops the controls previewed onto it", () => {
    const { session } = rigged();
    session.setControl("mouth_open", 0.5);
    session.setControl("mouth_smile", 0.5);
    const before = session.inputs();
    session.setParam("jaw", 0.1);
    expect(session.inputs()).not.toBe(before);
    expect(session.inputs().controls).toEqual({ mouth_smile: 0.5 });
  });

  test("a clip plays by name, reports where it is, and stops", () => {
    const { session, repaints, revisions } = rigged();
    expect(session.playing()).toBeUndefined();
    expect(session.play("nope")).toBe(false);
    expect(session.play("wave")).toBe(true);
    expect(session.playing()).toEqual({ name: "wave", time: 0, frame: 0 });
    session.stop();
    expect(session.playing()).toBeUndefined();
    expect(repaints()).toBe(3);
    expect(revisions()).toBe(0);
  });

  test("a closed session previews nothing", () => {
    const { replica, session } = rigged();
    session.close();
    session.setControl("mouth_open", 0.5);
    expect(session.play("wave")).toBe(false);
    expect(replica.preview.controls).toEqual({});
    expect(session.inputs()).toEqual({ controls: {}, expressions: {}, visemes: {} });
  });
});

describe("reads", () => {
  test("a replica query answers synchronously, with no round trip", async () => {
    const { backend, session } = open();
    backend.changed(1, 1);
    await tick();

    const tree = session.tree();
    expect(tree.id).toBe("root");
    expect(session.params()).toEqual([]);
    expect(backend.sent).toHaveLength(0);
  });

  test("a node reads back in full, and a node that is gone reads as undefined", async () => {
    const { backend, replica, session } = open();
    const doc = emptyDoc("one part");
    doc.root.children.push({
      id: "root/part-1",
      name: "Body",
      kind: "part",
      z_order: 3,
      enabled: true,
      children: [],
    });
    replica.applyStructure(structureBytes(doc), 1);

    const info = session.nodeInfo("root/part-1");
    expect(info?.kind).toBe("part");
    expect(info?.name).toBe("Body");
    expect(info?.parent).toBe("root");
    expect(info?.z_order).toBe(3);
    expect(info?.vertex_count).toBe(0);
    expect(info?.triangle_count).toBe(0);

    expect(session.nodeInfo("root/part-9")).toBeUndefined();
    expect(backend.sent).toHaveLength(0);
  });

  test("an err reply throws a ProtocolError carrying its code", async () => {
    const { backend, session } = open();
    backend.changed(1, 1);
    await tick();

    expect(() => session.query({ cmd: "weld_list" })).toThrow(ProtocolError);
    try {
      session.query({ cmd: "weld_list" });
    } catch (error) {
      expect(error).toMatchObject({ code: "bad_request" });
    }
  });

  test("the posed bounds come straight off the replica", () => {
    const { backend, replica, session } = open();

    expect(session.bounds()).toBeUndefined();

    replica.box = [-8, -1.5, 8, 1.5];
    expect(session.bounds()).toEqual([-8, -1.5, 8, 1.5]);
    expect(backend.sent).toHaveLength(0);
  });

  test("a server query goes over the wire and changes nothing", async () => {
    const { backend, session } = open();
    backend.replies.set("session_get", { body: { result: "empty" }, rev: 1 });

    await session.queryServer({ cmd: "session_get" });
    expect(backend.sent).toEqual([{ cmd: "session_get", session: 1 }]);
    expect(session.getRevision()).toBe(0);
  });
});

describe("closing", () => {
  test("frees the replica and stops following the model", async () => {
    const { backend, replica, session } = open();
    session.close();
    backend.changed(1, 7);
    await tick();

    expect(replica.freed).toBe(true);
    expect(replica.rev()).toBe(0);
    expect(backend.feeds).toHaveLength(0);
  });

  test("a feed still reading the replica keeps it alive until it lets go", async () => {
    const backend = new ScriptedBackend();
    const replica = new GuardedReplica();
    const session = new Session(backend, 1, replica);
    let release!: () => void;
    backend.hold = new Promise<void>((resolve) => {
      release = resolve;
    });

    backend.changed(1, 3);
    await tick();

    const complaints: unknown[][] = [];
    const wasError = console.error;
    console.error = (...args: unknown[]): void => {
      complaints.push(args);
    };
    try {
      session.close();
      // The feed holds a pointer into the replica: freeing here is use-after-free.
      expect(replica.freed).toBe(false);
      release();
      await tick();
    } finally {
      console.error = wasError;
    }

    expect(replica.usedAfterFree).toEqual([]);
    expect(complaints).toEqual([]);
    expect(replica.freed).toBe(true);
  });
});

describe("one method per kind of command", () => {
  test("the wrong method does not typecheck", () => {
    const { session } = open();

    // @ts-expect-error — presence never edits the model.
    void (() => session.send({ cmd: "presence_set", pose: [], selection: null }));

    // @ts-expect-error — the replica cannot answer `session_get`.
    void (() => session.query({ cmd: "session_get" }));

    // @ts-expect-error — `node_set` is an edit, not presence.
    void (() => session.sendPresence({ cmd: "node_set", node: "hair" }));

    // @ts-expect-error — the session fills this in.
    void (() => session.queryServer({ cmd: "session_get", session: 2 }));

    // @ts-expect-error — `session_create` belongs on the editor.
    void (() => session.send({ cmd: "session_create", name: null }));

    // @ts-expect-error — `nod_add` is not a command.
    void (() => session.send({ cmd: "nod_add", parent: "root" }));

    expect(session.id).toBe(1);
  });
});
