/**
 * Reading the replica from React, with no mirror in between.
 *
 * The model is already in this tab and answers synchronously, so a store that
 * copies it into React state would be a second version of the truth that can
 * disagree with the canvas. Instead every read is a plain call, and what React
 * subscribes to is one number: the session's revision. A read is redone when
 * that number moves, and not otherwise.
 */

import type { ParamInfo, Session, TreeNode } from "@catchlight/core";
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

const readParams = (session: Session): ParamInfo[] => session.params();
const readTree = (session: Session): TreeNode => session.tree();
