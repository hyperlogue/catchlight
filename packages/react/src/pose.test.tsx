/**
 * The pose reaches the editor without a slider knowing, and a reset is a pose.
 */

import "./test/setup.js";

import { afterEach, describe, expect, jest, test } from "bun:test";
import type { Command, ParamInfo, Session, SessionDocumentCommand } from "@catchlight/core";
import type { FakeEditor } from "@catchlight/core/fakes";

import {
  POSE_INTERVAL_MS,
  ParamSlider,
  PresenceProvider,
  usePosePublisher,
  useResetPose,
} from "./index.js";
import { fire, harness, mount, run, settle } from "./test/harness.js";

const param = (name: string, fallback: number): SessionDocumentCommand => ({
  cmd: "param_add",
  name,
  min: 0,
  max: 1,
  default: fallback,
  key_positions: [0, 1],
});

function published(wasm: FakeEditor): Array<Extract<Command, { cmd: "presence_set" }>> {
  return wasm.requests.filter(
    (request): request is Extract<Command, { cmd: "presence_set" }> & { id: number } =>
      request.cmd === "presence_set",
  );
}

function Publisher({ session }: { session: Session }) {
  usePosePublisher(session);
  return null;
}

afterEach(() => {
  jest.useRealTimers();
});

describe("publishing the pose", () => {
  test("moving a slider publishes every param, and the slider was not told", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(param("eye_open", 1)));
    await run(() => session.send(param("mouth_open", 0)));
    const [eye, mouth] = session.params() as [ParamInfo, ParamInfo];

    const view = await mount(
      <PresenceProvider session={session}>
        <Publisher session={session} />
        <ParamSlider.Root session={session} param={eye} />
      </PresenceProvider>,
    );
    await settle();
    jest.useFakeTimers();

    const slider = view.container.querySelector("input") as HTMLInputElement;
    slider.value = "0.25";
    await fire(slider, new Event("input", { bubbles: true }));
    expect(published(wasm)).toHaveLength(0);

    await run(() => jest.advanceTimersByTime(POSE_INTERVAL_MS));

    // The whole pose: the param that moved at its value, the one that did not
    // at its default, so a follower can set both.
    expect(published(wasm)).toHaveLength(1);
    expect(published(wasm)[0]).toMatchObject({
      pose: [
        { param: eye.id, value: 0.25 },
        { param: mouth.id, value: 0 },
      ],
    });

    await view.unmount();
  });

  test("reset puts every param at its default, and that is what goes out", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(param("eye_open", 1)));
    await run(() => session.send(param("mouth_open", 0)));
    const [eye, mouth] = session.params() as [ParamInfo, ParamInfo];
    session.setParam(eye.id, 0.3);
    session.setParam(mouth.id, 0.7);

    let reset: (() => void) | undefined;
    function Host() {
      reset = useResetPose(session);
      return null;
    }
    const view = await mount(
      <PresenceProvider session={session}>
        <Publisher session={session} />
        <Host />
      </PresenceProvider>,
    );
    await settle();
    jest.useFakeTimers();

    await run(() => reset?.());

    expect(session.paramValue(eye.id)).toBe(1);
    expect(session.paramValue(mouth.id)).toBe(0);

    await run(() => jest.advanceTimersByTime(POSE_INTERVAL_MS));
    expect(published(wasm)).toHaveLength(1);
    expect(published(wasm)[0]).toMatchObject({
      pose: [
        { param: eye.id, value: 1 },
        { param: mouth.id, value: 0 },
      ],
    });

    await view.unmount();
  });
});
