import type {
  Session,
  NodeInfo,
  SlotPair,
  ExtensionRef,
  ExtensionValueInfo,
} from "@catchlight/core";
import { ReviewBadge, ReviewCommentButton } from "./review.js";
import { useState } from "react";
import {
  useCommand,
  useModelQuery,
  useSessionStatus,
  flattenTree,
  type ErrorHandler,
} from "./authoring.js";
import { Disclosure, EmptyState, Field, Icon, IconButton, Modal, NumberField } from "./controls.js";
import { useTree, useReplica } from "./replica.js";

export function MaskPanel({
  session,
  info,
  onError,
}: {
  session: Session;
  info: NodeInfo;
  onError: ErrorHandler;
}) {
  const tree = useTree(session);
  const { run, busy } = useCommand(session, onError);
  const [source, setSource] = useState("");
  if (info.kind !== "part" && info.kind !== "composite") return null;
  const parts = flattenTree(tree).filter((n) => n.kind === "part" && n.id !== info.id);
  const masks = info.masks ?? [];
  return (
    <Disclosure
      title="Masks"
      defaultOpen={masks.length > 0}
      badge={<span data-catchlight-badge="">{masks.length}</span>}
    >
      <fieldset disabled={busy}>
        {masks.map((mask, i) => (
          <div data-catchlight-mask="" key={`${i}:${mask.source}`}>
            <Icon name="part" />
            <span title={mask.source}>
              {parts.find((p) => p.id === mask.source)?.name ?? mask.source}
            </span>
            <select
              aria-label={`Mask ${i + 1} mode`}
              value={mask.mode}
              onChange={(e) =>
                void run({
                  cmd: "edit_apply",
                  if_rev: session.getRevision(),
                  edits: [
                    ...masks.map((_, index) => ({ op: "mask_delete" as const, node: info.id, index: masks.length - index - 1 })),
                    ...masks.map((item, index) => ({ op: "mask_add" as const, node: info.id, source: item.source, mode: index === i ? e.currentTarget.value as "mask" | "dodge" : item.mode })),
                  ],
                })
              }
            >
              <option value="mask">Keep inside</option>
              <option value="dodge">Cut out</option>
            </select>
            <IconButton
              icon="close"
              label={`Remove mask ${i + 1}`}
              onClick={() => void run({ cmd: "mask_delete", node: info.id, index: i })}
            />
          </div>
        ))}
        <div data-catchlight-inline="">
          <select
            aria-label="Mask source"
            value={source}
            onChange={(e) => setSource(e.currentTarget.value)}
          >
            <option value="">Choose a source part…</option>
            {parts.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
          <IconButton
            icon="plus"
            label="Add mask"
            disabled={!source}
            onClick={() =>
              void run({
                cmd: "mask_add",
                node: info.id,
                source,
                mode: "mask",
              }).then(() => setSource(""))
            }
          />
        </div>
        {!masks.length && (
          <p data-catchlight-hint="">Clip this artwork to another part’s silhouette.</p>
        )}
      </fieldset>
    </Disclosure>
  );
}

export function SlotsPanel({
  session,
  info,
  onError,
}: {
  session: Session;
  info: NodeInfo;
  onError: ErrorHandler;
}) {
  const { slots } = useModelQuery(session, { cmd: "slot_list", node: info.id }, "slots");
  const { run, busy } = useCommand(session, onError);
  return (
    <Disclosure
      title="Vertex slots"
      defaultOpen={false}
      badge={<span data-catchlight-badge="">{slots.length}</span>}
    >
      <p data-catchlight-hint="">Give a vertex an Id so welds and motion responses can refer to it.</p>
      <fieldset disabled={busy}>
        {slots.map((slot) => (
          <div key={slot.id} data-catchlight-slot="">
            <span title={slot.id}>{slot.id}</span>
            <ReviewBadge target={{ kind: "slot", node: info.id, slot: slot.id }} label={slot.id} />
            <ReviewCommentButton
              target={{ kind: "slot", node: info.id, slot: slot.id }}
              label={`slot ${slot.id}`}
            />
            <NumberField
              label={`${slot.id} vertex`}
              value={slot.vertex ?? 0}
              min={0}
              max={Math.max(0, (info.vertex_count ?? 1) - 1)}
              step={1}
              onCommit={(vertex) =>
                void run({
                  cmd: "slot_fill",
                  node: info.id,
                  slot: slot.id,
                  vertex,
                })
              }
            />
            {slot.vertex == null ? (
              <button
                type="button"
                onClick={() =>
                  void run({
                    cmd: "slot_fill",
                    node: info.id,
                    slot: slot.id,
                    vertex: 0,
                  })
                }
                disabled={!info.vertex_count}
              >
                Fill
              </button>
            ) : (
              <IconButton
                icon="minus"
                label={`Clear ${slot.id}`}
                onClick={() => void run({ cmd: "slot_clear", node: info.id, slot: slot.id })}
              />
            )}
            <IconButton
              icon="trash"
              label={`Delete ${slot.id}`}
              onClick={() => void run({ cmd: "slot_delete", node: info.id, slot: slot.id })}
            />
          </div>
        ))}
        <button
          type="button"
          data-catchlight-action=""
          onClick={() => void run({ cmd: "slot_add", node: info.id })}
        >
          <Icon name="plus" />
          Add slot
        </button>
      </fieldset>
    </Disclosure>
  );
}

export function WeldsPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const { welds } = useModelQuery(session, { cmd: "weld_list" }, "welds");
  const parts = flattenTree(useTree(session)).filter((n) => n.kind === "part");
  const { run, busy } = useCommand(session, onError);
  const [adding, setAdding] = useState(false);
  const name = (id: string) => parts.find((p) => p.id === id)?.name ?? id;
  return (
    <>
      {!welds.length && (
        <EmptyState icon="link" title="No connected parts">
          Weld pairs of slots to keep a join closed as the artwork deforms.
        </EmptyState>
      )}
      {welds.map((w) => (
        <Disclosure key={`${w.a}:${w.b}`} title={`${name(w.a)} ↔ ${name(w.b)}`}>
          {w.pairs.map((pair) => (
            <Field key={pair.a} label={`${pair.a} → ${pair.b}`}>
              <NumberField
                label={`${pair.a} weld weight`}
                value={pair.weight}
                min={0}
                max={1}
                step={0.05}
                onCommit={(weight) =>
                  void run({
                    cmd: "edit_apply",
                    if_rev: session.getRevision(),
                    edits: [{
                      op: "weld_set",
                      a: w.a,
                      b: w.b,
                      pairs: w.pairs.map((item) => item.a === pair.a ? { ...item, weight } : item),
                    }],
                  })
                }
              />
            </Field>
          ))}
          <button
            type="button"
            data-catchlight-danger=""
            disabled={busy}
            onClick={() => void run({ cmd: "weld_delete", a: w.a, b: w.b })}
          >
            Remove weld
          </button>
        </Disclosure>
      ))}
      <button
        type="button"
        data-catchlight-action=""
        disabled={parts.length < 2}
        onClick={() => setAdding(true)}
      >
        <Icon name="plus" />
        Connect parts
      </button>
      {adding && (
        <WeldDialog session={session} onError={onError} onClose={() => setAdding(false)} />
      )}
    </>
  );
}
function WeldDialog({
  session,
  onError,
  onClose,
}: {
  session: Session;
  onError: ErrorHandler;
  onClose: () => void;
}) {
  const parts = flattenTree(useTree(session)).filter((n) => n.kind === "part");
  const [a, setA] = useState(parts[0]?.id ?? "");
  const [b, setB] = useState(parts[1]?.id ?? "");
  const [pairs, setPairs] = useState<SlotPair[]>([]);
  const left = useModelQuery(session, { cmd: "slot_list", node: a }, "slots").slots;
  const right = useModelQuery(session, { cmd: "slot_list", node: b }, "slots").slots;
  const { run, busy } = useCommand(session, onError);
  return (
    <Modal title="Connect two parts" onClose={onClose}>
      <p data-catchlight-hint="">
        Choose the parts, then pair their named slots. A weight of 0.5 meets halfway.
      </p>
      <Field label="First part">
        <select
          aria-label="First weld part"
          value={a}
          onChange={(e) => {
            const next = e.currentTarget.value;
            setA(next);
            if (next === b) setB(a);
            setPairs([]);
          }}
        >
          {parts.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Second part">
        <select
          aria-label="Second weld part"
          value={b}
          onChange={(e) => {
            setB(e.currentTarget.value);
            setPairs([]);
          }}
        >
          {parts
            .filter((p) => p.id !== a)
            .map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
        </select>
      </Field>
      {left.map((slot) => (
        <Field key={slot.id} label={slot.id}>
          <select
            aria-label={`Pair ${slot.id}`}
            value={pairs.find((p) => p.a === slot.id)?.b ?? ""}
            onChange={(e) => {
              const to = e.currentTarget.value;
              setPairs((previous) => [
                ...previous.filter((p) => p.a !== slot.id),
                ...(to ? [{ a: slot.id, b: to, weight: 0.5 }] : []),
              ]);
            }}
          >
            <option value="">Not paired</option>
            {right
              .filter((r) => !pairs.some((p) => p.b === r.id && p.a !== slot.id))
              .map((r) => (
                <option key={r.id} value={r.id}>
                  {r.id}
                </option>
              ))}
          </select>
        </Field>
      ))}
      {(!left.length || !right.length) && (
        <p data-catchlight-warning="">
          Both parts need slots. Add them in the properties panel first.
        </p>
      )}
      <div data-catchlight-dialog-actions="">
        <button type="button" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          data-primary=""
          disabled={busy || a === b || !pairs.length}
          onClick={() =>
            void run({ cmd: "weld_set", a, b, pairs }).then((body) => {
              if (body) onClose();
            })
          }
        >
          Create weld
        </button>
      </div>
    </Modal>
  );
}

export function ModelHealth({ session }: { session: Session }) {
  const { warnings } = useModelQuery(session, { cmd: "model_check" }, "warnings");
  const tree = useTree(session);
  const count = flattenTree(tree).length;
  const params = useReplica(session, (s) => s.params());
  const textures = useReplica(session, (s) => s.textures());
  return (
    <>
      <div data-catchlight-model-stats="">
        <div>
          <b>{count}</b>
          <span>nodes</span>
        </div>
        <div>
          <b>{params.length}</b>
          <span>params</span>
        </div>
        <div>
          <b>{textures.length}</b>
          <span>textures</span>
        </div>
      </div>
      {!warnings.length ? (
        <div data-catchlight-health="">
          <Icon name="check" />
          <div>
            <strong>Ready to create</strong>
            <p>Your model has no validation warnings.</p>
          </div>
        </div>
      ) : (
        <>
          <p data-catchlight-hint="">
            {warnings.length} {warnings.length === 1 ? "thing" : "things"} to check
          </p>
          {warnings.map((w, i) => (
            <p key={i} data-catchlight-warning="">
              <Icon name="warning" />
              {w}
            </p>
          ))}
        </>
      )}
    </>
  );
}

function describeExtension(value: ExtensionValueInfo): string {
  switch (value.kind) {
    case "json":
      return "JSON";
    case "bytes":
      return `${value.size.toLocaleString()} bytes`;
    case "maintained": {
      const refs = `${value.refs.length} ${value.refs.length === 1 ? "reference" : "references"}`;
      return value.size == null
        ? `Version ${value.version}, ${refs}`
        : `Version ${value.version}, ${refs}, ${value.size.toLocaleString()} bytes`;
    }
  }
}

function describeRef(ref: ExtensionRef): string {
  if ("node" in ref) return `Node ${ref.node}`;
  if ("param" in ref) return `Param ${ref.param}`;
  if ("texture" in ref) return `Texture ${ref.texture}`;
  if ("slot" in ref) return `Vertex slot ${ref.slot[1]} on ${ref.slot[0]}`;
  return `Removed: ${describeRef(ref.removed)}`;
}

const noop = () => {};

/** The format's own; the editor refuses to change one through this panel. */
function isOfficial(key: string): boolean {
  return key.startsWith("catchlight.");
}

type Editing = {
  key: string;
  value: string;
  /** Kept as they were; only the payload is edited. */
  maintained?: { version: number; refs: ExtensionRef[] };
  readOnly: boolean;
};

export function ExtensionsPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const { extensions } = useModelQuery(session, { cmd: "extension_list" }, "extensions");
  const { run, busy } = useCommand(session, onError);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [problem, setProblem] = useState("");
  const update = (patch: Partial<Editing>) => setEditing((e) => (e ? { ...e, ...patch } : e));
  return (
    <>
      <p data-catchlight-hint="">
        Optional data for your tools and pipeline. Stored with the model.
      </p>
      {extensions.map((ext) => {
        const official = isOfficial(ext.key);
        const value = ext.value;
        const payload =
          value.kind === "json" || (value.kind === "maintained" && value.value != null)
            ? value.value
            : undefined;
        return (
          <div data-catchlight-extension="" key={ext.key}>
            <Icon name="code" />
            <span>
              {ext.key}
              <small>{describeExtension(value)}</small>
            </span>
            {payload !== undefined && (
              <button
                type="button"
                onClick={() => {
                  setProblem("");
                  setEditing({
                    key: ext.key,
                    value: JSON.stringify(payload, null, 2),
                    ...(value.kind === "maintained"
                      ? { maintained: { version: value.version, refs: value.refs } }
                      : {}),
                    readOnly: official,
                  });
                }}
              >
                {official ? "View" : "Edit"}
              </button>
            )}
            {!official && (
              <IconButton
                icon="trash"
                label={`Remove ${ext.key}`}
                onClick={() => void run({ cmd: "extension_delete", key: ext.key })}
              />
            )}
          </div>
        );
      })}
      <button
        type="button"
        data-catchlight-action=""
        onClick={() => {
          setProblem("");
          setEditing({ key: "", value: "{\n  \n}", readOnly: false });
        }}
      >
        <Icon name="plus" />
        Add extension
      </button>
      {editing && (
        <Modal title="Extension" onClose={() => setEditing(null)}>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (editing.readOnly) return;
              try {
                const parsed: unknown = JSON.parse(editing.value);
                setProblem("");
                void run({
                  cmd: "extension_set",
                  key: editing.key,
                  value: editing.maintained
                    ? { kind: "maintained", ...editing.maintained, value: parsed }
                    : { kind: "json", value: parsed },
                }).then((body) => {
                  if (body) setEditing(null);
                });
              } catch {
                setProblem("Enter valid JSON before saving.");
              }
            }}
          >
            <label data-catchlight-form-label="">
              Key
              <input
                autoFocus
                required
                readOnly={!!editing.maintained || editing.readOnly}
                value={editing.key}
                placeholder="studio.notes"
                pattern="[^.]+\..+"
                onChange={(e) => update({ key: e.currentTarget.value })}
              />
            </label>
            {editing.maintained && (
              <div data-catchlight-extension-refs="">
                <p data-catchlight-hint="">
                  Version {editing.maintained.version}. The editor keeps these references current
                  as the model changes.
                </p>
                {editing.maintained.refs.length === 0 ? (
                  <p data-catchlight-hint="">No references.</p>
                ) : (
                  <ul aria-label="References">
                    {editing.maintained.refs.map((ref, i) => (
                      <li key={i}>{describeRef(ref)}</li>
                    ))}
                  </ul>
                )}
              </div>
            )}
            <label data-catchlight-form-label="">
              {editing.maintained ? "JSON payload" : "JSON value"}
              <textarea
                required
                readOnly={editing.readOnly}
                spellCheck={false}
                rows={10}
                value={editing.value}
                onInput={(e) => update({ value: e.currentTarget.value })}
                onChange={noop}
              />
            </label>
            {problem && (
              <p role="alert" data-catchlight-warning="">
                {problem}
              </p>
            )}
            <div data-catchlight-dialog-actions="">
              <button type="button" onClick={() => setEditing(null)}>
                {editing.readOnly ? "Close" : "Cancel"}
              </button>
              {!editing.readOnly && (
                <button type="submit" data-primary="" disabled={busy}>
                  Save extension
                </button>
              )}
            </div>
          </form>
        </Modal>
      )}
    </>
  );
}
