import { describe, expect, jest, test } from "bun:test";

import { FakeReplica, ScriptedBackend } from "./fakes.js";
import type { Review } from "./protocol.gen.js";
import { REVIEW_LISTENING_POLL_MS, Session } from "./session.js";

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function thread(id: number): Review["threads"][number] {
  return {
    thread: id,
    target: { kind: "model" },
    target_missing: false,
    status: "open",
    pose: [],
    comments: [
      { comment: id, author: { role: "human" }, body: "hi", delivery: "unsent", rev: 0 },
    ],
  };
}

type Served = Pick<Review, "threads"> & Partial<Pick<Review, "agents">>;

function open(): { backend: ScriptedBackend; session: Session; serve(review: Served): void } {
  const backend = new ScriptedBackend();
  const session = new Session(backend, 1, new FakeReplica());
  const serve = (review: Served): void => {
    backend.replies.set("review_get", {
      body: { result: "review", review: { threads: review.threads, agents: review.agents ?? [], kept: "stored" } },
    });
  };
  serve({ threads: [] });
  return { backend, session, serve };
}

const reads = (backend: ScriptedBackend): number =>
  backend.sent.filter((command) => command.cmd === "review_get").length;

describe("a session's review", () => {
  test("is read when first subscribed, and again on its own session's event only", async () => {
    const { backend, session, serve } = open();
    let told = 0;
    session.subscribeReview(() => {
      told += 1;
    });
    await tick();
    expect(reads(backend)).toBe(1);
    // An empty review is what a fresh session already holds.
    expect(told).toBe(0);

    serve({ threads: [thread(1)] });
    backend.emit({ event: "review_changed", session: 2 });
    await tick();
    expect(reads(backend)).toBe(1);

    backend.emit({ event: "review_changed", session: 1 });
    await tick();
    expect(session.getReview().threads).toHaveLength(1);
    expect(told).toBe(1);
  });

  test("nobody subscribed means no read", async () => {
    const { backend } = open();
    backend.emit({ event: "review_changed", session: 1 });
    await tick();
    expect(reads(backend)).toBe(0);
  });

  test("a review command re-reads before it resolves", async () => {
    const { backend, session, serve } = open();
    session.subscribeReview(() => {});
    await tick();
    serve({ threads: [thread(1)] });

    await session.sendReview({ cmd: "review_submit" });

    expect(backend.sent.map((command) => command.cmd)).toContain("review_submit");
    expect(session.getReview().threads).toHaveLength(1);
  });

  test("keeps re-reading while any agent listens, to notice it leaving", async () => {
    jest.useFakeTimers();
    try {
      const { backend, session, serve } = open();
      const codex = { id: "codex:t1", name: "codex", listening: false };
      serve({ threads: [], agents: [codex, { id: "claude:s1", name: "claude", listening: true }] });
      const stop = session.subscribeReview(() => {});
      await Promise.resolve();
      await Promise.resolve();
      expect(session.getReview().agents[1]?.listening).toBe(true);

      serve({ threads: [], agents: [codex, { id: "claude:s1", name: "claude", listening: false }] });
      jest.advanceTimersByTime(REVIEW_LISTENING_POLL_MS);
      await Promise.resolve();
      await Promise.resolve();
      expect(session.getReview().agents[1]?.listening).toBe(false);

      const settled = reads(backend);
      jest.advanceTimersByTime(REVIEW_LISTENING_POLL_MS * 3);
      await Promise.resolve();
      expect(reads(backend)).toBe(settled);
      stop();
    } finally {
      jest.useRealTimers();
    }
  });
});
