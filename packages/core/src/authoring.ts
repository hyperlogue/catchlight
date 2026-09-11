/** Owned Rust authoring drafts and gesture commits. The browser owns their
 * lifetime; Rust owns geometry and key values. Every commit carries the
 * revision it started from, so a delayed HTTP request cannot overwrite a
 * newer edit even when the replica has not received that edit yet. */
import type {
  BindingKeyEntry,
  BindingParams,
  MeshInfo,
  NodeId,
  NodePatch,
} from "./protocol.gen.js";
import type { Session } from "./session.js";
import type { WasmMeshDraft, WasmRecording } from "./wasm.js";

export interface MeshDraftView {
  mesh: MeshInfo;
  constraints: [number, number][];
  texture_rect: [number, number, number, number];
  texture: string;
  dirty: boolean;
  can_undo: boolean;
  can_redo: boolean;
}

export type RecordProperties = Pick<
  NodePatch,
  | "translate"
  | "rotate"
  | "scale"
  | "z_order"
  | "opacity"
  | "tint"
  | "screen_tint"
>;
export type RecordingTarget = BindingParams & {
  node: NodeId;
  cell: [number, number];
};

export class MeshDraft {
  readonly revision: number;
  readonly node: NodeId;
  readonly handle: WasmMeshDraft;
  #closed = false;
  #session: Session;
  constructor(session: Session, node: NodeId) {
    this.#session = session;
    this.node = node;
    this.revision = session.getRevision();
    this.handle = session.replica.meshDraft(node);
  }
  get stale(): boolean {
    return (
      this.#session.closed || this.revision !== this.#session.getRevision()
    );
  }
  view(): MeshDraftView {
    return JSON.parse(this.handle.view()) as MeshDraftView;
  }
  async apply() {
    if (!this.view().dirty) return undefined;
    if (this.stale)
      throw new Error(
        "The model changed. Reload the mesh draft before applying it.",
      );
    const mesh = JSON.parse(this.handle.finish()) as MeshInfo;
    if (!mesh.indices.length)
      throw new Error(
        "The mesh does not cover any artwork. Add vertices over the image before applying.",
      );
    return this.#session.send({
      cmd: "mesh_set",
      node: this.node,
      if_rev: this.revision,
      ...mesh,
    });
  }
  dispose() {
    if (!this.#closed) {
      this.#closed = true;
      this.handle.free();
    }
  }
}

export class RecordingGesture {
  readonly revision: number;
  readonly target: RecordingTarget;
  readonly posed: RecordProperties;
  #session: Session;
  #handle: WasmRecording;
  #closed = false;
  constructor(session: Session, target: RecordingTarget) {
    this.#session = session;
    this.revision = session.getRevision();
    this.target = { ...target, cell: [...target.cell] };
    this.#handle = session.replica.recording(
      target.node,
      target.param,
      target.param_y ?? undefined,
      ...target.cell,
    );
    this.posed = JSON.parse(this.#handle.posed()) as RecordProperties;
  }
  #check() {
    if (
      this.#closed ||
      this.#session.closed ||
      this.#session.getRevision() !== this.revision
    ) {
      throw new Error(
        "The model changed during this gesture. Start the gesture again.",
      );
    }
  }
  async patch(
    properties: RecordProperties,
    authoredBasis = false,
  ): Promise<void> {
    this.#check();
    const entries = JSON.parse(
      this.#handle.patch(JSON.stringify(properties), authoredBasis),
    ) as BindingKeyEntry[];
    if (entries.length)
      await this.#session.send({
        cmd: "binding_keys",
        ...this.target,
        if_rev: this.revision,
        entries,
      });
  }
  async deform(deltas: Float32Array): Promise<void> {
    this.#check();
    if (!deltas.some((v) => Math.abs(v) > 1e-6)) return;
    const values = this.#handle.deform(deltas);
    const offsets: [number, number][] = [];
    for (let i = 0; i < values.length; i += 2)
      offsets.push([values[i]!, values[i + 1]!]);
    await this.#session.send({
      cmd: "deform_vertices",
      ...this.target,
      if_rev: this.revision,
      offsets,
    });
  }
  dispose() {
    if (!this.#closed) {
      this.#closed = true;
      this.#handle.free();
    }
  }
}
