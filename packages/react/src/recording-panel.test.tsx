/** Recording navigation preserves binding-local cells and bounds its shelf. */
import "./test/setup.js";

import { expect, test } from "bun:test";
import { useEffect } from "react";
import type { BindingTarget } from "@catchlight/core";
import { EditorProvider } from "./index.js";
import { EditingProvider, useEditing } from "./editing.js";
import type { Editing } from "./editing.js";
import { RecordingInspector, RecordingKeys } from "./recording-panel.js";
import { SelectionProvider, useSelection } from "./selection.js";
import { fakeReplica, fire, harness, mount, run } from "./test/harness.js";

async function scene(grids: { target: BindingTarget; positions: number[][]; cell?: [number, number] }[]) {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  await session.send({ cmd: "node_add", parent: "root", kind: "group", name: "Panel" });
  const node = session.tree().children[0]!.id;
  const paired = grids[0]!.positions.length === 2;
  for (const name of paired ? ["Horizontal", "Vertical"] : ["Horizontal"]) {
    await session.send({ cmd: "param_add", name, min: 0, max: 1, default: 0 });
  }
  const [x, y] = session.params();
  if (y) fakeReplica(session).recordingPairs = () => JSON.stringify([[x!.id, y.id]]);
  for (const { target, positions, cell } of grids) {
    const address = { node, param: x!.id, param_y: y?.id ?? null, target };
    await session.send({ cmd: "binding_add", ...address, key_positions: positions });
    if (cell) await session.send({
      cmd: "binding_cells_set", if_rev: session.getRevision(), ...address, cells: [{ cell, value: { scalar: 10 } }],
    });
  }
  let editing: Editing;
  function Probe() {
    editing = useEditing()!;
    const { select } = useSelection();
    useEffect(() => select(node), [select]);
    return <><RecordingKeys /><RecordingInspector /></>;
  }
  const view = await mount(
    <EditorProvider editor={editor}>
      <SelectionProvider session={session}>
        <EditingProvider session={session} onError={(cause) => { throw cause; }} onNotice={() => {}}>
          <Probe />
        </EditingProvider>
      </SelectionProvider>
    </EditorProvider>,
  );
  return { ...view, session, wasm, x: x!, y, editing: () => editing! };
}

test("recording marks and clears the exact key among close explicit positions", async () => {
  const view = await scene([{ target: "tx", positions: [[0, 0.1, 0.100001, 1]], cell: [2, 0] }]);
  try {
    const key = view.container.querySelector<HTMLButtonElement>('[data-key-x="2"]')!;
    expect(key.hasAttribute("data-authored")).toBe(true);
    await run(() => key.click());
    expect(view.editing().authored).toBe(true);
    expect(view.container.querySelector("[data-catchlight-key-state]")?.textContent).toBe("1 property keyed");
    await run(() => view.session.setParam(view.x.id, 0.1000009));
    expect(view.editing().authored).toBe(true);
    expect(view.container.querySelector("[data-catchlight-key-state]")?.textContent).toBe("1 property keyed");
    await run(() => view.editing().keyAction("clear"));
    const request = view.wasm.requests.filter((request) => request.cmd === "edit_apply").at(-1);
    expect(request?.cmd === "edit_apply" && request.edits[0]).toMatchObject({
      op: "binding_cells_unset", cells: [[2, 0]],
    });
  } finally {
    await view.unmount();
  }
});

test("large combined grids keep bounded navigation and continuous pose controls", async () => {
  const positions = Array.from({ length: 20 }, (_, index) => index / 19);
  const view = await scene([
    { target: "tx", positions: [positions, [0, 1]] },
    { target: "ty", positions: [[0, 1], positions] },
  ]);
  try {
    expect(view.container.querySelector("[data-catchlight-record-matrix]") === null).toBe(true);
    expect(view.container.querySelectorAll("[data-catchlight-record-key]")).toHaveLength(0);
    expect(view.container.textContent).toContain("Use the sliders or step through each param’s key positions");
    const commands = view.wasm.requests.length;
    const sliders = view.container.querySelectorAll<HTMLInputElement>("input[type=range]");
    expect(sliders).toHaveLength(2);
    expect(view.container.querySelectorAll("[data-catchlight-record-key-strip] button")).toHaveLength(4);
    sliders[0]!.value = "0.25";
    await fire(sliders[0]!, new Event("input", { bubbles: true }));
    expect(view.session.paramValue(view.x.id)).toBe(0.25);
    const next = view.container.querySelector<HTMLButtonElement>('[aria-label="Next Horizontal key position"]')!;
    await run(() => next.click());
    expect(view.session.paramValue(view.x.id)).toBe(positions[5]);
    expect(view.session.paramValue(view.y!.id) ?? view.y!.default).toBe(0);
    const nextVertical = view.container.querySelector<HTMLButtonElement>('[aria-label="Next Vertical key position"]')!;
    await run(() => nextVertical.click());
    expect(view.session.paramValue(view.y!.id)).toBe(positions[1]);
    expect(view.session.paramValue(view.x.id)).toBe(positions[5]);
    expect(view.wasm.requests.length).toBe(commands);
  } finally {
    await view.unmount();
  }
});

test("long single-param shelves keep bounded stepping with endpoint controls", async () => {
  const positions = [...Array.from({ length: 257 }, (_, index) => index / 256), 0.1, 0.100001]
    .sort((a, b) => a - b);
  const view = await scene([{ target: "tx", positions: [positions] }]);
  try {
    expect(view.container.querySelectorAll("[data-catchlight-record-key]")).toHaveLength(0);
    const previous = view.container.querySelector<HTMLButtonElement>('[aria-label="Previous Horizontal key position"]')!;
    const next = view.container.querySelector<HTMLButtonElement>('[aria-label="Next Horizontal key position"]')!;
    expect(previous.disabled).toBe(true);
    await run(() => next.click());
    expect(view.session.paramValue(view.x.id)).toBe(positions[1]);
    expect(previous.disabled).toBe(false);
    await run(() => view.session.setParam(view.x.id, 1));
    expect(next.disabled).toBe(true);
    await run(() => previous.click());
    expect(view.session.paramValue(view.x.id)).toBe(positions.at(-2));
    await run(() => view.session.setParam(view.x.id, 0.1));
    await run(() => next.click());
    expect(view.session.paramValue(view.x.id)).toBe(0.100001);
    await run(() => previous.click());
    expect(view.session.paramValue(view.x.id)).toBe(0.1);
  } finally {
    await view.unmount();
  }
});

test("small paired grids keep every navigation keypoint", async () => {
  const view = await scene([{ target: "tx", positions: [[0, 0.5, 1], [0, 1]] }]);
  try {
    expect(view.container.querySelector("[data-catchlight-record-matrix]")).not.toBeNull();
    expect(view.container.querySelectorAll("[data-catchlight-record-key]")).toHaveLength(6);
  } finally {
    await view.unmount();
  }
});
