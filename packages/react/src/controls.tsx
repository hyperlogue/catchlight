/**
 * Unstyled editor controls. A numeric draft belongs to the field until Enter
 * or blur; Escape restores the model value, and invalid text never becomes a
 * command. Native dialogs own focus containment and return focus on close.
 */
import { useEffect, useId, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";

const paths = {
  select: "m5 3 14 10-7 1-3 7Z",
  hand: "M8 12V6a2 2 0 0 1 4 0v5-7a2 2 0 0 1 4 0v8-5a2 2 0 0 1 4 0v7c0 5-3 8-7 8h-1c-3 0-5-2-7-5l-2-3a2 2 0 0 1 3-2l2 2",
  plus: "M12 5v14M5 12h14",
  close: "m6 6 12 12M6 18 18 6",
  search: "M21 21l-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0",
  chevron: "m9 5 7 7-7 7",
  down: "m6 9 6 6 6-6",
  group: "M3 7V5h7l2 2h9v13H3Z",
  part: "M4 3h16v18H4ZM4 16l5-5 4 4 3-3 4 4M8 7h.01",
  mesh: "M3 4h18v16H3ZM3 4l9 8 9-8M3 20l9-8 9 8M12 12V4m0 8v8",
  composite: "m12 3 10 5-10 5L2 8Zm-10 9 10 5 10-5M2 16l10 5 10-5",
  spine: "m5 20 4-7 6-3 4-6M5 20h.01M9 13h.01M15 10h.01M19 4h.01",
  physics: "M3 4h18M12 4v10m0 0a4 4 0 1 0 0 8 4 4 0 0 0 0-8",
  eye: "M2 12s3-7 10-7 10 7 10 7-3 7-10 7S2 12 2 12Zm10-3a3 3 0 1 0 0 6 3 3 0 0 0 0-6",
  hidden:
    "m3 3 18 18M9 5a12 12 0 0 1 3 0c7 0 10 7 10 7a17 17 0 0 1-4 5M6 6a20 20 0 0 0-4 6s3 7 10 7a12 12 0 0 0 5-1",
  undo: "M3 10h11a6 6 0 0 1 0 12M3 10l5-5m-5 5 5 5",
  redo: "M21 10H10a6 6 0 0 0 0 12m11-12-5-5m5 5-5 5",
  save: "M5 3h12l4 4v14H3V3Zm2 0v6h10V3M7 21v-8h10v8",
  upload: "M12 16V3m-5 5 5-5 5 5M3 15v6h18v-6",
  download: "M12 3v13m-5-5 5 5 5-5M3 16v5h18v-5",
  fit: "M9 3H3v6m12-6h6v6M3 15v6h6m6 0h6v-6M8 8h8v8H8Z",
  minus: "M5 12h14",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  check: "m5 12 4 4L19 6",
  warning: "m12 3 10 18H2Zm0 6v5m0 3h.01",
  trash: "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7",
  duplicate: "M8 8h13v13H8ZM16 8V3H3v13h5",
  settings: "M4 7h16M4 17h16M8 4v6m8 4v6",
  play: "m8 4 12 8-12 8Z",
  pause: "M8 4v16M16 4v16",
  reset: "M3 10a9 9 0 1 1 1 8M3 3v7h7",
  link: "m9 15 6-6m-7 3-3 3a4 4 0 0 0 6 6l3-3m-4-12 3-3a4 4 0 0 1 6 6l-3 3",
  key: "m12 4 8 8-8 8-8-8Z",
  code: "m8 6-6 6 6 6m8-12 6 6-6 6m-5 3 2-18",
  help: "M9 8a3 3 0 1 1 4 3c-1 1-1 2-1 3m0 3h.01M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0",
  grid: "M3 3h18v18H3ZM3 9h18M3 15h18M9 3v18M15 3v18",
  panel: "M3 4h18v16H3ZM9 4v16",
  arrow: "M5 12h14m-5-5 5 5-5 5",
} as const;
export type IconName = keyof typeof paths;
export function Icon({ name, ...props }: { name: IconName } & ComponentProps<"svg">) {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      <path d={paths[name]} />
    </svg>
  );
}
export function IconButton({
  icon,
  label,
  children,
  ...props
}: { icon: IconName; label: string } & ComponentProps<"button">) {
  return (
    <button
      type="button"
      data-catchlight-icon-button=""
      aria-label={label}
      title={label}
      {...props}
    >
      <Icon name={icon} />
      {children}
    </button>
  );
}
export function Field({
  label,
  children,
  hint,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
}) {
  return (
    <div data-catchlight-field="">
      <span data-catchlight-field-label="">{label}</span>
      <div data-catchlight-field-control="">{children}</div>
      {hint && <small data-catchlight-field-hint="">{hint}</small>}
    </div>
  );
}
export function NumberField({
  label,
  value,
  onCommit,
  prefix,
  unit,
  min,
  max,
  step = "any",
  ...props
}: {
  label: string;
  value: number;
  onCommit: (value: number) => void;
  /** Axis or channel label before the value; physical units follow it. */
  prefix?: string;
  unit?: string;
  min?: number;
  max?: number;
} & Omit<ComponentProps<"input">, "value" | "onChange" | "type" | "min" | "max">) {
  const [draft, setDraft] = useState(String(round(value)));
  const [editing, setEditing] = useState(false);
  useEffect(() => {
    if (!editing) setDraft(String(round(value)));
  }, [value, editing]);
  const next = Number(draft);
  const invalid =
    draft.trim() === "" ||
    !Number.isFinite(next) ||
    (Number(step) === 1 && !Number.isInteger(next)) ||
    (min !== undefined && next < min) ||
    (max !== undefined && next > max);
  const commit = () => {
    setEditing(false);
    if (!invalid && next !== value) onCommit(next);
    else setDraft(String(round(value)));
  };
  return (
    <span data-catchlight-number="" data-invalid={editing && invalid ? "" : undefined}>
      {prefix && <span data-catchlight-number-prefix="" aria-hidden="true">{prefix}</span>}
      <input
        type="number"
        inputMode="decimal"
        aria-label={label}
        aria-invalid={editing && invalid}
        value={draft}
        min={min}
        max={max}
        step={step}
        onFocus={() => setEditing(true)}
        onChange={(e) => setDraft(e.currentTarget.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            e.currentTarget.blur();
          }
          if (e.key === "Escape") {
            setDraft(String(round(value)));
            setEditing(false);
            e.preventDefault();
            e.stopPropagation();
          }
        }}
        {...props}
      />
      {unit && <span data-catchlight-number-unit="" aria-hidden="true">{unit}</span>}
    </span>
  );
}
export function TextField({
  label,
  value,
  onCommit,
  ...props
}: { label: string; value: string; onCommit: (value: string) => void } & Omit<
  ComponentProps<"input">,
  "value" | "onChange"
>) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <input
      type="text"
      aria-label={label}
      value={draft}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={() => {
        if (draft.trim() && draft !== value) onCommit(draft.trim());
        else setDraft(value);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") {
          setDraft(value);
          e.stopPropagation();
        }
      }}
      {...props}
    />
  );
}
export function Disclosure({
  title,
  children,
  defaultOpen = true,
  badge,
  ...props
}: {
  title: string;
  children: ReactNode;
  badge?: ReactNode;
  defaultOpen?: boolean;
} & ComponentProps<"details">) {
  return (
    <details data-catchlight-disclosure="" open={defaultOpen} {...props}>
      <summary>
        <Icon name="chevron" />
        <span>{title}</span>
        {badge}
      </summary>
      <div data-catchlight-disclosure-body="">{children}</div>
    </details>
  );
}
export function EmptyState({
  icon = "select",
  title,
  children,
}: {
  icon?: IconName;
  title: string;
  children?: ReactNode;
}) {
  return (
    <div data-catchlight-empty-state="">
      <Icon name={icon} width="28" height="28" />
      <strong>{title}</strong>
      {children && <p>{children}</p>}
    </div>
  );
}
export function Modal({
  title,
  children,
  onClose,
  wide = false,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const id = useId();
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const before = document.activeElement;
    dialog.showModal?.();
    return () => {
      dialog.close?.();
      if (before instanceof HTMLElement && before.isConnected) before.focus();
    };
  }, []);
  return (
    <dialog
      ref={ref}
      data-catchlight-dialog=""
      data-wide={wide ? "" : undefined}
      aria-labelledby={id}
      aria-modal="true"
      onCancel={(e) => {
        e.preventDefault();
        close.current();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          const r = e.currentTarget.getBoundingClientRect();
          if (
            e.clientX < r.left ||
            e.clientX > r.right ||
            e.clientY < r.top ||
            e.clientY > r.bottom
          )
            close.current();
        }
      }}
    >
      <header>
        <h2 id={id}>{title}</h2>
        <IconButton icon="close" label="Close dialog" onClick={onClose} />
      </header>
      <div data-catchlight-dialog-body="">{children}</div>
    </dialog>
  );
}
export const round = (n: number) => Math.round(n * 1000) / 1000;
