import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { ParamInfo, SessionEditCommand } from "@catchlight/core";

import { EditorProvider, ParamList, ParamSlider } from "./index.js";
import { fire, harness, mount, run } from "./test/harness.js";

const eyeOpen = (name = "eye_open"): SessionEditCommand => ({
  cmd: "param_add",
  name,
  min: 0,
  max: 1,
  default: 1,

});

describe("posing a param", () => {
  test("moving the slider poses the replica and sends no command", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newSession();
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
    expect(slider.value).toBe("1");

    slider.value = "0.25";
    await fire(slider, new Event("input", { bubbles: true }));

    expect(session.paramValue(param.id)).toBe(0.25);
    expect(view.container.querySelector("input")?.value).toBe("0.25");
    expect(wasm.requests.length).toBe(commands);
    await view.unmount();
  });

  test("a pose from elsewhere shows up without a revision", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
    await run(() => session.send(eyeOpen()));
    const param = session.params()[0] as ParamInfo;

    const view = await mount(
      <EditorProvider editor={editor}>
        <ParamSlider.Root session={session} param={param} />
      </EditorProvider>,
    );

    await run(() => session.setParam(param.id, 0.5));

    expect(view.container.querySelector("input")?.value).toBe("0.5");
    await view.unmount();
  });

  test("the list is every param, and the render prop replaces the row", async () => {
    const { editor } = await harness();
    const session = await editor.newSession();
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
    const rows = [...view.container.querySelectorAll("[role=listitem] > b")];
    expect(rows.map((row) => row.textContent)).toEqual(["eye_open", "mouth_open"]);
    await view.unmount();
  });
});
