/**
 * Editing params and the bindings they drive: one call per command.
 *
 * **Every action is a command, and nothing here touches the model.** What a
 * panel draws comes back from the replica once the editor says the document
 * moved, so an action's whole job is to send the right command and hand its
 * promise on. The same shape as `useNodeActions`, for the same reason.
 *
 * **A pose is not in here.** Clicking a keypoint or a grid cell moves the
 * puppet to that key and authors nothing — that is `session.setParam`, on the
 * repaint channel, and it stays out of this hook so a panel cannot reach for a
 * document command by accident.
 *
 * **A binding is addressed the way the wire addresses it**: the node, the
 * target's name, and the one or two params whose key positions its grid spans.
 * There is no handle and no local index, so an action built from what a panel
 * read a moment ago still names the same binding after somebody else edited
 * the document.
 */

import type { BindingParams, NodeId, ParamId, ResponseBody, Session } from "@catchlight/core";
import { useMemo } from "react";

/** A cell of a binding's grid, `[x, y]` — `y` is 0 for a one-param binding. */
export type BindingCell = [number, number];

/** What a new param needs. The range defaults to 0..1, keyed at both ends. */
export interface NewParam {
  name: string;
  min?: number;
  max?: number;
  default?: number;
  /** Normalized 0..1. Empty (the default) is the two endpoints. */
  keyPositions?: number[];
}

/** What a `param_set` can change. An absent field is left alone. */
export interface ParamPatch {
  name?: string;
  min?: number;
  max?: number;
  default?: number;
}

/** The param and binding edits a panel makes, each one command. */
export interface ParamActions {
  /** Creates a param and hands back the Id the editor minted. */
  add(param: NewParam): Promise<ParamId>;
  /** Changes a param's label or its range. Key positions are normalized, so a
   * range change does not move them. */
  set(param: ParamId, patch: ParamPatch): Promise<ResponseBody>;
  /** Deletes the param and every binding it drove. */
  remove(param: ParamId): Promise<ResponseBody>;
  /** Adds a key position at normalized `value`, strictly inside (0, 1). */
  insertKey(param: ParamId, value: number): Promise<ResponseBody>;
  /** Removes an interior key position, dropping the cells authored at it. */
  deleteKey(param: ParamId, index: number): Promise<ResponseBody>;
  /** Moves an interior key position, which must stay between its neighbours. */
  moveKey(param: ParamId, index: number, value: number): Promise<ResponseBody>;
  /** Mirrors the param: positions reflect and cells move, values untouched. */
  flip(param: ParamId): Promise<ResponseBody>;

  /** Creates an everywhere-unset binding on one of the node's properties. */
  addBinding(node: NodeId, target: string, params: BindingParams): Promise<ResponseBody>;
  /** Deletes the whole binding. */
  deleteBinding(node: NodeId, target: string, params: BindingParams): Promise<ResponseBody>;
  /** Authors one cell. Creates the binding if this is its first key. */
  setKey(
    node: NodeId,
    target: string,
    params: BindingParams,
    cell: BindingCell,
    value: number,
  ): Promise<ResponseBody>;
  /** Authors the target's identity at a cell (1 for scale and opacity, 0 else). */
  resetKey(
    node: NodeId,
    target: string,
    params: BindingParams,
    cell: BindingCell,
  ): Promise<ResponseBody>;
  /** Un-authors a cell, so the model derives it again. */
  unsetKey(
    node: NodeId,
    target: string,
    params: BindingParams,
    cell: BindingCell,
  ): Promise<ResponseBody>;
  /** How the binding reads between its cells: nearest | stepped | linear | cubic. */
  interpolate(
    node: NodeId,
    target: string,
    params: BindingParams,
    mode: string,
  ): Promise<ResponseBody>;
  /** Negates every authored value. */
  invert(node: NodeId, target: string, params: BindingParams): Promise<ResponseBody>;
  /** Authors the value the binding evaluates at `from` into the cell `to`. */
  copyKey(
    node: NodeId,
    target: string,
    params: BindingParams,
    from: BindingCell,
    to: BindingCell,
  ): Promise<ResponseBody>;
}

/** The param and binding edits, bound to one session. */
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
          key_positions: param.keyPositions ?? [],
        });
        if (body.result !== "param") {
          throw new Error(`param_add answered ${body.result}, not the param it added`);
        }
        return body.param;
      },

      set(param, patch) {
        // Every field travels, because absent and null mean the same thing to
        // the editor: leave it alone.
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

      insertKey(param, value) {
        return session.send({ cmd: "param_key_insert", param, value });
      },

      deleteKey(param, index) {
        return session.send({ cmd: "param_key_delete", param, index });
      },

      moveKey(param, index, value) {
        return session.send({ cmd: "param_key_move", param, index, value });
      },

      flip(param) {
        return session.send({ cmd: "param_flip", param });
      },

      addBinding(node, target, params) {
        return session.send({ cmd: "binding_add", node, target, ...params });
      },

      deleteBinding(node, target, params) {
        return session.send({ cmd: "binding_delete", node, target, ...params });
      },

      setKey(node, target, params, cell, value) {
        return session.send({ cmd: "binding_key", node, target, cell, value, ...params });
      },

      resetKey(node, target, params, cell) {
        return session.send({ cmd: "binding_reset", node, target, cell, ...params });
      },

      unsetKey(node, target, params, cell) {
        return session.send({ cmd: "binding_unset", node, target, cell, ...params });
      },

      interpolate(node, target, params, mode) {
        return session.send({ cmd: "binding_interpolate", node, target, mode, ...params });
      },

      invert(node, target, params) {
        return session.send({ cmd: "binding_invert", node, target, ...params });
      },

      copyKey(node, target, params, from, to) {
        return session.send({ cmd: "binding_copy_key", node, target, from, to, ...params });
      },
    }),
    [session],
  );
}
