/**
 * Reading a node's bindings, the two string tables a binding panel picks from,
 * and where a pose sits among a param's key positions.
 *
 * A binding is a replica read like the tree is: `binding_list` is a pure
 * function of the model, so a panel calls it during render and re-reads when
 * the revision moves. Nothing is mirrored into React state.
 *
 * **The names come from the wire, not from an enum this package invented.**
 * Both tables are typed by the generated `ScalarTarget` and `Interpolate`
 * unions, and each is checked against its union below, so a `<select>` built
 * from either sends a word the editor already accepts and a target added in
 * Rust fails the typecheck here. `deform` is deliberately absent from the
 * targets: a deform binding is authored by dragging vertices, and
 * `binding_add` refuses it.
 */

import type {
  BindingInfo,
  Interpolate,
  NodeId,
  ParamInfo,
  ScalarTarget,
  Session,
} from "@catchlight/core";
import { useMemo } from "react";

import { useRevision } from "./replica.js";

/**
 * The properties a binding can drive, in the order a picker lists them: the
 * transform, then z order, then the colour a drawable carries.
 */
export const BINDING_TARGETS: readonly ScalarTarget[] = [
  "tx",
  "ty",
  "sx",
  "sy",
  "rx",
  "ry",
  "rz",
  "z_order",
  "opacity",
  "tintr",
  "tintg",
  "tintb",
  "screentintr",
  "screentintg",
  "screentintb",
  "outputscalex",
  "outputscaley",
];

/** How a binding reads between its cells. */
export const INTERPOLATE_MODES: readonly Interpolate[] = ["nearest", "stepped", "linear", "cubic"];

// Typing the tables only rules out a word the wire does not carry. These say
// the other half: a target or a mode added in Rust is missing from them.
type Unlisted<Union extends string, Listed extends string> = Exclude<Union, Listed>;
type _EveryTargetListed = Unlisted<ScalarTarget, (typeof BINDING_TARGETS)[number]> extends never
  ? true
  : never;
type _EveryModeListed = Unlisted<Interpolate, (typeof INTERPOLATE_MODES)[number]> extends never
  ? true
  : never;

/**
 * Every binding on one node, redone whenever the model moves.
 *
 * `[]` for the two cases a panel draws the same way: nothing is selected, and
 * the selected node is gone.
 *
 * Not [`useReplica`], for the same reason [`useNodeInfo`] is not: the read
 * depends on which node is asked for as well as on the revision.
 */
export function useBindings(session: Session, node: NodeId | undefined): BindingInfo[] {
  const revision = useRevision(session);
  return useMemo(
    () => (node === undefined ? [] : session.bindings(node)),
    [session, revision, node],
  );
}

/**
 * The bindings of `bindings` that `param` drives, along either axis.
 *
 * A two-param binding is driven by both of its params, so it shows up under
 * each of them — the grid is the same grid either way, just entered from a
 * different column.
 */
export function bindingsOfParam(bindings: BindingInfo[], param: string | undefined): BindingInfo[] {
  if (param === undefined) return [];
  return bindings.filter((b) => b.param === param || b.param_y === param);
}

/**
 * What to pose `param` at so it lands exactly on its key position `index`.
 *
 * `ParamInfo.key_positions` are normalized 0..1 across `[min, max]`, so this
 * is that normalization read backwards. Out-of-range indices answer the
 * param's default, which is where the puppet already is.
 */
export function valueAtKey(param: ParamInfo, index: number): number {
  const position = param.key_positions[index];
  if (position === undefined) return param.default;
  return param.min + position * (param.max - param.min);
}

/** Where `value` sits in `[min, max]`, on the 0..1 scale key positions use. */
export function normalizedValue(param: ParamInfo, value: number): number {
  const span = param.max - param.min;
  if (span === 0) return 0;
  return Math.min(1, Math.max(0, (value - param.min) / span));
}

/**
 * The key position `value` is sitting on, or `undefined` if it is between two.
 *
 * What "sitting on" means is a tolerance rather than an equality, because the
 * value came from a continuous slider: a param is a scalar and its key
 * positions say where its bindings sample it, not where the slider may stop.
 */
export function keyIndexNear(param: ParamInfo, value: number, within = 0.005): number | undefined {
  const at = normalizedValue(param, value);
  let best: number | undefined;
  let distance = within;
  param.key_positions.forEach((position, index) => {
    const gap = Math.abs(position - at);
    if (gap <= distance) {
      distance = gap;
      best = index;
    }
  });
  return best;
}
