import { describe, expect, test } from "bun:test";

import { FakeReplica, ScriptedBackend } from "./fakes.js";
import type { VersionInfo } from "./protocol.gen.js";
import { Session } from "./session.js";

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function version(n: number): VersionInfo {
  return { version: n, author: { role: "human" }, time: 1, rev: n, byte_length: 3, sha256: "" };
}

describe("a session's versions", () => {
  test("are read when first subscribed, and again on its own session's event only", async () => {
    const backend = new ScriptedBackend();
    const session = new Session(backend, 1, new FakeReplica());
    const serve = (versions: VersionInfo[]) =>
      backend.replies.set("version_list", { body: { result: "versions", versions, kept: "unsaved" } });
    serve([version(1)]);
    let told = 0;
    session.subscribeVersions(() => {
      told += 1;
    });
    await tick();
    expect(session.getVersions()).toEqual({ versions: [version(1)], kept: "unsaved", kept_problem: null });
    expect(told).toBe(1);

    serve([version(1), version(2)]);
    backend.emit({ event: "versions_changed", session: 2 });
    await tick();
    expect(session.getVersions().versions).toHaveLength(1);
    backend.emit({ event: "versions_changed", session: 1 });
    await tick();
    expect(session.getVersions().versions).toHaveLength(2);
    expect(told).toBe(2);
    session.close();
  });
});
