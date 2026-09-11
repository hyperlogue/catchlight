/**
 * The node tree, as a tree — and the panel that edits it.
 *
 * Real `ul`/`li` with the tree roles on them, because a panel that is a pile
 * of divs is a panel a keyboard cannot walk. State goes out as data
 * attributes — `data-selected`, `data-kind`, `data-drop` — so a host styles
 * the selection and the drop target without this package shipping a
 * stylesheet or a class name.
 *
 * The item is exported for the same reason the render prop exists elsewhere: a
 * host that wants a disclosure triangle, a different toggle or its own drag
 * handle writes its own row, keeps the recursion, and still sends its edits
 * through [`useNodeActions`].
 *
 * **The gesture is this side's, the arithmetic is not.** A row decides which
 * of the three places a drop is aimed at from the pointer's height in it, and
 * hands the answer to `dropIndex`, which turns it into the parent and index
 * the model counts in. The same split the drag on the canvas is built to.
 *
 * **What is being dragged is module state, not React state.** A browser has
 * one drag at a time and `dataTransfer` cannot be read while hovering — only
 * on the drop — so a row that has to decide *now* whether it may accept has
 * nowhere else to look. Putting it in state instead would re-render every row
 * in the tree on the way past.
 */

import type { NodeId, NodeKindArg, Session, TreeNode } from "@catchlight/core";
import { useState } from "react";
import type { ComponentProps, DragEvent, KeyboardEvent } from "react";

import type { DropAt } from "./node-actions.js";
import { dropIndex, findNode, siblingIndex, subtreeIds, useNodeActions } from "./node-actions.js";
import { useTree } from "./replica.js";
import { useSelection } from "./selection.js";
import { Icon } from "./controls.js";
import { kindIcons, kindLabels } from "./authoring.js";

/** The node kinds a panel can add, in the order the picker lists them. */
export const NODE_KINDS: readonly NodeKindArg[] = ["group", "part", "composite", "mesh_group"];

/** What a failed edit is told to, when a host passed nothing. */
type ErrorSink = ((cause: unknown) => void) | undefined;

// `onError` is also a DOM event on every element; these ones win.
export interface NodeTreeRootProps extends Omit<ComponentProps<"ul">, "children" | "onError"> {
  session: Session;
  onError?: ErrorSink;
  filter?: string;
}

export function NodeTreeRoot({ session, onError, filter = "", ...rest }: NodeTreeRootProps) {
  const root = useTree(session);
  const needle = filter.trim().toLocaleLowerCase();
  const matching = (node: TreeNode): TreeNode | undefined => {
    if (!needle || node.name.toLocaleLowerCase().includes(needle)) return node;
    const children = node.children.map(matching).filter((child): child is TreeNode => !!child);
    return children.length ? { ...node, children } : undefined;
  };
  const shown = matching(root);
  return (
    <ul role="tree" aria-label="Model structure" data-catchlight-node-tree="" {...rest}>
      {shown ? (
        <NodeTreeItem
          key={needle ? "filtered" : "all"}
          session={session}
          node={shown}
          isRoot
          onError={onError}
        />
      ) : (
        <li data-catchlight-empty="">No matching nodes.</li>
      )}
    </ul>
  );
}

export interface NodeTreeItemProps extends Omit<ComponentProps<"li">, "children" | "onError"> {
  session: Session;
  node: TreeNode;
  /** The one node that cannot be dragged, dropped beside, or deleted. */
  isRoot?: boolean | undefined;
  onError?: ErrorSink;
}

export function NodeTreeItem({ session, node, isRoot, onError, ...rest }: NodeTreeItemProps) {
  const { node: selected, select } = useSelection();
  const actions = useNodeActions(session);
  const [drop, setDrop] = useState<DropAt | undefined>(undefined);
  const [renaming, setRenaming] = useState(false);
  const [expanded, setExpanded] = useState(true);
  const isSelected = selected === node.id;

  const onDragStart = (event: DragEvent<HTMLElement>): void => {
    if (isRoot) return;
    dragging = { node: node.id, subtree: subtreeIds(node) };
    event.dataTransfer?.setData("text/plain", node.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  };

  const onDragEnd = (): void => {
    dragging = undefined;
    setDrop(undefined);
  };

  const onDragOver = (event: DragEvent<HTMLElement>): void => {
    const where = placement(event, node.id, isRoot === true);
    setDrop(where);
    if (where === undefined) return;
    // Only a prevented `dragover` makes an element a drop target at all.
    event.preventDefault();
    event.stopPropagation();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
  };

  const onDragLeave = (event: DragEvent<HTMLElement>): void => {
    // Crossing into the checkbox or the label is not leaving the row.
    const to = event.relatedTarget;
    if (to instanceof Node && event.currentTarget.contains(to)) return;
    setDrop(undefined);
  };

  const onDrop = (event: DragEvent<HTMLElement>): void => {
    const where = placement(event, node.id, isRoot === true);
    const source = dragging?.node;
    dragging = undefined;
    setDrop(undefined);
    if (where === undefined || source === undefined) return;
    event.preventDefault();
    event.stopPropagation();
    const landing = dropIndex(session.tree(), source, node.id, where);
    if (!landing) return;
    report(onError, actions.move(source, landing.parent, landing.index));
  };

  const commitRename = (value: string): void => {
    setRenaming(false);
    const name = value.trim();
    if (name === "" || name === node.name) return;
    report(onError, actions.rename(node.id, name));
  };

  return (
    <li
      role="treeitem"
      aria-selected={isSelected}
      aria-expanded={node.children.length ? expanded : undefined}
      data-catchlight-node=""
      data-node={node.id}
      data-kind={node.kind}
      data-selected={isSelected ? "" : undefined}
      data-drop={drop}
      {...rest}
    >
      <div
        data-catchlight-node-row=""
        draggable={isRoot ? undefined : true}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
      >
        {node.children.length ? (
          <button
            type="button"
            data-catchlight-disclosure-toggle=""
            aria-label={`${expanded ? "Collapse" : "Expand"} ${node.name}`}
            onClick={() => setExpanded(!expanded)}
          >
            <Icon name={expanded ? "down" : "chevron"} width="12" height="12" />
          </button>
        ) : (
          <span data-catchlight-disclosure-spacer="" />
        )}
        <span data-catchlight-kind-icon="" title={kindLabels[node.kind]}>
          <Icon name={kindIcons[node.kind]} width="16" height="16" />
        </span>
        <label data-catchlight-visibility="" title={node.enabled ? "Hide node" : "Show node"}>
          <input
            type="checkbox"
            data-catchlight-node-enabled=""
            aria-label={`${node.name} enabled`}
            checked={node.enabled}
            onChange={(event) =>
              report(onError, actions.setEnabled(node.id, event.currentTarget.checked))
            }
          />
          <Icon name={node.enabled ? "eye" : "hidden"} width="15" height="15" />
        </label>
        {renaming ? (
          <NodeRename
            name={node.name}
            onCommit={commitRename}
            onCancel={() => setRenaming(false)}
          />
        ) : (
          <button
            type="button"
            data-catchlight-node-label=""
            onClick={() => select(node.id)}
            onDoubleClick={() => setRenaming(true)}
            onKeyDown={(event) => {
              if (event.key === "F2") {
                event.preventDefault();
                setRenaming(true);
              }
              if (event.key === "ArrowRight") {
                event.preventDefault();
                setExpanded(true);
              }
              if (event.key === "ArrowLeft") {
                event.preventDefault();
                setExpanded(false);
              }
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const tree = event.currentTarget.closest('[role="tree"]');
                const rows = [
                  ...(tree?.querySelectorAll<HTMLButtonElement>("[data-catchlight-node-label]") ??
                    []),
                ];
                const at = rows.indexOf(event.currentTarget);
                rows[at + (event.key === "ArrowDown" ? 1 : -1)]?.focus();
              }
            }}
          >
            {node.name}
          </button>
        )}
      </div>
      {node.children.length > 0 && expanded && (
        <ul role="group">
          {node.children.map((child) => (
            <NodeTreeItem session={session} node={child} key={child.id} onError={onError} />
          ))}
        </ul>
      )}
    </li>
  );
}

/**
 * The label while it is being edited.
 *
 * Its own component so the input is mounted fresh with the name it started
 * from: an uncontrolled field is the one that lets a person type without every
 * keystroke going through React, and `defaultValue` is only read once.
 */
function NodeRename({
  name,
  onCommit,
  onCancel,
}: {
  name: string;
  onCommit: (value: string) => void;
  onCancel: () => void;
}) {
  // Escape unmounts the input, and a blur must not then commit what Escape
  // just threw away.
  const [cancelled, setCancelled] = useState(false);

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>): void => {
    if (event.key === "Enter") {
      event.preventDefault();
      onCommit(event.currentTarget.value);
      return;
    }
    if (event.key !== "Escape") return;
    event.preventDefault();
    setCancelled(true);
    onCancel();
  };

  return (
    <input
      type="text"
      data-catchlight-node-rename=""
      aria-label={`rename ${name}`}
      defaultValue={name}
      autoFocus
      onKeyDown={onKeyDown}
      onBlur={(event) => {
        if (!cancelled) onCommit(event.currentTarget.value);
      }}
    />
  );
}

// `onError` is also a DOM event on every element; this one wins.
export interface NodeTreeActionsProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  onError?: ErrorSink;
}

/**
 * The toolbar over the tree: what to add, and what to do to the selection.
 *
 * Every button reads whether it applies from the tree rather than guessing —
 * the root cannot be deleted, the first child cannot move up — so a disabled
 * button is the panel saying what the editor would refuse anyway.
 */
export function NodeTreeActions({ session, onError, ...rest }: NodeTreeActionsProps) {
  const tree = useTree(session);
  const { node: selected, select } = useSelection();
  const actions = useNodeActions(session);
  const [kind, setKind] = useState<NodeKindArg>("group");

  // A selection outlives the node it names, so a stale one adds under the root
  // rather than failing.
  const target = selected !== undefined && findNode(tree, selected) ? selected : tree.id;
  const at = selected === undefined ? undefined : siblingIndex(tree, selected);
  const movable = at !== undefined;

  const add = (): void => {
    report(
      onError,
      actions.addChild(target, kind).then((added) => {
        select(added);
      }),
    );
  };

  return (
    <div data-catchlight-node-actions="" {...rest}>
      <select
        data-catchlight-node-kind=""
        aria-label="node kind"
        value={kind}
        onChange={(event) => setKind(event.currentTarget.value as NodeKindArg)}
      >
        {NODE_KINDS.map((each) => (
          <option value={each} key={each}>
            {each}
          </option>
        ))}
      </select>
      <button type="button" data-catchlight-node-add="" onClick={add}>
        Add
      </button>
      <button
        type="button"
        data-catchlight-node-delete=""
        disabled={!movable}
        onClick={() => selected !== undefined && report(onError, actions.remove(selected))}
      >
        Delete
      </button>
      <button
        type="button"
        data-catchlight-node-duplicate=""
        disabled={!movable}
        onClick={() => selected !== undefined && report(onError, actions.duplicate(selected))}
      >
        Duplicate
      </button>
      <button
        type="button"
        data-catchlight-node-up=""
        disabled={!at || at.index === 0}
        onClick={() => selected !== undefined && report(onError, actions.moveUp(selected))}
      >
        Up
      </button>
      <button
        type="button"
        data-catchlight-node-down=""
        disabled={!at || at.index >= at.parent.children.length - 1}
        onClick={() => selected !== undefined && report(onError, actions.moveDown(selected))}
      >
        Down
      </button>
    </div>
  );
}

export const NodeTree = {
  Root: NodeTreeRoot,
  Item: NodeTreeItem,
  Actions: NodeTreeActions,
};

/** The drag this browser is in the middle of, and what it may not be dropped on. */
let dragging: { node: NodeId; subtree: Set<NodeId> } | undefined;

/**
 * Which of the three places a pointer at this height is aiming at, or
 * `undefined` when this row cannot take the drag at all.
 *
 * The bands are quarters: the outer ones insert beside the row, and the wide
 * middle one — the easy target — puts the node inside it. The root has no
 * siblings, so every drop on it is an "into".
 */
function placement(
  event: DragEvent<HTMLElement>,
  node: NodeId,
  isRoot: boolean,
): DropAt | undefined {
  const drag = dragging;
  if (!drag || drag.subtree.has(node)) return undefined;
  if (isRoot) return "into";
  const box = event.currentTarget.getBoundingClientRect();
  if (box.height <= 0) return "into";
  const at = (event.clientY - box.top) / box.height;
  if (at < 0.25) return "before";
  if (at > 0.75) return "after";
  return "into";
}

/** Hands a failed edit to the host, or says so where a developer will see it. */
function report(onError: ErrorSink, work: Promise<unknown>): void {
  void work.catch((cause: unknown) => {
    if (onError) onError(cause);
    else console.warn("catchlight: the tree edit failed", cause);
  });
}
