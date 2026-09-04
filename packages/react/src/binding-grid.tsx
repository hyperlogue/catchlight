/**
 * One param's bindings on one node, as grids of keypoints.
 *
 * A row per binding target, a cell per keypoint. The grid is the product of
 * the binding's params' key positions, so it is exactly as wide as the param
 * has keys and one row tall unless a second param is driving it too.
 *
 * **A cell says whether anybody authored it.** `data-set` and `data-unset` are
 * the whole point of the read behind this: the model stores only the cells a
 * rigger keyed and derives the rest at puppet build, so an empty cell is a
 * state to act on rather than a zero. A `deform` binding's cells are authored
 * with a vertex list instead of a number, so they read as set and hold no
 * value to edit.
 *
 * **Clicking a cell is a pose, not an edit.** It moves the puppet to that
 * cell's key on each of the binding's params, so what the canvas shows and
 * what the cell describes are the same thing. Authoring is the number in the
 * cell, and every other control on the row.
 *
 * **Two-param bindings are shown, not created.** The add control makes a
 * binding on the one param the panel is showing; a grid that spans two is an
 * XY pad's business, and this part draws the one it finds.
 */

import type {
  BindingInfo,
  Interpolate,
  NodeId,
  ParamId,
  ParamInfo,
  ScalarTarget,
  Session,
} from "@catchlight/core";
import { useState } from "react";
import type { ComponentProps, KeyboardEvent } from "react";

import {
  BINDING_TARGETS,
  INTERPOLATE_MODES,
  bindingsOfParam,
  useBindings,
  valueAtKey,
} from "./bindings.js";
import { useParamActions } from "./param-actions.js";
import { useParams } from "./replica.js";

/** What a failed edit is told to, when a host passed nothing. */
type ErrorSink = ((cause: unknown) => void) | undefined;

/** One cell of one binding, as this panel points at it. */
interface CellRef {
  binding: string;
  x: number;
  y: number;
}

// `onError` is also a DOM event on every element; this one wins.
export interface BindingGridRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  /** The node whose bindings are shown. Nothing selected draws nothing. */
  node: NodeId | undefined;
  /** The param the panel is showing. Nothing chosen draws nothing. */
  param: ParamId | undefined;
  onError?: ErrorSink;
}

export function BindingGridRoot({
  session,
  node,
  param,
  onError,
  ...rest
}: BindingGridRootProps) {
  const actions = useParamActions(session);
  const params = useParams(session);
  const all = useBindings(session, node);
  const bindings = bindingsOfParam(all, param);
  const [selected, setSelected] = useState<CellRef | undefined>(undefined);
  const [target, setTarget] = useState<ScalarTarget>(BINDING_TARGETS[0] ?? "tx");
  const [copying, setCopying] = useState<CellRef | undefined>(undefined);

  if (node === undefined || param === undefined) {
    return <div data-catchlight-binding-grid="" data-empty="" {...rest} />;
  }

  const paramInfo = (id: string): ParamInfo | undefined => params.find((p) => p.id === id);

  /** Poses every param this binding is keyed by at the cell's own key. */
  const pose = (binding: BindingInfo, x: number, y: number): void => {
    const along = paramInfo(binding.param);
    if (along) session.setParam(binding.param, valueAtKey(along, x));
    const up = binding.param_y ? paramInfo(binding.param_y) : undefined;
    if (up && binding.param_y) session.setParam(binding.param_y, valueAtKey(up, y));
  };

  const click = (binding: BindingInfo, x: number, y: number): void => {
    const at: CellRef = { binding: keyOf(binding), x, y };
    // A copy that was armed on this binding lands on the cell that ends it;
    // anywhere else cancels it rather than copying somewhere unexpected.
    if (copying) {
      setCopying(undefined);
      if (copying.binding === at.binding) {
        report(
          onError,
          actions.copyKey(node, binding.target, paramsOf(binding), [copying.x, copying.y], [x, y]),
        );
        return;
      }
    }
    setSelected(at);
    pose(binding, x, y);
  };

  const commit = (binding: BindingInfo, x: number, y: number, text: string): void => {
    // A deform cell holds a vertex list, so there is no number to type into
    // it and `binding_key` would refuse the target anyway.
    if (binding.target === "deform") return;
    const value = Number(text);
    const was = binding.keys[y]?.[x];
    if (text.trim() === "" || !Number.isFinite(value) || value === was) return;
    report(onError, actions.setKey(node, binding.target, paramsOf(binding), [x, y], value));
  };

  return (
    <div
      data-catchlight-binding-grid=""
      data-node={node}
      data-copying={copying ? "" : undefined}
      {...rest}
    >
      <div data-catchlight-binding-add="">
        <select
          data-catchlight-binding-target=""
          aria-label="Binding target"
          value={target}
          onChange={(event) => setTarget(event.currentTarget.value as ScalarTarget)}
        >
          {BINDING_TARGETS.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        <button
          type="button"
          data-catchlight-binding-add-submit=""
          onClick={() => report(onError, actions.addBinding(node, target, { param }))}
        >
          Bind
        </button>
      </div>

      {bindings.map((binding) => {
        const id = keyOf(binding);
        return (
          <div
            key={id}
            data-catchlight-binding=""
            data-target={binding.target}
            data-param={binding.param}
            data-param-y={binding.param_y ?? undefined}
          >
            <div data-catchlight-binding-head="">
              <span data-catchlight-binding-name="">{binding.target}</span>
              <select
                data-catchlight-binding-interpolate=""
                aria-label={`${binding.target} interpolation`}
                value={binding.interpolate}
                onChange={(event) =>
                  report(
                    onError,
                    actions.interpolate(
                      node,
                      binding.target,
                      paramsOf(binding),
                      event.currentTarget.value as Interpolate,
                    ),
                  )
                }
              >
                {INTERPOLATE_MODES.map((mode) => (
                  <option key={mode} value={mode}>
                    {mode}
                  </option>
                ))}
              </select>
              <button
                type="button"
                data-catchlight-binding-invert=""
                title="Negate every authored value"
                onClick={() =>
                  report(onError, actions.invert(node, binding.target, paramsOf(binding)))
                }
              >
                Invert
              </button>
              <button
                type="button"
                data-catchlight-binding-delete=""
                title={`Delete the ${binding.target} binding`}
                onClick={() =>
                  report(onError, actions.deleteBinding(node, binding.target, paramsOf(binding)))
                }
              >
                ×
              </button>
            </div>
            {binding.keys.map((row, y) => (
              <div data-catchlight-binding-row="" data-y={y} key={y}>
                {row.map((value, x) => {
                  const authored = binding.authored[y]?.[x] === true;
                  const isSelected =
                    selected?.binding === id && selected.x === x && selected.y === y;
                  return (
                    <input
                      // Keyed on what it was built from, so an edit from
                      // anywhere redraws it and nothing is mirrored.
                      key={`${x}:${value ?? "-"}:${String(authored)}`}
                      type="number"
                      step="any"
                      data-catchlight-binding-cell=""
                      data-cell={`${x},${y}`}
                      data-set={authored ? "" : undefined}
                      data-unset={authored ? undefined : ""}
                      data-selected={isSelected ? "" : undefined}
                      aria-label={`${binding.target} cell ${x},${y}`}
                      // A deform cell holds a vertex list; there is no number
                      // to type into it, and `binding_key` refuses the target.
                      readOnly={binding.target === "deform"}
                      defaultValue={value === null ? "" : String(value)}
                      onClick={() => click(binding, x, y)}
                      onBlur={(event) => commit(binding, x, y, event.currentTarget.value)}
                      onKeyDown={commitOnEnter}
                    />
                  );
                })}
              </div>
            ))}
            {selected?.binding === id ? (
              <div data-catchlight-binding-cell-actions="">
                <button
                  type="button"
                  data-catchlight-binding-reset=""
                  title="Author the identity value here"
                  onClick={() =>
                    report(
                      onError,
                      actions.resetKey(node, binding.target, paramsOf(binding), [
                        selected.x,
                        selected.y,
                      ]),
                    )
                  }
                >
                  Reset
                </button>
                <button
                  type="button"
                  data-catchlight-binding-unset=""
                  title="Un-author this cell"
                  onClick={() =>
                    report(
                      onError,
                      actions.unsetKey(node, binding.target, paramsOf(binding), [
                        selected.x,
                        selected.y,
                      ]),
                    )
                  }
                >
                  Unset
                </button>
                <button
                  type="button"
                  data-catchlight-binding-copy=""
                  data-armed={copying ? "" : undefined}
                  title="Copy this cell's value into the next cell clicked"
                  onClick={() => setCopying(copying ? undefined : selected)}
                >
                  {copying ? "Pick a cell" : "Copy to…"}
                </button>
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

export const BindingGrid = { Root: BindingGridRoot };

/** What names this binding among a node's: its target and its params. */
function keyOf(binding: BindingInfo): string {
  return `${binding.target}|${binding.param}|${binding.param_y ?? ""}`;
}

/** The binding's params, as every binding command carries them. */
function paramsOf(binding: BindingInfo): { param: ParamId; param_y?: ParamId | null } {
  return { param: binding.param, param_y: binding.param_y ?? null };
}

/** Enter commits the cell the way leaving it does. */
function commitOnEnter(event: KeyboardEvent<HTMLInputElement>): void {
  if (event.key === "Enter") event.currentTarget.blur();
}

/** Hands a failed edit to the host, or says so where a developer will see it. */
function report(onError: ErrorSink, work: Promise<unknown>): void {
  void work.catch((cause: unknown) => {
    if (onError) onError(cause);
    else console.warn("catchlight: the binding edit failed", cause);
  });
}
