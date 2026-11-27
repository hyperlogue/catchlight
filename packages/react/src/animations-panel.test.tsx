import "./test/setup.js";

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { ClipInfo } from "@catchlight/core";
import type { FakeDoc } from "@catchlight/core/fakes";

import { AnimationsPanel, EditorProvider, describeLanes } from "./index.js";
import { fakeReplica, harness, mount, run } from "./test/harness.js";

/** Frames run only when a test steps them, so polling can be counted. */
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

async function step(): Promise<void> {
  const due = [...frames.values()];
  frames.clear();
  await run(() => {
    for (const callback of due) callback(performance.now());
  });
}

const walk: ClipInfo = {
  name: "walk",
  timestep: 1 / 30,
  length: 60,
  lead_in: null,
  lead_out: null,
  lanes: [
    { target: { kind: "param", param: "param-1" }, keyframes: 4 },
    { target: { kind: "param", param: "param-2" }, keyframes: 2 },
    { target: { kind: "control", name: "head_yaw" }, keyframes: 3 },
    { target: { kind: "viseme", name: "aa" }, keyframes: 1 },
  ],
};
const idle: ClipInfo = { ...walk, name: "idle", length: 15, timestep: 0.1, lanes: [{ target: { kind: "expression", name: "happy" }, keyframes: 2 }] };

async function seeded(setup?: (doc: FakeDoc) => void) {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  const errors: unknown[] = [];
  await run(() => session.send({ cmd: "param_add", name: "Jaw", min: 0, max: 1, default: 0 }));
  if (setup) {
    setup(wasm.docs.get(session.id)!);
    await run(() => session.send({ cmd: "param_add", name: "Spare", min: 0, max: 1, default: 0 }));
  }
  const view = await mount(
    <EditorProvider editor={editor}>
      <AnimationsPanel session={session} onError={(e) => errors.push(e)} />
    </EditorProvider>,
  );
  return { session, wasm, view, errors };
}

function clip(container: HTMLElement, name: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`[data-catchlight-clip][data-name="${name}"]`);
  if (!found) throw new Error(`no clip ${name}`);
  return found;
}

function play(container: HTMLElement, name: string): HTMLButtonElement {
  return clip(container, name).querySelector<HTMLButtonElement>("[data-catchlight-clip-play]")!;
}

describe("summarising a clip's lanes", () => {
  test("counts each kind it drives", () => {
    expect(describeLanes(walk)).toBe("2 params, 1 control, 1 viseme");
    expect(describeLanes(idle)).toBe("1 expression");
    expect(describeLanes({ ...walk, lanes: [] })).toBe("No lanes");
  });
});

describe("the animations panel", () => {
  test("a model without animations says where they come from", async () => {
    const { view } = await seeded();
    expect(view.container.querySelector("[data-catchlight-empty-state]")?.textContent).toContain(
      "Animations come from an import or the command line",
    );
    expect(view.container.querySelector("[data-catchlight-clip]")).toBeNull();
    await view.unmount();
  });

  test("lists each clip with its duration and what its lanes drive", async () => {
    const { view } = await seeded((doc) => {
      doc.clips = [walk, idle];
    });
    const walking = clip(view.container, "walk");
    expect(walking.querySelector("[data-catchlight-clip-duration]")?.textContent).toBe("2.00 s");
    expect(walking.querySelector("[data-catchlight-clip-lanes]")?.textContent).toBe("2 params, 1 control, 1 viseme");
    expect(clip(view.container, "idle").querySelector("[data-catchlight-clip-duration]")?.textContent).toBe("1.50 s");
    await view.unmount();
  });

  test("plays one clip at a time, marks it, and stops it", async () => {
    const { session, view } = await seeded((doc) => {
      doc.clips = [walk, idle];
    });
    await run(() => play(view.container, "walk").click());
    expect(session.playing()?.name).toBe("walk");
    expect(clip(view.container, "walk").hasAttribute("data-playing")).toBe(true);
    expect(play(view.container, "walk").getAttribute("aria-label")).toBe("Stop walk");
    expect(play(view.container, "walk").getAttribute("aria-pressed")).toBe("true");

    await run(() => play(view.container, "idle").click());
    expect(session.playing()?.name).toBe("idle");
    expect(clip(view.container, "walk").hasAttribute("data-playing")).toBe(false);
    expect(clip(view.container, "idle").hasAttribute("data-playing")).toBe(true);

    await run(() => play(view.container, "idle").click());
    expect(session.playing()).toBeUndefined();
    expect(view.container.querySelector("[data-playing]")).toBeNull();
    await view.unmount();
  });

  test("the playhead is polled every frame while a clip plays, and not once it stops", async () => {
    const { session, view } = await seeded((doc) => {
      doc.clips = [walk];
    });
    expect(frames.size).toBe(0);
    await run(() => play(view.container, "walk").click());
    expect(frames.size).toBeGreaterThan(0);
    const playhead = () => clip(view.container, "walk").querySelector("[data-catchlight-clip-playhead]")?.textContent;
    expect(playhead()).toContain("0.00 s");
    expect(playhead()).toContain("frame 0");

    // The replica moves the clip without invalidating anything.
    fakeReplica(session).clip = { name: "walk", time: 0.5, frame: 15.4 };
    await step();
    expect(playhead()).toContain("0.50 s");
    expect(playhead()).toContain("frame 15");
    expect(frames.size).toBeGreaterThan(0);

    await run(() => session.stop());
    await step();
    expect(frames.size).toBe(0);
    expect(playhead()).toBeUndefined();
    expect(clip(view.container, "walk").querySelector("[data-catchlight-clip-duration]")?.textContent).toBe("2.00 s");
    await view.unmount();
  });

  test("a clip that ends without an invalidation is noticed on the next frame", async () => {
    const { session, view } = await seeded((doc) => {
      doc.clips = [walk];
    });
    await run(() => play(view.container, "walk").click());
    // Editing stops playback in the replica, as entering mesh or record mode does.
    fakeReplica(session).setEditing(true);
    await step();
    expect(clip(view.container, "walk").hasAttribute("data-playing")).toBe(false);
    expect(frames.size).toBe(0);
    await view.unmount();
  });

  test("a clip the replica refuses to play is reported", async () => {
    const { view, errors, wasm, session } = await seeded((doc) => {
      doc.clips = [walk];
    });
    // The replica's own copy loses the clip; the list still shows it.
    fakeReplica(session).doc!.clips = [];
    await run(() => play(view.container, "walk").click());
    expect(errors).toHaveLength(1);
    expect(String(errors[0])).toContain("could not be played");
    expect(wasm.docs.get(session.id)!.clips).toHaveLength(1);
    await view.unmount();
  });

  test("shows the animations warnings and marks the clip whose lane param they concern", async () => {
    const { view } = await seeded((doc) => {
      doc.clips = [walk, idle];
      doc.extensionWarnings = [
        {
          key: "catchlight.animations",
          code: "catchlight/dangling-reference",
          message: "clip \"walk\" has a lane on a removed param",
          subjects: [{ removed: { param: "param-2" } }],
        },
      ];
    });
    const warnings = view.container.querySelector<HTMLDetailsElement>("[data-catchlight-extension-warnings]")!;
    expect(warnings.open).toBe(true);
    expect(warnings.querySelector("summary")?.textContent).toBe("1 warning");
    expect(warnings.querySelector("[data-catchlight-warning]")?.textContent).toBe(
      "Clip \"walk\" has a lane on a removed param",
    );
    expect(clip(view.container, "walk").hasAttribute("data-warned")).toBe(true);
    expect(clip(view.container, "idle").hasAttribute("data-warned")).toBe(false);
    await view.unmount();
  });

  test("more than two warnings start folded, so the clips stay in view", async () => {
    const { view } = await seeded((doc) => {
      doc.clips = [walk];
      doc.extensionWarnings = ["a", "b", "c"].map((message) => ({
        key: "catchlight.animations",
        code: "catchlight.animations/missing-viseme",
        message,
        subjects: [],
      }));
    });
    const warnings = view.container.querySelector<HTMLDetailsElement>("[data-catchlight-extension-warnings]")!;
    expect(warnings.open).toBe(false);
    expect(warnings.querySelector("summary")?.textContent).toBe("3 warnings");
    expect(warnings.querySelectorAll("[data-catchlight-warning]")).toHaveLength(3);
    await view.unmount();
  });

  test("animations this build cannot read are reported, with nothing to play", async () => {
    const { view } = await seeded((doc) => {
      doc.clips = [walk];
      doc.unread = ["catchlight.animations"];
    });
    expect(view.container.querySelector("[data-catchlight-extension-unread]")?.textContent).toContain(
      "cannot read the model’s animations",
    );
    expect(view.container.querySelector("button")).toBeNull();
    await view.unmount();
  });
});
