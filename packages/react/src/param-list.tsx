/**
 * Every param the model carries, one row each — and the two controls that
 * change what is in the list.
 *
 * The list is a replica read, so it moves when the document does — a param
 * added by an agent on the socket appears here without this component knowing
 * anything happened. The render prop is the composition seam: a host that
 * wants a name, a value readout and a binding count writes the row, and still
 * gets the list.
 *
 * **The add form and the per-param fields are separate parts, not part of the
 * list.** A host that wants a read-only column of sliders should get exactly
 * that from [`ParamListRoot`]; making it grow a form would put an edit into
 * every panel that lists params. So they compose instead, and the assembled
 * editor places all three.
 *
 * **A field commits on blur or Enter, never per keystroke.** Every one of
 * these is a document command with a revision and an undo entry behind it, and
 * typing "0.25" one character at a time is not four edits. The inputs are
 * uncontrolled and keyed on the values they were built from, so a change from
 * anywhere — this form, another panel, an agent — redraws them.
 */

import type { ParamInfo, Session } from "@catchlight/core";
import { useState } from "react";
import type { ComponentProps, KeyboardEvent, ReactNode } from "react";

import { useParamActions } from "./param-actions.js";
import { ParamSliderRoot } from "./param-slider.js";
import { useParams } from "./replica.js";

/** What a failed edit is told to, when a host passed nothing. */
type ErrorSink = ((cause: unknown) => void) | undefined;

export interface ParamListRootProps extends Omit<ComponentProps<"div">, "children"> {
  session: Session;
  children?: (param: ParamInfo) => ReactNode;
}

export function ParamListRoot({ session, children, ...rest }: ParamListRootProps) {
  const params = useParams(session);
  return (
    <div role="list" data-catchlight-param-list="" {...rest}>
      {params.map((param) => (
        <div role="listitem" data-catchlight-param-item="" data-param={param.id} key={param.id}>
          {children ? children(param) : <ParamSliderRoot session={session} param={param} />}
        </div>
      ))}
    </div>
  );
}

// `onError` is also a DOM event on every element; this one wins.
export interface ParamAddRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  onError?: ErrorSink;
}

/**
 * The control that creates a param: a name and the range it runs over.
 *
 * Key positions are left to the editor, which keys a new param at both
 * endpoints — the strip is where a rigger adds the ones in between, at a value
 * they can see the puppet standing at.
 */
export function ParamAddRoot({ session, onError, ...rest }: ParamAddRootProps) {
  const actions = useParamActions(session);
  const [name, setName] = useState("");
  const [min, setMin] = useState("0");
  const [max, setMax] = useState("1");
  const [fallback, setFallback] = useState("0");

  const submit = (): void => {
    const label = name.trim();
    if (label === "") return;
    setName("");
    report(
      onError,
      actions.add({
        name: label,
        min: number(min, 0),
        max: number(max, 1),
        default: number(fallback, 0),
      }),
    );
  };

  return (
    <div data-catchlight-param-add="" {...rest}>
      <input
        data-catchlight-param-add-name=""
        aria-label="New param name"
        placeholder="New param"
        value={name}
        onInput={(event) => setName(event.currentTarget.value)}
        onChange={noop}
        onKeyDown={(event) => {
          if (event.key === "Enter") submit();
        }}
      />
      <input
        type="number"
        step="any"
        data-catchlight-param-add-min=""
        aria-label="Minimum"
        value={min}
        onInput={(event) => setMin(event.currentTarget.value)}
        onChange={noop}
      />
      <input
        type="number"
        step="any"
        data-catchlight-param-add-max=""
        aria-label="Maximum"
        value={max}
        onInput={(event) => setMax(event.currentTarget.value)}
        onChange={noop}
      />
      <input
        type="number"
        step="any"
        data-catchlight-param-add-default=""
        aria-label="Default"
        value={fallback}
        onInput={(event) => setFallback(event.currentTarget.value)}
        onChange={noop}
      />
      <button
        type="button"
        data-catchlight-param-add-submit=""
        disabled={name.trim() === ""}
        onClick={submit}
      >
        Add
      </button>
    </div>
  );
}

export interface ParamFieldsRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  param: ParamInfo;
  onError?: ErrorSink;
}

/**
 * One param's own values: its label, its range, its default — and the button
 * that deletes it.
 *
 * The range is metadata, not a pose: key positions are normalized across it,
 * so widening a param moves nothing a binding authored. Deleting one is the
 * opposite, and takes every binding it drove with it — the same deliberate act
 * as renaming an Id, which is why it is a button of its own rather than a key
 * on the row.
 */
export function ParamFieldsRoot({ session, param, onError, ...rest }: ParamFieldsRootProps) {
  const actions = useParamActions(session);
  const stamp = `${param.name}|${param.min}|${param.max}|${param.default}`;

  const rename = (value: string): void => {
    const name = value.trim();
    if (name === "" || name === param.name) return;
    report(onError, actions.set(param.id, { name }));
  };

  const setNumber = (field: "min" | "max" | "default", value: string): void => {
    const next = Number(value);
    if (!Number.isFinite(next) || next === param[field]) return;
    report(onError, actions.set(param.id, { [field]: next }));
  };

  return (
    <div data-catchlight-param-fields="" data-param={param.id} {...rest}>
      <input
        key={`name:${stamp}`}
        data-catchlight-param-rename=""
        aria-label={`${param.name} name`}
        defaultValue={param.name}
        onBlur={(event) => rename(event.currentTarget.value)}
        onKeyDown={commitOnEnter}
      />
      {(["min", "max", "default"] as const).map((field) => (
        <input
          key={`${field}:${stamp}`}
          type="number"
          step="any"
          data-catchlight-param-field={field}
          aria-label={`${param.name} ${field}`}
          defaultValue={param[field]}
          onBlur={(event) => setNumber(field, event.currentTarget.value)}
          onKeyDown={commitOnEnter}
        />
      ))}
      <button
        type="button"
        data-catchlight-param-delete=""
        title={`Delete ${param.name} and its ${param.bindings} binding(s)`}
        onClick={() => report(onError, actions.remove(param.id))}
      >
        ×
      </button>
    </div>
  );
}

export const ParamList = { Root: ParamListRoot };
export const ParamAdd = { Root: ParamAddRoot };
export const ParamFields = { Root: ParamFieldsRoot };

/**
 * React wants a controlled input to declare a change handler, and its
 * `onChange` is the same native `input` event `onInput` already took. Handling
 * it once, under the name the DOM uses, is what keeps it from firing twice.
 */
function noop(): void {}

/** Enter commits the field the way leaving it does. */
function commitOnEnter(event: KeyboardEvent<HTMLInputElement>): void {
  if (event.key === "Enter") event.currentTarget.blur();
}

/** What a form field says, or `fallback` when it says nothing usable. */
function number(text: string, fallback: number): number {
  const value = Number(text);
  return Number.isFinite(value) ? value : fallback;
}

/** Hands a failed edit to the host, or says so where a developer will see it. */
function report(onError: ErrorSink, work: Promise<unknown>): void {
  void work.catch((cause: unknown) => {
    if (onError) onError(cause);
    else console.warn("catchlight: the param edit failed", cause);
  });
}
