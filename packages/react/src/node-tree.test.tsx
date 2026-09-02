/**
 * The tree panel: real tree markup, and a selection other clients can see.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import { act } from "react";

import { EditorProvider, NodeTree, SelectionProvider, useSelection } from "./index.js";
import { harness, mount, run } from "./test/harness.js";

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
