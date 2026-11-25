/**
 * Publishes the pose from the repaint channel, so sliders only pose the
 * replica. Values are read at send time, never per repaint.
 */

import type { ParamInfo, ParamPose, Session } from "@catchlight/core";
import { useCallback, useEffect } from "react";

import { useLatest } from "./latest.js";
import { usePose } from "./presence.js";
import { useParams } from "./replica.js";

/** Call once per session, anywhere under the `PresenceProvider`. */
export function usePosePublisher(session: Session): void {
  const { publish } = usePose();
  const params = useParams(session);
  const latest = useLatest(params);
  useEffect(
    () => session.onInvalidate(() => publish(() => readPose(session, latest.current))),
    [session, publish, latest],
  );
}

export function readPose(session: Session, params: ParamInfo[] = session.params()): ParamPose[] {
  return params.map((param) => ({
    param: param.id,
    value: session.paramValue(param.id) ?? param.default,
  }));
}

/**
 * Poses only; the publisher hears the repaints. Clears the preview inputs too,
 * and leaves a playing clip playing.
 */
export function useResetPose(session: Session): () => void {
  return useCallback((): void => {
    session.clearInputs();
    for (const param of session.params()) session.setParam(param.id, param.default);
  }, [session]);
}
