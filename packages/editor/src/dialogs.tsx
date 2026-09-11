import type { Session } from "@catchlight/core";
import {
  EmptyState,
  Icon,
  Modal,
  flattenTree,
  kindIcons,
  kindLabels,
  useSelection,
  useEditing,
} from "@catchlight/react";
import type { IconName } from "@catchlight/react";
import { useEffect, useId, useRef, useState } from "react";

export interface PaletteAction {
  name: string;
  detail: string;
  icon: IconName;
  key?: string;
  run: () => unknown;
  disabled?: boolean;
}
export function CommandPalette({
  session,
  actions,
  onClose,
}: {
  session: Session | undefined;
  actions: PaletteAction[];
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const { select } = useSelection();
  const editing = useEditing();
  const id = useId();
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    list.current
      ?.querySelector("[data-active]")
      ?.scrollIntoView({ block: "nearest" });
  }, [active, query]);
  const nodes: PaletteAction[] = session
    ? flattenTree(session.tree()).map((n) => ({
        name: n.name,
        detail: kindLabels[n.kind],
        icon: kindIcons[n.kind],
        run: () => (editing ? editing.guard(() => select(n.id)) : select(n.id)),
      }))
    : [];
  const results = [...actions, ...nodes].filter(
    (a) =>
      !a.disabled &&
      `${a.name} ${a.detail}`.toLowerCase().includes(query.toLowerCase()),
  );
  return (
    <Modal title="Find your next move" onClose={onClose}>
      <div data-catchlight-palette="">
        <label data-catchlight-search="">
          <Icon name="search" />
          <input
            autoFocus
            role="combobox"
            aria-controls={`${id}-results`}
            aria-expanded="true"
            aria-autocomplete="list"
            aria-activedescendant={
              results.length ? `${id}-${active}` : undefined
            }
            aria-label="Search commands and nodes"
            placeholder="Search commands, tools, and nodes…"
            value={query}
            onChange={(e) => {
              setQuery(e.currentTarget.value);
              setActive(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setActive((i) =>
                  Math.max(0, Math.min(results.length - 1, i + 1)),
                );
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setActive((i) => Math.max(0, i - 1));
              }
              if (e.key === "Enter" && results[active]) {
                e.preventDefault();
                results[active]?.run();
                onClose();
              }
            }}
          />
        </label>
        <div
          ref={list}
          id={`${id}-results`}
          role="listbox"
          aria-label="Commands and nodes"
          data-catchlight-palette-results=""
        >
          {results.map((action, i) => (
            <button
              type="button"
              role="option"
              id={`${id}-${i}`}
              aria-selected={i === active}
              tabIndex={-1}
              data-active={i === active ? "" : undefined}
              key={`${i}:${action.name}`}
              onMouseEnter={() => setActive(i)}
              onClick={() => {
                action.run();
                onClose();
              }}
            >
              <Icon name={action.icon} />
              <span>
                <strong>{action.name}</strong>
                <small>{action.detail}</small>
              </span>
              {action.key && <kbd>{action.key}</kbd>}
            </button>
          ))}
          {!results.length && (
            <EmptyState icon="search" title="No matches">
              Try a node name or an action like “fit” or “save”.
            </EmptyState>
          )}
        </div>
        <small data-catchlight-palette-hint="">
          <kbd>↑</kbd>
          <kbd>↓</kbd> navigate <kbd>↵</kbd> choose <kbd>esc</kbd> close
        </small>
      </div>
    </Modal>
  );
}
export function Shortcuts({ onClose }: { onClose: () => void }) {
  return (
    <Modal title="A faster way to create" onClose={onClose}>
      <p data-catchlight-hint="">
        Use Ctrl on Windows and Linux, or ⌘ on macOS.
      </p>
      <div data-catchlight-shortcuts="">
        {[
          ["Select and move", "V"],
          ["Hand tool", "H"],
          ["Pan temporarily", "Space + drag"],
          ["Fit the model", "F"],
          ["Focus mode", "Shift F"],
          ["Edit mesh on artwork", "M"],
          ["Open / toggle recording", "R"],
          ["Grid overlay", "G"],
          ["Find a command or node", "Ctrl / ⌘ K"],
          ["Save model", "Ctrl / ⌘ S"],
          ["Open model", "Ctrl / ⌘ O"],
          ["Undo / redo", "Ctrl / ⌘ Z / Shift Z"],
          ["Duplicate selection", "Ctrl / ⌘ D"],
          ["Delete selection", "Delete"],
          ["Nudge selection on canvas", "↑ ↓ ← →"],
          ["Larger nudge", "Shift + arrow"],
          ["Rename a tree node", "F2 / double-click"],
          ["Cancel / clear selection", "Escape"],
        ].map(([label, keys]) => (
          <div key={label}>
            <span>{label}</span>
            <kbd>{keys}</kbd>
          </div>
        ))}
      </div>
    </Modal>
  );
}
