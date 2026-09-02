/**
 * One node in full: what it is, and every field a `node_set` can change.
 *
 * **The reply is the form.** `node_info` hands back the settable fields under
 * [`NodePatch`]'s own names, and leaves out the ones the node's kind does not
 * carry — colour on a group, `mg_*` on anything but a mesh group. So which
 * controls exist is read off the reply rather than decided from a table here:
 * a kind that grows a field grows a row, and this file does not learn about
 * it. The one exception is `texture`, absent both on a part that draws none
 * and on a node that could never have one; `kind` is what tells those apart.
 *
 * **What is being typed is local; everything else is the replica.** A number
 * a person is halfway through entering is React state until it commits, or a
 * revision landing mid-edit — an agent on the socket, another tab — would
 * replace the text under the cursor. Nothing else is mirrored: after a commit
 * the read wins, and the read re-runs when the revision moves.
 *
 * **A commit is one field.** Numbers and text go on Enter or blur and only
 * when the value actually changed; checkboxes and selects go immediately. An
 * unchanged blur authors nothing, so tabbing through the panel costs no undo
 * entries.
 *
 * There is no live preview here. A drag previews through the scratch because
 * it runs per pointer move; a number typed into a box does not, and showing it
 * before it commits would be a second uncommitted value to reconcile.
 */

import type { NodeInfo, NodePatch, Session, TexInfo } from "@catchlight/core";
import { useCallback, useState } from "react";
import type { ComponentProps, ReactNode } from "react";

import { useNodePatch } from "./node-patch.js";
import { useNodeInfo, useReplica } from "./replica.js";
import { useSelection } from "./selection.js";

/**
 * Every blend mode the model has, in the order `BlendMode` declares them
 * (`crates/catchlight-core/src/components.rs`). The names are the wire
 * spelling: `node_set` refuses one it cannot parse rather than falling back to
 * Normal, so a select built from anything else is a command that fails.
 */
export const BLEND_MODES = [
  "Normal",
  "Multiply",
  "ColorDodge",
  "LinearDodge",
  "Screen",
  "ClipToLower",
  "SliceFromLower",
  "Overlay",
  "ColorBurn",
  "LinearBurn",
  "Darken",
  "Lighten",
  "Add",
  "Inverse",
  "Subtract",
] as const;

// `onError` is also a DOM event on every element; this one wins.
export interface InspectorRootProps extends Omit<ComponentProps<"div">, "onError"> {
  session: Session;
  onError?: (cause: unknown) => void;
}

export function InspectorRoot({ session, onError, ...rest }: InspectorRootProps) {
  const { node } = useSelection();
  const info = useNodeInfo(session, node);
  const patch = useNodePatch(session);
  const textures = useReplica(session, readTextures);
  const id = info?.id;

  /**
   * Sends one patch and reports a refusal, without ever rejecting: a control
   * clears the text it was holding once the send settles, and a promise that
   * throws would leave that text on screen for a value the model never took.
   */
  const submit = useCallback(
    (fields: NodePatch): Promise<void> => {
      if (id === undefined) return Promise.resolve();
      return patch(id, fields).catch((cause: unknown) => {
        if (onError) onError(cause);
        else console.warn("catchlight: setting a node field failed", cause);
      });
    },
    [patch, id, onError],
  );

  // Nothing selected, and a selection that outlived its node, are one state: a
  // selection is view state and does not follow a deletion.
  if (!info) {
    return (
      <div data-catchlight-inspector="" data-catchlight-inspector-empty="" {...rest}>
        <p>No node selected.</p>
      </div>
    );
  }

  return (
    <div data-catchlight-inspector="" {...rest}>
      {/* Keyed on the node, so moving the selection drops every draft rather
          than showing one node's half-typed number against another's. */}
      <Fields info={info} textures={textures} submit={submit} key={info.id} />
    </div>
  );
}

export const Inspector = { Root: InspectorRoot };

/** What a control does with a value it decided to author. */
type Submit = (fields: NodePatch) => Promise<void>;

function Fields({
  info,
  textures,
  submit,
}: {
  info: NodeInfo;
  textures: TexInfo[];
  submit: Submit;
}): ReactNode {
  // Pulled out as constants so a `!= null` here narrows inside the handlers
  // too: TypeScript keeps that for a `const`, and drops it for `info.tint`.
  const { translate, rotate, scale, opacity, mask_threshold, blend_mode, tint, screen_tint } = info;
  const { propagate_meshgroup, mg_dynamic, mg_translate_children } = info;

  return (
    <>
      <Row label="Id" field="id">
        <span data-catchlight-inspector-value="">{info.id}</span>
      </Row>
      <Row label="Kind" field="kind">
        <span data-catchlight-inspector-value="">{info.kind}</span>
      </Row>
      <Row label="Parent" field="parent">
        <span data-catchlight-inspector-value="">{info.parent ?? "—"}</span>
      </Row>

      <Row label="Name" field="name">
        <TextInput
          field="name"
          label="Name"
          value={info.name}
          commit={(next) => submit({ name: next })}
        />
      </Row>
      <Row label="Translate" field="translate">
        <Axes
          field="translate"
          label="Translate"
          axes={XYZ}
          values={translate}
          commit={(axis, next) => submit({ translate: with3(translate, axis, next) })}
        />
      </Row>
      <Row label="Rotate" field="rotate">
        <Axes
          field="rotate"
          label="Rotate"
          axes={XYZ}
          values={rotate}
          commit={(axis, next) => submit({ rotate: with3(rotate, axis, next) })}
        />
      </Row>
      <Row label="Scale" field="scale">
        <Axes
          field="scale"
          label="Scale"
          axes={XY}
          values={scale}
          commit={(axis, next) => submit({ scale: with2(scale, axis, next) })}
        />
      </Row>
      <Row label="Z order" field="z_order">
        <NumberInput
          field="z_order"
          label="Z order"
          value={info.z_order}
          commit={(next) => submit({ z_order: next })}
        />
      </Row>
      <Row label="Enabled" field="enabled">
        <CheckInput
          field="enabled"
          label="Enabled"
          value={info.enabled}
          commit={(next) => submit({ enabled: next })}
        />
      </Row>
      <Row label="Lock to root" field="lock_to_root">
        <CheckInput
          field="lock_to_root"
          label="Lock to root"
          value={info.lock_to_root}
          commit={(next) => submit({ lock_to_root: next })}
        />
      </Row>

      {opacity != null && (
        <Row label="Opacity" field="opacity">
          <NumberInput
            field="opacity"
            label="Opacity"
            value={opacity}
            min={0}
            max={1}
            commit={(next) => submit({ opacity: next })}
          />
        </Row>
      )}
      {mask_threshold != null && (
        <Row label="Mask threshold" field="mask_threshold">
          <NumberInput
            field="mask_threshold"
            label="Mask threshold"
            value={mask_threshold}
            min={0}
            max={1}
            commit={(next) => submit({ mask_threshold: next })}
          />
        </Row>
      )}
      {blend_mode != null && (
        <Row label="Blend" field="blend_mode">
          <SelectInput
            field="blend_mode"
            label="Blend mode"
            value={blend_mode}
            options={blendOptions(blend_mode)}
            commit={(next) => submit({ blend_mode: next })}
          />
        </Row>
      )}
      {tint != null && (
        <Row label="Tint" field="tint">
          <Axes
            field="tint"
            label="Tint"
            axes={RGB}
            values={tint}
            min={0}
            max={1}
            commit={(axis, next) => submit({ tint: with3(tint, axis, next) })}
          />
        </Row>
      )}
      {screen_tint != null && (
        <Row label="Screen tint" field="screen_tint">
          <Axes
            field="screen_tint"
            label="Screen tint"
            axes={RGB}
            values={screen_tint}
            min={0}
            max={1}
            commit={(axis, next) => submit({ screen_tint: with3(screen_tint, axis, next) })}
          />
        </Row>
      )}
      {info.kind === "part" && (
        <Row label="Texture" field="texture">
          <SelectInput
            field="texture"
            label="Texture"
            value={info.texture ?? NO_TEXTURE}
            options={textureOptions(textures)}
            commit={(next) =>
              next === NO_TEXTURE ? submit({ clear_texture: true }) : submit({ texture: next })
            }
          />
        </Row>
      )}
      {propagate_meshgroup != null && (
        <Row label="Propagate mesh group" field="propagate_meshgroup">
          <CheckInput
            field="propagate_meshgroup"
            label="Propagate mesh group"
            value={propagate_meshgroup}
            commit={(next) => submit({ propagate_meshgroup: next })}
          />
        </Row>
      )}
      {mg_dynamic != null && (
        <Row label="Dynamic" field="mg_dynamic">
          <CheckInput
            field="mg_dynamic"
            label="Dynamic"
            value={mg_dynamic}
            commit={(next) => submit({ mg_dynamic: next })}
          />
        </Row>
      )}
      {mg_translate_children != null && (
        <Row label="Translate children" field="mg_translate_children">
          <CheckInput
            field="mg_translate_children"
            label="Translate children"
            value={mg_translate_children}
            commit={(next) => submit({ mg_translate_children: next })}
          />
        </Row>
      )}
    </>
  );
}

/** One labelled line. The label is visual; every control names itself. */
function Row({
  label,
  field,
  children,
}: {
  label: string;
  field: string;
  children: ReactNode;
}): ReactNode {
  return (
    <div data-catchlight-inspector-row="" data-field={field}>
      <span data-catchlight-inspector-label="">{label}</span>
      <div data-catchlight-inspector-controls="">{children}</div>
    </div>
  );
}

/** The components of one vector field, each its own box over the same key. */
function Axes({
  field,
  label,
  axes,
  values,
  min,
  max,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  axes: readonly string[];
  values: readonly number[];
  min?: number | undefined;
  max?: number | undefined;
  commit: (axis: number, next: number) => Promise<void>;
}): ReactNode {
  return (
    <>
      {values.map((value, axis) => {
        const name = axes[axis] ?? String(axis);
        return (
          <NumberInput
            key={name}
            field={field}
            axis={name}
            label={`${label} ${name}`}
            value={value}
            min={min}
            max={max}
            commit={(next) => commit(axis, next)}
          />
        );
      })}
    </>
  );
}

function NumberInput({
  field,
  label,
  value,
  axis,
  min,
  max,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  value: number;
  axis?: string | undefined;
  min?: number | undefined;
  max?: number | undefined;
  commit: (next: number) => Promise<void>;
}): ReactNode {
  const draft = useDraft(String(value), (text) => {
    const next = Number(text);
    // A box holding nothing, or `1.2.3`, is not an edit. Reverting is what a
    // person who typed it expects, and there is nothing to send.
    if (text.trim() === "" || !Number.isFinite(next) || next === value) return undefined;
    return commit(next);
  });

  return (
    <input
      type="number"
      data-catchlight-field={field}
      data-axis={axis}
      aria-label={label}
      min={min}
      max={max}
      // A transform is continuous; the arrows are a convenience, not the grid.
      step="any"
      value={draft.text}
      onInput={draft.type}
      onChange={noop}
      onBlur={draft.commit}
      onKeyDown={draft.keys}
    />
  );
}

function TextInput({
  field,
  label,
  value,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  value: string;
  commit: (next: string) => Promise<void>;
}): ReactNode {
  const draft = useDraft(value, (text) => (text === value ? undefined : commit(text)));
  return (
    <input
      type="text"
      data-catchlight-field={field}
      aria-label={label}
      value={draft.text}
      onInput={draft.type}
      onChange={noop}
      onBlur={draft.commit}
      onKeyDown={draft.keys}
    />
  );
}

function CheckInput({
  field,
  label,
  value,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  value: boolean;
  commit: (next: boolean) => Promise<void>;
}): ReactNode {
  return (
    <input
      type="checkbox"
      data-catchlight-field={field}
      aria-label={label}
      checked={value}
      onChange={(event) => {
        void commit(event.currentTarget.checked);
      }}
    />
  );
}

/** One option in a select: what is sent, and what is read. */
interface Option {
  value: string;
  label: string;
}

function SelectInput({
  field,
  label,
  value,
  options,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  value: string;
  options: Option[];
  commit: (next: string) => Promise<void>;
}): ReactNode {
  return (
    <select
      data-catchlight-field={field}
      aria-label={label}
      value={value}
      onChange={(event) => {
        const next = event.currentTarget.value;
        if (next === value) return;
        void commit(next);
      }}
    >
      {options.map((option) => (
        <option value={option.value} key={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

/**
 * React wants a controlled input to declare a change handler, and its
 * `onChange` is the same native `input` event `onInput` already took. Taking
 * it once, under the name the DOM uses, is what keeps a host's own handler
 * from firing twice — and what `ParamSliderRoot` does for the same reason.
 */
function noop(): void {}

/** What a text box is showing, and the three things that can happen to it. */
interface Draft {
  text: string;
  type(event: { currentTarget: { value: string } }): void;
  commit(): void;
  keys(event: { key: string; preventDefault(): void }): void;
}

/**
 * A box holding what is being typed, and the rules for letting go of it.
 *
 * `authored` decides whether the text is worth a command and sends it, or
 * answers `undefined` for the two cases that are not an edit: unchanged, and
 * unparseable. The draft outlives the send so the box does not flash the old
 * value for the length of a round trip; once the model has it, the read wins.
 */
function useDraft(shown: string, authored: (text: string) => Promise<void> | undefined): Draft {
  const [draft, setDraft] = useState<string | undefined>(undefined);

  const commit = (): void => {
    if (draft === undefined) return;
    const sending = authored(draft);
    if (!sending) {
      setDraft(undefined);
      return;
    }
    void sending.then(() => setDraft(undefined));
  };

  return {
    text: draft ?? shown,
    type: (event) => setDraft(event.currentTarget.value),
    commit,
    keys: (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        commit();
      } else if (event.key === "Escape") {
        // Dropping the draft is the whole of a cancel: the box goes back to
        // what the model says, and the blur that follows finds nothing to do.
        setDraft(undefined);
      }
    },
  };
}

const XYZ = ["x", "y", "z"] as const;
const XY = ["x", "y"] as const;
const RGB = ["r", "g", "b"] as const;

/**
 * What a part with no albedo shows, and what picking it authors.
 *
 * `node_set` reads `texture` as "point at this one" and reads the field being
 * absent as "unchanged", so the empty value cannot travel under that key at
 * all. `clear_texture` is the spelling for "draw none", which is why this
 * option commits a different field from every other one in the select.
 */
const NO_TEXTURE = "";

function blendOptions(current: string): Option[] {
  const names: string[] = [...BLEND_MODES];
  // A file can carry a mode this build does not list. Showing it is how the
  // select stays a readout as well as a control.
  if (!names.includes(current)) names.unshift(current);
  return names.map((name) => ({ value: name, label: name }));
}

function textureOptions(textures: TexInfo[]): Option[] {
  return [
    { value: NO_TEXTURE, label: "none" },
    ...textures.map((texture) => ({
      value: texture.id,
      label: `${texture.id} (${texture.width}×${texture.height})`,
    })),
  ];
}

type Vec3 = [number, number, number];
type Vec2 = [number, number];

function with3(v: Vec3, axis: number, next: number): Vec3 {
  const out: Vec3 = [v[0], v[1], v[2]];
  out[axis] = next;
  return out;
}

function with2(v: Vec2, axis: number, next: number): Vec2 {
  const out: Vec2 = [v[0], v[1]];
  out[axis] = next;
  return out;
}

const readTextures = (session: Session): TexInfo[] => session.textures();
