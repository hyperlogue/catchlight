import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { FakeEditor } from "@catchlight/core/fakes";

import { MeshPanel } from "./index.js";
import { fire, harness, mount, run, settle } from "./test/harness.js";

stubLegacyEventApi();

describe("the mesh panel", () => {
  test("a contour margin above the editor's limit is not sent", async () => {
    const { wasm, view } = await meshing();
    const margin = input(view.container, "Mesh margin");

    await commit(margin, "129");
    await generate(view.container);
    expect(generated(wasm).at(-1)?.mode).toEqual({ mode: "contour", spacing: 32, margin: 4 });

    await commit(margin, "128");
    await generate(view.container);
    expect(generated(wasm).at(-1)?.mode).toEqual({ mode: "contour", spacing: 32, margin: 128 });
    await view.unmount();
  });

  test("a grid offers no pixel margin and sends none", async () => {
    const { wasm, view } = await meshing();
    await commit(input(view.container, "Mesh margin"), "8");

    const method = view.container.querySelector(
      'select[aria-label="Mesh method"]',
    ) as HTMLSelectElement;
    method.value = "grid";
    await fire(method, new Event("change", { bubbles: true }));
    await settle();

    expect(view.container.querySelector('input[aria-label="Mesh margin"]')).toBeNull();
    await generate(view.container);
    expect(generated(wasm).at(-1)?.mode).toEqual({ mode: "grid", cols: 6, rows: 8 });
    await view.unmount();
  });
});

async function meshing() {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  const added = await run(() =>
    session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }),
  );
  const node = added.result === "node" ? added.node : "";
  await run(() =>
    session.sendWith({ cmd: "texture_add", node, encoding: "png" }, [
      ["texture", new TextEncoder().encode("not really a png")],
    ]),
  );
  const info = session.nodeInfo(node);
  if (!info) throw new Error("the part has no info");
  const view = await mount(<MeshPanel session={session} info={info} onError={() => {}} />);
  await settle();
  return { wasm, view };
}

function input(container: HTMLElement, label: string): HTMLInputElement {
  return container.querySelector(`input[aria-label="${label}"]`) as HTMLInputElement;
}

function generated(wasm: FakeEditor) {
  return wasm.requests.filter((request) => request.cmd === "mesh_generate") as {
    mode?: unknown;
  }[];
}

async function generate(container: HTMLElement): Promise<void> {
  const button = [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.includes("Generate mesh"),
  );
  if (!button) throw new Error("no generate button");
  await run(() => button.click());
  await settle();
}

// Through the prototype's setter: React's instance setter would record the
// value and then fire no `onChange`.
async function commit(field: HTMLInputElement, text: string): Promise<void> {
  const assign = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  if (!assign) throw new Error("HTMLInputElement.value is not an accessor here");
  await fire(field, new FocusEvent("focusin", { bubbles: true }));
  assign.call(field, text);
  await fire(field, new Event("input", { bubbles: true }));
  await fire(field, new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  await fire(field, new FocusEvent("focusout", { bubbles: true }));
  await settle();
}

// IE methods react-dom's fallback path calls; happy-dom throws without them.
function stubLegacyEventApi(): void {
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  if (typeof proto["attachEvent"] === "function") return;
  proto["attachEvent"] = function attachEvent(): void {};
  proto["detachEvent"] = function detachEvent(): void {};
}
