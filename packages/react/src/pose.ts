/**
 * The pose, published without the sliders knowing.
 *
 * A slider poses the replica and tells nobody: that is the scratch path, and
 * it stays a typed call with no round trip. What tells other clients where the
 * params are is one subscription to the session's repaint channel, which fires
 * on every pose change, reading the values back at the moment the provider is
 * ready to send them. The sliders, the reset button and an animation all
 * publish through it by doing nothing more than posing.
 *
 * The read is deferred, never done per repaint: a drag invalidates on every
 * pointer move, and the params list is a replica query. The list comes from
 * the revision-keyed read the panels share, so it is fetched again when the
 * model moves and never per frame; only the values are read at send time.
 */

import type { ParamInfo, ParamPose, Session } from "@catchlight/core";
import { useCallback, useEffect } from "react";

import { useLatest } from "./latest.js";
import { usePose } from "./presence.js";
import { useParams } from "./replica.js";

/**
 * Publishes `session`'s pose through the nearest `PresenceProvider` whenever
 * the picture changes. Call it once per session, anywhere under the provider.
 */
export function usePosePublisher(session: Session): void {
  const { publish } = usePose();
  const params = useParams(session);
  const latest = useLatest(params);
  useEffect(
    () => session.onInvalidate(() => publish(() => readPose(session, latest.current))),
    [session, publish, latest],
  );
}

/** Every param at what the replica shows, or its default before anything posed it. */
export function readPose(session: Session, params: ParamInfo[] = session.params()): ParamPose[] {
  return params.map((param) => ({
    param: param.id,
    value: session.paramValue(param.id) ?? param.default,
  }));
}

/**
 * Puts every param back at its default. Poses only: the publisher hears the
 * repaints and publishes the result, so this needs no provider of its own.
 */
export function useResetPose(session: Session): () => void {
  return useCallback((): void => {
    for (const param of session.params()) session.setParam(param.id, param.default);
  }, [session]);
}
