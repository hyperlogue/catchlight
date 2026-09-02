/**
 * One param, as a range input.
 *
 * **A pose is not an edit.** Moving this changes what the puppet is showing
 * and nothing in the document: no revision, no undo entry, nothing to save. So
 * the value cannot come from the revision the way a tree does — it comes from
 * the replica's pose, which announces itself on the repaint channel. Reading a
 * number there per repaint costs a map lookup, and React re-renders only when
 * the number actually differs.
 */

import type { ParamInfo, Session } from "@catchlight/core";
import { useCallback, useSyncExternalStore } from "react";
import type { ComponentProps } from "react";

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

export const ParamSlider = { Root: ParamSliderRoot };

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
