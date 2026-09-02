/**
 * What the tree edits put on the wire, and the two things they work out for
 * themselves: where a reorder lands, and when a delete takes the selection
 * with it.
 *
 * Against a scripted backend rather than the fake editor, because what is
 * asserted here is the command — the exact arm, with the exact index — and a
 * fake that also *applies* it would let a wrong index pass as long as it was
 * wrong in both places.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Command, NodeId, Session, TreeNode } from "@catchlight/core";
import { Session as DocumentSession } from "@catchlight/core";
import { emptyDoc, FakeReplica, ScriptedBackend, structureBytes } from "@catchlight/core/fakes";

import { dropIndex, useNodeActions } from "./node-actions.js";
import type { NodeActions } from "./node-actions.js";
import { SelectionProvider, useSelection } from "./selection.js";
import { mount, run } from "./test/harness.js";

/** A tree node with the fields the panels read and nothing else. */
function node(id: string, children: TreeNode[] = []): TreeNode {
  return { id, name: id, kind: "group", z_order: 0, enabled: true, children };
}

/** A session whose replica holds `root`, over a backend that records commands. */
function scripted(root: TreeNode): { backend: ScriptedBackend; session: Session } {
  const backend = new ScriptedBackend();
  const replica = new FakeReplica();
  const doc = emptyDoc("scripted");
  doc.root = root;
  doc.rev = 1;
  replica.applyStructure(structureBytes(doc), 1);
  return { backend, session: new DocumentSession(backend, 1, replica) };
}

/** Mounts the hook under a selection, and hands both back. */
async function open(
  root: TreeNode,
  select?: NodeId,
): Promise<{
  backend: ScriptedBackend;
  session: Session;
  actions: NodeActions;
  selected: () => NodeId | undefined;
  unmount: () => Promise<void>;
}> {
  const { backend, session } = scripted(root);
  let actions: NodeActions | undefined;
  let selected: NodeId | undefined;
  let choose: (next: NodeId | undefined) => void = () => {};

  function Probe() {
    actions = useNodeActions(session);
    const selection = useSelection();
    selected = selection.node;
    choose = selection.select;
    return null;
  }

  const view = await mount(
    <SelectionProvider session={session}>
      <Probe />
    </SelectionProvider>,
  );
  if (select !== undefined) await run(() => choose(select));

  return {
    backend,
    session,
    actions: actions as NodeActions,
    selected: () => selected,
    unmount: view.unmount,
  };
}

/** Every document command the backend was sent, in order. */
function commands(backend: ScriptedBackend): Command[] {
  return backend.sent.filter((command) => command.cmd !== "presence_set");
}

describe("the tree edits on the wire", () => {
  test("each action sends its own command, with the session filled in", async () => {
    const tree = node("root", [node("root/a"), node("root/b")]);
    const { backend, actions, unmount } = await open(tree);

    backend.replies.set("node_add", { body: { result: "node", node: "root/group-9" } });
    const added = await run(() => actions.addChild("root/a", "part", "hair"));
    await run(() => actions.rename("root/b", "brow"));
    await run(() => actions.setEnabled("root/b", false));
    await run(() => actions.duplicate("root/b"));
    await run(() => actions.move("root/b", "root/a", 0));

    expect(added).toBe("root/group-9");
    expect(commands(backend)).toEqual([
      { cmd: "node_add", session: 1, parent: "root/a", kind: "part", name: "hair" },
      { cmd: "node_set", session: 1, node: "root/b", name: "brow" },
      { cmd: "node_set", session: 1, node: "root/b", enabled: false },
      { cmd: "node_duplicate", session: 1, node: "root/b" },
      { cmd: "node_move", session: 1, parent: "root/a", node: "root/b", index: 0 },
    ]);
    await unmount();
  });

  test("a name nobody gave goes as null, the way the arm declares it", async () => {
    const { backend, actions, unmount } = await open(node("root"));
    backend.replies.set("node_add", { body: { result: "node", node: "root/group-1" } });

    await run(() => actions.addChild("root", "mesh_group"));

    expect(commands(backend)[0]).toEqual({
      cmd: "node_add",
      session: 1,
      parent: "root",
      kind: "mesh_group",
      name: null,
    });
    await unmount();
  });

  test("a node_add that answered something else is a failure, not an Id", async () => {
    const { backend, actions, unmount } = await open(node("root"));
    backend.replies.set("node_add", { body: { result: "empty" } });

    await expect(actions.addChild("root", "group")).rejects.toThrow(/node_add/);
    await unmount();
  });
});

describe("moving among siblings", () => {
  test("the first child cannot move up, and nothing is sent", async () => {
    const tree = node("root", [node("root/a"), node("root/b"), node("root/c")]);
    const { backend, actions, unmount } = await open(tree);

    await run(() => actions.moveUp("root/a"));

    expect(commands(backend)).toEqual([]);
    await unmount();
  });

  test("the last child cannot move down, and neither can the root", async () => {
    const tree = node("root", [node("root/a"), node("root/b")]);
    const { backend, actions, unmount } = await open(tree);

    await run(() => actions.moveDown("root/b"));
    await run(() => actions.moveUp("root"));
    await run(() => actions.moveDown("root"));

    expect(commands(backend)).toEqual([]);
    await unmount();
  });

  test("up is one place earlier; down is one place later once the node is out", async () => {
    const tree = node("root", [node("root/a"), node("root/b"), node("root/c")]);
    const { backend, actions, unmount } = await open(tree);

    await run(() => actions.moveUp("root/c"));
    // The model takes the node out before it inserts, so "one later" counts
    // the siblings that are left: b at 1 becomes index 2, not 1.
    await run(() => actions.moveDown("root/b"));

    expect(commands(backend)).toEqual([
      { cmd: "node_reorder", session: 1, node: "root/c", index: 1 },
      { cmd: "node_reorder", session: 1, node: "root/b", index: 2 },
    ]);
    await unmount();
  });

  test("a nested child is ordered among its own siblings", async () => {
    const tree = node("root", [node("root/a", [node("root/a/x"), node("root/a/y")])]);
    const { backend, actions, unmount } = await open(tree);

    await run(() => actions.moveUp("root/a/y"));

    expect(commands(backend)).toEqual([
      { cmd: "node_reorder", session: 1, node: "root/a/y", index: 0 },
    ]);
    await unmount();
  });
});

describe("deleting and the selection", () => {
  test("deleting the selected node clears the selection", async () => {
    const tree = node("root", [node("root/a"), node("root/b")]);
    const { backend, actions, selected, unmount } = await open(tree, "root/a");
    expect(selected()).toBe("root/a");

    await run(() => actions.remove("root/a"));

    expect(commands(backend)).toEqual([{ cmd: "node_delete", session: 1, node: "root/a" }]);
    expect(selected()).toBeUndefined();
    await unmount();
  });

  test("deleting an ancestor of the selected node clears it too", async () => {
    const tree = node("root", [node("root/a", [node("root/a/x")])]);
    const { actions, selected, unmount } = await open(tree, "root/a/x");

    await run(() => actions.remove("root/a"));

    expect(selected()).toBeUndefined();
    await unmount();
  });

  test("deleting anything else leaves the selection alone", async () => {
    const tree = node("root", [node("root/a"), node("root/b")]);
    const { actions, selected, unmount } = await open(tree, "root/a");

    await run(() => actions.remove("root/b"));

    expect(selected()).toBe("root/a");
    await unmount();
  });
});

describe("where a drop lands", () => {
  const tree = node("root", [
    node("root/a", [node("root/a/x")]),
    node("root/b"),
    node("root/c"),
  ]);

  test("into a row appends to its children", () => {
    expect(dropIndex(tree, "root/c", "root/a", "into")).toEqual({ parent: "root/a", index: 1 });
  });

  test("beside a row counts the siblings the dragged node is not in", () => {
    // c sits after b, so dropping c before b is index 1 in [a, b] — the list
    // the model reinserts into.
    expect(dropIndex(tree, "root/c", "root/b", "before")).toEqual({ parent: "root", index: 1 });
    expect(dropIndex(tree, "root/c", "root/b", "after")).toEqual({ parent: "root", index: 2 });
    // A node from elsewhere is not in that list at all.
    expect(dropIndex(tree, "root/a/x", "root/b", "before")).toEqual({ parent: "root", index: 1 });
  });

  test("a node cannot be dropped on itself or into its own subtree", () => {
    expect(dropIndex(tree, "root/a", "root/a", "into")).toBeUndefined();
    expect(dropIndex(tree, "root/a", "root/a/x", "into")).toBeUndefined();
    expect(dropIndex(tree, "root/a", "root/a/x", "before")).toBeUndefined();
  });

  test("the root has no siblings to land beside", () => {
    expect(dropIndex(tree, "root/a", "root", "before")).toBeUndefined();
    expect(dropIndex(tree, "root/a", "root", "into")).toEqual({ parent: "root", index: 3 });
  });
});
