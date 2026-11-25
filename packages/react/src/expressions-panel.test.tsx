import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { FakeDoc } from "@catchlight/core/fakes";

import { EditorProvider, ExpressionsPanel } from "./index.js";
import { enter, fakeReplica, fire, harness, mount, run } from "./test/harness.js";

async function seeded(setup?: (doc: FakeDoc) => void) {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  const errors: unknown[] = [];
  await run(() => session.send({ cmd: "param_add", name: "Jaw", min: 0, max: 1, default: 0 }));
  await run(() => session.send({ cmd: "param_add", name: "Brow", min: -1, max: 1, default: 0 }));
  if (setup) {
    setup(wasm.docs.get(session.id)!);
    await run(() => session.send({ cmd: "param_add", name: "Spare", min: 0, max: 1, default: 0 }));
  }
  const view = await mount(
    <EditorProvider editor={editor}>
      <ExpressionsPanel session={session} onError={(e) => errors.push(e)} />
    </EditorProvider>,
  );
  return { session, wasm, view, errors };
}

const happy = { name: "happy", settings: [{ control: "mouth_smile", value: 1 }, { param: "param-1", value: 0.4 }] };

function row(container: HTMLElement, name: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`[data-catchlight-expression-row][data-name="${name}"]`);
  if (!found) throw new Error(`no row for ${name}`);
  return found;
}

function button(within: Element, text: string): HTMLButtonElement {
  const found = [...within.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text);
  if (!found) throw new Error(`no ${text} button`);
  return found;
}

function dialog(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>("dialog");
  if (!found) throw new Error("no dialog");
  return found;
}

async function slide(input: HTMLInputElement, value: number): Promise<void> {
  input.value = String(value);
  await fire(input, new Event("input", { bubbles: true }));
}

async function choose(select: HTMLSelectElement, value: string): Promise<void> {
  select.value = value;
  await fire(select, new Event("change", { bubbles: true }));
}

async function submit(form: HTMLFormElement): Promise<void> {
  await fire(form, new Event("submit", { bubbles: true, cancelable: true }));
}

describe("the expressions panel", () => {
  test("a model without expressions says so in both lists", async () => {
    const { view } = await seeded();
    expect(view.container.querySelectorAll("[data-catchlight-expression-row]")).toHaveLength(0);
    const groups = [...view.container.querySelectorAll("[data-catchlight-expression-group]")];
    expect(groups.map((g) => g.querySelector("h3")?.textContent)).toEqual(["Expressions", "Visemes"]);
    expect(groups.every((g) => g.querySelector("[data-catchlight-hint]")?.textContent?.startsWith("No "))).toBe(true);
    await view.unmount();
  });

  test("lists each member with its settings count, and its slider previews a weight cleared at 0", async () => {
    const { session, view } = await seeded((doc) => {
      doc.expressions = [happy];
      doc.visemes = [{ name: "aa", settings: [{ control: "mouth_open", value: 1 }] }];
    });
    expect(row(view.container, "happy").querySelector("[data-catchlight-expression-count]")?.textContent).toBe("2 settings");
    expect(row(view.container, "aa").querySelector("[data-catchlight-expression-count]")?.textContent).toBe("1 setting");

    await slide(row(view.container, "happy").querySelector<HTMLInputElement>("[aria-label='happy weight']")!, 0.6);
    await slide(row(view.container, "aa").querySelector<HTMLInputElement>("[aria-label='aa weight']")!, 1);
    expect(fakeReplica(session).preview).toEqual({ controls: {}, expressions: { happy: 0.6 }, visemes: { aa: 1 } });
    expect(row(view.container, "happy").hasAttribute("data-previewing")).toBe(true);

    await slide(row(view.container, "happy").querySelector<HTMLInputElement>("[aria-label='happy weight']")!, 0);
    expect(fakeReplica(session).preview.expressions).toEqual({});
    expect(row(view.container, "happy").hasAttribute("data-previewing")).toBe(false);
    await view.unmount();
  });

  test("delete removes the member and its preview weight", async () => {
    const { session, wasm, view } = await seeded((doc) => {
      doc.expressions = [happy];
    });
    await slide(row(view.container, "happy").querySelector<HTMLInputElement>("input[type=range]")!, 0.5);
    await run(() => row(view.container, "happy").querySelector<HTMLButtonElement>("[aria-label='Delete happy']")!.click());
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "expression_delete", group: "expression", name: "happy" });
    expect(view.container.querySelector("[data-catchlight-expression-row]")).toBeNull();
    expect(fakeReplica(session).preview.expressions).toEqual({});
    await view.unmount();
  });

  test("Add offers the standard names the model lacks, accepts any other, and refuses a taken one", async () => {
    const { view } = await seeded((doc) => {
      doc.expressions = [happy];
    });
    await run(() => button(view.container, "Add expression").click());
    const naming = dialog(view.container);
    const offered = [...naming.querySelectorAll("[data-catchlight-name-choices] button")].map((b) => b.textContent);
    expect(offered).toEqual(["angry", "sad", "relaxed", "surprised"]);

    const name = naming.querySelector<HTMLInputElement>("label input")!;
    await enter(name, "happy");
    expect(naming.querySelector("[role=alert]")?.textContent).toContain("already an expression named “happy”");
    expect(button(naming, "Continue").disabled).toBe(true);

    await enter(name, "smug");
    expect(button(naming, "Continue").disabled).toBe(false);
    await submit(naming.querySelector("form")!);
    expect(dialog(view.container).querySelector("h2")?.textContent).toBe("New expression “smug”");
    await view.unmount();
  });

  test("a new viseme is authored from settings rows and saved whole", async () => {
    const { session, wasm, view } = await seeded();
    await run(() => button(view.container, "Add viseme").click());
    await run(() => button(dialog(view.container), "ee").click());
    await submit(dialog(view.container).querySelector("form")!);

    const editor = dialog(view.container);
    expect(editor.querySelector("h2")?.textContent).toBe("New viseme “ee”");
    expect(button(editor, "Save viseme").disabled).toBe(true);
    expect(editor.textContent).toContain("Add at least one setting");

    await run(() => button(editor, "Add setting").click());
    await choose(editor.querySelector<HTMLSelectElement>("[aria-label='Setting 1 control']")!, "mouth_open");
    const value = editor.querySelector<HTMLInputElement>("[aria-label='Setting 1 value']")!;
    // A one-sided control's field stops at its range.
    expect([value.min, value.max]).toEqual(["0", "1"]);
    await enter(value, "0.7");

    await run(() => button(editor, "Add setting").click());
    await choose(editor.querySelector<HTMLSelectElement>("[aria-label='Setting 2 target']")!, "param");
    await choose(editor.querySelector<HTMLSelectElement>("[aria-label='Setting 2 param']")!, "param-2");
    await enter(editor.querySelector<HTMLInputElement>("[aria-label='Setting 2 value']")!, "-0.5");

    await run(() => button(editor, "Add setting").click());
    await run(() => editor.querySelector<HTMLButtonElement>("[aria-label='Remove setting 3']")!.click());

    await submit(editor.querySelector("form")!);
    expect(wasm.requests.at(-1)).toEqual({
      id: expect.any(Number),
      session: session.id,
      cmd: "expression_set",
      group: "viseme",
      name: "ee",
      settings: [
        { control: "mouth_open", value: 0.7 },
        { param: "param-2", value: -0.5 },
      ],
      nonstandard: false,
    });
    expect(view.container.querySelector("dialog")).toBeNull();
    expect(row(view.container, "ee").getAttribute("data-group")).toBe("viseme");
    await view.unmount();
  });

  test("editing changes an existing member, and Cancel discards the draft", async () => {
    const { wasm, view } = await seeded((doc) => {
      doc.expressions = [happy];
    });
    await run(() => row(view.container, "happy").querySelector<HTMLButtonElement>("[aria-label='Edit happy']")!.click());
    let editor = dialog(view.container);
    expect(editor.querySelector("h2")?.textContent).toBe("Edit expression “happy”");
    expect(editor.querySelectorAll("[data-catchlight-expression-setting]")).toHaveLength(2);
    await run(() => editor.querySelector<HTMLButtonElement>("[aria-label='Remove setting 2']")!.click());
    const sent = wasm.requests.length;
    await run(() => button(editor, "Cancel").click());
    expect(view.container.querySelector("dialog")).toBeNull();
    expect(wasm.requests).toHaveLength(sent);

    await run(() => row(view.container, "happy").querySelector<HTMLButtonElement>("[aria-label='Edit happy']")!.click());
    editor = dialog(view.container);
    expect(editor.querySelectorAll("[data-catchlight-expression-setting]")).toHaveLength(2);
    await enter(editor.querySelector<HTMLInputElement>("[aria-label='Setting 1 value']")!, "-0.25");
    await submit(editor.querySelector("form")!);
    expect(wasm.requests.at(-1)).toMatchObject({
      cmd: "expression_set",
      group: "expression",
      name: "happy",
      settings: [
        { control: "mouth_smile", value: -0.25 },
        { param: "param-1", value: 0.4 },
      ],
    });
    expect(row(view.container, "happy").querySelector("[data-catchlight-expression-count]")?.textContent).toBe("2 settings");
    await view.unmount();
  });

  test("saving a control outside the standard asks first, then sends it as nonstandard", async () => {
    const { wasm, view } = await seeded((doc) => {
      doc.expressions = [{ name: "wag", settings: [{ control: "tail_wag", value: 1 }] }];
    });
    await run(() => row(view.container, "wag").querySelector<HTMLButtonElement>("[aria-label='Edit wag']")!.click());
    const editor = dialog(view.container);
    expect(editor.querySelector<HTMLSelectElement>("[aria-label='Setting 1 control']")!.value).toBe("tail_wag");
    await submit(editor.querySelector("form")!);
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "expression_set", nonstandard: false });
    const asking = [...view.container.querySelectorAll("dialog")].at(-1)!;
    expect(asking.textContent).toContain("\"tail_wag\" is not a standard humanoid control");

    await run(() => button(asking, "Keep the name").click());
    expect(wasm.requests.at(-1)).toMatchObject({
      cmd: "expression_set",
      name: "wag",
      settings: [{ control: "tail_wag", value: 1 }],
      nonstandard: true,
    });
    expect(view.container.querySelector("dialog")).toBeNull();
    await view.unmount();
  });

  test("adding the standard visemes to a model without visemes asks nothing and reports", async () => {
    const { wasm, view } = await seeded();
    await run(() => button(view.container, "Add standard visemes").click());
    expect(view.container.querySelector("dialog")).toBeNull();
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "viseme_add_standard", replace: false });
    expect(view.container.querySelector("[data-catchlight-viseme-report]")?.textContent).toBe(
      "Standard visemes: 3 added, 0 replaced, 0 skipped.",
    );
    expect(view.container.querySelectorAll("[data-catchlight-expression-row][data-group=viseme]")).toHaveLength(3);
    await view.unmount();
  });

  test("with visemes already there, keep or replace is asked", async () => {
    const { wasm, view } = await seeded((doc) => {
      doc.visemes = [{ name: "aa", settings: [{ control: "mouth_open", value: 1 }] }];
    });
    await run(() => button(view.container, "Add standard visemes").click());
    expect(dialog(view.container).textContent).toContain("already has 1 viseme");
    await run(() => button(dialog(view.container), "Keep mine").click());
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "viseme_add_standard", replace: false });
    expect(view.container.querySelector("[data-catchlight-viseme-report]")?.textContent).toBe(
      "Standard visemes: 2 added, 0 replaced, 1 skipped.",
    );

    await run(() => button(view.container, "Add standard visemes").click());
    await run(() => button(dialog(view.container), "Replace them").click());
    expect(wasm.requests.at(-1)).toMatchObject({ cmd: "viseme_add_standard", replace: true });
    expect(view.container.querySelector("[data-catchlight-viseme-report]")?.textContent).toBe(
      "Standard visemes: 0 added, 3 replaced, 0 skipped.",
    );

    const sent = wasm.requests.length;
    await run(() => button(view.container, "Add standard visemes").click());
    await run(() => button(dialog(view.container), "Cancel").click());
    expect(wasm.requests).toHaveLength(sent);
    await view.unmount();
  });

  test("shows the expressions warnings and marks the member whose param they concern", async () => {
    const { view } = await seeded((doc) => {
      doc.expressions = [happy, { name: "sad", settings: [{ control: "mouth_smile", value: -1 }] }];
      doc.extensionWarnings = [
        {
          key: "catchlight.expressions",
          code: "catchlight/dangling-reference",
          message: "expression \"happy\" sets a removed param",
          subjects: [{ removed: { param: "param-1" } }],
        },
        { key: "catchlight.humanoid", code: "x", message: "not ours", subjects: [{ param: "param-1" }] },
      ];
    });
    const shown = [...view.container.querySelectorAll("[data-catchlight-extension-warnings] [data-catchlight-warning]")];
    expect(shown.map((w) => w.textContent)).toEqual(["Expression \"happy\" sets a removed param"]);
    expect(row(view.container, "happy").hasAttribute("data-warned")).toBe(true);
    expect(row(view.container, "sad").hasAttribute("data-warned")).toBe(false);
    await view.unmount();
  });

  test("expressions data this build cannot read is reported, with nothing to edit", async () => {
    const { view } = await seeded((doc) => {
      doc.expressions = [happy];
      doc.unread = ["catchlight.expressions"];
    });
    expect(view.container.querySelector("[data-catchlight-extension-unread]")?.textContent).toContain(
      "cannot read the model’s expressions and visemes",
    );
    expect(view.container.querySelector("button, input, select")).toBeNull();
    await view.unmount();
  });
});
