/**
 * The tree panel: real tree markup, and a selection other clients can see.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import { act } from "react";

import { EditorProvider, NodeTree, SelectionProvider, useSelection } from "./index.js";
import { fire, harness, mount, run, settle, stubLayout } from "./test/harness.js";

describe("the node tree", () => {
  test("renders the document's tree, nested", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );

    const items = [...view.container.querySelectorAll("[data-catchlight-node]")];
    expect(items.map((item) => item.getAttribute("data-node"))).toEqual([
      "root",
      "root/part-1",
    ]);
    expect(items.map((item) => item.getAttribute("role"))).toEqual(["treeitem", "treeitem"]);
    expect(items[1]?.getAttribute("data-kind")).toBe("part");
    // The child is inside its parent's group, not a sibling of it.
    expect(items[0]?.querySelector("ul[role=group]")?.contains(items[1] ?? null)).toBe(true);
    await view.unmount();
  });

  test("clicking a node selects it and publishes the selection", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    let selected: string | undefined = "unset";
    function Watch() {
      selected = useSelection().node;
      return null;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
          <Watch />
        </SelectionProvider>
      </EditorProvider>,
    );
    expect(selected).toBeUndefined();

    const button = view.container.querySelector(
      "[data-node='root/part-1'] button",
    ) as HTMLButtonElement;
    await act(async () => {
      button.click();
    });

    expect(selected).toBe("root/part-1");
    const item = view.container.querySelector("[data-node='root/part-1']");
    expect(item?.getAttribute("data-selected")).toBe("");
    expect(item?.getAttribute("aria-selected")).toBe("true");
    // The root is not selected, so it carries no attribute at all.
    expect(view.container.querySelector("[data-node='root']")?.hasAttribute("data-selected")).toBe(
      false,
    );

    // An agent on the socket reads what this person is looking at from
    // presence, so the click has to reach the editor.
    const presence = wasm.requests.filter((request) => request.cmd === "presence_set");
    expect(presence).toHaveLength(1);
    expect(presence[0]).toMatchObject({ selection: "root/part-1", pose: [] });
    await view.unmount();
  });

  test("a node added by anyone appears without the panel being told", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );
    expect(view.container.querySelectorAll("[data-catchlight-node]").length).toBe(1);

    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    expect(view.container.querySelectorAll("[data-catchlight-node]").length).toBe(2);
    await view.unmount();
  });
});

describe("editing a row", () => {
  test("the checkbox authors the node's enabled flag", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );

    const box = view.container.querySelector(
      "[data-node='root/part-1'] [data-catchlight-node-enabled]",
    ) as HTMLInputElement;
    expect(box.checked).toBe(true);
    await act(async () => {
      box.click();
    });
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toEqual([
      expect.objectContaining({ cmd: "node_set", node: "root/part-1", enabled: false }),
    ]);
    await view.unmount();
  });

  test("a double-clicked label renames on Enter", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );

    const row = "[data-node='root/part-1']";
    const label = view.container.querySelector(`${row} [data-catchlight-node-label]`) as HTMLElement;
    await fire(label, new MouseEvent("dblclick", { bubbles: true }));

    const input = view.container.querySelector(
      `${row} [data-catchlight-node-rename]`,
    ) as HTMLInputElement;
    expect(input.value).toBe("hair");
    input.value = "brow";
    await fire(input, new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toEqual([
      expect.objectContaining({ cmd: "node_set", node: "root/part-1", name: "brow" }),
    ]);
    // The field is gone, and the label is back.
    expect(view.container.querySelector(`${row} [data-catchlight-node-rename]`)).toBeNull();
    await view.unmount();
  });

  test("Escape closes the field and authors nothing", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );

    const row = "[data-node='root/part-1']";
    const label = view.container.querySelector(`${row} [data-catchlight-node-label]`) as HTMLElement;
    await fire(label, new MouseEvent("dblclick", { bubbles: true }));
    const input = view.container.querySelector(
      `${row} [data-catchlight-node-rename]`,
    ) as HTMLInputElement;
    input.value = "brow";
    await fire(input, new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_set")).toEqual([]);
    expect(view.container.querySelector(`${row} [data-catchlight-node-rename]`)).toBeNull();
    expect(view.container.querySelector(`${row} [data-catchlight-node-label]`)?.textContent).toBe(
      "hair",
    );
    await view.unmount();
  });
});

describe("dragging a row onto another", () => {
  /** A drag event with the one measurement the placement reads. */
  function dragEvent(type: string, clientY: number): Event {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clientY", { value: clientY });
    return event;
  }

  /** Two parts under the root, and the panel showing them. */
  async function twoParts() {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "brow" }));
    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Root session={session} />
        </SelectionProvider>
      </EditorProvider>,
    );
    const row = (node: string): HTMLElement =>
      view.container.querySelector(`[data-node='${node}'] [data-catchlight-node-row]`) as HTMLElement;
    const item = (node: string): HTMLElement =>
      view.container.querySelector(`[data-node='${node}']`) as HTMLElement;
    return { wasm, view, row, item };
  }

  test("a drop in the middle of a row moves the node into it", async () => {
    // Every row is 20px tall at the top of the page, so a Y says which band.
    const restore = stubLayout(200, 20, 0, 0);
    const { wasm, view, row, item } = await twoParts();

    await fire(row("root/part-1"), dragEvent("dragstart", 0));
    await fire(row("root/part-2"), dragEvent("dragover", 10));
    expect(item("root/part-2").getAttribute("data-drop")).toBe("into");

    await fire(row("root/part-2"), dragEvent("drop", 10));
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_move")).toEqual([
      expect.objectContaining({
        cmd: "node_move",
        node: "root/part-1",
        parent: "root/part-2",
        index: 0,
      }),
    ]);
    // The hint is gone with the drag.
    expect(item("root/part-2").hasAttribute("data-drop")).toBe(false);
    await view.unmount();
    restore();
  });

  test("the upper and lower quarters reorder among the target's siblings", async () => {
    const restore = stubLayout(200, 20, 0, 0);
    const { wasm, view, row, item } = await twoParts();

    await fire(row("root/part-2"), dragEvent("dragstart", 0));
    await fire(row("root/part-1"), dragEvent("dragover", 2));
    expect(item("root/part-1").getAttribute("data-drop")).toBe("before");
    await fire(row("root/part-1"), dragEvent("drop", 2));
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_move")).toEqual([
      expect.objectContaining({ cmd: "node_move", node: "root/part-2", parent: "root", index: 0 }),
    ]);
    await view.unmount();
    restore();
  });

  test("a row leaves its hint behind when the drag moves on", async () => {
    const restore = stubLayout(200, 20, 0, 0);
    const { view, row, item } = await twoParts();

    await fire(row("root/part-1"), dragEvent("dragstart", 0));
    await fire(row("root/part-2"), dragEvent("dragover", 10));
    expect(item("root/part-2").getAttribute("data-drop")).toBe("into");
    await fire(row("root/part-2"), dragEvent("dragleave", 10));

    expect(item("root/part-2").hasAttribute("data-drop")).toBe(false);
    await fire(row("root/part-1"), dragEvent("dragend", 0));
    await view.unmount();
    restore();
  });

  test("a node refuses itself, and the root is never dragged", async () => {
    const restore = stubLayout(200, 20, 0, 0);
    const { wasm, view, row, item } = await twoParts();

    expect(row("root").getAttribute("draggable")).toBeNull();
    expect(row("root/part-1").getAttribute("draggable")).toBe("true");

    await fire(row("root/part-1"), dragEvent("dragstart", 0));
    await fire(row("root/part-1"), dragEvent("dragover", 10));
    expect(item("root/part-1").hasAttribute("data-drop")).toBe(false);
    await fire(row("root/part-1"), dragEvent("drop", 10));
    await settle();

    expect(wasm.requests.filter((request) => request.cmd === "node_move")).toEqual([]);
    await view.unmount();
    restore();
  });
});

describe("the tree's toolbar", () => {
  async function panel() {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument();
    let selected: string | undefined = "unset";
    const problems: unknown[] = [];

    function Watch() {
      selected = useSelection().node;
      return null;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={session}>
          <NodeTree.Actions session={session} onError={(cause) => problems.push(cause)} />
          <NodeTree.Root session={session} />
          <Watch />
        </SelectionProvider>
      </EditorProvider>,
    );
    const button = (what: string): HTMLButtonElement =>
      view.container.querySelector(`[data-catchlight-node-${what}]`) as HTMLButtonElement;
    const click = async (what: string): Promise<void> => {
      await act(async () => {
        button(what).click();
      });
      await settle();
    };
    return { session, wasm, view, button, click, problems, selection: () => selected };
  }

  test("with nothing selected, Add builds under the root and selects what it made", async () => {
    const { wasm, view, button, click, selection } = await panel();

    expect(button("delete").disabled).toBe(true);
    expect(button("duplicate").disabled).toBe(true);
    expect(button("up").disabled).toBe(true);
    expect(button("down").disabled).toBe(true);

    await click("add");

    expect(wasm.requests.filter((request) => request.cmd === "node_add")).toEqual([
      expect.objectContaining({ cmd: "node_add", parent: "root", kind: "group", name: null }),
    ]);
    expect(selection()).toBe("root/group-1");
    // And the panel now shows it, because the document moved.
    expect(view.container.querySelector("[data-node='root/group-1']")).not.toBeNull();
    await view.unmount();
  });

  test("the kind picker chooses what Add makes", async () => {
    const { wasm, view, click } = await panel();

    const kind = view.container.querySelector("[data-catchlight-node-kind]") as HTMLSelectElement;
    expect([...kind.options].map((option) => option.value)).toEqual([
      "group",
      "part",
      "composite",
      "mesh_group",
    ]);
    await act(async () => {
      kind.value = "composite";
      kind.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await click("add");

    expect(wasm.requests.filter((request) => request.cmd === "node_add")).toEqual([
      expect.objectContaining({ kind: "composite" }),
    ]);
    await view.unmount();
  });

  test("the selection decides what the buttons do, and whether they can", async () => {
    const { session, wasm, view, button, click, selection } = await panel();
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));
    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "brow" }));

    const label = view.container.querySelector(
      "[data-node='root/part-1'] [data-catchlight-node-label]",
    ) as HTMLButtonElement;
    await act(async () => {
      label.click();
    });

    // The first of two: it can go down, but not up.
    expect(button("up").disabled).toBe(true);
    expect(button("down").disabled).toBe(false);
    await click("down");
    await click("duplicate");
    await click("delete");

    expect(
      wasm.requests
        .filter((request) => request.cmd.startsWith("node_") && request.cmd !== "node_add")
        .map((request) => ({ ...request, id: 0 })),
    ).toEqual([
      { id: 0, cmd: "node_reorder", session: session.id, node: "root/part-1", index: 1 },
      { id: 0, cmd: "node_duplicate", session: session.id, node: "root/part-1" },
      { id: 0, cmd: "node_delete", session: session.id, node: "root/part-1" },
    ]);
    // The node it named is gone, so nothing is selected any more.
    expect(selection()).toBeUndefined();
    await view.unmount();
  });

  test("the root can be selected but not deleted", async () => {
    const { view, button } = await panel();

    const label = view.container.querySelector(
      "[data-node='root'] [data-catchlight-node-label]",
    ) as HTMLButtonElement;
    await act(async () => {
      label.click();
    });

    expect(button("delete").disabled).toBe(true);
    expect(button("duplicate").disabled).toBe(true);
    expect(button("up").disabled).toBe(true);
    expect(button("down").disabled).toBe(true);
    await view.unmount();
  });

  test("a refused edit reaches the host, not the console", async () => {
    const { wasm, view, click, problems } = await panel();
    wasm.refuse.set("node_add", { code: "bad_request", message: "no" });

    await click("add");

    expect(problems).toHaveLength(1);
    expect(String(problems[0])).toContain("no");
    await view.unmount();
  });
});
