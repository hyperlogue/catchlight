/**
 * One param: the range input that poses it, and the strip of its key
 * positions.
 *
 * **A pose is not an edit.** Moving this changes what the puppet is showing
 * and nothing in the document: no revision, no undo entry, nothing to save. So
 * the value cannot come from the revision the way a tree does — it comes from
 * the replica's pose, which announces itself on the repaint channel. Reading a
 * number there per repaint costs a map lookup, and React re-renders only when
 * the number actually differs.
 */

import type { ParamInfo, Session } from "@catchlight/core";
import { useCallback, useRef, useState, useSyncExternalStore } from "react";
import type { ComponentProps, PointerEvent as ReactPointerEvent } from "react";

import { keyIndexNear, normalizedValue, valueAtKey } from "./bindings.js";
import { useParamActions } from "./param-actions.js";

/** What a failed edit is told to, when a host passed nothing. */
type ErrorSink = ((cause: unknown) => void) | undefined;

/** Whatever React currently calls a range input's live-value event. */
type InputHandler = NonNullable<ComponentProps<"input">["onInput"]>;

export interface ParamSliderRootProps
  extends Omit<ComponentProps<"input">, "type" | "value" | "min" | "max" | "step"> {
  session: Session;
  param: ParamInfo;
}

export function ParamSliderRoot({ session, param, onInput, ...rest }: ParamSliderRootProps) {
  const value = useParamValue(session, param);

  const handleInput: InputHandler = (event) => {
    const next = Number(event.currentTarget.value);
    if (Number.isFinite(next)) session.setParam(param.id, next);
    onInput?.(event);
  };

  return (
    <input
      type="range"
      data-catchlight-param-slider=""
      data-param={param.id}
      aria-label={param.name}
      min={param.min}
      max={param.max}
      // A param is continuous. `key_positions` says where its bindings sample
      // it, not where the slider is allowed to stop.
      step="any"
      value={value}
      onInput={handleInput}
      onChange={noop}
      {...rest}
    />
  );
}

/** A marker being dragged, and how far it has come. */
interface KeyDrag {
  index: number;
  at: number;
  moved: boolean;
}

// `onError` is also a DOM event on every element; this one wins.
export interface ParamKeysRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  param: ParamInfo;
  onError?: ErrorSink;
}

/**
 * The param's key positions, as markers along the same 0..1 track the slider
 * runs on.
 *
 * Clicking a marker poses the param exactly on that key, which is what makes a
 * grid cell and the puppet agree about which cell is being looked at. Dragging
 * one is a `param_key_move` — an edit — and it is committed on release rather
 * than per pointer move, so a drag of any length is one revision and one undo
 * entry.
 *
 * The two buttons act at the pose: insert adds a key where the slider is
 * standing, delete removes the interior key it is standing on. Both are
 * disabled when the pose is somewhere they cannot mean anything, which is also
 * what stops a client sending a command the model would refuse.
 */
export function ParamKeysRoot({ session, param, onError, ...rest }: ParamKeysRootProps) {
  const actions = useParamActions(session);
  const value = useParamValue(session, param);
  const track = useRef<HTMLDivElement | null>(null);
  const [drag, setDrag] = useState<KeyDrag | undefined>(undefined);

  const last = param.key_positions.length - 1;
  const at = normalizedValue(param, value);
  const on = keyIndexNear(param, value);
  const interior = (index: number): boolean => index > 0 && index < last;

  /** Where along the track a pointer is, on the 0..1 scale key positions use. */
  const positionOf = (event: ReactPointerEvent<HTMLElement>): number => {
    const box = track.current?.getBoundingClientRect();
    if (!box || box.width <= 0) return at;
    return Math.min(1, Math.max(0, (event.clientX - box.left) / box.width));
  };

  const down = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    if (!interior(index)) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    setDrag({ index, at: param.key_positions[index] ?? 0, moved: false });
  };

  const move = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    if (!drag || drag.index !== index) return;
    const to = between(param.key_positions, index, positionOf(event));
    const budged = Math.abs(to - (param.key_positions[index] ?? 0)) > 1e-4;
    setDrag({ index, at: to, moved: drag.moved || budged });
  };

  const up = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    const held = drag;
    setDrag(undefined);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    // A press that never moved is a click, and a click is a pose.
    if (!held || held.index !== index || !held.moved) {
      session.setParam(param.id, valueAtKey(param, index));
      return;
    }
    report(onError, actions.moveKey(param.id, index, held.at));
  };

  return (
    <div data-catchlight-param-keys="" data-param={param.id} {...rest}>
      {/* The one inline style in this package's parts other than the
          viewport's: where a marker sits along the track *is* the key
          position, so it is data, not decoration a theme could supply. */}
      <div ref={track} data-catchlight-param-key-track="">
        {param.key_positions.map((position, index) => (
          <button
            type="button"
            key={index}
            data-catchlight-param-key=""
            data-index={index}
            data-interior={interior(index) ? "" : undefined}
            data-current={on === index ? "" : undefined}
            data-dragging={drag?.index === index ? "" : undefined}
            style={{ left: `${(drag?.index === index ? drag.at : position) * 100}%` }}
            aria-label={`${param.name} key ${index}`}
            onPointerDown={(event) => down(event, index)}
            onPointerMove={(event) => move(event, index)}
            onPointerUp={(event) => up(event, index)}
            onClick={() => session.setParam(param.id, valueAtKey(param, index))}
          />
        ))}
      </div>
      <button
        type="button"
        data-catchlight-param-key-insert=""
        // A key position lands strictly inside (0, 1), and never on one that
        // is already there.
        disabled={at <= 0 || at >= 1 || on !== undefined}
        title="Add a key at the current value"
        onClick={() => report(onError, actions.insertKey(param.id, at))}
      >
        +
      </button>
      <button
        type="button"
        data-catchlight-param-key-delete=""
        disabled={on === undefined || !interior(on)}
        title="Delete the key at the current value"
        onClick={() => {
          if (on !== undefined) report(onError, actions.deleteKey(param.id, on));
        }}
      >
        −
      </button>
    </div>
  );
}

export const ParamSlider = { Root: ParamSliderRoot };
export const ParamKeys = { Root: ParamKeysRoot };

/**
 * `to`, held strictly between key `index`'s neighbours — which is where the
 * model requires a moved key position to land.
 *
 * Clamping here rather than letting the editor refuse keeps a drag continuous:
 * the marker stops at the neighbour instead of the whole gesture failing at
 * the moment it went one pixel too far.
 */
function between(positions: number[], index: number, to: number): number {
  const gap = 1e-4;
  const low = (positions[index - 1] ?? 0) + gap;
  const high = (positions[index + 1] ?? 1) - gap;
  if (low >= high) return positions[index] ?? to;
  return Math.min(high, Math.max(low, to));
}

/** Hands a failed edit to the host, or says so where a developer will see it. */
function report(onError: ErrorSink, work: Promise<unknown>): void {
  void work.catch((cause: unknown) => {
    if (onError) onError(cause);
    else console.warn("catchlight: the param edit failed", cause);
  });
}

/** What the puppet is posed at, or the param's default before anything posed it. */
function useParamValue(session: Session, param: ParamInfo): number {
  const read = useCallback(
    () => session.paramValue(param.id) ?? param.default,
    [session, param.id, param.default],
  );
  return useSyncExternalStore(session.onInvalidate, read, read);
}

/**
 * React wants a controlled input to declare a change handler, and its
 * `onChange` is the same native `input` event `onInput` already took. Handling
 * it once, under the name the DOM uses, is what keeps a host's own `onInput`
 * from firing twice.
 */
function noop(): void {}
