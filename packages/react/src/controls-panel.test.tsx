import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Session } from "@catchlight/core";
import type { FakeDoc } from "@catchlight/core/fakes";

import { ControlsPanel, EditorProvider, groupControls } from "./index.js";
import { enter, fakeReplica, fire, harness, mount, run } from "./test/harness.js";

async function seeded(setup?: (doc: FakeDoc) => void) {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  const errors: unknown[] = [];
  await run(() => session.send({ cmd: "param_add", name: "Jaw", min: 0, max: 1, default: 0 }));
  await run(() => session.send({ cmd: "param_add", name: "Turn", min: -30, max: 30, default: 0 }));
  if (setup) {
    setup(wasm.docs.get(session.id)!);
    // Any edit feeds the replica what the test put on the doc.
    await run(() => session.send({ cmd: "param_add", name: "Spare", min: 0, max: 1, default: 0 }));
  }
  const view = await mount(
    <EditorProvider editor={editor}>
      <ControlsPanel session={session} onError={(e) => errors.push(e)} />
    </EditorProvider>,
  );
  return { session, wasm, view, errors };
}

function card(container: HTMLElement, control: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`[data-catchlight-control-card][data-control="${control}"]`);
  if (!found) throw new Error(`no card for ${control}`);
  return found;
}

function paramId(session: Session, name: string): string {
  return session.params().find((p) => p.name === name)!.id;
}

async function choose(select: HTMLSelectElement, value: string): Promise<void> {
  select.value = value;
  await fire(select, new Event("change", { bubbles: true }));
}

async function slide(input: HTMLInputElement, value: number): Promise<void> {
  input.value = String(value);
  await fire(input, new Event("input", { bubbles: true }));
}

describe("grouping the controls", () => {
  test("puts each standard control under its heading and an unknown mapped one under Other", () => {
    const groups = groupControls(
      [
        { name: "mouth_open", one_sided: true },
        { name: "head_yaw", one_sided: false },
        { name: "blush", one_sided: true },
        { name: "leg_left_swing", one_sided: false },
      ],
      [{ control: "tail_wag", param: "p", stops: [0, 0, 1] }],
    );
    expect(groups.map((g) => [g.title, g.entries.map((e) => e.name)])).toEqual([
      ["Head", ["head_yaw"]],
      ["Body", ["leg_left_swing"]],
      ["Cheeks", ["blush"]],
      ["Mouth", ["mouth_open"]],
      ["Other", ["tail_wag"]],
    ]);
    expect(groups.at(-1)!.entries[0]!.standard).toBe(false);
  });
});

describe("the controls panel", () => {
  test("shows every standard control unmapped on a model without mappings", async () => {
    const { view } = await seeded();
    const headings = [...view.container.querySelectorAll("[data-catchlight-control-group] > h3")].map((h) => h.textContent);
    expect(headings).toEqual(["Head", "Mouth"]);
    expect(view.container.querySelectorAll("[data-catchlight-control-card]")).toHaveLength(3);
    expect(view.container.querySelector("[data-catchlight-control-count]")?.textContent).toBe("0 of 3 mapped");
    const slider = card(view.container, "head_yaw").querySelector<HTMLInputElement>("[data-catchlight-control-slider]")!;
    expect(slider.disabled).toBe(true);
    expect(slider.min).toBe("-1");
    expect(card(view.container, "mouth_open").querySelector<HTMLInputElement>("input[type=range]")!.min).toBe("0");
    expect(card(view.container, "head_yaw").querySelector("[data-catchlight-control-stops]")).toBeNull();
    await view.unmount();
  });

  test("picking a param maps with the server's stops, and Unmapped unmaps", async () => {
    const { session, wasm, view } = await seeded();
    const jaw = paramId(session, "Jaw");
    await choose(card(view.container, "mouth_open").querySelector("select")!, jaw);
    expect(wasm.requests.at(-1)).toEqual({
      id: expect.any(Number),
      session: session.id,
      cmd: "humanoid_map",
      control: "mouth_open",
      param: jaw,
      nonstandard: false,
    });
    const mapped = card(view.container, "mouth_open");
    expect(mapped.hasAttribute("data-mapped")).toBe(true);
    expect(mapped.querySelector<HTMLInputElement>("[data-catchlight-control-slider]")!.disabled).toBe(false);
    expect([...mapped.querySelectorAll<HTMLInputElement>("[data-catchlight-control-stops] input")].map((i) => i.value)).toEqual(["0", "0", "1"]);
    expect(view.container.querySelector("[data-catchlight-control-count]")?.textContent).toBe("1 of 3 mapped");

    await choose(mapped.querySelector("select")!, "");
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "humanoid_unmap", control: "mouth_open" });
    expect(card(view.container, "mouth_open").hasAttribute("data-mapped")).toBe(false);
    await view.unmount();
  });

  test("editing a stop re-maps with all three stops", async () => {
    const { session, wasm, view } = await seeded((doc) => {
      doc.humanoid = [{ control: "head_yaw", param: doc.params.find((p) => p.name === "Turn")!.id, stops: [-30, 0, 30] }];
    });
    const plus = card(view.container, "head_yaw").querySelector<HTMLInputElement>("[aria-label='head_yaw stop at +1']")!;
    await enter(plus, "20");
    expect(wasm.requests.at(-1)).toMatchObject({
      cmd: "humanoid_map",
      control: "head_yaw",
      param: paramId(session, "Turn"),
      stops: [-30, 0, 20],
      nonstandard: false,
    });
    await view.unmount();
  });

  test("the slider gives a control a live value, and clearing it removes the value instead of setting 0", async () => {
    const { session, view } = await seeded((doc) => {
      doc.humanoid = [{ control: "head_yaw", param: doc.params[1]!.id, stops: [-30, 0, 30] }];
    });
    const yaw = card(view.container, "head_yaw");
    const clear = yaw.querySelector<HTMLButtonElement>("[aria-label='Clear head_yaw live value']")!;
    expect(clear.disabled).toBe(true);
    await slide(yaw.querySelector<HTMLInputElement>("[data-catchlight-control-slider]")!, 0.5);
    expect(fakeReplica(session).live.controls).toEqual({ head_yaw: 0.5 });
    expect(yaw.hasAttribute("data-live")).toBe(true);
    expect(yaw.querySelector("output")?.textContent).toBe("0.50");

    await run(() => clear.click());
    expect(fakeReplica(session).live.controls).toEqual({});
    expect(yaw.hasAttribute("data-live")).toBe(false);
    expect(yaw.querySelector("output")?.textContent).toBe("–");
    await view.unmount();
  });

  test("moving the mapped param's slider clears the control's live value", async () => {
    const { session, view } = await seeded((doc) => {
      doc.humanoid = [{ control: "head_yaw", param: doc.params[1]!.id, stops: [-30, 0, 30] }];
    });
    await slide(card(view.container, "head_yaw").querySelector<HTMLInputElement>("[data-catchlight-control-slider]")!, -1);
    await run(() => session.setParam(paramId(session, "Turn"), 4));
    expect(card(view.container, "head_yaw").hasAttribute("data-live")).toBe(false);
    await view.unmount();
  });

  test("Mapped only hides what is still to map", async () => {
    const { view } = await seeded((doc) => {
      doc.humanoid = [{ control: "head_yaw", param: doc.params[1]!.id, stops: [-30, 0, 30] }];
    });
    const toggle = view.container.querySelector<HTMLInputElement>("[data-catchlight-toggle] input")!;
    expect(toggle.checked).toBe(false);
    await run(() => toggle.click());
    expect([...view.container.querySelectorAll("[data-catchlight-control-card]")].map((c) => c.getAttribute("data-control"))).toEqual(["head_yaw"]);
    await view.unmount();
  });

  test("a control outside the standard is listed under Other and asks before re-mapping", async () => {
    const { session, wasm, view } = await seeded((doc) => {
      doc.humanoid = [{ control: "tail_wag", param: doc.params[0]!.id, stops: [0, 0, 1] }];
    });
    const other = view.container.querySelector("[data-catchlight-control-group][aria-label='Other']")!;
    expect(other.querySelector("[data-control='tail_wag']")?.hasAttribute("data-nonstandard")).toBe(true);

    await enter(card(view.container, "tail_wag").querySelector<HTMLInputElement>("[aria-label='tail_wag stop at +1']")!, "0.5");
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "humanoid_map", nonstandard: false });
    const dialog = view.container.querySelector("dialog")!;
    expect(dialog.textContent).toContain("is not a standard humanoid control");

    await run(() => [...dialog.querySelectorAll("button")].find((b) => b.textContent === "Keep the name")!.click());
    expect(wasm.requests.at(-1)).toMatchObject({
      cmd: "humanoid_map",
      control: "tail_wag",
      stops: [0, 0, 0.5],
      nonstandard: true,
    });
    expect(view.container.querySelector("dialog")).toBeNull();
    expect(wasm.docs.get(session.id)!.humanoid[0]!.stops).toEqual([0, 0, 0.5]);

    // Kept once, the name is sent as nonstandard from then on.
    await enter(card(view.container, "tail_wag").querySelector<HTMLInputElement>("[aria-label='tail_wag stop at 0']")!, "0.1");
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "humanoid_map", nonstandard: true });
    expect(view.container.querySelector("dialog")).toBeNull();
    await view.unmount();
  });

  test("cancelling the nonstandard question sends nothing more", async () => {
    const { wasm, view } = await seeded((doc) => {
      doc.humanoid = [{ control: "tail_wag", param: doc.params[0]!.id, stops: [0, 0, 1] }];
    });
    await enter(card(view.container, "tail_wag").querySelector<HTMLInputElement>("[aria-label='tail_wag stop at +1']")!, "0.5");
    const sent = wasm.requests.length;
    await run(() => [...view.container.querySelectorAll<HTMLButtonElement>("dialog button")].find((b) => b.textContent === "Cancel")!.click());
    expect(view.container.querySelector("dialog")).toBeNull();
    expect(wasm.requests).toHaveLength(sent);
    await view.unmount();
  });

  test("shows the humanoid warnings and marks the card whose param they concern", async () => {
    const { view } = await seeded((doc) => {
      const turn = doc.params[1]!.id;
      doc.humanoid = [
        { control: "head_yaw", param: turn, stops: [-30, 0, 30] },
        { control: "mouth_smile", param: turn, stops: [-30, 0, 30] },
      ];
      doc.extensionWarnings = [
        {
          key: "catchlight.humanoid",
          code: "catchlight.humanoid/shared-param",
          message: "controls head_yaw, mouth_smile all map onto Turn",
          subjects: [{ param: turn }],
        },
        { key: "catchlight.expressions", code: "x", message: "not ours", subjects: [] },
      ];
    });
    const shown = [...view.container.querySelectorAll("[data-catchlight-extension-warnings] [data-catchlight-warning]")];
    expect(shown.map((w) => w.textContent)).toEqual(["Controls head_yaw, mouth_smile all map onto Turn"]);
    expect(card(view.container, "head_yaw").hasAttribute("data-warned")).toBe(true);
    expect(card(view.container, "mouth_smile").hasAttribute("data-warned")).toBe(true);
    expect(card(view.container, "mouth_open").hasAttribute("data-warned")).toBe(false);
    expect(card(view.container, "head_yaw").querySelector("[data-catchlight-warning-mark]")?.getAttribute("aria-label")).toContain("all map onto Turn");
    await view.unmount();
  });

  test("humanoid data this build cannot read is reported, with nothing to edit", async () => {
    const { view } = await seeded((doc) => {
      doc.unread = ["catchlight.humanoid"];
    });
    expect(view.container.querySelector("[data-catchlight-extension-unread]")?.textContent).toContain("cannot read");
    expect(view.container.querySelector("[data-catchlight-control-card]")).toBeNull();
    expect(view.container.querySelector("select, input")).toBeNull();
    await view.unmount();
  });
});
