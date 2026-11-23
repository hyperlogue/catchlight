/**
 * Node field authoring. Only the given fields are sent: extra fields would
 * join the same undo entry and be silently restored by undo.
 */

import type { NodeId, NodePatch, Session } from "@catchlight/core";
import { useCallback } from "react";

/** Resolves once the replica can answer for `fields`. */
export type NodePatchFn = (node: NodeId, fields: NodePatch) => Promise<void>;

export function useNodePatch(session: Session): NodePatchFn {
  return useCallback(
    async (node: NodeId, fields: NodePatch): Promise<void> => {
      await session.send({ cmd: "node_set", node, ...fields });
    },
    [session],
  );
}
