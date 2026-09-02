/**
 * Authoring fields on a node, one command per commit.
 *
 * **Only the fields it was given.** A `node_set` leaves out what it does not
 * carry, so a form that sent every field it renders would author a dozen
 * values nobody touched — and bundle them into one undo entry, so undoing the
 * number a person actually changed would silently restore eleven others. The
 * hook fills in nothing and defaults nothing; picking which keys to send is
 * the caller's whole job.
 *
 * The keys are [`NodeInfo`]'s own, by design: a value read out of a node comes
 * back under the name that sets it, so an inspector never translates between
 * the two.
 */

import type { NodeId, NodePatch, Session } from "@catchlight/core";
import { useCallback } from "react";

/**
 * Sends `fields` for `node`, resolving once the replica can answer for them —
 * so a caller that re-reads the node after awaiting sees what it wrote.
 */
export type NodePatchFn = (node: NodeId, fields: NodePatch) => Promise<void>;

/** The one way this layer changes a node. */
export function useNodePatch(session: Session): NodePatchFn {
  return useCallback(
    async (node: NodeId, fields: NodePatch): Promise<void> => {
      await session.send({ cmd: "node_set", node, ...fields });
    },
    [session],
  );
}
