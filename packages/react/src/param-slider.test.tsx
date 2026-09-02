/**
 * A slider poses the puppet. It must never author anything.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { ParamInfo, SessionDocumentCommand } from "@catchlight/core";

import { EditorProvider, ParamList, ParamSlider } from "./index.js";
import { fakeReplica, fire, harness, mount, run } from "./test/harness.js";

/** One continuous param, keyed at both ends. */
const eyeOpen = (name = "eye_open"): SessionDocumentCommand => ({
  cmd: "param_add",
  name,
  min: 0,
  max: 1,
  default: 1,
  key_positions: [0, 1],
});

describe("posing a param", () => {
  test("moving the slider poses the replica and sends no command", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(eyeOpen()));
    const param = session.params()[0] as ParamInfo;
    const commands = wasm.requests.length;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamSlider.Root session={session} param={param} />
      </EditorProvider>,
    );
    const slider = view.container.querySelector("input") as HTMLInputElement;
    expect(slider.type).toBe("range");
    expect(slider.getAttribute("data-catchlight-param-slider")).toBe("");
    // The default until something poses it.
    expect(slider.value).toBe("1");

    slider.value = "0.25";
    await fire(slider, new Event("input", { bubbles: true }));

    expect(session.paramValue(param.id)).toBe(0.25);
    expect(view.container.querySelector("input")?.value).toBe("0.25");
    // A pose is not an edit: the editor never heard about it.
    expect(wasm.requests.length).toBe(commands);
    await view.unmount();
  });

  test("a pose from elsewhere shows up without a revision", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(eyeOpen()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamSlider.Root session={session} param={param} />
      </EditorProvider>,
    );

    // An animation, an agent, another panel: the repaint channel is what
    // carries it, because the document did not move.
    await run(() => session.setParam(param.id, 0.5));

    expect(view.container.querySelector("input")?.value).toBe("0.5");
    await view.unmount();
  });

  test("the list is every param, and the render prop replaces the row", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(eyeOpen()));
    await run(() => session.send(eyeOpen("mouth_open")));

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamList.Root session={session} />
      </EditorProvider>,
    );
    expect(view.container.querySelectorAll("input[type=range]").length).toBe(2);
    expect(view.container.querySelector("[data-catchlight-param-list]")?.getAttribute("role")).toBe(
      "list",
    );

    await view.render(
      <EditorProvider editor={editor}>
        <ParamList.Root session={session}>{(param) => <b>{param.name}</b>}</ParamList.Root>
      </EditorProvider>,
    );
    expect(view.container.querySelectorAll("input").length).toBe(0);
    expect(view.container.textContent).toBe("eye_openmouth_open");
    await view.unmount();
  });

  test("the value is the replica's pose, not React state", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send(eyeOpen()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamSlider.Root session={session} param={param} />
      </EditorProvider>,
    );
    const slider = view.container.querySelector("input") as HTMLInputElement;
    slider.value = "0.75";
    await fire(slider, new Event("input", { bubbles: true }));

    expect(fakeReplica(session).pose.get(param.id)).toBe(0.75);
    await view.unmount();
  });
});
