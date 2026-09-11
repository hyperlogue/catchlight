/** Shared workspace actions and canvas gestures. Layouts call intentions;
 * selection, protocol routing and gesture rollback live here. */
import type {
  NodeId,
  NodeKind,
  Session,
  RecordingGesture,
} from "@catchlight/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { useEditing } from "./editing.js";
import { useSelection } from "./selection.js";
import {
  flattenTree,
  importArtwork,
  useSessionStatus,
  type ErrorHandler,
} from "./authoring.js";
import type { ViewportPointerEvent } from "./viewport.js";

export function useWorkspaceActions(
  session: Session | undefined,
  onError: ErrorHandler,
) {
  const editing = useEditing();
  const { node, select } = useSelection();
  const status = useSessionStatus(session);
  const historyAvailable = !editing?.busy;
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async (action: () => Promise<unknown>) => {
      setBusy(true);
      try {
        await action();
      } catch (cause) {
        onError(cause);
      } finally {
        setBusy(false);
      }
    },
    [onError],
  );
  return {
    busy,
    canRemove:
      !!session &&
      !!node &&
      node !== session.tree().id &&
      (!editing || editing.mode === "arrange"),
    canDuplicate:
      !!session &&
      !!node &&
      node !== session.tree().id &&
      (!editing || editing.mode === "arrange"),
    canUndo:
      historyAvailable &&
      (editing?.mode === "mesh"
        ? !!editing.mesh?.can_undo && !editing.stale
        : !!status?.undo_steps),
    canRedo:
      historyAvailable &&
      (editing?.mode === "mesh"
        ? !!editing.mesh?.can_redo && !editing.stale
        : !!status?.redo_steps),
    undo: () =>
      historyAvailable &&
      (editing?.mode === "mesh"
        ? editing.meshAction((d) => d.handle.undo())
        : session &&
          status?.undo_steps &&
          run(async () => {
            editing?.stop();
            await session.send({ cmd: "undo" });
          })),
    redo: () =>
      historyAvailable &&
      (editing?.mode === "mesh"
        ? editing.meshAction((d) => d.handle.redo())
        : session &&
          status?.redo_steps &&
          run(async () => {
            editing?.stop();
            await session.send({ cmd: "redo" });
          })),
    remove: () =>
      session &&
      node &&
      run(async () => {
        if (
          node === session.tree().id ||
          (editing && editing.mode !== "arrange")
        )
          return;
        await session.send({ cmd: "node_delete", node });
        select(undefined);
      }),
    duplicate: () =>
      session &&
      node &&
      run(async () => {
        if (
          node === session.tree().id ||
          (editing && editing.mode !== "arrange")
        )
          return;
        const reply = await session.send({ cmd: "node_duplicate", node });
        if (reply.result === "node") select(reply.node);
      }),
    add: (kind: NodeKind) =>
      session &&
      run(async () => {
        const parent = node ?? session.tree().id;
        const reply =
          kind === "physics"
            ? await session.send({
                cmd: "physics_add",
                parent,
                name: "Pendulum",
                kind: "rigid",
                target_params: {},
                length: 100,
                gravity: 1,
                frequency: null,
                angle_damping: null,
                length_damping: null,
              })
            : kind === "spine"
              ? await session.send({
                  cmd: "spine_add",
                  parent,
                  name: "Spine",
                  joints: [
                    [0, -50],
                    [0, -100],
                    [0, -150],
                  ],
                  targets: null,
                  chain: null,
                })
              : await session.send({
                  cmd: "node_add",
                  parent,
                  name: null,
                  kind,
                });
        if (reply.result === "node") select(reply.node);
      }),
    importImages: (files: readonly File[]) =>
      session &&
      run(async () => {
        const next = await importArtwork(session, files, session.tree().id);
        if (next) select(next);
      }),
    nudge: (x: number, y: number) =>
      session &&
      node &&
      run(async () => {
        const translate = session.translationAfterWorldDelta(node, x, y);
        if (!translate || editing?.mode === "mesh") return;
        if (editing?.mode === "record") {
          if (editing.recording && editing.recordTool === "transform")
            await editing.patch({ translate }, true);
        } else await session.send({ cmd: "node_set", node, translate });
      }),
    resetPose: () => {
      if (session)
        for (const p of session.params()) session.setParam(p.id, p.default);
    },
  };
}

interface Move {
  session: Session;
  node: NodeId;
  pointer: number;
  from: [number, number];
  poseDelta: [number, number, number];
  translate?: [number, number, number];
  recording?: RecordingGesture | undefined;
}
export function useCanvasSelection(
  session: Session | undefined,
  onError: ErrorHandler,
) {
  const edit = useEditing();
  const { node, select } = useSelection();
  const active = useRef<Move | undefined>(undefined);
  const [dragging, setDragging] = useState(false);
  const cancel = useCallback(() => {
    const move = active.current;
    active.current = undefined;
    setDragging(false);
    move?.recording?.dispose();
    if (move && !move.session.closed)
      move.session.clearScratchTransform(move.node);
  }, []);
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        (e.key === "Escape" ||
          ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z")) &&
        active.current
      ) {
        e.preventDefault();
        e.stopImmediatePropagation();
        cancel();
      }
    };
    window.addEventListener("keydown", handler);
    window.addEventListener("blur", cancel);
    return () => {
      window.removeEventListener("keydown", handler);
      window.removeEventListener("blur", cancel);
      const move = active.current;
      active.current = undefined;
      move?.recording?.dispose();
      if (move && !move.session.closed)
        move.session.clearScratchTransform(move.node);
    };
  }, [cancel, session, edit?.mode, edit?.recording, edit?.recordTool]);
  useEffect(() => {
    if (active.current && active.current.node !== node) cancel();
  }, [node, cancel]);
  return {
    dragging,
    handlers: {
      onPointerDown: ({ world, event }: ViewportPointerEvent) => {
        if (
          !session ||
          event.button !== 0 ||
          edit?.busy ||
          edit?.mode === "mesh"
        )
          return;
        const picked = session.pickNode(...world);
        // A selected group is a useful move tool too: clicking one of its
        // descendants keeps the group selected until the user clicks elsewhere.
        const chosen =
          node &&
          picked &&
          flattenTree(session.tree()).find((n) => n.id === node);
        const keepGroup =
          chosen &&
          chosen.kind !== "part" &&
          flattenTree(chosen).some((n) => n.id === picked);
        const target = keepGroup ? node : picked;
        select(target);
        if (
          edit?.mode === "record" &&
          (!edit.recording ||
            edit.recordTool !== "transform" ||
            target !== node)
        )
          return;
        if (target) {
          const recording =
            edit?.mode === "record" ? edit.beginRecording() : undefined;
          if (edit?.mode === "record" && !recording) return;
          const base = session.nodeInfo(target)?.translate ?? [0, 0, 0];
          const posed = session.nodeLocalTransform(target);
          const poseDelta = base.map((v, i) => (posed?.[i] ?? v) - v) as [
            number,
            number,
            number,
          ];
          active.current = {
            session,
            node: target,
            pointer: event.pointerId,
            from: world,
            poseDelta,
            recording,
          };
        }
      },
      onPointerMove: ({ world, event }: ViewportPointerEvent) => {
        const move = active.current;
        if (!move || move.pointer !== event.pointerId) return;
        const dx = world[0] - move.from[0],
          dy = world[1] - move.from[1];
        if (Math.hypot(dx, dy) < 0.01 && !move.translate) return;
        const translate = move.session.translationAfterWorldDelta(
          move.node,
          dx,
          dy,
        );
        if (!translate) return;
        move.translate = translate;
        setDragging(true);
        move.session.scratchTransform(move.node, {
          translate: translate.map((v, i) => v + move.poseDelta[i]!) as [
            number,
            number,
            number,
          ],
        });
      },
      onPointerUp: ({ event }: ViewportPointerEvent) => {
        const move = active.current;
        if (!move || move.pointer !== event.pointerId) return;
        active.current = undefined;
        setDragging(false);
        if (!move.translate) {
          move.recording?.dispose();
          return;
        }
        const work = move.recording
          ? edit!.runGesture(move.recording, () =>
              move.recording!.patch({ translate: move.translate! }, true),
            )
          : move.session.send({
              cmd: "node_set",
              node: move.node,
              translate: move.translate,
            });
        void work.catch(onError).finally(() => {
          if (!move.session.closed)
            move.session.clearScratchTransform(move.node);
        });
      },
      onPointerCancel: cancel,
    },
  };
}

/** The overlay follows the evaluated mesh, including live physics. State only
 * changes when coordinates do, and a hidden tab pauses requestAnimationFrame. */
export function useSelectionVertices(
  session: Session | undefined,
  node: NodeId | undefined,
) {
  const [points, setPoints] = useState<number[]>([]);
  useEffect(() => {
    setPoints([]);
    if (!session || !node || typeof requestAnimationFrame === "undefined")
      return;
    let frame = 0;
    let previous: Float32Array | undefined;
    const update = () => {
      if (session.closed) return;
      const values = session.nodeVertices(node);
      if (
        values?.length !== previous?.length ||
        values?.some((value, i) => value !== previous?.[i])
      ) {
        setPoints(values ? Array.from(values) : []);
      }
      previous = values;
      frame = requestAnimationFrame(update);
    };
    frame = requestAnimationFrame(update);
    return () => cancelAnimationFrame(frame);
  }, [session, node]);
  return points;
}
