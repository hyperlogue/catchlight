/**
 * What this client is looking at, published as one record.
 *
 * Presence is view state, not document state: it moves no revision and is
 * never undone. It still goes to the editor, because an agent on the socket
 * and another tab both read "what is this person looking at, and how have they
 * posed it" from there, and a selection only React knows about is invisible
 * to them.
 *
 * **One record, so one command.** The editor keeps a single presence per
 * session and `presence_set` replaces it whole. A selection that published
 * itself with an empty pose would wipe the pose the sliders had published a
 * moment before, and the other way round — so the two are held here together,
 * and every publish carries both.
 *
 * **A selection goes out at once; a pose is coalesced.** A click is one event
 * and the other side should see it now. A slider drag is a stream, and a
 * socket frame per pointer move is the traffic the scratch path exists to
 * avoid — so a pose is sent at most once per [`POSE_INTERVAL_MS`], and what is
 * sent is whatever is current when the timer fires. A pose may be handed over
 * as a thunk, so a caller that hears "the picture changed" per pointer move
 * defers the read to the one moment it is needed.
 *
 * **Nothing goes out twice.** The last command sent is remembered, and an
 * identical one is dropped: a repaint that changed no param costs nothing on
 * the wire.
 *
 * **What was there is adopted.** When a session is taken up, the presence the
 * editor already holds — an agent's pose, another tab's selection — is asked
 * for and applied locally, so a tab joins what is already going on rather
 * than overwriting it with an empty record on its first click.
 *
 * **No document, nobody to tell.** The provider may be given no session at
 * all — a screen that keeps its cells mounted while nothing is open, so that
 * its canvas survives (see `viewport.tsx`). A selection is then held locally
 * and a pose is dropped; both start over when a session arrives.
 */

import type { NodeId, ParamPose, Session, SessionPresenceCommand } from "@catchlight/core";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

/** How long a pose is held before it goes out, and the least time between two. */
export const POSE_INTERVAL_MS = 150;

export interface Selection {
  node: NodeId | undefined;
  select(node: NodeId | undefined): void;
}

/** A pose, or a read that produces one when the provider is ready to send it. */
export type PoseSource = ParamPose[] | (() => ParamPose[]);

export interface Pose {
  /** Publishes `pose` as this client's, coalesced with whatever else is pending. */
  publish(pose: PoseSource): void;
}

const SelectionContext = createContext<Selection | undefined>(undefined);
const PoseContext = createContext<Pose | undefined>(undefined);

export interface PresenceProviderProps {
  session: Session | undefined;
  children?: ReactNode;
}

/** Everything a publish reads, kept out of React state so a timer never sees a stale render. */
interface Held {
  session: Session | undefined;
  node: NodeId | undefined;
  pose: PoseSource;
  timer: ReturnType<typeof setTimeout> | undefined;
  /** The last command sent, serialized, so an identical one is not sent again. */
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
  // The selection is remembered with the session it was made in, so a switch
  // to another document shows nothing selected rather than a node id that
  // belongs to the previous one — and does so on the very render that
  // switches, with no effect and no flash in between.
  const [chosen, setChosen] = useState<{
    session: Session | undefined;
    node: NodeId | undefined;
  }>({ session, node: undefined });
  const node = chosen.session === session ? chosen.node : undefined;

  const held = useRef<Held>(fresh(session));

  const send = useCallback((): void => {
    const box = held.current;
    box.timer = undefined;
    if (!box.session) return;
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
    // Fire and forget: presence is a courtesy to other clients, and a publish
    // that failed must not fail the click or the drag that caused it.
    void box.session.sendPresence(command).catch((cause: unknown) => {
      console.warn("catchlight: publishing presence failed", cause);
    });
  }, []);

  const select = useCallback(
    (next: NodeId | undefined): void => {
      const box = held.current;
      box.node = next;
      setChosen({ session: box.session, node: next });
      // At once, and carrying whatever pose was waiting for its turn.
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

  // A new session starts from nothing pending, and from whatever the editor
  // already holds for it.
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

/** The selection of the nearest [`PresenceProvider`]. */
export function useSelection(): Selection {
  const selection = useContext(SelectionContext);
  if (!selection) throw new Error("no <PresenceProvider> above this component");
  return selection;
}

/** The pose channel of the nearest [`PresenceProvider`]. Stable for its life. */
export function usePose(): Pose {
  const pose = useContext(PoseContext);
  if (!pose) throw new Error("no <PresenceProvider> above this component");
  return pose;
}
