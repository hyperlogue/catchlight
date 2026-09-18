/** Fixed-texture mesh placement. SVG displays the original artwork and the
 * Rust draft's geometry; dragging never touches the posed model's scratch. */
import type { Camera, MeshDraftView } from "@catchlight/core";
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useEditing } from "./editing.js";
import {
  fitCamera,
  panTo,
  screenAt,
  worldAt,
  zoomAbout,
  wheelNotches,
  ZOOM_PER_NOTCH,
} from "./camera.js";
import type { Bounds, Point, Size } from "./camera.js";
import {
  Disclosure,
  EmptyState,
  Field,
  Icon,
  IconButton,
  NumberField,
} from "./controls.js";

export function meshBounds(mesh: MeshDraftView) {
  if (!mesh.mesh.verts.length) {
    const [x0, y0, x1, y1] = mesh.texture_rect;
    return [
      Math.min(x0, x1),
      Math.min(y0, y1),
      Math.max(x0, x1),
      Math.max(y0, y1),
    ] as [number, number, number, number];
  }
  const bounds: [number, number, number, number] = [
    Infinity,
    Infinity,
    -Infinity,
    -Infinity,
  ];
  for (const [x, y] of mesh.mesh.verts) {
    bounds[0] = Math.min(bounds[0], x);
    bounds[1] = Math.min(bounds[1], y);
    bounds[2] = Math.max(bounds[2], x);
    bounds[3] = Math.max(bounds[3], y);
  }
  return bounds;
}

export function MeshCanvas({
  camera,
  onCameraChange,
  onFit,
  onResize,
  panMode,
}: {
  camera: Camera;
  onCameraChange: (c: Camera) => void;
  onFit: (c: Camera, bounds?: Bounds) => void;
  onResize?: (size: Size) => void;
  panMode: boolean;
}) {
  const edit = useEditing()!;
  const { mesh, session } = edit;
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<Size>({ width: 0, height: 0 });
  const [source, setSource] = useState<string>();
  const [space, setSpace] = useState(false);
  const [opacity, setOpacity] = useState(1);
  const gesture = useRef<
    | {
        pointer: number;
        index?: number;
        indices?: number[];
        from: Point;
        camera: Camera;
        moved: boolean;
      }
    | undefined
  >(undefined);
  const last = useRef({ edit, camera, size, onCameraChange, onFit, onResize });
  last.current = { edit, camera, size, onCameraChange, onFit, onResize };
  const framed = useRef(false);
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const resize = () => {
      const next = { width: element.clientWidth, height: element.clientHeight };
      setSize(next);
      last.current.onResize?.(next);
      if (
        !framed.current &&
        next.width > 0 &&
        next.height > 0 &&
        last.current.edit.mesh
      ) {
        framed.current = true;
        const bounds = meshBounds(last.current.edit.mesh);
        const fitted = fitCamera(bounds, next);
        if (fitted) {
          last.current.onCameraChange(fitted);
          last.current.onFit(fitted, bounds);
        }
      }
    };
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    resize();
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    let live = true,
      url: string | undefined;
    setSource(undefined);
    if (!session || !mesh) return;
    try {
      const image = session.textureImage(mesh.texture);
      if (!image) throw new Error("The artwork could not be opened.");
      const canvas = document.createElement("canvas");
      canvas.width = image.width;
      canvas.height = image.height;
      canvas
        .getContext("2d")!
        .putImageData(
          new ImageData(
            new Uint8ClampedArray(image.rgba),
            image.width,
            image.height,
          ),
          0,
          0,
        );
      canvas.toBlob((blob) => {
        if (!blob || !live) return;
        url = URL.createObjectURL(blob);
        setSource(url);
      }, "image/png");
    } catch (e) {
      edit.meshAction(() => {
        throw e;
      });
    }
    return () => {
      live = false;
      if (url) URL.revokeObjectURL(url);
    };
  }, [session, mesh?.texture]);
  const cancel = () => {
    if (!gesture.current) return;
    if (gesture.current.index !== undefined)
      last.current.edit.meshAction((d) => d.handle.endGesture(false));
    gesture.current = undefined;
  };
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLElement &&
        (e.target.matches("input,textarea,select,button,summary") ||
          e.target.isContentEditable)
      )
        return;
      if (e.code === "Space") {
        setSpace(true);
        e.preventDefault();
      }
      if (
        (e.key === "Escape" ||
          ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z")) &&
        gesture.current
      ) {
        cancel();
        e.preventDefault();
        e.stopImmediatePropagation();
      }
    };
    const up = (e: KeyboardEvent) => {
      if (e.code === "Space") setSpace(false);
    };
    const blur = () => {
      setSpace(false);
      cancel();
    };
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    return () => {
      cancel();
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
    };
  }, []);
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      if (gesture.current) return;
      const rect = element.getBoundingClientRect();
      const current = last.current;
      current.onCameraChange(
        zoomAbout(
          current.camera,
          current.size,
          [e.clientX - rect.left, e.clientY - rect.top],
          ZOOM_PER_NOTCH ** wheelNotches(e.deltaY, e.deltaMode),
        ),
      );
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, []);
  if (!mesh) return null;
  const local = (e: ReactPointerEvent<SVGSVGElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    return worldAt(camera, size, [e.clientX - rect.left, e.clientY - rect.top]);
  };
  const start = (e: ReactPointerEvent<SVGSVGElement>) => {
    if (e.button !== 0 && e.button !== 1) return;
    e.preventDefault();
    e.currentTarget.focus();
    const from = local(e);
    if (panMode || space || e.button === 1) {
      gesture.current = {
        pointer: e.pointerId,
        from: [e.clientX, e.clientY],
        camera,
        moved: false,
      };
      e.currentTarget.setPointerCapture(e.pointerId);
      return;
    }
    if (edit.stale || edit.busy) return;
    const index = Number(
      (e.target as Element).getAttribute("data-mesh-vertex"),
    );
    const hit = (e.target as Element).hasAttribute("data-mesh-vertex");
    if (!hit) {
      if (edit.meshTool === "add")
        edit.meshAction((d) =>
          edit.setMeshSelection([d.handle.addVertex(...from)]),
        );
      else if (!e.shiftKey) edit.setMeshSelection([]);
      return;
    }
    if (edit.meshTool === "connect") {
      const previous = edit.meshSelection.at(-1);
      if (previous !== undefined && previous !== index)
        edit.meshAction((d) => d.handle.toggleEdge(previous, index));
      edit.setMeshSelection([index]);
      return;
    }
    if (e.shiftKey) {
      edit.setMeshSelection(
        edit.meshSelection.includes(index)
          ? edit.meshSelection.filter((v) => v !== index)
          : [...edit.meshSelection, index],
      );
      return;
    }
    const indices = edit.meshSelection.includes(index)
      ? edit.meshSelection
      : [index];
    edit.setMeshSelection(indices);
    edit.meshAction((d) => d.handle.beginGesture());
    gesture.current = {
      pointer: e.pointerId,
      index,
      indices,
      from,
      camera,
      moved: false,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const move = (e: ReactPointerEvent<SVGSVGElement>) => {
    const active = gesture.current;
    if (!active || active.pointer !== e.pointerId) return;
    if (active.index === undefined) {
      onCameraChange(
        panTo(active.camera, active.from, size, [e.clientX, e.clientY]),
      );
      return;
    }
    const at = local(e),
      dx = at[0] - active.from[0],
      dy = at[1] - active.from[1];
    if (Math.hypot(dx, dy) < 1e-6 && !active.moved) return;
    active.moved = true;
    edit.moveMesh(active.indices!, [dx, dy]);
  };
  const end = (e: ReactPointerEvent<SVGSVGElement>) => {
    if (gesture.current?.pointer !== e.pointerId) return;
    if (gesture.current.index !== undefined)
      edit.meshAction((d) => d.handle.endGesture(true));
    gesture.current = undefined;
    if (e.currentTarget.hasPointerCapture(e.pointerId))
      e.currentTarget.releasePointerCapture(e.pointerId);
  };
  const points = mesh.mesh.verts.map((p) => screenAt(camera, size, p));
  const a = screenAt(camera, size, [
    mesh.texture_rect[0],
    mesh.texture_rect[1],
  ]);
  const b = screenAt(camera, size, [
    mesh.texture_rect[2],
    mesh.texture_rect[3],
  ]);
  const nudge = (dx: number, dy: number) =>
    edit.meshAction((d) => {
      d.handle.beginGesture();
      try {
        d.handle.translateVertices(new Uint32Array(edit.meshSelection), dx, dy);
        d.handle.endGesture(true);
      } catch (e) {
        d.handle.endGesture(false);
        throw e;
      }
    });
  return (
    <div
      ref={box}
      data-catchlight-mesh-canvas=""
      data-tool={panMode || space ? "hand" : edit.meshTool}
    >
      <svg
        width={size.width}
        height={size.height}
        role="application"
        aria-label="Mesh on original artwork"
        tabIndex={0}
        onPointerDown={start}
        onPointerMove={move}
        onPointerUp={end}
        onPointerCancel={cancel}
        onLostPointerCapture={cancel}
        onKeyDown={(e) => {
          const n = e.shiftKey ? 10 : 1;
          const deltas: Record<string, [number, number]> = {
            ArrowLeft: [-n, 0],
            ArrowRight: [n, 0],
            ArrowUp: [0, n],
            ArrowDown: [0, -n],
          };
          if (deltas[e.key]) {
            e.preventDefault();
            e.stopPropagation();
            nudge(...deltas[e.key]!);
          }
          if (e.key === "Delete" || e.key === "Backspace") {
            e.preventDefault();
            e.stopPropagation();
            edit.meshAction((d) => {
              d.handle.deleteVertices(new Uint32Array(edit.meshSelection));
              edit.setMeshSelection([]);
            });
          }
          if ((e.metaKey || e.ctrlKey) && e.key === "a") {
            e.preventDefault();
            e.stopPropagation();
            edit.setMeshSelection(points.map((_, i) => i));
          }
        }}
      >
        {source && (
          <image
            data-catchlight-mesh-artwork=""
            href={source}
            width="1"
            height="1"
            preserveAspectRatio="none"
            opacity={opacity}
            transform={`matrix(${b[0] - a[0]},0,0,${b[1] - a[1]},${a[0]},${a[1]})`}
          />
        )}
        <g data-catchlight-draft-triangles="">
          {mesh.mesh.indices.map((tri, i) => (
            <polygon
              key={i}
              points={tri.map((index) => points[index]?.join(",")).join(" ")}
            />
          ))}
        </g>
        {edit.meshTool === "connect" && (
          <g data-catchlight-draft-constraints="">
            {mesh.constraints.map(([a, b]) => (
              <line
                key={`${a}:${b}`}
                x1={points[a]?.[0]}
                y1={points[a]?.[1]}
                x2={points[b]?.[0]}
                y2={points[b]?.[1]}
              />
            ))}
          </g>
        )}
        {points.map((p, i) => (
          <g
            key={i}
            data-selected={edit.meshSelection.includes(i) ? "" : undefined}
            data-catchlight-draft-vertex=""
          >
            <circle cx={p[0]} cy={p[1]} r="4" />
            <circle
              cx={p[0]}
              cy={p[1]}
              r="11"
              data-mesh-vertex={i}
              aria-label={`Mesh vertex ${i + 1}`}
              role="button"
              tabIndex={i === (edit.meshSelection[0] ?? 0) ? 0 : -1}
              onFocus={() => {
                if (!edit.meshSelection.includes(i)) edit.setMeshSelection([i]);
              }}
              data-catchlight-mesh-hit=""
            />
          </g>
        ))}
      </svg>
      <div data-catchlight-stage-caption="">
        <span>
          <Icon name="part" width="13" height="13" />
          Original artwork
        </span>
        <span>
          {edit.meshTool === "connect"
            ? "Solid edges are pinned · select two vertices to toggle"
            : "Texture space · image stays fixed"}
        </span>
      </div>
      <div data-catchlight-mesh-opacity="">
        <label>
          Artwork
          <input
            aria-label="Artwork preview opacity"
            type="range"
            min="0.1"
            max="1"
            step="0.05"
            value={opacity}
            onChange={(e) => setOpacity(Number(e.currentTarget.value))}
          />
        </label>
      </div>
      {!source && (
        <div data-catchlight-stage-loading="" role="status">
          Opening artwork…
        </div>
      )}
    </div>
  );
}

export function MeshTools() {
  const edit = useEditing()!;
  return (
    <div data-catchlight-mesh-tools="" role="toolbar" aria-label="Mesh tools">
      {(
        [
          ["move", "select", "Move vertices"],
          ["add", "plus", "Add vertex"],
          ["connect", "link", "Connect vertices"],
        ] as const
      ).map(([tool, icon, label]) => (
        <button
          type="button"
          key={tool}
          aria-pressed={edit.meshTool === tool}
          disabled={edit.busy || edit.stale}
          onClick={() => edit.setMeshTool(tool)}
        >
          <Icon name={icon} width="15" height="15" />
          {label}
        </button>
      ))}
      <IconButton
        icon="trash"
        label="Delete selected vertices"
        disabled={
          !edit.meshSelection.length ||
          edit.busy ||
          edit.stale ||
          (edit.mesh?.mesh.verts.length ?? 0) - edit.meshSelection.length < 3
        }
        onClick={() =>
          edit.meshAction((d) => {
            d.handle.deleteVertices(new Uint32Array(edit.meshSelection));
            edit.setMeshSelection([]);
          })
        }
      />
    </div>
  );
}

export function MeshInspector() {
  const edit = useEditing()!,
    mesh = edit.mesh;
  const [method, setMethod] = useState<"contour" | "grid">("contour");
  const [spacing, setSpacing] = useState(32),
    [margin, setMargin] = useState(4),
    [cols, setCols] = useState(8),
    [rows, setRows] = useState(8);
  if (!mesh)
    return (
      <EmptyState title="Select artwork">
        Choose a textured part to edit its mesh.
      </EmptyState>
    );
  const index =
    edit.meshSelection.length === 1 ? edit.meshSelection[0] : undefined;
  const vertex = index === undefined ? undefined : mesh.mesh.verts[index];
  return (
    <div data-catchlight-mesh-inspector="">
      <div data-catchlight-selection-heading="">
        <Icon name="mesh" />
        <div>
          <strong>{edit.info?.name ?? "Mesh"}</strong>
          <small>Editing base mesh</small>
        </div>
      </div>
      <div data-catchlight-edit-explainer="">
        <strong>Place the mesh on your artwork</strong>
        <p>Vertices move over the image. Your drawing stays fixed.</p>
      </div>
      <div data-catchlight-metrics="">
        <span>
          <b>{mesh.mesh.verts.length}</b> vertices
        </span>
        <span>
          <b>{mesh.mesh.indices.length}</b> triangles
        </span>
      </div>
      {edit.stale && (
        <div data-catchlight-warning="" role="alert">
          <strong>The model changed</strong>
          <p>This draft is preserved, but it can’t overwrite newer edits.</p>
          <button type="button" onClick={edit.reloadMesh}>
            Reload mesh
          </button>
        </div>
      )}
      {edit.meshError && (
        <p data-catchlight-warning="" role="status">
          {edit.meshError}
        </p>
      )}
      <Disclosure title="Selection">
        {vertex && index !== undefined ? (
          <>
            <p data-catchlight-hint="">Vertex {index + 1}</p>
            <Field label="Position">
              {([0, 1] as const).map((axis) => (
                <NumberField
                  key={axis}
                  label={`Mesh vertex ${axis === 0 ? "X" : "Y"}`}
                  prefix={axis === 0 ? "X" : "Y"}
                  value={vertex[axis]}
                  disabled={edit.stale || edit.busy}
                  onCommit={(value) =>
                    edit.meshAction((d) => {
                      d.handle.beginGesture();
                      try {
                        d.handle.moveVertex(
                          index,
                          axis === 0 ? value : vertex[0],
                          axis === 1 ? value : vertex[1],
                        );
                        d.handle.endGesture(true);
                      } catch (e) {
                        d.handle.endGesture(false);
                        throw e;
                      }
                    })
                  }
                />
              ))}
            </Field>
          </>
        ) : (
          <p data-catchlight-hint="">
            {edit.meshSelection.length
              ? `${edit.meshSelection.length} vertices selected`
              : "Click a vertex to select it. Shift-click to select several."}
          </p>
        )}
        <p data-catchlight-hint="">
          Arrow keys nudge by 1. Hold Shift for 10. Escape cancels a drag.
        </p>
      </Disclosure>
      <Disclosure title="Generate mesh" defaultOpen={false}>
        <fieldset disabled={edit.busy || edit.stale}>
          <Field label="Method">
            <select
              aria-label="Draft mesh method"
              value={method}
              onChange={(e) =>
                setMethod(e.currentTarget.value as typeof method)
              }
            >
              <option value="contour">Trace artwork</option>
              <option value="grid">Regular grid</option>
            </select>
          </Field>
          {method === "contour" ? (
            <>
              <Field label="Spacing">
                <NumberField
                  label="Draft mesh spacing"
                  value={spacing}
                  min={1}
                  max={4096}
                  unit="px"
                  onCommit={setSpacing}
                />
              </Field>
              <Field label="Margin">
                <NumberField
                  label="Draft mesh margin"
                  value={margin}
                  min={0}
                  max={128}
                  unit="px"
                  onCommit={setMargin}
                />
              </Field>
            </>
          ) : (
            <Field label="Grid">
              <NumberField
                label="Draft mesh columns"
                value={cols}
                min={2}
                max={128}
                step={1}
                prefix="C"
                onCommit={setCols}
              />
              <NumberField
                label="Draft mesh rows"
                value={rows}
                min={2}
                max={128}
                step={1}
                prefix="R"
                onCommit={setRows}
              />
            </Field>
          )}
          <button
            type="button"
            data-catchlight-action=""
            onClick={() =>
              edit.meshAction((d) => {
                if (method === "contour")
                  d.handle.generateContour(spacing, margin);
                else d.handle.generateGrid(cols, rows);
                edit.setMeshSelection([]);
              })
            }
          >
            <Icon name="mesh" />
            Generate draft
          </button>
          <p data-catchlight-hint="">
            Replaces this draft’s topology. You can Undo or Cancel before
            applying.
          </p>
        </fieldset>
      </Disclosure>
      <div data-catchlight-edit-footer="">
        <p>
          Apply saves one mesh edit and refits existing shape keys. Filled slots
          will need reassignment.
        </p>
        <span data-catchlight-draft-state="">
          {mesh.dirty ? "Unsaved mesh changes" : "No mesh changes"}
        </span>
      </div>
    </div>
  );
}
