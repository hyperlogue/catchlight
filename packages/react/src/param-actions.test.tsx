/**
 * The param half of the panel: what each control sends, and what the strip
 * deliberately does not.
 *
 * Every assertion here is about the *wire*. A param is metadata and its key
 * positions are part of the document, so adding, renaming, re-ranging or
 * keying one has to be a command the editor sees — while posing the puppet at
 * a key has to be none.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { ParamInfo, Request, SessionDocumentCommand } from "@catchlight/core";
import { act } from "react";

import { EditorProvider, ParamAdd, ParamFields, ParamKeys } from "./index.js";
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

/** Every command the editor was sent, newest last. */
function sent(requests: Request[]): Request[] {
  return requests.filter((request) => request.cmd !== "session_new");
}

function last(requests: Request[]): Request {
  const request = sent(requests).at(-1);
  if (!request) throw new Error("nothing was sent");
  return request;
}

describe("editing params", () => {
  test("the add form sends one param_add with the range it was given", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamAdd.Root session={session} />
      </EditorProvider>,
    );

    const field = (name: string): HTMLInputElement =>
      view.container.querySelector(`[data-catchlight-param-add-${name}]`) as HTMLInputElement;
    field("name").value = "head:yaw";
    await fire(field("name"), new Event("input", { bubbles: true }));
    field("min").value = "-2";
    await fire(field("min"), new Event("input", { bubbles: true }));
    field("max").value = "2";
    await fire(field("max"), new Event("input", { bubbles: true }));
    field("default").value = "0.5";
    await fire(field("default"), new Event("input", { bubbles: true }));

    const submit = view.container.querySelector(
      "[data-catchlight-param-add-submit]",
    ) as HTMLButtonElement;
    await act(async () => {
      submit.click();
    });

    expect(last(wasm.requests)).toMatchObject({
      cmd: "param_add",
      name: "head:yaw",
      min: -2,
      max: 2,
      default: 0.5,
      key_positions: [],
    });
    // The form empties, so the next param does not inherit this one's name.
    expect(field("name").value).toBe("");
    await view.unmount();
  });

  test("renaming, re-ranging and deleting each send one command", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(yaw()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamFields.Root session={session} param={param} />
      </EditorProvider>,
    );

    const name = view.container.querySelector(
      "[data-catchlight-param-rename]",
    ) as HTMLInputElement;
    expect(name.value).toBe("head:yaw");
    name.value = "head:turn";
    await fire(name, new Event("focusout", { bubbles: true }));
    // Absent and null mean the same thing to the editor: leave it alone.
    expect(last(wasm.requests)).toMatchObject({
      cmd: "param_set",
      param: param.id,
      name: "head:turn",
      min: null,
      max: null,
      default: null,
    });

    const max = view.container.querySelector(
      "[data-catchlight-param-field=max]",
    ) as HTMLInputElement;
    max.value = "3";
    await fire(max, new Event("focusout", { bubbles: true }));
    expect(last(wasm.requests)).toMatchObject({ cmd: "param_set", max: 3, name: null });

    // A field left at what the param already says changes nothing.
    const before = sent(wasm.requests).length;
    max.value = String(param.max);
    await fire(max, new Event("focusout", { bubbles: true }));
    expect(sent(wasm.requests).length).toBe(before);

    const remove = view.container.querySelector(
      "[data-catchlight-param-delete]",
    ) as HTMLButtonElement;
    await act(async () => {
      remove.click();
    });
    expect(last(wasm.requests)).toMatchObject({ cmd: "param_delete", param: param.id });
    await view.unmount();
  });
});

describe("the keypoint strip", () => {
  test("clicking a marker poses the param and sends nothing", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(yaw()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamKeys.Root session={session} param={param} />
      </EditorProvider>,
    );

    const markers = [...view.container.querySelectorAll("[data-catchlight-param-key]")];
    expect(markers.length).toBe(3);
    // Only the interior key can be moved or deleted; the endpoints are the
    // param's range.
    expect(markers.map((m) => m.getAttribute("data-interior"))).toEqual([null, "", null]);

    const commands = sent(wasm.requests).length;
    await act(async () => {
      (markers[2] as HTMLButtonElement).click();
    });

    // Key position 1 on a param that runs -1..1 is the top of the range.
    expect(session.paramValue(param.id)).toBe(1);
    expect(sent(wasm.requests).length).toBe(commands);
    await view.unmount();
  });

  test("insert keys at the pose, and delete the one it is standing on", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(yaw()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamKeys.Root session={session} param={param} />
      </EditorProvider>,
    );

    const insert = view.container.querySelector(
      "[data-catchlight-param-key-insert]",
    ) as HTMLButtonElement;
    const remove = view.container.querySelector(
      "[data-catchlight-param-key-delete]",
    ) as HTMLButtonElement;

    // The pose starts at the default, which is already a key: there is
    // nothing to insert there, and that key is the one delete would take.
    expect(insert.disabled).toBe(true);
    expect(remove.disabled).toBe(false);

    await run(() => session.setParam(param.id, 0.5));
    expect(insert.disabled).toBe(false);
    expect(remove.disabled).toBe(true);

    await act(async () => {
      insert.click();
    });
    // Normalized across [-1, 1], a pose of 0.5 is three quarters along.
    expect(last(wasm.requests)).toMatchObject({
      cmd: "param_key_insert",
      param: param.id,
      value: 0.75,
    });

    await run(() => session.setParam(param.id, 0));
    await act(async () => {
      remove.click();
    });
    expect(last(wasm.requests)).toMatchObject({
      cmd: "param_key_delete",
      param: param.id,
      index: 1,
    });
    await view.unmount();
  });

  test("dragging a marker is one param_key_move, on release", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(yaw()));
    const param = session.params()[0] as ParamInfo;

    const restore = stubTrack(200);
    try {
      const view = await mount(
        <EditorProvider editor={editor}>
          <ParamKeys.Root session={session} param={param} />
        </EditorProvider>,
      );
      const marker = view.container.querySelectorAll(
        "[data-catchlight-param-key]",
      )[1] as HTMLElement;
      const commands = sent(wasm.requests).length;

      await fire(marker, pointer("pointerdown", 100));
      await fire(marker, pointer("pointermove", 150));
      // Nothing is authored while the pointer is down: a drag of any length
      // is one revision and one undo entry.
      expect(sent(wasm.requests).length).toBe(commands);

      await fire(marker, pointer("pointerup", 150));
      expect(last(wasm.requests)).toMatchObject({
        cmd: "param_key_move",
        param: param.id,
        index: 1,
        value: 0.75,
      });
      await view.unmount();
    } finally {
      restore();
    }
  });
});

/** A pointer event at `x` across the page, with the capture calls stubbed. */
function pointer(type: string, x: number): Event {
  const event = new Event(type, { bubbles: true }) as Event & {
    clientX: number;
    pointerId: number;
  };
  Object.defineProperty(event, "clientX", { value: x });
  Object.defineProperty(event, "pointerId", { value: 1 });
  return event;
}

/**
 * A 200px-wide strip starting at the left edge, plus the pointer-capture calls
 * happy-dom does not implement. Returns a restore function.
 */
function stubTrack(width: number): () => void {
  const rect = Element.prototype.getBoundingClientRect;
  const capture = Element.prototype.setPointerCapture;
  const release = Element.prototype.releasePointerCapture;
  const has = Element.prototype.hasPointerCapture;
  Element.prototype.getBoundingClientRect = function box(): DOMRect {
    return {
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      width,
      height: 12,
      right: width,
      bottom: 12,
      toJSON: () => ({}),
    } as DOMRect;
  };
  Element.prototype.setPointerCapture = function noop(): void {};
  Element.prototype.releasePointerCapture = function noop(): void {};
  Element.prototype.hasPointerCapture = function no(): boolean {
    return false;
  };
  return () => {
    Element.prototype.getBoundingClientRect = rect;
    Element.prototype.setPointerCapture = capture;
    Element.prototype.releasePointerCapture = release;
    Element.prototype.hasPointerCapture = has;
  };
}
