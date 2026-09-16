/** Workspace intentions and draft lifetime. The replica remains authoritative;
 * only a mesh draft and an armed recording destination live in React state.
 * Every route out of a dirty draft goes through the same finish dialog. */
import { MeshDraft, RecordingGesture } from "@catchlight/core";
import type {
  MeshDraftView,
  BindingCellValue,
  EditOp,
  ParamId,
  RecordProperties,
  RecordingTarget,
  Session,
} from "@catchlight/core";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { ReactNode } from "react";
import { useSelection } from "./selection.js";
import { keyIndexNear, normalizedValue, valueAtKey } from "./bindings.js";
import { Modal } from "./controls.js";
import type { ErrorHandler } from "./authoring.js";

export type EditingMode = "arrange" | "mesh" | "record";
export type RecordingTool = "shape" | "transform";
const noop = () => () => {};
const zero = () => 0;
function useEditingController(
  session: Session | undefined,
  onError: ErrorHandler,
  onNotice: (s: string) => void,
) {
  const { node } = useSelection();
  const revision = useSyncExternalStore(
    session?.subscribe ?? noop,
    session?.getRevision ?? zero,
    zero,
  );
  const params = useMemo(() => session?.params() ?? [], [session, revision]);
  const info = useMemo(
    () => (node ? session?.nodeInfo(node) : undefined),
    [session, node, revision],
  );
  const bindings = useMemo(
    () => (node ? (session?.bindings(node) ?? []) : []),
    [session, node, revision],
  );
  const [mode, setMode] = useState<EditingMode>("arrange");
  const [param, setParam] = useState<ParamId>();
  const [pair, setPair] = useState<[string, string]>();
  const [recordTool, setRecordTool] = useState<RecordingTool>("shape");
  const [armed, setArmed] = useState<{ address: string; pose: string }>();
  const [busy, setBusy] = useState(false);
  const [mesh, setMesh] = useState<MeshDraftView>();
  const draft = useRef<MeshDraft | undefined>(undefined);
  const [meshError, setMeshError] = useState<string>();
  const [meshSelection, setMeshSelection] = useState<number[]>([]);
  const [meshTool, setMeshTool] = useState<"move" | "add" | "connect">("move");
  const [pending, setPending] = useState<() => void>();
  const [emptiedSlots, setEmptiedSlots] = useState<string[]>([]);
  const [poseVersion, bumpPose] = useState(0);
  const readPose = useCallback(
    () => params.map((p) => session?.paramValue(p.id) ?? p.default).join(";"),
    [session, params],
  );
  const subscribePose = useCallback(
    (fn: () => void) => session?.onInvalidate(fn) ?? (() => {}),
    [session],
  );
  const pose = useSyncExternalStore(subscribePose, readPose, () => "");
  const primary = params.find((p) => p.id === (pair?.[0] ?? param));
  const secondary = params.find((p) => p.id === pair?.[1]);
  const xValue = primary
    ? (session?.paramValue(primary.id) ?? primary.default)
    : 0;
  const yValue = secondary
    ? (session?.paramValue(secondary.id) ?? secondary.default)
    : 0;
  const relevant = bindings.filter(
    (b) => b.param === primary?.id && b.param_y == secondary?.id &&
      (recordTool === "shape" ? b.target === "deform" : b.target !== "deform"),
  );
  // A shared shelf is only a navigation aid. Each property's authored cells
  // are looked up through its own axes when recording or editing a key.
  const axisPositions = (axis: number, parameter: typeof primary): number[] =>
    [...new Set(relevant.flatMap((b) => b.key_positions[axis] ?? [])
      .concat(parameter ? [0, normalizedValue(parameter, parameter.default), 1] : [0]))]
      .sort((a, b) => a - b);
  const xPositions = axisPositions(0, primary);
  const yPositions = axisPositions(1, secondary);
  const position: [number, number] = [
    primary ? normalizedValue(primary, xValue) : 0,
    secondary ? normalizedValue(secondary, yValue) : 0,
  ];
  const x = primary ? keyIndexNear(primary, xValue, xPositions, 0.00001) : undefined;
  const y = secondary ? keyIndexNear(secondary, yValue, yPositions, 0.00001) : 0;
  const target: RecordingTarget | undefined = node && primary
    ? { node, param: primary.id, ...(secondary ? { param_y: secondary.id } : {}), position }
    : undefined;
  const address = JSON.stringify([session?.id, node, primary?.id, secondary?.id, position, recordTool]);
  const recording = mode === "record" && !!target && armed?.address === address && armed.pose === pose;
  const cellAt = (binding: typeof bindings[number], at: [number, number]): [number, number] | undefined => {
    const indices = binding.key_positions.map((axis, i) => axis.findIndex((value) => Math.abs(value - at[i]!) < 0.00001));
    return indices.every((index) => index >= 0) ? [indices[0]!, indices[1] ?? 0] : undefined;
  };
  const keyAuthored = (at: [number, number]) => relevant.some((binding) => {
    const cell = cellAt(binding, at);
    return cell && binding.authored[cell[1]]?.[cell[0]];
  });
  const authored = !!target && keyAuthored(position);
  const stale = !!draft.current?.stale;
  const pairChoices = useMemo(
    () => (session && node && param ? session.recordingPairs(node, param) : []),
    [session, node, param, revision],
  );

  useEffect(() => {
    setMode("arrange");
    setArmed(undefined);
    setPair(undefined);
    setParam(session?.params()[0]?.id);
    setMesh(undefined);
    setMeshSelection([]);
    setEmptiedSlots([]);
    return () => {
      draft.current?.dispose();
      draft.current = undefined;
      session?.setEditing(false);
    };
  }, [session]);
  useEffect(() => {
    if (param && !params.some((p) => p.id === param)) {
      setParam(params[0]?.id);
      setPair(undefined);
      setArmed(undefined);
    }
  }, [params, param]);
  useEffect(() => {
    const next =
      pairChoices.find((p) => p[0] === pair?.[0] && p[1] === pair?.[1]) ??
      pairChoices[0];
    if (next?.join("\0") !== pair?.join("\0")) {
      setPair(next);
      setArmed(undefined);
    }
  }, [pairChoices]);
  useEffect(() => {
    setArmed(undefined);
    if (info && info.kind !== "part" && info.kind !== "mesh_group")
      setRecordTool("transform");
  }, [node]);
  useEffect(() => {
    if (armed && (armed.address !== address || armed.pose !== pose)) {
      setArmed(undefined);
      onNotice("Recording stopped when the pose changed.");
    }
  }, [armed, address, pose, onNotice]);
  useEffect(() => {
    session?.setEditing(mode !== "arrange");
  }, [session, mode]);
  useEffect(() => {
    if (!mesh?.dirty) return;
    const before = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", before);
    return () => window.removeEventListener("beforeunload", before);
  }, [mesh?.dirty]);

  const discardMesh = () => {
    draft.current?.dispose();
    draft.current = undefined;
    setMesh(undefined);
    setMeshSelection([]);
    setMeshError(undefined);
    setMode("arrange");
    setPending(undefined);
  };
  const guard = (action: () => void) => {
    if (busy) return;
    if (draft.current && mesh?.dirty) {
      setPending(() => action);
      return;
    }
    if (draft.current) discardMesh();
    action();
  };
  const openMesh = () => {
    if (!session || !node || draft.current) return;
    try {
      const next = new MeshDraft(session, node);
      draft.current = next;
      setMesh(next.view());
      setMeshSelection([]);
      setMeshTool("move");
      setMeshError(undefined);
      setMode("mesh");
      setArmed(undefined);
      setEmptiedSlots([]);
      session.setEditing(true);
    } catch (e) {
      onError(e);
    }
  };
  const requestMode = (next: EditingMode) => {
    if (next === mode) return;
    guard(() => {
      setArmed(undefined);
      if (next === "mesh") openMesh();
      else setMode(next);
    });
  };
  const applyMesh = async (then?: () => void) => {
    const current = draft.current;
    if (!current || busy) return;
    if (!current.view().dirty) {
      discardMesh();
      then?.();
      return;
    }
    setBusy(true);
    try {
      const result = await current.apply();
      const emptied = result?.result === "emptied" ? result.slots : [];
      discardMesh();
      setEmptiedSlots(emptied);
      onNotice(
        emptied.length
          ? `Mesh applied. ${emptied.length} slot${emptied.length === 1 ? " needs" : "s need"} reassignment.`
          : "Mesh applied. Existing shape keys were refitted.",
      );
      then?.();
    } catch (e) {
      setMeshError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const reloadMesh = () => {
    discardMesh();
    // Opening reads the latest replica after disposing the obsolete draft.
    openMesh();
  };
  const refreshMesh = () => {
    if (draft.current) setMesh(draft.current.view());
  };
  const meshAction = (action: (d: MeshDraft) => void) => {
    if (!draft.current || busy || stale) return;
    try {
      action(draft.current);
      refreshMesh();
      setMeshError(undefined);
    } catch (e) {
      setMeshError(String(e instanceof Error ? e.message : e));
    }
  };
  const moveMesh = (selectedIndices: number[], delta: [number, number]) => {
    const current = draft.current;
    if (!current || stale || busy) return;
    try {
      current.handle.translateVertices(
        new Uint32Array(selectedIndices),
        ...delta,
      );
      const vertices = current.handle.vertices(),
        triangles = current.handle.triangles();
      const verts: [number, number][] = [],
        indices: [number, number, number][] = [];
      for (let i = 0; i < vertices.length; i += 2)
        verts.push([vertices[i]!, vertices[i + 1]!]);
      for (let i = 0; i < triangles.length; i += 3)
        indices.push([triangles[i]!, triangles[i + 1]!, triangles[i + 2]!]);
      setMesh((old) =>
        old
          ? { ...old, dirty: true, mesh: { ...old.mesh, verts, indices } }
          : old,
      );
      setMeshError(undefined);
    } catch (e) {
      setMeshError(String(e instanceof Error ? e.message : e));
    }
  };
  const selectParam = (id: ParamId) => {
    setArmed(undefined);
    setParam(id);
    setPair(undefined);
  };
  const selectCell = (cx: number, cy = y ?? 0) => {
    if (!session || !primary || busy) return;
    setArmed(undefined);
    session.setParam(primary.id, valueAtKey(primary, cx, xPositions));
    if (secondary) session.setParam(secondary.id, valueAtKey(secondary, cy, yPositions));
  };
  const arm = () => {
    if (busy || !session || !info || !primary) return;
    if (!target) return;
    if (recordTool === "shape" && !info.vertex_count) return;
    try {
      // Validate with Rust before showing the armed state. This authors nothing.
      const check = new RecordingGesture(session, target);
      check.dispose();
      setArmed({ address, pose: readPose() });
    } catch (e) {
      onError(e);
    }
  };
  const beginRecording = (): RecordingGesture | undefined => {
    if (!recording || !session || !target || busy) return;
    try {
      return new RecordingGesture(session, target);
    } catch (e) {
      setArmed(undefined);
      onError(e);
    }
  };
  const runGesture = async (
    gesture: RecordingGesture,
    commit: () => Promise<void>,
  ) => {
    setBusy(true);
    try {
      await commit();
      bumpPose((v) => v + 1);
    } catch (e) {
      setArmed(undefined);
      onError(e);
    } finally {
      gesture.dispose();
      setBusy(false);
    }
  };
  const patch = async (fields: RecordProperties, authoredBasis = false) => {
    const gesture = beginRecording();
    if (gesture)
      await runGesture(gesture, () => gesture.patch(fields, authoredBasis));
  };
  const keyAction = async (action: "neutral" | "clear") => {
    if (!session || !target || busy) return;
    setArmed(undefined);
    setBusy(true);
    try {
      const edits: EditOp[] = relevant.flatMap((binding): EditOp[] => {
        const cell = cellAt(binding, target.position);
        if (!cell) return [];
        const address = { node: target.node, param: binding.param, param_y: binding.param_y ?? null, target: binding.target };
        if (action === "clear") return [{ op: "binding_cells_unset", ...address, cells: [cell] }];
        const identity = binding.identity;
        const value: BindingCellValue = "scalar" in identity ? { scalar: identity.scalar }
          : { offsets: Array.from({ length: identity.vertex_count }, () => [...identity.offset] as [number, number]) };
        return [{ op: "binding_cells_set", ...address, cells: [{ cell, value }] }];
      });
      if (edits.length) await session.send({ cmd: "edit_apply", if_rev: revision, edits });
    } catch (e) { onError(e); }
    finally { setBusy(false); }
  };
  const posed = useMemo(() => {
    if (!session || !target) return undefined;
    try {
      const g = new RecordingGesture(session, target);
      const properties = g.posed;
      g.dispose();
      return properties;
    } catch {
      return undefined;
    }
  }, [session, address, pose, revision, poseVersion]);

  return {
    session,
    revision,
    mode,
    requestMode,
    guard,
    node,
    info,
    params,
    bindings,
    param,
    selectParam,
    primary,
    secondary,
    pair,
    pairChoices,
    setPair: (next: [string, string]) => {
      setArmed(undefined);
      setPair(next);
    },
    x,
    y,
    xValue,
    yValue,
    target,
    relevant,
    authored,
    recording,
    arm,
    stop: () => setArmed(undefined),
    recordTool,
    setRecordTool: (next: RecordingTool) => {
      setArmed(undefined);
      setRecordTool(next);
    },
    xPositions,
    yPositions,
    keyAuthored,
    selectCell,
    beginRecording,
    runGesture,
    patch,
    keyAction,
    posed,
    busy,
    mesh,
    meshSelection,
    setMeshSelection,
    meshTool,
    setMeshTool,
    meshError,
    stale,
    meshAction,
    moveMesh,
    applyMesh,
    discardMesh,
    reloadMesh,
    emptiedSlots,
    pending,
    setPending,
  };
}
export type Editing = ReturnType<typeof useEditingController>;
const Context = createContext<Editing | undefined>(undefined);
export function useEditing() {
  return useContext(Context);
}
export function EditingProvider({
  session,
  onError,
  onNotice,
  children,
}: {
  session: Session | undefined;
  onError: ErrorHandler;
  onNotice: (s: string) => void;
  children: ReactNode;
}) {
  const value = useEditingController(session, onError, onNotice);
  const continuation = value.pending;
  return (
    <Context.Provider value={value}>
      {children}
      {continuation && (
        <Modal
          title="Finish your mesh edit"
          onClose={() => value.setPending(undefined)}
        >
          <p data-catchlight-dialog-description="">
            Apply your mesh changes before continuing, or discard this draft.
            Your artwork and existing poses are preserved when you apply.
          </p>
          {value.stale && (
            <p role="alert" data-catchlight-warning="">
              The model changed while this draft was open. Reload it before
              applying.
            </p>
          )}
          <div data-catchlight-dialog-actions="">
            <button
              type="button"
              disabled={value.busy}
              onClick={() => value.setPending(undefined)}
            >
              Keep editing
            </button>
            <button
              type="button"
              disabled={value.busy}
              onClick={() => {
                value.discardMesh();
                continuation();
              }}
            >
              Discard draft
            </button>
            <button
              type="button"
              data-primary=""
              disabled={value.busy || value.stale}
              onClick={() => void value.applyMesh(continuation)}
            >
              {value.busy ? "Applying…" : "Apply & continue"}
            </button>
          </div>
        </Modal>
      )}
    </Context.Provider>
  );
}
