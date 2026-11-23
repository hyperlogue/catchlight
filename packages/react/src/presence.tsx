/**
 * This client's selection and pose, published as one presence record.
 *
 * `presence_set` replaces the record whole, so every publish carries both
 * selection and pose. A selection goes out at once; a pose is coalesced to one
 * send per [`POSE_INTERVAL_MS`]. An existing record is adopted when a session
 * is taken up, so a tab joins rather than overwrites it.
 */

import type { NodeId, ParamPose, Session, SessionPresenceCommand } from "@catchlight/core";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

export const POSE_INTERVAL_MS = 150;

export interface Selection {
  node: NodeId | undefined;
  select(node: NodeId | undefined): void;
}

export type PoseSource = ParamPose[] | (() => ParamPose[]);

export interface Pose {
  publish(pose: PoseSource): void;
}

const SelectionContext = createContext<Selection | undefined>(undefined);
const PoseContext = createContext<Pose | undefined>(undefined);

export interface PresenceProviderProps {
  session: Session | undefined;
  children?: ReactNode;
}

/** Kept out of React state so a timer never sees a stale render. */
interface Held {
  session: Session | undefined;
  node: NodeId | undefined;
  pose: PoseSource;
  timer: ReturnType<typeof setTimeout> | undefined;
  sent: string | undefined;
}

function fresh(session: Session | undefined): Held {
  return { session, node: undefined, pose: [], timer: undefined, sent: undefined };
}

function clearTimer(held: Held): void {
  if (held.timer === undefined) return;
  clearTimeout(held.timer);
  held.timer = undefined;
}

export function PresenceProvider({ session, children }: PresenceProviderProps): ReactNode {
  // Keyed by session so a model switch shows no stale selection on the very
  // render that switches.
  const [chosen, setChosen] = useState<{
    session: Session | undefined;
    node: NodeId | undefined;
  }>({ session, node: undefined });
  const node = chosen.session === session ? chosen.node : undefined;

  const held = useRef<Held>(fresh(session));

  const send = useCallback((): void => {
    const box = held.current;
    box.timer = undefined;
    if (!box.session || box.session.closed) return;
    const pose = typeof box.pose === "function" ? box.pose() : box.pose;
    box.pose = pose;
    const command: SessionPresenceCommand = {
      cmd: "presence_set",
      pose,
      selection: box.node ?? null,
    };
    const key = JSON.stringify(command);
    if (key === box.sent) return;
    box.sent = key;
    // Presence is a courtesy; a failed publish must not fail the gesture.
    void box.session.sendPresence(command).catch((cause: unknown) => {
      console.warn("catchlight: publishing presence failed", cause);
    });
  }, []);

  const select = useCallback(
    (next: NodeId | undefined): void => {
      const box = held.current;
      box.node = next;
      setChosen({ session: box.session, node: next });
      clearTimer(box);
      send();
    },
    [send],
  );

  const publish = useCallback(
    (pose: PoseSource): void => {
      const box = held.current;
      box.pose = pose;
      if (box.timer === undefined) box.timer = setTimeout(send, POSE_INTERVAL_MS);
    },
    [send],
  );

  useEffect(() => {
    const box = fresh(session);
    held.current = box;
    if (!session) return () => clearTimer(box);
    let live = true;
    void session
      .queryServer({ cmd: "presence_get" })
      .then((body) => {
        if (!live || body.result !== "presence" || !body.presence) return;
        const { pose, selection } = body.presence;
        for (const { param, value } of pose) session.setParam(param, value);
        if (selection !== undefined && selection !== null) {
          box.node = selection;
          setChosen({ session, node: selection });
        }
      })
      .catch((cause: unknown) => {
        console.warn("catchlight: reading presence failed", cause);
      });
    return () => {
      live = false;
      clearTimer(box);
    };
  }, [session]);

  const selection = useMemo<Selection>(() => ({ node, select }), [node, select]);
  const poseValue = useMemo<Pose>(() => ({ publish }), [publish]);
  return (
    <SelectionContext.Provider value={selection}>
      <PoseContext.Provider value={poseValue}>{children}</PoseContext.Provider>
    </SelectionContext.Provider>
  );
}

export function useSelection(): Selection {
  const selection = useContext(SelectionContext);
  if (!selection) throw new Error("no <PresenceProvider> above this component");
  return selection;
}

/** Stable for the provider's life. */
export function usePose(): Pose {
  const pose = useContext(PoseContext);
  if (!pose) throw new Error("no <PresenceProvider> above this component");
  return pose;
}
