/**
 * A translate drag end to end: preview while it moves, one command when it
 * ends, and the preview held until the revision lands.
 */

import "./test/setup.js";

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { ResponseBody, Session } from "@catchlight/core";

import { EditorProvider, useNodeDrag, Viewport } from "./index.js";
import type { ViewportPointerEvent } from "./index.js";
import { fakeReplica, fire, harness, mount, pointer, run, settle, stubLayout } from "./test/harness.js";

/** An 800x600 canvas at the top-left of the page, so world units are 1/300 of a pixel. */
let restore: () => void;
beforeEach(() => {
  restore = stubLayout(800, 600);
});
afterEach(() => restore());

function Dragger({ session, node }: { session: Session; node: string }) {
  const { handlers, dragging } = useNodeDrag(session, node);
  return <Viewport.Root session={session} data-dragging={dragging ? "" : undefined} {...handlers} />;
}

async function documentWithNode() {
  const { editor, wasm } = await harness();
  const session = await editor.newDocument();
  const body = await run(() =>
    session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }),
  );
  return { editor, wasm, session, node: nodeOf(body) };
}

function nodeOf(body: ResponseBody): string {
  return body.result === "node" ? body.node : "";
}

describe("dragging a node", () => {
  test("a move previews through the scratch, and only the end authors anything", async () => {
    const { editor, wasm, session, node } = await documentWithNode();
    const replica = fakeReplica(session);
    const commands = wasm.requests.length;

    const view = await mount(
      <EditorProvider editor={editor}>
        <Dragger session={session} node={node} />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    expect(canvas.getAttribute("data-dragging")).toBe("");

    // 30 pixels right at 1/300 world units per pixel. The replica answers in
    // f32, so the delta comes back rounded to a float's worth of precision.
    await fire(canvas, pointer("pointermove", { clientX: 430, clientY: 300 }));

    const scratch = replica.scratchTransforms.get(node) ?? [];
    expect(scratch[0]).toBeCloseTo(0.1, 6);
    expect(scratch[1]).toBeCloseTo(0, 6);
    // A gesture of any length is still nothing the editor has heard of.
    expect(wasm.requests.length).toBe(commands);

    await fire(canvas, pointer("pointerup", { clientX: 430, clientY: 300 }));
    await settle();

    const authored = wasm.requests.filter((request) => request.cmd === "node_set");
    expect(authored).toHaveLength(1);
    expect(authored[0]).toMatchObject({ node });
    const translate = (authored[0] as { translate: [number, number, number] }).translate;
    expect(translate[0]).toBeCloseTo(0.1, 6);
    expect(translate[1]).toBeCloseTo(0, 6);
    expect(replica.scratchTransforms.has(node)).toBe(false);
    expect(canvas.getAttribute("data-dragging")).toBeNull();
    await view.unmount();
  });

  test("the preview is held until the revision lands, not until the send is posted", async () => {
    const { editor, wasm, session, node } = await documentWithNode();
    const replica = fakeReplica(session);

    // Hold the command open: everything between posting it and the replica
    // being able to answer for it is the window this rule is about.
    let land = (): void => {};
    const landed = new Promise<void>((resolve) => {
      land = resolve;
    });
    const send = session.send.bind(session);
    session.send = async (command) => {
      await landed;
      return send(command);
    };

    const view = await mount(
      <EditorProvider editor={editor}>
        <Dragger session={session} node={node} />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointermove", { clientX: 430, clientY: 300 }));
    await fire(canvas, pointer("pointerup", { clientX: 430, clientY: 300 }));
    await settle();

    // Clearing here would put the node back where it started for as long as
    // the round trip takes, which reads as the drag snapping back.
    expect(replica.scratchTransforms.has(node)).toBe(true);
    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toHaveLength(0);

    land();
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toHaveLength(1);
    expect(replica.scratchTransforms.has(node)).toBe(false);
    await view.unmount();
  });

  test("a refused command clears the preview too", async () => {
    const { editor, wasm, session, node } = await documentWithNode();
    const replica = fakeReplica(session);
    wasm.refuse.set("node_set", { code: "bad_request", message: "no" });

    const view = await mount(
      <EditorProvider editor={editor}>
        <Dragger session={session} node={node} />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointermove", { clientX: 430, clientY: 300 }));
    await fire(canvas, pointer("pointerup", { clientX: 430, clientY: 300 }));
    await settle();

    // The document never moved, so the preview is a lie and has to go.
    expect(replica.scratchTransforms.has(node)).toBe(false);
    await view.unmount();
  });

  test("a cancelled gesture authors nothing and drops the preview", async () => {
    const { editor, wasm, session, node } = await documentWithNode();
    const replica = fakeReplica(session);

    const view = await mount(
      <EditorProvider editor={editor}>
        <Dragger session={session} node={node} />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointermove", { clientX: 430, clientY: 300 }));
    await fire(canvas, pointer("pointercancel", { clientX: 430, clientY: 300 }));
    await settle();

    expect(replica.scratchTransforms.has(node)).toBe(false);
    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toHaveLength(0);
    await view.unmount();
  });

  test("a press that never moved authors nothing", async () => {
    const { editor, wasm, session, node } = await documentWithNode();

    const view = await mount(
      <EditorProvider editor={editor}>
        <Dragger session={session} node={node} />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointerup", { clientX: 400, clientY: 300 }));
    await settle();

    // Otherwise every click on the canvas is an undo entry.
    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toHaveLength(0);
    await view.unmount();
  });

  test("without a node the handlers do nothing", async () => {
    const { editor, wasm, session } = await documentWithNode();
    const replica = fakeReplica(session);
    const commands = wasm.requests.length;

    function Idle({ handlers }: { handlers: Record<string, (e: ViewportPointerEvent) => void> }) {
      return <Viewport.Root session={session} {...handlers} />;
    }
    function Host() {
      const { handlers, dragging } = useNodeDrag(session, undefined);
      expect(dragging).toBe(false);
      return <Idle handlers={handlers} />;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Host />
      </EditorProvider>,
    );
    await settle();
    const canvas = view.container.querySelector("canvas") as HTMLCanvasElement;

    await fire(canvas, pointer("pointerdown", { clientX: 400, clientY: 300, button: 0 }));
    await fire(canvas, pointer("pointermove", { clientX: 430, clientY: 300 }));
    await fire(canvas, pointer("pointerup", { clientX: 430, clientY: 300 }));
    await settle();

    expect(replica.scratchTransforms.size).toBe(0);
    expect(wasm.requests.length).toBe(commands);
    await view.unmount();
  });
});
