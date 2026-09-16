/**
 * One param: the range input that poses it, and the strip of its key
 * positions.
 *
 * **A pose is not an edit.** Moving this changes what the puppet is showing
 * and nothing in the model: no revision, no undo entry, nothing to save. So
 * the value cannot come from the revision the way a tree does — it comes from
 * the replica's pose, which announces itself on the repaint channel. Reading a
 * number there per repaint costs a map lookup, and React re-renders only when
 * the number actually differs.
 */

import type { BindingInfo, NodeId, ParamInfo, Session } from "@catchlight/core";
import { useCallback, useRef, useState, useSyncExternalStore } from "react";
import type { ComponentProps, PointerEvent as ReactPointerEvent } from "react";

import { keyIndexNear, normalizedValue, valueAtKey } from "./bindings.js";
import { useParamActions } from "./param-actions.js";

/** What a failed edit is told to, when a host passed nothing. */
type ErrorSink = ((cause: unknown) => void) | undefined;

/** Whatever React currently calls a range input's live-value event. */
type InputHandler = NonNullable<ComponentProps<"input">["onInput"]>;

export interface ParamSliderRootProps extends Omit<
  ComponentProps<"input">,
  "type" | "value" | "min" | "max" | "step"
> {
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
  revision: number;
}

// `onError` is also a DOM event on every element; this one wins.
export interface ParamKeysRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  param: ParamInfo;
  onError?: ErrorSink;
  /** Grid being edited. Without one, this is a pose-only range/default strip. */
  binding?: BindingInfo;
  node?: NodeId;
}

/** Binding-local key positions, or range/default pose shortcuts when no
 * binding is selected. Clicking poses; dragging a binding marker commits once.
 * Insert/delete affect only this binding's chosen axis. At least one position
 * remains, and endpoints can be moved or removed like other positions. */
export function ParamKeysRoot({ session, param, binding, node, onError, ...rest }: ParamKeysRootProps) {
  const actions = useParamActions(session);
  const value = useParamValue(session, param);
  const track = useRef<HTMLDivElement | null>(null);
  const [drag, setDrag] = useState<KeyDrag | undefined>(undefined);
  const suppressClick = useRef(false);

  const positions = binding?.key_positions[binding.param === param.id ? 0 : 1]
    ?? [...new Set([0, normalizedValue(param, param.default), 1])].sort((a, b) => a - b);
  const address = binding && node ? { node, target: binding.target, param: binding.param, param_y: binding.param_y ?? null } : undefined;
  const last = positions.length - 1;
  const at = normalizedValue(param, value);
  const on = keyIndexNear(param, value, positions);
  const editable = (index: number): boolean => !!address && index >= 0 && index <= last;

  /** Where along the track a pointer is, on the 0..1 scale key positions use. */
  const positionOf = (event: ReactPointerEvent<HTMLElement>): number => {
    const box = track.current?.getBoundingClientRect();
    if (!box || box.width <= 0) return at;
    return Math.min(1, Math.max(0, (event.clientX - box.left) / box.width));
  };

  const down = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    suppressClick.current = false;
    if (!editable(index)) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    setDrag({ index, at: positions[index] ?? 0, moved: false, revision: session.getRevision() });
  };

  const move = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    if (!drag || drag.index !== index) return;
    const to = between(positions, index, positionOf(event));
    const budged = Math.abs(to - (positions[index] ?? 0)) > 1e-4;
    setDrag({ ...drag, index, at: to, moved: drag.moved || budged });
  };

  const up = (event: ReactPointerEvent<HTMLElement>, index: number): void => {
    const held = drag;
    setDrag(undefined);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    // A press that never moved is a click, and a click is a pose.
    if (!held || held.index !== index || !held.moved) {
      session.setParam(param.id, valueAtKey(param, index, positions));
      return;
    }
    suppressClick.current = true;
    if (address) report(onError, actions.moveKey(address, param.id, index, held.at, held.revision).then(() => {
      session.setParam(param.id, param.min + held.at * (param.max - param.min));
    }));
  };

  return (
    <div data-catchlight-param-keys="" data-param={param.id} {...rest}>
      {/* The one inline style in this package's parts other than the
          viewport's: where a marker sits along the track *is* the key
          position, so it is data, not decoration a theme could supply. */}
      <div ref={track} data-catchlight-param-key-track="">
        {positions.map((position, index) => (
          <button
            type="button"
            key={index}
            data-catchlight-param-key=""
            data-index={index}
            data-editable={editable(index) ? "" : undefined}
            data-current={on === index ? "" : undefined}
            data-dragging={drag?.index === index ? "" : undefined}
            style={{
              left: `${(drag?.index === index ? drag.at : position) * 100}%`,
            }}
            aria-label={`${param.name} key ${index}`}
            title={`${valueAtKey(param, index, positions)}${address ? " · Drag to move this binding’s key position" : " · Click to pose"}`}
            onPointerDown={(event) => down(event, index)}
            onPointerMove={(event) => move(event, index)}
            onPointerUp={(event) => up(event, index)}
            onPointerCancel={() => setDrag(undefined)}
            onLostPointerCapture={() => setDrag(undefined)}
            onClick={() => {
              if (suppressClick.current) { suppressClick.current = false; return; }
              session.setParam(param.id, valueAtKey(param, index, positions));
            }}
          />
        ))}
      </div>
      {address && <div data-catchlight-param-key-actions=""><button
        type="button"
        data-catchlight-param-key-insert=""
        aria-label={`Add ${param.name} key position`}
        // A binding may omit either endpoint; insert any missing position.
        disabled={on !== undefined}
        title="Add a key at the current value"
        onClick={() => report(onError, actions.insertKey(address, param.id, at))}
      >
        +
      </button>
      <button
        type="button"
        data-catchlight-param-key-delete=""
        aria-label={`Delete ${param.name} key position`}
        disabled={on === undefined || positions.length < 2}
        title="Delete the key at the current value"
        onClick={() => {
          if (on !== undefined) report(onError, actions.deleteKey(address, param.id, on));
        }}
      >
        −
      </button></div>}
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
  const low = index === 0 ? 0 : positions[index - 1]! + gap;
  const high = index === positions.length - 1 ? 1 : positions[index + 1]! - gap;
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
export function useParamValue(session: Session, param: ParamInfo): number {
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
