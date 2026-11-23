/**
 * Node-drag handlers to spread onto the viewport. A world delta goes to Rust,
 * which resolves the parent's frame. The scratch preview is cleared only once
 * the commit's revision lands (or the send is refused), so the node does not
 * snap back during the round trip. With no session, every handler is a no-op.
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
      if (!session) return;
      const translate = active.translate;
      if (!translate) {
        // A click with no movement must not create an undo entry.
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
