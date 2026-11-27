import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Session } from "@catchlight/core";
import type { FakeEditor } from "@catchlight/core/fakes";

import { EditorProvider, ExtensionsPanel } from "./index.js";
import { fire, harness, mount, run } from "./test/harness.js";

const ignore = () => {};

/** A plain vendor value, a maintained vendor value, and an official one. */
async function seeded(): Promise<{ session: Session; wasm: FakeEditor; render: () => ReturnType<typeof mount> }> {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  wasm.docs.get(session.id)!.extensions.push({
    key: "catchlight.humanoid",
    kind: "maintained",
    version: 1,
    refs: [{ param: "jaw" }],
    value: { controls: {} },
  });
  await run(() =>
    session.send({ cmd: "extension_set", key: "acme.notes", value: { kind: "json", value: { a: 1 } } }),
  );
  await run(() =>
    session.send({
      cmd: "extension_set",
      key: "acme.framing",
      value: {
        kind: "maintained",
        version: 2,
        refs: [{ param: "eye" }, { removed: { slot: ["head", "slot-1"] } }],
        value: { shot: 0 },
      },
    }),
  );
  const render = () =>
    mount(
      <EditorProvider editor={editor}>
        <ExtensionsPanel session={session} onError={ignore} />
      </EditorProvider>,
    );
  return { session, wasm, render };
}

function row(container: HTMLElement, key: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>("[data-catchlight-extension]")].find(
    (r) => r.textContent?.startsWith(key),
  );
  if (!found) throw new Error(`no row for ${key}`);
  return found;
}

function button(within: HTMLElement, text: string): HTMLButtonElement | undefined {
  return [...within.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

describe("the extensions panel", () => {
  test("lists every extension, with a maintained one's version and references", async () => {
    const { render } = await seeded();
    const view = await render();
    const rows = [...view.container.querySelectorAll("[data-catchlight-extension]")];
    const keys = ["acme.framing", "acme.notes", "catchlight.humanoid"];
    expect(rows).toHaveLength(keys.length);
    rows.forEach((r, i) => expect(r.textContent?.startsWith(keys[i]!)).toBe(true));
    expect(row(view.container, "acme.framing").textContent).toContain("Version 2, 2 references");
    expect(row(view.container, "acme.notes").textContent).toContain("JSON");
    await view.unmount();
  });

  test("an official extension can be viewed but neither edited nor removed", async () => {
    const { render } = await seeded();
    const view = await render();
    const official = row(view.container, "catchlight.humanoid");
    expect(button(official, "Edit")).toBeUndefined();
    expect(official.querySelector("[aria-label^='Remove']")).toBeNull();
    expect(row(view.container, "acme.framing").querySelector("[aria-label='Remove acme.framing']")).not.toBeNull();

    await run(() => button(official, "View")!.click());
    const dialog = view.container.querySelector("dialog") as HTMLElement;
    expect(dialog.querySelector("textarea")?.readOnly).toBe(true);
    expect(button(dialog, "Save extension")).toBeUndefined();
    expect(dialog.querySelector("ul[aria-label='References']")?.textContent).toBe("Param jaw");
    await view.unmount();
  });

  test("editing a maintained payload keeps its version and references", async () => {
    const { render, wasm } = await seeded();
    const view = await render();
    await run(() => button(row(view.container, "acme.framing"), "Edit")!.click());
    const dialog = view.container.querySelector("dialog") as HTMLElement;
    expect([...dialog.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      "Param eye",
      "Removed: Vertex slot slot-1 on head",
    ]);
    expect(dialog.textContent).toContain("Version 2");
    const key = dialog.querySelector("input") as HTMLInputElement;
    expect(key.readOnly).toBe(true);
    const payload = dialog.querySelector("textarea") as HTMLTextAreaElement;
    expect(JSON.parse(payload.value)).toEqual({ shot: 0 });

    await run(() => {
      // Through the prototype's setter, so React sees a change to report.
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(
        payload,
        '{"shot": 3}',
      );
      payload.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await fire(dialog.querySelector("form")!, new Event("submit", { bubbles: true, cancelable: true }));

    const sent = wasm.requests.at(-1);
    expect(sent).toMatchObject({
      cmd: "extension_set",
      key: "acme.framing",
      value: {
        kind: "maintained",
        version: 2,
        refs: [{ param: "eye" }, { removed: { slot: ["head", "slot-1"] } }],
        value: { shot: 3 },
      },
    });
    expect(view.container.querySelector("dialog")).toBeNull();
    await view.unmount();
  });

  test("a vendor extension is removed from its row", async () => {
    const { render, wasm } = await seeded();
    const view = await render();
    const remove = row(view.container, "acme.notes").querySelector<HTMLButtonElement>(
      "[aria-label='Remove acme.notes']",
    );
    await run(() => remove!.click());
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "extension_delete", key: "acme.notes" });
    expect(() => row(view.container, "acme.notes")).toThrow();
    await view.unmount();
  });
});
