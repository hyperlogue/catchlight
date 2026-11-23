/**
 * Replica reads from React. Nothing is mirrored into state; what React
 * subscribes to is the session's revision.
 */

import type { NodeId, NodeInfo, ParamInfo, Session, TreeNode } from "@catchlight/core";
import { useMemo, useSyncExternalStore } from "react";

import { useLatest } from "./latest.js";

export function useRevision(session: Session): number {
  return useSyncExternalStore(session.subscribe, session.getRevision, session.getRevision);
}

/**
 * Memoized on the session and revision, deliberately not on `read`, so an
 * inline arrow does not re-read every render; the newest `read` runs.
 */
export function useReplica<T>(session: Session, read: (session: Session) => T): T {
  const revision = useRevision(session);
  const latest = useLatest(read);
  return useMemo(() => latest.current(session), [session, revision, latest]);
}

export function useParams(session: Session): ParamInfo[] {
  return useReplica(session, readParams);
}

export function useTree(session: Session): TreeNode {
  return useReplica(session, readTree);
}

/**
 * `undefined` when nothing is selected or the node is gone. Not
 * [`useReplica`], which ignores a changed `read` at the same revision.
 */
export function useNodeInfo(session: Session, node: NodeId | undefined): NodeInfo | undefined {
  const revision = useRevision(session);
  return useMemo(
    () => (node === undefined ? undefined : session.nodeInfo(node)),
    [session, revision, node],
  );
}

const readParams = (session: Session): ParamInfo[] => session.params();
const readTree = (session: Session): TreeNode => session.tree();
