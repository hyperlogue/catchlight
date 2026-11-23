/**
 * One node's settable fields, as `node_info` reports them for its kind.
 *
 * A field being typed is local state until it commits, so a revision landing
 * mid-edit does not replace the text under the cursor. An unchanged blur
 * authors nothing, so tabbing through the panel costs no undo entries.
 */

import type { BlendMode, NodeInfo, NodePatch, Session, TexInfo } from "@catchlight/core";
import { useCallback, useState } from "react";
import type { ComponentProps, ReactNode } from "react";

import { useNodePatch } from "./node-patch.js";
import { useNodeInfo, useReplica } from "./replica.js";
import { useSelection } from "./selection.js";

/** Wire spellings, in `BlendMode` declaration order; `node_set` rejects any other name. */
export const BLEND_MODES: readonly BlendMode[] = [
  "normal",
  "multiply",
  "color_dodge",
  "linear_dodge",
  "screen",
  "clip_to_lower",
  "slice_from_lower",
  "overlay",
  "color_burn",
  "linear_burn",
  "darken",
  "lighten",
  "add",
  "inverse",
  "subtract",
];

type _EveryBlendModeListed =
  Exclude<BlendMode, (typeof BLEND_MODES)[number]> extends never ? true : never;

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

  // Never rejects: a control clears its draft once the send settles.
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

  // A selection is view state and outlives a deleted node.
  if (!info) {
    return (
      <div data-catchlight-inspector="" data-catchlight-inspector-empty="" {...rest}>
        <p>No node selected.</p>
      </div>
    );
  }

  return (
    <div data-catchlight-inspector="" {...rest}>
      {/* Keyed on the node so a selection change drops every draft. */}
      <Fields info={info} textures={textures} submit={submit} key={info.id} />
    </div>
  );
}

export const Inspector = { Root: InspectorRoot };

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
  // Destructured so `!= null` narrows inside the handlers too.
  const { translate, rotate, scale, opacity, mask_threshold, blend_mode, tint, screen_tint } = info;
  const { propagate_meshgroup, mg_translate_children } = info;

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
      <section data-catchlight-inspector-section="" aria-label="Transform">
        <h3>Transform</h3>
        <Row label="Position" field="translate">
          <Axes
            field="translate"
            label="Translate"
            axes={XYZ}
            values={translate}
            commit={(axis, next) => submit({ translate: with3(translate, axis, next) })}
          />
        </Row>
        <Row label="Rotation" field="rotate">
          <Axes
            field="rotate"
            unit="°"
            label="Rotate"
            axes={XYZ}
            values={rotate.map((v) => (v * 180) / Math.PI)}
            commit={(axis, next) => submit({ rotate: with3(rotate, axis, (next * Math.PI) / 180) })}
          />
        </Row>
        <Row label="Scale" field="scale">
          <Axes
            field="scale"
            unit="×"
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
        <Row label="Lock to root" field="lock_to_root">
          <CheckInput
            field="lock_to_root"
            label="Lock to root"
            value={info.lock_to_root}
            commit={(next) => submit({ lock_to_root: next })}
          />
        </Row>
      </section>
      <section data-catchlight-inspector-section="" aria-label="Appearance">
        <h3>Appearance</h3>
        <Row label="Enabled" field="enabled">
          <CheckInput
            field="enabled"
            label="Enabled"
            value={info.enabled}
            commit={(next) => submit({ enabled: next })}
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
              commit={(next) => submit({ texture: next === NO_TEXTURE ? null : next })}
            />
          </Row>
        )}
      </section>
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

function Axes({
  field,
  label,
  axes,
  values,
  unit,
  min,
  max,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  axes: readonly string[];
  values: readonly number[];
  unit?: string;
  min?: number | undefined;
  max?: number | undefined;
  commit: (axis: number, next: number) => Promise<void>;
}): ReactNode {
  return (
    <>
      {values.map((value, axis) => {
        const name = axes[axis] ?? String(axis);
        return (
          <label key={name} data-catchlight-axis-input="">
            <span data-catchlight-number-prefix="" aria-hidden="true">{name}</span>
            <NumberInput
              field={field}
              axis={name}
              label={`${label} ${name}`}
              value={value}
              min={min}
              max={max}
              commit={(next) => commit(axis, next)}
            />
            {unit && <span data-catchlight-number-unit="" aria-hidden="true">{unit}</span>}
          </label>
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
  const draft = useDraft(String(Math.round(value * 1000) / 1000), (text) => {
    const next = Number(text);
    // Empty or unparseable text is not an edit; the box reverts.
    if (
      text.trim() === "" ||
      !Number.isFinite(next) ||
      next === value ||
      (min !== undefined && next < min) ||
      (max !== undefined && next > max)
    )
      return undefined;
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

interface Option<T extends string = string> {
  value: T;
  label: string;
}

function SelectInput<T extends string>({
  field,
  label,
  value,
  options,
  commit,
}: {
  field: keyof NodePatch;
  label: string;
  value: T;
  options: Option<T>[];
  commit: (next: T) => Promise<void>;
}): ReactNode {
  return (
    <select
      data-catchlight-field={field}
      aria-label={label}
      value={value}
      onChange={(event) => {
        const next = event.currentTarget.value as T;
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

// React requires a controlled input's `onChange`; the native input event is
// taken once through `onInput` so a host's handler does not fire twice.
function noop(): void {}

interface Draft {
  text: string;
  type(event: { currentTarget: { value: string } }): void;
  commit(): void;
  keys(event: { key: string; preventDefault(): void }): void;
}

// The draft outlives the send so the box does not flash the old value for a
// round trip. `authored` returns `undefined` when the text is not an edit.
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
        setDraft(undefined);
      }
    },
  };
}

const XYZ = ["x", "y", "z"] as const;
const XY = ["x", "y"] as const;
const RGB = ["r", "g", "b"] as const;

// `texture` is a merge patch: absent is unchanged, `null` draws none. This
// option is the one that sends `null`.
const NO_TEXTURE = "";

function blendOptions(current: BlendMode): Option<BlendMode>[] {
  const names: BlendMode[] = [...BLEND_MODES];
  // A file can carry a mode this build does not list.
  if (!names.includes(current)) names.unshift(current);
  return names.map((name) => ({
    value: name,
    label: name.replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase()),
  }));
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
