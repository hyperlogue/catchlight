/**
 * The inspector authors one field at a time, and only when something changed.
 *
 * What can go wrong here is not arithmetic, it is over-sending: a form that
 * puts every box it renders into one command authors a dozen values nobody
 * touched and bundles them into one undo entry. So every assertion is about
 * which commands went out and what keys they carried.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { ResponseBody, Session } from "@catchlight/core";
import type { FakeEditor } from "@catchlight/core/fakes";
import { useEffect } from "react";

import { Inspector, SelectionProvider, useSelection } from "./index.js";
import { fire, harness, mount, run, settle } from "./test/harness.js";

stubLegacyEventApi();

describe("the inspector", () => {
  test("with nothing selected it is the empty state", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();

    const view = await mount(<Panel session={session} node={undefined} />);
    await settle();

    expect(view.container.querySelector("[data-catchlight-inspector-empty]")).not.toBeNull();
    expect(view.container.querySelectorAll("input")).toHaveLength(0);
    await view.unmount();
  });

  test("a selection that outlived its node is the empty state too", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();

    const view = await mount(<Panel session={session} node="root/gone-9" />);
    await settle();

    expect(view.container.querySelector("[data-catchlight-inspector-empty]")).not.toBeNull();
    await view.unmount();
  });

  test("a translate edit sends node_set with only translate", async () => {
    const { wasm, session, node, view } = await inspecting("part");

    const x = field(view.container, "translate", "x");
    // The node is at the rest pose, so the box shows the model's own number.
    expect(x.value).toBe("0");

    await focus(x);
    await type(x, "12");
    await press(x, "Enter");
    await settle();

    const sent = nodeSets(wasm);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ cmd: "node_set", node, translate: [12, 0, 0] });
    // Every other box the panel rendered stayed out of it.
    expect(keys(sent[0])).toEqual(["cmd", "id", "node", "session", "translate"]);
    await view.unmount();
  });

  test("a blend select sends only blend_mode, on the change itself", async () => {
    const { wasm, session, node, view } = await inspecting("part");

    const blend = view.container.querySelector(
      'select[data-catchlight-field="blend_mode"]',
    ) as HTMLSelectElement;
    expect(blend.value).toBe("Normal");

    blend.value = "Multiply";
    await fire(blend, new Event("change", { bubbles: true }));
    await settle();

    const sent = nodeSets(wasm);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ cmd: "node_set", node, blend_mode: "Multiply" });
    expect(keys(sent[0])).toEqual(["blend_mode", "cmd", "id", "node", "session"]);
    await view.unmount();
  });

  test("the texture select points at one, and clears with its own field", async () => {
    const { wasm, session, node, view } = await inspecting("part");
    // Something to clear: a part drawing nothing is already at "none", and a
    // select that did not move authors nothing either way.
    wasm.staged.set("hair.png", new TextEncoder().encode("not really a png"));
    await run(() => session.send({ cmd: "texture_add", node, path: "hair.png" }));
    await settle();

    const texture = view.container.querySelector(
      'select[data-catchlight-field="texture"]',
    ) as HTMLSelectElement;
    expect(texture.value).toBe("tex-1");
    // The "none" option is an edit now, not a readout.
    expect([...texture.options].map((option) => option.value)).toEqual(["", "tex-1"]);

    texture.value = "";
    await fire(texture, new Event("change", { bubbles: true }));
    await settle();

    const cleared = nodeSets(wasm);
    expect(cleared).toHaveLength(1);
    // `texture` has no spelling for "point at nothing", so the empty value
    // must not travel under it — `clear_texture` is the whole command.
    expect(cleared[0]).toMatchObject({ cmd: "node_set", node, clear_texture: true });
    expect(keys(cleared[0])).toEqual(["clear_texture", "cmd", "id", "node", "session"]);

    // And picking a texture again is still the ordinary field.
    texture.value = "tex-1";
    await fire(texture, new Event("change", { bubbles: true }));
    await settle();

    const pointed = nodeSets(wasm)[1];
    expect(pointed).toMatchObject({ cmd: "node_set", node, texture: "tex-1" });
    expect(keys(pointed)).toEqual(["cmd", "id", "node", "session", "texture"]);
    await view.unmount();
  });

  test("a checkbox commits on the click", async () => {
    const { wasm, session, node, view } = await inspecting("part");

    const enabled = view.container.querySelector(
      '[data-catchlight-field="enabled"]',
    ) as HTMLInputElement;
    expect(enabled.checked).toBe(true);
    await run(() => enabled.click());
    await settle();

    expect(nodeSets(wasm)).toHaveLength(1);
    expect(nodeSets(wasm)[0]).toMatchObject({ cmd: "node_set", node, enabled: false });
    await view.unmount();
  });

  test("a group renders no colour controls, and a part does", async () => {
    const group = await inspecting("group");
    expect(field(group.view.container, "translate", "x")).toBeDefined();
    for (const absent of ["opacity", "blend_mode", "tint", "screen_tint", "mask_threshold"]) {
      expect(group.view.container.querySelector(`[data-catchlight-field="${absent}"]`)).toBeNull();
    }
    // `texture` is a part's, and the row is decided by the kind rather than by
    // whether this one happens to draw anything.
    expect(group.view.container.querySelector('[data-catchlight-field="texture"]')).toBeNull();
    await group.view.unmount();

    const part = await inspecting("part");
    for (const present of ["opacity", "blend_mode", "tint", "screen_tint", "mask_threshold"]) {
      expect(part.view.container.querySelector(`[data-catchlight-field="${present}"]`)).not.toBeNull();
    }
    // Three boxes over one key, one per component.
    expect(part.view.container.querySelectorAll('[data-catchlight-field="tint"]')).toHaveLength(3);
    expect(part.view.container.querySelector('[data-catchlight-field="texture"]')).not.toBeNull();
    await part.view.unmount();
  });

  test("a mesh group renders its own two, and nothing else's", async () => {
    const { view } = await inspecting("mesh_group");
    expect(view.container.querySelector('[data-catchlight-field="mg_dynamic"]')).not.toBeNull();
    expect(
      view.container.querySelector('[data-catchlight-field="mg_translate_children"]'),
    ).not.toBeNull();
    expect(view.container.querySelector('[data-catchlight-field="opacity"]')).toBeNull();
    expect(view.container.querySelector('[data-catchlight-field="propagate_meshgroup"]')).toBeNull();
    await view.unmount();
  });

  test("typing then Escape authors nothing, and neither does the blur after it", async () => {
    const { wasm, session, view } = await inspecting("part");

    const x = field(view.container, "translate", "x");
    await focus(x);
    await type(x, "42");
    expect(x.value).toBe("42");

    await press(x, "Escape");
    // The box is back to what the model says, so there is nothing left to send.
    expect(x.value).toBe("0");
    await blur(x);
    await settle();

    expect(nodeSets(wasm)).toHaveLength(0);
    await view.unmount();
  });

  test("a blur that changed nothing authors nothing", async () => {
    const { wasm, session, view } = await inspecting("part");

    const name = view.container.querySelector(
      '[data-catchlight-field="name"]',
    ) as HTMLInputElement;
    expect(name.value).toBe("hair");

    // Tabbed into and straight back out of.
    await blur(name);
    // And re-typed the same thing, which is the same non-edit.
    await type(name, "hair");
    await blur(name);
    await settle();

    expect(nodeSets(wasm)).toHaveLength(0);
    await view.unmount();
  });

  test("a revision landing mid-edit leaves the box being typed in alone", async () => {
    const { wasm, session, view } = await inspecting("part");

    const x = field(view.container, "translate", "x");
    await type(x, "7");

    // Something else moved the document: another panel, an agent on the socket.
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "group", name: "hat" }));
    await settle();

    expect(field(view.container, "translate", "x").value).toBe("7");
    expect(nodeSets(wasm)).toHaveLength(0);
    await view.unmount();
  });
});

/** A panel over one session, selecting `node` as soon as it is mounted. */
function Panel({ session, node }: { session: Session; node: string | undefined }) {
  return (
    <SelectionProvider session={session}>
      <Pick node={node} />
      <Inspector.Root session={session} />
    </SelectionProvider>
  );
}

function Pick({ node }: { node: string | undefined }) {
  const { node: current, select } = useSelection();
  useEffect(() => {
    if (node !== current) select(node);
  }, [node, current, select]);
  return null;
}

/** A document holding one node of `kind`, with the panel mounted on it. */
async function inspecting(kind: "group" | "part" | "composite" | "mesh_group") {
  const { editor, wasm } = await harness();
  const session = await editor.newDocument();
  const body = await run(() =>
    session.send({ cmd: "node_add", parent: "root", kind, name: "hair" }),
  );
  const node = named(body);
  const view = await mount(<Panel session={session} node={node} />);
  await settle();
  return { editor, wasm, session, node, view };
}

function named(body: ResponseBody): string {
  return body.result === "node" ? body.node : "";
}

/** One component of a vector field: the same key, one box per axis. */
function field(container: HTMLElement, name: string, axis: string): HTMLInputElement {
  return container.querySelector(
    `[data-catchlight-field="${name}"][data-axis="${axis}"]`,
  ) as HTMLInputElement;
}

/** Every `node_set` the editor was sent, in order. */
function nodeSets(wasm: FakeEditor) {
  return wasm.requests.filter((request) => request.cmd === "node_set");
}

/** The keys one command carried, sorted, so "only this field" is testable. */
function keys(command: object | undefined): string[] {
  return Object.keys(command ?? {}).sort();
}

/**
 * Types `text` into a controlled box, the way a person replacing it would.
 *
 * Through the prototype's setter rather than `input.value =`, which React has
 * replaced on the instance with one that also updates its own record of what
 * the box holds — assign through that and React concludes nothing changed and
 * fires no `onChange` at all.
 */
async function type(input: HTMLInputElement, text: string): Promise<void> {
  const assign = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  if (!assign) throw new Error("HTMLInputElement.value is not an accessor here");
  assign.call(input, text);
  await fire(input, new Event("input", { bubbles: true }));
}

/**
 * React's `onFocus` is the native `focusin`, and a box has to be focused
 * before it can be typed into — here, unusually, that matters: react-dom
 * decides at load time whether the browser has `input` events, decides "no"
 * against happy-dom, and falls back to its pre-`input` path. That path tracks
 * the *focused* element, and a keydown with nothing focused takes its whole
 * dispatch down. Focusing first is what a person does anyway.
 */
async function focus(input: HTMLInputElement): Promise<void> {
  await fire(input, new FocusEvent("focusin", { bubbles: true }));
}

async function press(input: HTMLInputElement, key: string): Promise<void> {
  await fire(input, new KeyboardEvent("keydown", { key, bubbles: true }));
}

/** React's `onBlur` is the native `focusout`, which is the one that bubbles. */
async function blur(input: HTMLInputElement): Promise<void> {
  await fire(input, new FocusEvent("focusout", { bubbles: true }));
}

/**
 * The two methods react-dom's fallback path calls on the element it is
 * watching. They are Internet Explorer's, so happy-dom has neither, and
 * focusing a text box throws without them.
 */
function stubLegacyEventApi(): void {
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  if (typeof proto["attachEvent"] === "function") return;
  proto["attachEvent"] = function attachEvent(): void {};
  proto["detachEvent"] = function detachEvent(): void {};
}
