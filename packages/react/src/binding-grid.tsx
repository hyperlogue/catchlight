/**
 * One param's bindings on one node, as grids of keypoints over each binding's
 * own key positions. `data-set`/`data-unset` mark authored cells; unset cells
 * are derived at puppet build. Clicking a cell poses, it does not edit.
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
  BINDING_LABELS,
  INTERPOLATE_MODES,
  bindingsOfParam,
  useBindings,
  valueAtKey,
} from "./bindings.js";
import { useParamActions } from "./param-actions.js";
import { ParamKeysRoot } from "./param-slider.js";
import { useParams } from "./replica.js";

type ErrorSink = ((cause: unknown) => void) | undefined;

interface CellRef {
  binding: string;
  x: number;
  y: number;
}

// `onError` is also a DOM event on every element; this one wins.
export interface BindingGridRootProps extends Omit<ComponentProps<"div">, "children" | "onError"> {
  session: Session;
  node: NodeId | undefined;
  param: ParamId | undefined;
  onError?: ErrorSink;
}

export function BindingGridRoot({ session, node, param, onError, ...rest }: BindingGridRootProps) {
  const actions = useParamActions(session);
  const params = useParams(session);
  const all = useBindings(session, node);
  const bindings = bindingsOfParam(all, param);
  const [selected, setSelected] = useState<CellRef | undefined>(undefined);
  const [target, setTarget] = useState<ScalarTarget>(BINDING_TARGETS[0] ?? "tx");
  const [copying, setCopying] = useState<CellRef | undefined>(undefined);
  const [secondary, setSecondary] = useState("");

  if (node === undefined || param === undefined) {
    return <div data-catchlight-binding-grid="" data-empty="" {...rest} />;
  }

  const paramInfo = (id: string): ParamInfo | undefined => params.find((p) => p.id === id);

  const pose = (binding: BindingInfo, x: number, y: number): void => {
    const along = paramInfo(binding.param);
    if (along) session.setParam(binding.param, valueAtKey(along, x, binding.key_positions[0]!));
    const up = binding.param_y ? paramInfo(binding.param_y) : undefined;
    if (up && binding.param_y) session.setParam(binding.param_y, valueAtKey(up, y, binding.key_positions[1]!));
  };

  const click = (binding: BindingInfo, x: number, y: number): void => {
    const at: CellRef = { binding: keyOf(binding), x, y };
    // An armed copy lands on the cell that ends it; anywhere else cancels.
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
    // A deform cell holds a vertex list, which scalar inputs cannot edit.
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
              {BINDING_LABELS[name]}
            </option>
          ))}
        </select>
        <select
          aria-label="Second driving param"
          value={secondary}
          onChange={(e) => setSecondary(e.currentTarget.value)}
        >
          <option value="">One param</option>
          {params
            .filter((p) => p.id !== param)
            .map((p) => (
              <option key={p.id} value={p.id}>
                × {p.name}
              </option>
            ))}
        </select>
        <button
          type="button"
          data-catchlight-binding-add-submit=""
          onClick={() =>
            report(
              onError,
              actions.addBinding(node, target, {
                param,
                param_y: secondary || null,
              }),
            )
          }
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
              <span data-catchlight-binding-name="">
                {BINDING_LABELS[binding.target]}
                {binding.param_y && <small> × {paramInfo(binding.param_y)?.name}</small>}
              </span>
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
            {[binding.param, ...(binding.param_y ? [binding.param_y] : [])].map((axis) => {
              const parameter = paramInfo(axis);
              return parameter && (
                <div key={axis} data-catchlight-binding-axis="">
                  <span>{parameter.name}</span>
                  <ParamKeysRoot session={session} param={parameter} binding={binding} node={node} onError={onError} />
                </div>
              );
            })}
            {binding.keys.map((row, y) => (
              <div data-catchlight-binding-row="" data-y={y} key={y}>
                {row.map((value, x) => {
                  const authored = binding.authored[y]?.[x] === true;
                  const isSelected =
                    selected?.binding === id && selected.x === x && selected.y === y;
                  return (
                    <input
                      // Keyed on its source value so an edit from anywhere redraws it.
                      key={`${x}:${value ?? "-"}:${String(authored)}`}
                      type="number"
                      step="any"
                      data-catchlight-binding-cell=""
                      data-cell={`${x},${y}`}
                      data-set={authored ? "" : undefined}
                      data-unset={authored ? undefined : ""}
                      data-selected={isSelected ? "" : undefined}
                      aria-label={`${binding.target} cell ${x},${y}`}
                      title={`${paramInfo(binding.param)?.name ?? binding.param}: ${valueAtKey(paramInfo(binding.param)!, x, binding.key_positions[0]!)}${binding.param_y ? ` · ${paramInfo(binding.param_y)?.name}: ${valueAtKey(paramInfo(binding.param_y)!, y, binding.key_positions[1]!)}` : ""}${binding.target.startsWith("r") ? " · radians" : ""}${authored ? " · authored key" : " · interpolated"}`}
                      placeholder={binding.target === "deform" ? (authored ? "Set" : "—") : "Auto"}
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

function keyOf(binding: BindingInfo): string {
  return `${binding.target}|${binding.param}|${binding.param_y ?? ""}`;
}

function paramsOf(binding: BindingInfo): {
  param: ParamId;
  param_y?: ParamId | null;
} {
  return { param: binding.param, param_y: binding.param_y ?? null };
}

function commitOnEnter(event: KeyboardEvent<HTMLInputElement>): void {
  if (event.key === "Enter") event.currentTarget.blur();
}

function report(onError: ErrorSink, work: Promise<unknown>): void {
  void work.catch((cause: unknown) => {
    if (onError) onError(cause);
    else console.warn("catchlight: the binding edit failed", cause);
  });
}
