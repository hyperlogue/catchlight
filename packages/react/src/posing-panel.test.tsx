import "./test/setup.js";

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { NodeId, ParamId, Session } from "@catchlight/core";
import type { FakeDoc } from "@catchlight/core/fakes";
import { useEffect } from "react";

import { EditorProvider, ParamsPanel, usePoseSweep } from "./index.js";
import { enter, fakeReplica, fire, harness, mount, run } from "./test/harness.js";

/** Frames run only when a test steps them. */
const frames = new Map<number, FrameRequestCallback>();
let nextFrame = 1;
const saved = { request: globalThis.requestAnimationFrame, cancel: globalThis.cancelAnimationFrame };
beforeEach(() => {
  frames.clear();
  globalThis.requestAnimationFrame = (callback) => {
    frames.set(nextFrame, callback);
    return nextFrame++;
  };
  globalThis.cancelAnimationFrame = (id) => {
    frames.delete(id);
  };
});
afterEach(() => {
  globalThis.requestAnimationFrame = saved.request;
  globalThis.cancelAnimationFrame = saved.cancel;
});

async function step(now = performance.now()): Promise<void> {
  const due = [...frames.values()];
  frames.clear();
  await run(() => {
    for (const callback of due) callback(now);
  });
}

const HAIR = "root/hair";

/** `Jaw` (param-1) is posed; `Sway` (param-2) is the output of the response `Hair` when `driven`. */
async function seeded(driven = true) {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  await run(() => session.send({ cmd: "param_add", name: "Jaw", min: 0, max: 1, default: 0 }));
  await run(() => session.send({ cmd: "param_add", name: "Sway", min: -1, max: 1, default: 0 }));
  const doc = wasm.docs.get(session.id) as FakeDoc;
  doc.root.children.push({ id: HAIR, name: "Hair", kind: "motion_response", z_order: 0, enabled: true, children: [] });
  if (driven) doc.params[1]!.driven_by = HAIR;
  await run(() => session.send({ cmd: "param_add", name: "Spare", min: 0, max: 1, default: 0 }));
  const selectedNodes: NodeId[] = [];
  const view = await mount(
    <EditorProvider editor={editor}>
      <ParamsPanel
        session={session}
        selected={undefined}
        onSelect={() => undefined}
        onSelectNode={(node) => selectedNodes.push(node)}
        onError={(e) => {
          throw e;
        }}
      />
    </EditorProvider>,
  );
  return { session, view, selectedNodes };
}

function card(container: HTMLElement, name: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>("[data-catchlight-param-card]")].find(
    (c) => c.querySelector("[data-catchlight-param-name]")?.textContent === name,
  );
  if (!found) throw new Error(`no card ${name}`);
  return found;
}

const slider = (c: HTMLElement) => c.querySelector<HTMLInputElement>("input[type=range]")!;
const number = (c: HTMLElement) => c.querySelector<HTMLInputElement>("input[type=number]")!;
const reset = (c: HTMLElement) => c.querySelector<HTMLButtonElement>("[data-catchlight-param-range] button")!;
const toggle = (container: HTMLElement) =>
  container.querySelector<HTMLButtonElement>("[data-catchlight-motion-toggle]")!;

describe("a param a motion response drives", () => {
  test("is a disabled readout under its own heading that names the response while motion runs", async () => {
    const { view, selectedNodes } = await seeded();
    const section = view.container.querySelector("[data-catchlight-driven-params]")!;
    expect(section.querySelector("h3")?.textContent).toBe("Driven by motion");
    expect(section.textContent).toContain("Pause motion to pose them by hand.");

    const sway = card(view.container, "Sway");
    expect(section.contains(sway)).toBe(true);
    expect(section.contains(card(view.container, "Jaw"))).toBe(false);
    expect(sway.hasAttribute("data-held")).toBe(true);
    expect(slider(sway).disabled).toBe(true);
    expect(number(sway).disabled).toBe(true);
    expect(reset(sway).disabled).toBe(true);
    expect(slider(card(view.container, "Jaw")).disabled).toBe(false);

    const driver = sway.querySelector<HTMLButtonElement>("[data-catchlight-param-driver] button")!;
    expect(driver.textContent).toBe("Driven by Hair");
    await fire(driver, new MouseEvent("click", { bubbles: true }));
    expect(selectedNodes).toEqual([HAIR]);
    await view.unmount();
  });

  test("follows the value the response writes each frame, and stops reading once paused", async () => {
    const { session, view } = await seeded();
    const replica = fakeReplica(session);
    replica.pose.set("param-2", 0.4);
    await step();
    expect(slider(card(view.container, "Sway")).value).toBe("0.4");
    expect(number(card(view.container, "Sway")).value).toBe("0.4");

    await fire(toggle(view.container), new MouseEvent("click", { bubbles: true }));
    expect(frames.size).toBe(0);
    await view.unmount();
  });

  test("poses like any param once the person pauses motion", async () => {
    const { session, view } = await seeded();
    const button = toggle(view.container);
    expect(button.textContent).toBe("Pause motion");
    await fire(button, new MouseEvent("click", { bubbles: true }));
    expect(session.motionPaused()).toBe(true);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(button.textContent).toBe("Resume motion");

    const sway = card(view.container, "Sway");
    expect(sway.hasAttribute("data-held")).toBe(false);
    expect(sway.querySelector("[data-catchlight-param-driver]")?.textContent).toContain("Motion paused");
    expect(slider(sway).disabled).toBe(false);
    slider(sway).value = "-0.5";
    await fire(slider(sway), new Event("input", { bubbles: true }));
    expect(session.paramValue("param-2")).toBe(-0.5);

    await fire(button, new MouseEvent("click", { bubbles: true }));
    expect(session.motionRunning()).toBe(true);
    expect(slider(card(view.container, "Sway")).disabled).toBe(true);
    await view.unmount();
  });

  test("an editing mode makes it editable too, without offering to resume", async () => {
    const { session, view } = await seeded();
    await run(() => session.setEditing(true));
    expect(slider(card(view.container, "Sway")).disabled).toBe(false);
    expect(toggle(view.container).disabled).toBe(true);
    expect(toggle(view.container).textContent).toBe("Motion paused");
    await view.unmount();
  });

  test("the heading shows only when the model has driven params, and search filters both groups", async () => {
    const plain = await seeded(false);
    expect(plain.view.container.querySelector("[data-catchlight-driven-params]") === null).toBe(true);
    expect(plain.view.container.querySelector("[data-catchlight-motion-toggle]") === null).toBe(true);
    expect(frames.size).toBe(0);
    await plain.view.unmount();

    const { view } = await seeded();
    const search = view.container.querySelector<HTMLInputElement>("[data-catchlight-search] input")!;
    await enter(search, "jaw");
    expect(view.container.querySelector("[data-catchlight-driven-params]") === null).toBe(true);
    expect(view.container.querySelectorAll("[data-catchlight-param-card]")).toHaveLength(1);
    await enter(search, "sway");
    expect(view.container.querySelector("[data-catchlight-driven-params]") === null).toBe(false);
    expect(view.container.querySelectorAll("[data-catchlight-param-card]")).toHaveLength(1);
    await view.unmount();
  });
});

function Sweep({ session, param }: { session: Session; param: ParamId }) {
  const sweep = usePoseSweep(session, param);
  useEffect(() => sweep.setPlaying(true), [sweep.held]);
  return <span data-held={sweep.held ? "" : undefined} data-playing={sweep.playing ? "" : undefined} />;
}

describe("the pose sweep", () => {
  test("skips a param a running motion response holds, and sweeps it once motion is paused", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newSession();
    await run(() => session.send({ cmd: "param_add", name: "Sway", min: -1, max: 1, default: 0 }));
    wasm.docs.get(session.id)!.params[0]!.driven_by = HAIR;
    await run(() => session.send({ cmd: "param_add", name: "Spare", min: 0, max: 1, default: 0 }));
    const view = await mount(
      <EditorProvider editor={editor}>
        <Sweep session={session} param="param-1" />
      </EditorProvider>,
    );
    const marker = view.container.querySelector("span")!;
    expect(marker.hasAttribute("data-held")).toBe(true);
    await step(1000);
    await step(2000);
    expect(session.paramValue("param-1")).toBeUndefined();

    await run(() => session.setMotionPaused(true));
    expect(marker.hasAttribute("data-held")).toBe(false);
    expect(marker.hasAttribute("data-playing")).toBe(true);
    await step(3000);
    expect(session.paramValue("param-1")).not.toBeUndefined();
    await view.unmount();
  });
});
