/** The canvas's transform and mesh handles. A gesture captures its authored
 * and evaluated starting values, previews only scratch, then commits once.
 * Cancel, loss of focus, and a model switch roll the preview back. */
import type {
  Camera,
  NodeId,
  NodeInfo,
  Session,
  RecordingGesture,
} from "@catchlight/core";
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { Point, Size } from "./camera.js";
import { screenAt, worldAt } from "./camera.js";
import { useNodeInfo } from "./replica.js";
import { useSelectionVertices } from "./workspace.js";
import { useEditing } from "./editing.js";
import type { ErrorHandler } from "./authoring.js";

type Handle = "rotate" | "scale" | number;
type Patch = { rotate?: [number, number, number]; scale?: [number, number] };
interface Gesture {
  kind: Handle;
  pointer: number;
  from: Point;
  pivot: Point;
  info: NodeInfo;
  local: Float32Array | undefined;
  patch?: Patch;
  offsets?: Float32Array;
  recording?: RecordingGesture | undefined;
}

export function SelectionOverlay({
  session,
  node,
  camera,
  size,
  mesh,
  onError,
}: {
  session: Session;
  node: NodeId;
  camera: Camera;
  size: Size;
  mesh: boolean;
  onError: ErrorHandler;
}) {
  const edit = useEditing();
  const info = useNodeInfo(session, node);
  const vertices = useSelectionVertices(session, node);
  const active = useRef<Gesture | undefined>(undefined);
  const [dragging, setDragging] = useState(false);
  const svg = useRef<SVGSVGElement>(null);
  const points: Point[] = [];
  let left = Infinity,
    top = Infinity,
    right = -Infinity,
    bottom = -Infinity;
  for (let i = 0; i < vertices.length; i += 2) {
    const point = screenAt(camera, size, [vertices[i]!, vertices[i + 1]!]);
    points.push(point);
    left = Math.min(left, point[0]);
    top = Math.min(top, point[1]);
    right = Math.max(right, point[0]);
    bottom = Math.max(bottom, point[1]);
  }
  const cancel = () => {
    const gesture = active.current;
    active.current = undefined;
    setDragging(false);
    gesture?.recording?.dispose();
    if (gesture && !session.closed) {
      if (typeof gesture.kind === "number") session.clearScratchDeform(node);
      else session.clearScratchTransform(node);
    }
  };
  const latestCancel = useRef(cancel);
  latestCancel.current = cancel;
  useEffect(() => {
    const escape = (e: KeyboardEvent) => {
      if (
        (e.key === "Escape" ||
          ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z")) &&
        active.current
      ) {
        e.preventDefault();
        e.stopImmediatePropagation();
        latestCancel.current();
      }
    };
    const blur = () => latestCancel.current();
    window.addEventListener("keydown", escape, true);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("blur", blur);
      latestCancel.current();
    };
  }, [session, node, edit?.mode, edit?.recording, edit?.recordTool]);
  const world = (e: ReactPointerEvent) => {
    const rect = svg.current!.getBoundingClientRect();
    return worldAt(camera, size, [e.clientX - rect.left, e.clientY - rect.top]);
  };
  const start = (kind: Handle, e: ReactPointerEvent) => {
    if (!info || e.button !== 0 || edit?.busy) return;
    if (typeof kind === "number" && !edit?.recording) return;
    if (edit?.mode === "record" && !edit.recording) return;
    const recording =
      edit?.mode === "record" ? edit.beginRecording() : undefined;
    if (edit?.mode === "record" && !recording) return;
    e.preventDefault();
    e.stopPropagation();
    const matrix = session.nodeWorldTransform(node);
    if (!matrix) {
      recording?.dispose();
      return;
    }
    svg.current!.setPointerCapture(e.pointerId);
    active.current = {
      kind,
      recording,
      pointer: e.pointerId,
      from: world(e),
      pivot: [matrix[12]!, matrix[13]!],
      info,
      local: session.nodeLocalTransform(node),
    };
    setDragging(true);
  };
  const move = (e: ReactPointerEvent) => {
    const drag = active.current;
    if (!drag || drag.pointer !== e.pointerId) return;
    const at = world(e);
    if (typeof drag.kind === "number") {
      const delta = session.nodeDeltaFromWorld(
        node,
        at[0] - drag.from[0],
        at[1] - drag.from[1],
      );
      const mesh = drag.info.mesh;
      if (!delta || !mesh) return;
      const index = drag.kind;
      const offsets = new Float32Array(mesh.verts.length * 2);
      offsets[index * 2] = delta[0]!;
      offsets[index * 2 + 1] = delta[1]!;
      drag.offsets = offsets;
      session.scratchDeform(node, offsets);
    } else if (drag.kind === "scale") {
      const before = Math.hypot(
        drag.from[0] - drag.pivot[0],
        drag.from[1] - drag.pivot[1],
      );
      if (before < 0.001) return;
      let ratio = Math.max(
        0.02,
        Math.hypot(at[0] - drag.pivot[0], at[1] - drag.pivot[1]) / before,
      );
      if (e.shiftKey) ratio = Math.max(0.1, Math.round(ratio * 10) / 10);
      drag.patch = {
        scale: [drag.info.scale[0] * ratio, drag.info.scale[1] * ratio],
      };
      session.scratchTransform(node, {
        scale: [
          (drag.local?.[6] ?? drag.info.scale[0]) * ratio,
          (drag.local?.[7] ?? drag.info.scale[1]) * ratio,
        ],
      });
    } else {
      let angle =
        Math.atan2(at[1] - drag.pivot[1], at[0] - drag.pivot[0]) -
        Math.atan2(drag.from[1] - drag.pivot[1], drag.from[0] - drag.pivot[0]);
      angle = Math.atan2(Math.sin(angle), Math.cos(angle));
      if (e.shiftKey)
        angle = Math.round(angle / (Math.PI / 12)) * (Math.PI / 12);
      drag.patch = {
        rotate: [
          drag.info.rotate[0],
          drag.info.rotate[1],
          drag.info.rotate[2] + angle,
        ],
      };
      session.scratchTransform(node, {
        rotate: [
          drag.local?.[3] ?? drag.info.rotate[0],
          drag.local?.[4] ?? drag.info.rotate[1],
          (drag.local?.[5] ?? drag.info.rotate[2]) + angle,
        ],
      });
    }
  };
  const finish = (e: ReactPointerEvent) => {
    const drag = active.current;
    if (!drag || drag.pointer !== e.pointerId) return;
    active.current = undefined;
    setDragging(false);
    const changed = !!drag.offsets || !!drag.patch;
    const work =
      drag.recording && changed
        ? edit!.runGesture(drag.recording, () =>
            drag.offsets
              ? drag.recording!.deform(drag.offsets)
              : drag.recording!.patch(drag.patch!, true),
          )
        : drag.patch && !drag.recording
          ? session.send({ cmd: "node_set", node, ...drag.patch })
          : undefined;
    if (!changed) drag.recording?.dispose();
    void work?.catch(onError).finally(() => {
      if (!session.closed) {
        if (typeof drag.kind === "number") session.clearScratchDeform(node);
        else session.clearScratchTransform(node);
      }
    });
  };
  const editVertex = (index: number, dx: number, dy: number) => {
    if (!info?.mesh || !edit?.recording || edit.busy) return;
    const gesture = edit.beginRecording();
    if (!gesture) return;
    const offsets = new Float32Array(info.mesh.verts.length * 2);
    offsets[index * 2] = dx;
    offsets[index * 2 + 1] = dy;
    void edit.runGesture(gesture, () => gesture.deform(offsets));
  };
  const patchTransform = (patch: Patch) => {
    if (edit?.busy) return;
    if (edit?.mode === "record") {
      if (edit.recording) void edit.patch(patch, true);
    } else
      void session.send({ cmd: "node_set", node, ...patch }).catch(onError);
  };
  if (!points.length) return null;
  return (
    <svg
      ref={svg}
      data-catchlight-selection-overlay=""
      data-recording={edit?.recording ? "" : undefined}
      data-gesturing={dragging ? "" : undefined}
      width={size.width}
      height={size.height}
      viewBox={`0 0 ${size.width} ${size.height}`}
      onPointerMove={move}
      onPointerUp={finish}
      onPointerCancel={cancel}
      onLostPointerCapture={() => {
        if (active.current) cancel();
      }}
    >
      <rect
        data-catchlight-selection-box=""
        x={left}
        y={top}
        width={right - left}
        height={bottom - top}
      />
      {mesh && info?.mesh ? (
        <g data-catchlight-mesh-lines="">
          {info.mesh?.indices.map((tri, i) => (
            <polygon
              key={i}
              points={tri
                .map((index) => points[index]?.join(",") ?? "")
                .join(" ")}
            />
          ))}
          {points.map(([x, y], i) => (
            <g key={i} data-catchlight-record-vertex="">
              <circle
                cx={x}
                cy={y}
                r="4"
                data-catchlight-record-vertex-dot=""
              />
              <circle
                cx={x}
                cy={y}
                r="11"
                data-catchlight-vertex-handle=""
                data-vertex={i}
                tabIndex={0}
                role="button"
                aria-label={`Mesh vertex ${i + 1}`}
                onPointerDown={(e) => start(i, e)}
                onKeyDown={(e) => {
                  const amount = e.shiftKey ? 10 : 1;
                  const delta: Record<string, Point> = {
                    ArrowLeft: [-amount, 0],
                    ArrowRight: [amount, 0],
                    ArrowUp: [0, amount],
                    ArrowDown: [0, -amount],
                  };
                  const change = delta[e.key];
                  if (change) {
                    e.preventDefault();
                    e.stopPropagation();
                    editVertex(i, ...change);
                  }
                }}
              >
                <title>Vertex {i + 1} · drag to reshape</title>
              </circle>
            </g>
          ))}
        </g>
      ) : (
        <>
          {[
            [left, top],
            [right, top],
            [right, bottom],
            [left, bottom],
          ].map(([x, y], i) => (
            <rect
              key={i}
              data-catchlight-selection-corner=""
              x={x! - 3}
              y={y! - 3}
              width="6"
              height="6"
              role="button"
              tabIndex={0}
              aria-label={`Scale selection from corner ${i + 1}`}
              onPointerDown={(e) => start("scale", e)}
              onKeyDown={(e) => {
                if (!info) return;
                const factor =
                  e.key === "ArrowUp" || e.key === "ArrowRight"
                    ? 1.1
                    : e.key === "ArrowDown" || e.key === "ArrowLeft"
                      ? 1 / 1.1
                      : 0;
                if (factor) {
                  e.preventDefault();
                  e.stopPropagation();
                  patchTransform({
                    scale: [info.scale[0] * factor, info.scale[1] * factor],
                  });
                }
              }}
            >
              <title>Scale proportionally · Shift to snap</title>
            </rect>
          ))}
          <path
            data-catchlight-rotation-stem=""
            d={`M${(left + right) / 2} ${top}v-24`}
          />
          <circle
            data-catchlight-rotation-handle=""
            cx={(left + right) / 2}
            cy={top - 24}
            r="4"
            role="button"
            tabIndex={0}
            aria-label="Rotate selection"
            onPointerDown={(e) => start("rotate", e)}
            onKeyDown={(e) => {
              if (info && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
                e.preventDefault();
                e.stopPropagation();
                patchTransform({
                  rotate: [
                    info.rotate[0],
                    info.rotate[1],
                    info.rotate[2] +
                      ((e.key === "ArrowLeft" ? 1 : -1) * Math.PI) / 12,
                  ],
                });
              }
            }}
          >
            <title>Rotate · Shift for 15° steps</title>
          </circle>
        </>
      )}
    </svg>
  );
}
