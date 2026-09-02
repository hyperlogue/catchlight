/**
 * The binding grid: what a cell shows, and what each control sends.
 *
 * The two rules this suite exists for. A cell has to say whether anybody
 * authored it, because the model derives the ones nobody did and "unset" is a
 * state a rigger acts on. And clicking a cell has to pose the puppet without
 * authoring anything, because looking at a keypoint is not editing it.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { NodeId, ParamInfo, Request, Session, SessionDocumentCommand } from "@catchlight/core";
import { act } from "react";

import { BindingGrid, EditorProvider } from "./index.js";
import { fire, harness, mount, run } from "./test/harness.js";

/** One param, keyed at both ends and in the middle. */
const yaw = (): SessionDocumentCommand => ({
  cmd: "param_add",
  name: "head:yaw",
  min: -1,
  max: 1,
  default: 0,
  key_positions: [0, 0.5, 1],
});

function sent(requests: Request[]): Request[] {
  return requests.filter((request) => request.cmd !== "session_new");
}

function last(requests: Request[]): Request {
  const request = sent(requests).at(-1);
  if (!request) throw new Error("nothing was sent");
  return request;
}

/** A document with one part and one param keyed in three places. */
interface Scene {
  editor: Awaited<ReturnType<typeof harness>>["editor"];
  wasm: Awaited<ReturnType<typeof harness>>["wasm"];
  session: Session;
  param: ParamInfo;
  node: NodeId;
}

async function scene(): Promise<Scene> {
  const { editor, wasm } = await harness();
  const session = await editor.newDocument();
  await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));
  await run(() => session.send(yaw()));
  const param = session.params()[0] as ParamInfo;
  const node = session.tree().children[0]?.id as NodeId;
  return { editor, wasm, session, param, node };
}

function mountGrid(s: Scene) {
  return mount(
    <EditorProvider editor={s.editor}>
      <BindingGrid.Root session={s.session} node={s.node} param={s.param.id} />
    </EditorProvider>,
  );
}

describe("the binding grid", () => {

  test("a cell per key position, and the unset ones say so", async () => {
    const s = await scene();
    await run(() =>
      s.session.send({
        cmd: "binding_key",
        node: s.node,
        target: "tx",
        param: s.param.id,
        cell: [2, 0],
        value: 12,
      }),
    );

    const view = await mountGrid(s);
    const cells = [...view.container.querySelectorAll("[data-catchlight-binding-cell]")];

    // The grid is as wide as the param has key positions, and one row tall.
    expect(cells.length).toBe(3);
    expect(cells.map((cell) => cell.getAttribute("data-cell"))).toEqual(["0,0", "1,0", "2,0"]);
    expect(cells.map((cell) => cell.getAttribute("data-set"))).toEqual([null, null, ""]);
    expect(cells.map((cell) => cell.getAttribute("data-unset"))).toEqual(["", "", null]);
    expect(cells.map((cell) => (cell as HTMLInputElement).value)).toEqual(["", "", "12"]);
    await view.unmount();
  });

  test("clicking a cell poses the param and sends nothing", async () => {
    const s = await scene();
    await run(() =>
      s.session.send({
        cmd: "binding_add",
        node: s.node,
        target: "tx",
        param: s.param.id,
      }),
    );

    const view = await mountGrid(s);
    const cells = [...view.container.querySelectorAll("[data-catchlight-binding-cell]")];
    const commands = sent(s.wasm.requests).length;

    await act(async () => {
      (cells[2] as HTMLElement).click();
    });

    // Key position 1 on a param running -1..1 is the top of its range.
    expect(s.session.paramValue(s.param.id)).toBe(1);
    expect(sent(s.wasm.requests).length).toBe(commands);
    expect(cells[2]?.getAttribute("data-selected")).toBe("");
    await view.unmount();
  });

  test("typing in a cell commits one binding_key at that cell", async () => {
    const s = await scene();
    await run(() =>
      s.session.send({
        cmd: "binding_add",
        node: s.node,
        target: "tx",
        param: s.param.id,
      }),
    );

    const view = await mountGrid(s);
    const cell = view.container.querySelectorAll(
      "[data-catchlight-binding-cell]",
    )[1] as HTMLInputElement;
    cell.value = "-4.5";
    await fire(cell, new Event("focusout", { bubbles: true }));

    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_key",
      node: s.node,
      target: "tx",
      param: s.param.id,
      cell: [1, 0],
      value: -4.5,
    });
    await view.unmount();
  });

  test("the add control binds the chosen target to the shown param", async () => {
    const s = await scene();
    const view = await mountGrid(s);

    // Nothing drives this node yet, so there is no grid — only the control.
    expect(view.container.querySelectorAll("[data-catchlight-binding]").length).toBe(0);

    const target = view.container.querySelector(
      "[data-catchlight-binding-target]",
    ) as HTMLSelectElement;
    target.value = "opacity";
    await fire(target, new Event("change", { bubbles: true }));
    const bind = view.container.querySelector(
      "[data-catchlight-binding-add-submit]",
    ) as HTMLButtonElement;
    await act(async () => {
      bind.click();
    });

    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_add",
      node: s.node,
      target: "opacity",
      param: s.param.id,
    });
    // And the binding it made is now a row.
    expect(
      view.container.querySelector("[data-catchlight-binding][data-target=opacity]"),
    ).not.toBeNull();
    await view.unmount();
  });

  test("each binding control sends its own command, and nothing else", async () => {
    const s = await scene();
    await run(() =>
      s.session.send({
        cmd: "binding_key",
        node: s.node,
        target: "tx",
        param: s.param.id,
        cell: [2, 0],
        value: 12,
      }),
    );
    const view = await mountGrid(s);
    const at = <T extends Element>(selector: string): T =>
      view.container.querySelector(selector) as T;
    const addressed = { node: s.node, target: "tx", param: s.param.id };

    const mode = at<HTMLSelectElement>("[data-catchlight-binding-interpolate]");
    expect(mode.value).toBe("linear");
    mode.value = "cubic";
    await fire(mode, new Event("change", { bubbles: true }));
    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_interpolate",
      ...addressed,
      mode: "cubic",
    });

    await act(async () => {
      at<HTMLButtonElement>("[data-catchlight-binding-invert]").click();
    });
    expect(last(s.wasm.requests)).toMatchObject({ cmd: "binding_invert", ...addressed });

    // The cell controls appear once a cell is picked, and address that cell.
    const cells = [...view.container.querySelectorAll("[data-catchlight-binding-cell]")];
    expect(at("[data-catchlight-binding-cell-actions]")).toBeNull();
    await act(async () => {
      (cells[0] as HTMLElement).click();
    });

    await act(async () => {
      at<HTMLButtonElement>("[data-catchlight-binding-reset]").click();
    });
    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_reset",
      ...addressed,
      cell: [0, 0],
    });

    await act(async () => {
      at<HTMLButtonElement>("[data-catchlight-binding-unset]").click();
    });
    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_unset",
      ...addressed,
      cell: [0, 0],
    });

    // A copy is armed on the selected cell and lands on the next one clicked.
    await act(async () => {
      at<HTMLButtonElement>("[data-catchlight-binding-copy]").click();
    });
    const armed = sent(s.wasm.requests).length;
    expect(at("[data-catchlight-binding-grid]")?.getAttribute("data-copying")).toBe("");
    expect(sent(s.wasm.requests).length).toBe(armed);
    await act(async () => {
      (view.container.querySelectorAll("[data-catchlight-binding-cell]")[2] as HTMLElement).click();
    });
    expect(last(s.wasm.requests)).toMatchObject({
      cmd: "binding_copy_key",
      ...addressed,
      from: [0, 0],
      to: [2, 0],
    });

    await act(async () => {
      at<HTMLButtonElement>("[data-catchlight-binding-delete]").click();
    });
    expect(last(s.wasm.requests)).toMatchObject({ cmd: "binding_delete", ...addressed });
    await view.unmount();
  });

  test("no node and no param draw nothing at all", async () => {
    const s = await scene();
    const view = await mount(
      <EditorProvider editor={s.editor}>
        <BindingGrid.Root session={s.session} node={undefined} param={s.param.id} />
      </EditorProvider>,
    );
    const root = view.container.querySelector("[data-catchlight-binding-grid]");
    expect(root?.getAttribute("data-empty")).toBe("");
    expect(root?.children.length).toBe(0);
    await view.unmount();
  });
});
