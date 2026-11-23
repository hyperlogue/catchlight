/**
 * Binding reads, the target and interpolation tables a binding panel picks
 * from, and key-position arithmetic. `deform` is deliberately absent from the
 * targets: deform offsets have their own authoring controls.
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

export const BINDING_TARGETS: readonly ScalarTarget[] = [
  "tx",
  "ty",
  "tz",
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
];

export const BINDING_LABELS: Record<ScalarTarget | "deform", string> = {
  tx: "Position X",
  ty: "Position Y",
  tz: "Depth",
  sx: "Scale X",
  sy: "Scale Y",
  rx: "Rotation X",
  ry: "Rotation Y",
  rz: "Rotation Z",
  z_order: "Draw order",
  opacity: "Opacity",
  tintr: "Tint red",
  tintg: "Tint green",
  tintb: "Tint blue",
  screentintr: "Screen red",
  screentintg: "Screen green",
  screentintb: "Screen blue",
  deform: "Mesh deform",
};

export const INTERPOLATE_MODES: readonly Interpolate[] = ["nearest", "stepped", "linear", "cubic"];

// A target or mode added in Rust fails these checks.
type Unlisted<Union extends string, Listed extends string> = Exclude<Union, Listed>;
type _EveryTargetListed =
  Unlisted<ScalarTarget, (typeof BINDING_TARGETS)[number]> extends never ? true : never;
type _EveryModeListed =
  Unlisted<Interpolate, (typeof INTERPOLATE_MODES)[number]> extends never ? true : never;

/** `[]` when nothing is selected or the node is gone. */
export function useBindings(session: Session, node: NodeId | undefined): BindingInfo[] {
  const revision = useRevision(session);
  return useMemo(
    () => (node === undefined ? [] : session.bindings(node)),
    [session, revision, node],
  );
}

/** Bindings `param` drives along either axis; a two-param binding appears under both. */
export function bindingsOfParam(bindings: BindingInfo[], param: string | undefined): BindingInfo[] {
  if (param === undefined) return [];
  return bindings.filter((b) => b.param === param || b.param_y === param);
}

/** Key positions are normalized 0..1 over `[min, max]`; an out-of-range index answers the default. */
export function valueAtKey(param: ParamInfo, index: number, positions: readonly number[]): number {
  const position = positions[index];
  if (position === undefined) return param.default;
  return param.min + position * (param.max - param.min);
}

export function normalizedValue(param: ParamInfo, value: number): number {
  const span = param.max - param.min;
  if (span === 0) return 0;
  return Math.min(1, Math.max(0, (value - param.min) / span));
}

/** Matches within a tolerance, since the value comes from a continuous slider. */
export function keyIndexNear(param: ParamInfo, value: number, positions: readonly number[], within = 0.005): number | undefined {
  const at = normalizedValue(param, value);
  let best: number | undefined;
  let distance = within;
  positions.forEach((position, index) => {
    const gap = Math.abs(position - at);
    if (gap <= distance) {
      distance = gap;
      best = index;
    }
  });
  return best;
}

/** As in Rust recording, the nearest key within 1e-5 normalized units absorbs
 * pose round-trip error; an exact key wins over another key within that tolerance. */
export function bindingCellAt(binding: Pick<BindingInfo, "key_positions">, position: readonly [number, number]): [number, number] | undefined {
  const indices = binding.key_positions.map((axis, i) => {
    let nearest = -1;
    let distance = Infinity;
    axis.forEach((value, index) => {
      const gap = Math.abs(value - position[i]!);
      if (gap <= 0.00001 && gap < distance) {
        nearest = index;
        distance = gap;
      }
    });
    return nearest;
  });
  return indices.every((index) => index >= 0) ? [indices[0]!, indices[1] ?? 0] : undefined;
}
