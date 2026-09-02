/**
 * Presence: one record, one command, and a pose that does not flood the wire.
 */

import "./test/setup.js";

import { afterEach, describe, expect, jest, test } from "bun:test";
import type { Command, ParamInfo } from "@catchlight/core";
import type { FakeEditor } from "@catchlight/core/fakes";

import {
  POSE_INTERVAL_MS,
  PresenceProvider,
  SelectionProvider,
  usePose,
  useSelection,
} from "./index.js";
import type { Pose, Selection } from "./index.js";
import { harness, mount, run, settle } from "./test/harness.js";

/** Every presence the editor was sent, in order. */
function published(wasm: FakeEditor): Array<Extract<Command, { cmd: "presence_set" }>> {
  return wasm.requests.filter(
    (request): request is Extract<Command, { cmd: "presence_set" }> & { id: number } =>
      request.cmd === "presence_set",
  );
}

afterEach(() => {
  jest.useRealTimers();
});

describe("publishing presence", () => {
  test("a selection goes out at once, and carries the pose that was waiting", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    let selection: Selection | undefined;
    let pose: Pose | undefined;

    function Host() {
      selection = useSelection();
      pose = usePose();
      return null;
    }
    const view = await mount(
      <PresenceProvider session={session}>
        <Host />
      </PresenceProvider>,
    );
    await settle();

    await run(() => pose?.publish([{ param: "eye", value: 0.5 }]));
    // Held: a pose is not worth a frame on its own until the interval passes.
    expect(published(wasm)).toHaveLength(0);

    await run(() => selection?.select("root/part-1"));

    // One command, both halves: a selection that went out with `pose: []`
    // would have erased the pose on the editor.
    expect(published(wasm)).toHaveLength(1);
    expect(published(wasm)[0]).toMatchObject({
      selection: "root/part-1",
      pose: [{ param: "eye", value: 0.5 }],
    });
    expect(selection?.node).toBe("root/part-1");

    // Clearing the selection is a selection too, and the pose stays with it.
    await run(() => selection?.select(undefined));
    expect(published(wasm)[1]).toMatchObject({ selection: null, pose: [{ param: "eye", value: 0.5 }] });
    expect(selection?.node).toBeUndefined();

    await view.unmount();
  });

  test("a pose is coalesced: many publishes, one command, the last one wins", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    let pose: Pose | undefined;

    function Host() {
      pose = usePose();
      return null;
    }
    const view = await mount(
      <PresenceProvider session={session}>
        <Host />
      </PresenceProvider>,
    );
    await settle();
    jest.useFakeTimers();

    for (const value of [0.1, 0.2, 0.3, 0.4]) {
      await run(() => pose?.publish([{ param: "eye", value }]));
    }
    // A thunk is read when the timer fires, not when it is handed over.
    let reads = 0;
    await run(() =>
      pose?.publish(() => {
        reads += 1;
        return [{ param: "eye", value: 0.5 }];
      }),
    );
    expect(published(wasm)).toHaveLength(0);
    expect(reads).toBe(0);

    await run(() => jest.advanceTimersByTime(POSE_INTERVAL_MS));

    expect(reads).toBe(1);
    expect(published(wasm)).toHaveLength(1);
    expect(published(wasm)[0]).toMatchObject({ pose: [{ param: "eye", value: 0.5 }], selection: null });

    // The same record again is nothing new, and is not sent.
    await run(() => pose?.publish([{ param: "eye", value: 0.5 }]));
    await run(() => jest.advanceTimersByTime(POSE_INTERVAL_MS));
    expect(published(wasm)).toHaveLength(1);

    // A different one is.
    await run(() => pose?.publish([{ param: "eye", value: 0.75 }]));
    await run(() => jest.advanceTimersByTime(POSE_INTERVAL_MS));
    expect(published(wasm)).toHaveLength(2);

    await view.unmount();
  });

  test("what the editor already holds is adopted when a session is taken up", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() =>
      session.send({ cmd: "param_add", name: "eye", min: 0, max: 1, default: 1, key_positions: [0, 1] }),
    );
    const param = session.params()[0] as ParamInfo;
    // An agent posed it and selected something before this tab attached.
    wasm.presence.set(session.id, {
      pose: [{ param: param.id, value: 0.25 }],
      selection: "root/part-1",
    });
    let selection: Selection | undefined;

    function Host() {
      selection = useSelection();
      return null;
    }
    const view = await mount(
      <PresenceProvider session={session}>
        <Host />
      </PresenceProvider>,
    );
    await settle();

    expect(session.paramValue(param.id)).toBe(0.25);
    expect(selection?.node).toBe("root/part-1");
    // Adopting is not publishing: nothing went back out for it.
    expect(published(wasm)).toHaveLength(0);

    await view.unmount();
  });

  test("the selection provider is this provider, under the name the panels use", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    let selection: Selection | undefined;

    function Host() {
      selection = useSelection();
      return null;
    }
    const view = await mount(
      <SelectionProvider session={session}>
        <Host />
      </SelectionProvider>,
    );
    await settle();

    expect(selection?.node).toBeUndefined();
    await run(() => selection?.select("root/part-1"));
    expect(selection?.node).toBe("root/part-1");
    expect(published(wasm)[0]).toMatchObject({ selection: "root/part-1", pose: [] });

    await view.unmount();
  });
});
