import type { Session } from "@catchlight/core";
import {
  Viewport,
  MeshCanvas,
  useEditing,
  SelectionOverlay,
  useCanvasSelection,
  useEditor,
  useSelection,
} from "@catchlight/react";
import type { ViewportCamera } from "@catchlight/react";
import { useCallback, useEffect, useState } from "react";
import type { RefObject } from "react";

/** Selection decoration follows the same evaluated vertices as picking. The
 * handles capture their gestures; other pointers reach the persistent canvas. */
export function Stage({
  session,
  view,
  meshView,
  tool,
  grid,
  canvas,
  onError,
}: {
  session: Session | undefined;
  view: ViewportCamera;
  meshView: ViewportCamera;
  tool: "select" | "hand";
  grid: boolean;
  canvas: RefObject<HTMLCanvasElement | null>;
  onError: (cause: unknown) => void;
}) {
  const edit = useEditing();
  const { node } = useSelection();
  const drag = useCanvasSelection(session, onError);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const resized = useCallback(
    (next: { width: number; height: number }) => {
      setSize(next);
      view.onResize(next);
    },
    [view.onResize],
  );
  return (
    <div
      data-catchlight-stage=""
      data-empty={session ? undefined : ""}
      data-tool={tool}
      data-mode={edit?.mode}
      data-dragging={drag.dragging ? "" : undefined}
    >
      <Viewport.Root
        ref={canvas}
        session={session}
        camera={view.camera}
        onCameraChange={view.onCameraChange}
        onFit={view.onFit}
        onResize={resized}
        onError={onError}
        panMode={tool === "hand"}
        tabIndex={0}
        aria-label="Model canvas"
        {...drag.handlers}
      />
      {edit?.mode === "mesh" && edit.mesh && (
        <MeshCanvas
          key={`${session?.id}:${node}`}
          camera={meshView.camera}
          onCameraChange={meshView.onCameraChange}
          onFit={meshView.onFit}
          panMode={tool === "hand"}
        />
      )}
      {grid && edit?.mode !== "mesh" && <div data-catchlight-grid-overlay="" />}
      {session &&
        node &&
        tool === "select" &&
        edit?.mode !== "mesh" &&
        (edit?.mode !== "record" || edit.recording) && (
          <SelectionOverlay
            key={`${session.id}:${node}`}
            session={session}
            node={node}
            camera={view.camera}
            size={size}
            mesh={edit?.mode === "record" && edit.recordTool === "shape"}
            onError={onError}
          />
        )}
    </div>
  );
}
export function Environment() {
  const editor = useEditor();
  const [tier, setTier] = useState(editor.gpuTier());
  useEffect(
    () => editor.onGpuChanged(() => setTier(editor.gpuTier())),
    [editor],
  );
  return (
    <span data-catchlight-environment="">
      <i />
      <span data-catchlight-backend="" title="Where edits are applied">
        {editor.backendKind()}
      </span>
      <span>·</span>
      <span data-catchlight-tier="" title="Graphics backend">
        {tier ?? "Starting graphics…"}
      </span>
    </span>
  );
}
