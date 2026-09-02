/**
 * Dragging a node, as a set of handlers to spread onto the viewport.
 *
 * The split this hook exists to hold: TypeScript owns the gesture — which
 * pointer, where it started, when it ends — and Rust owns every number that
 * comes out of the model. A world delta goes down as a delta and comes back as
 * the local translation the node would then have, so the parent's frame is
 * resolved once, in the place a native editor also resolves it.
 *
 * **The scratch is held until the revision lands.** A preview cleared the
 * moment `node_set` is sent leaves the node back where it started for as long
 * as the round trip takes, which reads as the drag snapping back. `send`
 * resolves only once the replica can answer at the new revision, so clearing
 * after it is the one point where the preview and the document agree. A
 * refused send clears too, because then the document never moved and the
 * preview is a lie.
 *
 * With no session — a stage that is showing nothing — every handler is a
 * no-op, so a host can keep the canvas mounted between documents.
 */

import type { NodeId, Session } from "@catchlight/core";
import { useCallback, useRef, useState } from "react";

import type { ViewportPointerEvent } from "./viewport.js";

export interface NodeDrag {
  handlers: {
    onPointerDown: (event: ViewportPointerEvent) => void;
    onPointerMove: (event: ViewportPointerEvent) => void;
    onPointerUp: (event: ViewportPointerEvent) => void;
    onPointerCancel: (event: ViewportPointerEvent) => void;
  };
  dragging: boolean;
}

/** A drag in flight: what is being moved, from where, and to what so far. */
interface Drag {
  pointerId: number;
  node: NodeId;
  from: [number, number];
  translate: [number, number, number] | undefined;
}

export function useNodeDrag(session: Session | undefined, node: NodeId | undefined): NodeDrag {
  const [dragging, setDragging] = useState(false);
  const drag = useRef<Drag | undefined>(undefined);

  const onPointerDown = useCallback(
    ({ world, event }: ViewportPointerEvent): void => {
      if (!session || node === undefined || event.button !== 0) return;
      drag.current = { pointerId: event.pointerId, node, from: world, translate: undefined };
      setDragging(true);
    },
    [session, node],
  );

  const onPointerMove = useCallback(
    ({ world, event }: ViewportPointerEvent): void => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId || !session) return;
      const moved = session.translationAfterWorldDelta(
        active.node,
        world[0] - active.from[0],
        world[1] - active.from[1],
      );
      if (!moved) return;
      active.translate = moved;
      session.scratchTransform(active.node, { translate: moved });
    },
    [session],
  );

  const onPointerUp = useCallback(
    ({ event }: ViewportPointerEvent): void => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId) return;
      drag.current = undefined;
      setDragging(false);
      // The document went away under the gesture: nothing to author or clear.
      if (!session) return;
      const translate = active.translate;
      if (!translate) {
        // A press that never moved: nothing to author, and authoring it anyway
        // would put an undo entry behind every click.
        session.clearScratchTransform(active.node);
        return;
      }
      void (async () => {
        try {
          await session.send({ cmd: "node_set", node: active.node, translate });
        } catch (cause) {
          console.warn("catchlight: committing the drag failed", cause);
        } finally {
          session.clearScratchTransform(active.node);
        }
      })();
    },
    [session],
  );

  const onPointerCancel = useCallback(
    ({ event }: ViewportPointerEvent): void => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId) return;
      drag.current = undefined;
      setDragging(false);
      session?.clearScratchTransform(active.node);
    },
    [session],
  );

  return {
    handlers: { onPointerDown, onPointerMove, onPointerUp, onPointerCancel },
    dragging,
  };
}
