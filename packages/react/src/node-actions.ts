/**
 * Tree edits as commands. Indices are read from the replica at call time, not
 * captured, since another client may have reordered the tree since render.
 */

import type { NodeId, NodeKindArg, ResponseBody, Session, TreeNode } from "@catchlight/core";
import { useMemo } from "react";

import { useLatest } from "./latest.js";
import { useSelection } from "./selection.js";

export type DropAt = "into" | "before" | "after";

export interface NodeActions {
  addChild(parent: NodeId, kind: NodeKindArg, name?: string): Promise<NodeId>;
  /** Deletes the node and its subtree, clearing the selection if it was in it. */
  remove(node: NodeId): Promise<ResponseBody>;
  /** Deep-copies the node's subtree as its next sibling. */
  duplicate(node: NodeId): Promise<ResponseBody>;
  rename(node: NodeId, name: string): Promise<ResponseBody>;
  setEnabled(node: NodeId, enabled: boolean): Promise<ResponseBody>;
  /** Puts the node under `parent` at `index`, counted with the node removed. */
  move(node: NodeId, parent: NodeId, index: number): Promise<ResponseBody>;
  moveUp(node: NodeId): Promise<ResponseBody>;
  moveDown(node: NodeId): Promise<ResponseBody>;
}

/** Must sit under a `<SelectionProvider>`: deleting the selected node clears the selection. */
export function useNodeActions(session: Session): NodeActions {
  // Boxed so a selection change does not rebuild every callback.
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
        // Asked before the delete: afterwards the tree cannot answer it.
        const held = subtreeHolds(findNode(session.tree(), node), selection.current.node);
        const body = await session.send({ cmd: "node_delete", node });
        if (held) selection.current.select(undefined);
        return body;
      },

      duplicate: (node) => session.send({ cmd: "node_duplicate", node }),

      rename: (node, name) => session.send({ cmd: "node_set", node, name }),

      setEnabled: (node, enabled) => session.send({ cmd: "node_set", node, enabled }),

      move: (node, parent, index) => session.send({ cmd: "edit_apply", if_rev: session.getRevision(), edits: [
        { op: "node_reparent", node, to: parent },
        { op: "node_reorder", node, index },
      ] }),

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

export function findNode(tree: TreeNode, node: NodeId): TreeNode | undefined {
  if (tree.id === node) return tree;
  for (const child of tree.children) {
    const found = findNode(child, node);
    if (found) return found;
  }
  return undefined;
}

export interface SiblingIndex {
  parent: TreeNode;
  index: number;
}

/** `undefined` for the root and for a node the tree does not carry. */
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

export interface Drop {
  parent: NodeId;
  index: number;
}

/**
 * A drop beside a row counts the target's siblings with the dragged node
 * already removed, the list the model reinserts into. `undefined` for a drop
 * onto the node itself, into its own subtree, or beside the root.
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

const NOTHING = (): Promise<ResponseBody> => Promise.resolve({ result: "empty" });
