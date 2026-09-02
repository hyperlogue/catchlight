/**
 * Reading the replica from React, with no mirror in between.
 *
 * The model is already in this tab and answers synchronously, so a store that
 * copies it into React state would be a second version of the truth that can
 * disagree with the canvas. Instead every read is a plain call, and what React
 * subscribes to is one number: the session's revision. A read is redone when
 * that number moves, and not otherwise.
 */

import type { NodeId, NodeInfo, ParamInfo, Session, TreeNode } from "@catchlight/core";
import { useMemo, useSyncExternalStore } from "react";

import { useLatest } from "./latest.js";

/** The revision the session's replica holds, as a render can depend on it. */
export function useRevision(session: Session): number {
  return useSyncExternalStore(session.subscribe, session.getRevision, session.getRevision);
}

/**
 * `read(session)`, redone whenever the document moves.
 *
 * Memoized on the session and its revision — deliberately not on `read`, so an
 * inline arrow does not re-read the tree on every render. The newest `read` is
 * the one that runs, so a closure over changing props is still current the
 * next time the revision moves.
 */
export function useReplica<T>(session: Session, read: (session: Session) => T): T {
  const revision = useRevision(session);
  const latest = useLatest(read);
  return useMemo(() => latest.current(session), [session, revision, latest]);
}

/** Every param the model carries, as the panels list them. */
export function useParams(session: Session): ParamInfo[] {
  return useReplica(session, readParams);
}

/** The node tree, from the root down. */
export function useTree(session: Session): TreeNode {
  return useReplica(session, readTree);
}

/**
 * One node in full, as an inspector shows it.
 *
 * `undefined` for the two cases a panel draws the same way: nothing is
 * selected, and the selected node is gone. A selection is view state and
 * outlives the node it names, so both are ordinary.
 *
 * Not [`useReplica`], because the read depends on which node is asked for as
 * well as on the revision: that hook deliberately ignores its `read`, so a
 * selection that moved between two nodes at one revision would keep showing
 * the first one.
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
