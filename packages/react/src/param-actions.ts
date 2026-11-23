/**
 * Param and binding edits as commands. Posing is `session.setParam` and stays
 * out of this hook. A binding is addressed by node, target and params, never a
 * local handle, so it stays valid across other clients' edits.
 */

import type {
  BindingParams,
  BindingCellValue,
  BindingCellWrite,
  BindingTarget,
  Interpolate,
  NodeId,
  ParamId,
  ResponseBody,
  ScalarTarget,
  Session,
} from "@catchlight/core";
import { useMemo } from "react";

/** `[x, y]`; `y` is 0 for a one-param binding. */
export type BindingCell = [number, number];

export type BindingAddress = BindingParams & { node: NodeId; target: BindingTarget };

/** The range defaults to 0..1. */
export interface NewParam {
  name: string;
  min?: number;
  max?: number;
  default?: number;
}

/** An absent field is left alone. */
export interface ParamPatch {
  name?: string;
  min?: number;
  max?: number;
  default?: number;
}

export interface ParamActions {
  add(param: NewParam): Promise<ParamId>;
  /** Key positions are normalized, so a range change does not move them. */
  set(param: ParamId, patch: ParamPatch): Promise<ResponseBody>;
  remove(param: ParamId): Promise<ResponseBody>;
  insertKey(binding: BindingAddress, axis: ParamId, value: number): Promise<ResponseBody>;
  deleteKey(binding: BindingAddress, axis: ParamId, index: number): Promise<ResponseBody>;
  moveKey(binding: BindingAddress, axis: ParamId, index: number, value: number, ifRev?: number): Promise<ResponseBody>;

  /** Creates an everywhere-unset binding. */
  addBinding(node: NodeId, target: ScalarTarget, params: BindingParams): Promise<ResponseBody>;
  deleteBinding(node: NodeId, target: BindingTarget, params: BindingParams): Promise<ResponseBody>;
  /** Creates the binding if this is its first key. */
  setKey(
    node: NodeId,
    target: ScalarTarget,
    params: BindingParams,
    cell: BindingCell,
    value: number,
  ): Promise<ResponseBody>;
  /** 1 for scale and opacity, 0 otherwise. */
  resetKey(
    node: NodeId,
    target: BindingTarget,
    params: BindingParams,
    cell: BindingCell,
  ): Promise<ResponseBody>;
  /** The model derives the cell again. */
  unsetKey(
    node: NodeId,
    target: BindingTarget,
    params: BindingParams,
    cell: BindingCell,
  ): Promise<ResponseBody>;
  interpolate(
    node: NodeId,
    target: BindingTarget,
    params: BindingParams,
    mode: Interpolate,
  ): Promise<ResponseBody>;
  invert(node: NodeId, target: BindingTarget, params: BindingParams): Promise<ResponseBody>;
  copyKey(
    node: NodeId,
    target: BindingTarget,
    params: BindingParams,
    from: BindingCell,
    to: BindingCell,
  ): Promise<ResponseBody>;
}

export function useParamActions(session: Session): ParamActions {
  return useMemo<ParamActions>(
    () => ({
      async add(param) {
        const body = await session.send({
          cmd: "param_add",
          name: param.name,
          min: param.min ?? 0,
          max: param.max ?? 1,
          default: param.default ?? 0,
        });
        if (body.result !== "param") {
          throw new Error(`param_add answered ${body.result}, not the param it added`);
        }
        return body.param;
      },

      set(param, patch) {
        // Absent and null both mean "leave alone" to the editor.
        return session.send({
          cmd: "param_set",
          param,
          name: patch.name ?? null,
          min: patch.min ?? null,
          max: patch.max ?? null,
          default: patch.default ?? null,
        });
      },

      remove(param) {
        return session.send({ cmd: "param_delete", param });
      },

      insertKey(binding, axis, value) {
        return session.send({ cmd: "binding_key_insert", ...binding, axis, value, if_rev: session.getRevision() });
      },
      deleteKey(binding, axis, index) {
        return session.send({ cmd: "binding_key_delete", ...binding, axis, index, if_rev: session.getRevision() });
      },
      moveKey(binding, axis, index, value, ifRev) {
        return session.send({ cmd: "binding_key_move", ...binding, axis, index, value, if_rev: ifRev ?? session.getRevision() });
      },

      addBinding(node, target, params) {
        return session.send({ cmd: "binding_add", node, target, ...params });
      },

      deleteBinding(node, target, params) {
        return session.send({ cmd: "binding_delete", node, target, ...params });
      },

      setKey(node, target, params, cell, value) {
        return session.send({ cmd: "edit_apply", if_rev: session.getRevision(), edits: [
          { op: "binding_add", node, target, ...params, key_positions: null },
          { op: "binding_cells_set", node, target, ...params, cells: [{ cell, value: { scalar: value } }] },
        ] });
      },

      resetKey(node, target, params, cell) {
        const binding = session.bindings(node).find((b) => b.target === target && b.param === params.param && b.param_y == params.param_y);
        if (!binding) return Promise.reject(new Error("The binding no longer exists."));
        const identity = binding.identity;
        const value: BindingCellValue = "scalar" in identity
          ? { scalar: identity.scalar }
          : { offsets: Array.from({ length: identity.vertex_count }, () => [...identity.offset] as [number, number]) };
        return writeCells(session, { node, target, ...params }, [{ cell, value }]);
      },

      unsetKey(node, target, params, cell) {
        return session.send({ cmd: "binding_cells_unset", node, target, cells: [cell], ...params, if_rev: session.getRevision() });
      },

      interpolate(node, target, params, mode) {
        return session.send({ cmd: "binding_interpolation_set", node, target, mode, ...params });
      },

      invert(node, target, params) {
        const address = { node, target, ...params };
        const binding = session.bindings(node).find((b) => b.target === target && b.param === params.param && b.param_y == params.param_y);
        if (!binding) return Promise.reject(new Error("The binding no longer exists."));
        const cells: [number, number][] = binding.authored.flatMap((row, y) => row.flatMap((authored, x) => authored ? [[x, y] as [number, number]] : []));
        if (cells.length === 0) return Promise.resolve({ result: "empty" });
        const read = readCells(session, address, cells, false);
        return writeCells(session, address, read.map(({ cell, value }) => ({ cell, value: "scalar" in value
          ? { scalar: -value.scalar }
          : { offsets: value.offsets.map(([x, y]) => [-x, -y]) } })));
      },

      copyKey(node, target, params, from, to) {
        const address = { node, target, ...params };
        const source = readCells(session, address, [from], true)[0];
        if (!source) return Promise.reject(new Error("The source key has no value."));
        return writeCells(session, address, [{ cell: to, value: source.value }]);
      },
    }),
    [session],
  );
}

function readCells(session: Session, binding: BindingAddress, cells: BindingCell[], includeDerived: boolean): BindingCellWrite[] {
  const read = session.query({ cmd: "binding_cells_get", ...binding, cells, include_derived: includeDerived, if_rev: session.getRevision() });
  if (read.result !== "binding_cells") throw new Error("The binding cells could not be read.");
  return read.cells.flatMap((cell) => {
    const value = cell.value ?? cell.derived;
    return value ? [{ cell: cell.cell, value }] : [];
  });
}
function writeCells(session: Session, binding: BindingAddress, cells: BindingCellWrite[]): Promise<ResponseBody> {
  return session.send({ cmd: "binding_cells_set", ...binding, cells, if_rev: session.getRevision() });
}
