/**
 * What a `Session` promises: when a command resolves, what moves a revision,
 * and what a drag costs.
 */

import { describe, expect, test } from "bun:test";

import { ProtocolError } from "./backend.js";
import { emptyDoc, FakeReplica, GuardedReplica, ScriptedBackend, structureBytes } from "./fakes.js";
import { Session } from "./session.js";

/** A re-feed timeout a suite can wait out, and no microtask can beat. */
const TIMEOUT = 5;

/** Lets every pending microtask and timer callback run. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** Waits `ms`, for the timers this suite has to outlast rather than race. */
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
    // The reply is in, and the replica still holds nothing: resolving here
    // would hand back an Id that no query can find.
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
    // An in-tab editor emits inside the same call the reply came out of, so
    // the feed is already running when `send` looks at the revision.
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

    // The feed and the send both finish the command; only one of them is a
    // revision, and React must not render twice for one edit.
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

    // One feed in flight, and everything behind it collapsed into one more.
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
    // The editor moved to 5 and the frame that says so never arrived: nothing
    // else will ever bring the replica there.
    expect(replica.rev()).toBe(0);
    expect(backend.feeds).toHaveLength(0);

    const body = await sending;

    expect(body).toEqual({ result: "node", node: "root/group-7" });
    // Resolved at the revision it was promised, and by a feed — the one path
    // into the replica — rather than by giving up on the wait.
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

    // One feed, the event's. A timer left armed behind a settled send is a
    // second structure fetch for an edit that already landed.
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

    // Awaited rather than asserted through `expect().rejects`, which reports
    // a rejection arriving on a timer as an error of its own.
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
    // Presence is what *other* clients read; this tab's own picture moves
    // through the replica, not through a round trip.
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

describe("reads", () => {
  test("a replica query answers synchronously, with no round trip", async () => {
    const { backend, session } = open();
    backend.changed(1, 1);
    await tick();

    const tree = session.tree();
    expect(tree.id).toBe("root");
    expect(session.params()).toEqual([]);
    // Nothing was asked of the backend: the model is in this tab.
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
    // A part carries a mesh, so it reports its size — an empty one here,
    // which is what an unmeshed part reports in a real model too.
    expect(info?.vertex_count).toBe(0);
    expect(info?.triangle_count).toBe(0);

    // A selection outlives the node it names, so this is a panel with nothing
    // to draw rather than a failed read.
    expect(session.nodeInfo("root/part-9")).toBeUndefined();
    expect(backend.sent).toHaveLength(0);
  });

  test("an err reply throws a ProtocolError carrying its code", async () => {
    const { backend, session } = open();
    backend.changed(1, 1);
    await tick();

    expect(() => session.query({ cmd: "welds" })).toThrow(ProtocolError);
    try {
      session.query({ cmd: "welds" });
    } catch (error) {
      expect(error).toMatchObject({ code: "bad_request" });
    }
  });

  test("the posed bounds come straight off the replica", () => {
    const { backend, replica, session } = open();

    // A replica that has drawn nothing has no box, and a fit has to survive
    // that rather than invent a camera.
    expect(session.bounds()).toBeUndefined();

    replica.box = [-8, -1.5, 8, 1.5];
    expect(session.bounds()).toEqual([-8, -1.5, 8, 1.5]);
    // A read, so it costs the backend nothing.
    expect(backend.sent).toHaveLength(0);
  });

  test("a server query goes over the wire and changes nothing", async () => {
    const { backend, session } = open();
    backend.replies.set("status", { body: { result: "empty" }, rev: 1 });

    await session.queryServer({ cmd: "status" });
    expect(backend.sent).toEqual([{ cmd: "status", session: 1 }]);
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
      // The feed is mid-fetch and holds a pointer into it, so freeing here
      // is the use-after-free this test exists for.
      expect(replica.freed).toBe(false);
      release();
      await tick();
    } finally {
      console.error = wasError;
    }

    expect(replica.usedAfterFree).toEqual([]);
    expect(complaints).toEqual([]);
    // Freed on the way out of the feed, so closing still ends the model.
    expect(replica.freed).toBe(true);
  });
});

describe("one method per kind of command", () => {
  test("the wrong method does not typecheck", () => {
    const { session } = open();

    // @ts-expect-error — `scratch_deform` is a scratch command. Sending it
    // through `send` would cost a round trip and a revision on every pointer
    // move, which is the bug this split exists to prevent.
    void (() => session.send({ cmd: "scratch_deform", node: "hair", offsets: [] }));

    // @ts-expect-error — `status` needs the editor's own state, so a replica
    // cannot answer it.
    void (() => session.query({ cmd: "status" }));

    // @ts-expect-error — `node_set` changes the model, so it cannot go out
    // as presence: the panels would never learn the edit happened.
    void (() => session.sendPresence({ cmd: "node_set", node: "hair" }));

    // @ts-expect-error — the session fills this in; a caller that could pass
    // it could address the wrong model.
    void (() => session.queryServer({ cmd: "status", session: 2 }));

    // @ts-expect-error — `session_new` names no session, so it belongs on the
    // editor rather than on one.
    void (() => session.send({ cmd: "session_new", name: null }));

    // @ts-expect-error — `nod_add` is not a command. The union is the spelling
    // check a `{ cmd: string }` placeholder could never do.
    void (() => session.send({ cmd: "nod_add", parent: "root" }));

    expect(session.id).toBe(1);
  });
});
