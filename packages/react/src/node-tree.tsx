/**
 * The node tree, as a tree.
 *
 * Real `ul`/`li` with the tree roles on them, because a panel that is a pile
 * of divs is a panel a keyboard cannot walk. State goes out as data
 * attributes — `data-selected`, `data-kind` — so a host styles the selection
 * without this package shipping a stylesheet or a class name.
 *
 * The item is exported for the same reason the render prop exists elsewhere: a
 * host that wants a disclosure triangle, a visibility toggle or a drag handle
 * per node writes its own row and keeps the recursion.
 */

import type { Session, TreeNode } from "@catchlight/core";
import type { ComponentProps } from "react";

import { useTree } from "./replica.js";
import { useSelection } from "./selection.js";

export interface NodeTreeRootProps extends Omit<ComponentProps<"ul">, "children"> {
  session: Session;
}

export function NodeTreeRoot({ session, ...rest }: NodeTreeRootProps) {
  const root = useTree(session);
  return (
    <ul role="tree" data-catchlight-node-tree="" {...rest}>
      <NodeTreeItem node={root} />
    </ul>
  );
}

export interface NodeTreeItemProps extends Omit<ComponentProps<"li">, "children"> {
  node: TreeNode;
}

export function NodeTreeItem({ node, ...rest }: NodeTreeItemProps) {
  const { node: selected, select } = useSelection();
  const isSelected = selected === node.id;
  return (
    <li
      role="treeitem"
      aria-selected={isSelected}
      data-catchlight-node=""
      data-node={node.id}
      data-kind={node.kind}
      data-selected={isSelected ? "" : undefined}
      {...rest}
    >
      <button type="button" data-catchlight-node-label="" onClick={() => select(node.id)}>
        {node.name}
      </button>
      {node.children.length > 0 && (
        <ul role="group">
          {node.children.map((child) => (
            <NodeTreeItem node={child} key={child.id} />
          ))}
        </ul>
      )}
    </li>
  );
}

export const NodeTree = { Root: NodeTreeRoot, Item: NodeTreeItem };
