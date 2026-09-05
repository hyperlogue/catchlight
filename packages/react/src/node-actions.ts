/**
 * Editing the tree: one call per command, and the arithmetic a row needs to
 * build one.
 *
 * **Every action is a command, and nothing here touches the model.** The tree
 * a panel draws comes back from the replica when the editor says the model
 * moved, so an action's whole job is to send the right command and hand its
 * promise on. A caller that wants to report a failure awaits it; a caller that
 * does not, ignores it.
 *
 * **The index is read at call time, not captured.** `moveUp` and `moveDown`
 * are "one place earlier/later among your siblings", which is a fact about the
 * tree as it is right now — an agent on the socket may have reordered it since
 * this component rendered. The replica answers synchronously, so reading it in
 * the callback costs nothing and cannot be stale.
 *
 * **The index a reorder carries is the index after the node is taken out.**
 * The model removes the node from its parent's children and reinserts it, so
 * moving down one place is `index + 1` rather than `index`, and a drop that
 * lands next to a sibling counts the siblings with the dragged node already
 * gone. [`dropIndex`] is that arithmetic, written once.
 *
 * **A delete decides about the selection before it sends.** Once the node is
 * gone the tree cannot answer whether the selection was inside it, so the
 * question is asked first and acted on when the command lands.
 */

import type { NodeId, NodeKindArg, ResponseBody, Session, TreeNode } from "@catchlight/core";
import { useMemo } from "react";

import { useLatest } from "./latest.js";
import { useSelection } from "./selection.js";

/** Where a drop lands relative to the row it was made on. */
export type DropAt = "into" | "before" | "after";

/** The tree edits a panel makes, each one command. */
export interface NodeActions {
  /** Adds a child under `parent` and hands back the Id the editor minted. */
  addChild(parent: NodeId, kind: NodeKindArg, name?: string): Promise<NodeId>;
  /** Deletes the node and its subtree, clearing the selection if it was in it. */
  remove(node: NodeId): Promise<ResponseBody>;
  /** Deep-copies the node's subtree as its next sibling. */
  duplicate(node: NodeId): Promise<ResponseBody>;
  /** Sets the label a person reads. Addresses nothing; the Id is unchanged. */
  rename(node: NodeId, name: string): Promise<ResponseBody>;
  /** Draws the node and its subtree, or does not. */
  setEnabled(node: NodeId, enabled: boolean): Promise<ResponseBody>;
  /** Puts the node under `parent` at `index`, counted with the node removed. */
  move(node: NodeId, parent: NodeId, index: number): Promise<ResponseBody>;
  /** One place earlier among its siblings. A no-op on the first one. */
  moveUp(node: NodeId): Promise<ResponseBody>;
  /** One place later among its siblings. A no-op on the last one. */
  moveDown(node: NodeId): Promise<ResponseBody>;
}

/**
 * The tree edits, bound to one session.
 *
 * Must sit under a `<SelectionProvider>`: deleting the selected node has to
 * clear the selection, and a selection only this hook's caller knew about
 * would leave the panels pointing at a node that is gone.
 */
export function useNodeActions(session: Session): NodeActions {
  // The newest selection, in a box: an action that reads it must not rebuild
  // every callback each time something else is clicked.
  const selection = useLatest(useSelection());

  return useMemo<NodeActions>(
    () => ({
      async addChild(parent, kind, name) {
        const body = await session.send({
          cmd: "node_add",
          parent,
          kind,
          name: name ?? null,
        });
        if (body.result !== "node") {
          throw new Error(`node_add answered ${body.result}, not the node it added`);
        }
        return body.node;
      },

      async remove(node) {
        // Asked before the delete: afterwards neither node is in the tree, so
        // the tree can no longer say whether one held the other.
        const held = subtreeHolds(findNode(session.tree(), node), selection.current.node);
        const body = await session.send({ cmd: "node_delete", node });
        if (held) selection.current.select(undefined);
        return body;
      },

      duplicate: (node) => session.send({ cmd: "node_duplicate", node }),

      rename: (node, name) => session.send({ cmd: "node_set", node, name }),

      setEnabled: (node, enabled) => session.send({ cmd: "node_set", node, enabled }),

      move: (node, parent, index) => session.send({ cmd: "node_move", node, parent, index }),

      moveUp(node) {
        const at = siblingIndex(session.tree(), node);
        if (!at || at.index === 0) return NOTHING();
        return session.send({ cmd: "node_reorder", node, index: at.index - 1 });
      },

      moveDown(node) {
        const at = siblingIndex(session.tree(), node);
        if (!at || at.index >= at.parent.children.length - 1) return NOTHING();
        return session.send({ cmd: "node_reorder", node, index: at.index + 1 });
      },
    }),
    [session, selection],
  );
}

/** The node with this Id, anywhere under `tree`. */
export function findNode(tree: TreeNode, node: NodeId): TreeNode | undefined {
  if (tree.id === node) return tree;
  for (const child of tree.children) {
    const found = findNode(child, node);
    if (found) return found;
  }
  return undefined;
}

/** A node's parent and its place among that parent's children. */
export interface SiblingIndex {
  parent: TreeNode;
  index: number;
}

/**
 * Where `node` sits among its siblings, or `undefined` for the root and for a
 * node the tree does not carry — the two cases in which reordering means
 * nothing.
 */
export function siblingIndex(tree: TreeNode, node: NodeId): SiblingIndex | undefined {
  const index = tree.children.findIndex((child) => child.id === node);
  if (index >= 0) return { parent: tree, index };
  for (const child of tree.children) {
    const found = siblingIndex(child, node);
    if (found) return found;
  }
  return undefined;
}

/** Whether `node` is `held`'s subtree root or anywhere inside it. */
export function subtreeHolds(held: TreeNode | undefined, node: NodeId | undefined): boolean {
  if (!held || node === undefined) return false;
  return findNode(held, node) !== undefined;
}

/** Every Id in this subtree, its own included — what a drop may not land on. */
export function subtreeIds(tree: TreeNode): Set<NodeId> {
  const ids = new Set<NodeId>();
  const walk = (node: TreeNode): void => {
    ids.add(node.id);
    for (const child of node.children) walk(child);
  };
  walk(tree);
  return ids;
}

/** The parent and index a drop of `node` at `where` on `target` should move to. */
export interface Drop {
  parent: NodeId;
  index: number;
}

/**
 * What `move` a drop means, in the model's own counting.
 *
 * A drop *into* a row appends, so it is the target's child count. A drop
 * beside one counts the target's siblings **with the dragged node already
 * taken out**, because that is the list the model reinserts into — dropping a
 * node just below its own next sibling is otherwise off by one.
 *
 * `undefined` when the drop cannot happen: onto the dragged node itself, into
 * its own subtree, or beside the root, which has no siblings.
 */
export function dropIndex(
  tree: TreeNode,
  node: NodeId,
  target: NodeId,
  where: DropAt,
): Drop | undefined {
  const dragged = findNode(tree, node);
  if (!dragged || subtreeHolds(dragged, target)) return undefined;
  if (where === "into") {
    const parent = findNode(tree, target);
    return parent ? { parent: target, index: parent.children.length } : undefined;
  }
  const at = siblingIndex(tree, target);
  if (!at) return undefined;
  const siblings = at.parent.children.filter((child) => child.id !== node);
  const index = siblings.findIndex((child) => child.id === target);
  if (index < 0) return undefined;
  return { parent: at.parent.id, index: where === "before" ? index : index + 1 };
}

/** What an action that decided there was nothing to send answers. */
const NOTHING = (): Promise<ResponseBody> => Promise.resolve({ result: "empty" });
