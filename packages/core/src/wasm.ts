/**
 * The wasm module's JS surface, declared structurally so the host injects the
 * module and tests can supply a fake. Per-frame calls are typed methods, not
 * JSON, so a drag spends no `JSON.parse` per event.
 */

import type { NodeId, ParamId, TexId } from "./protocol.gen.js";
import type { Attachment } from "./backend.js";

/**
 * Members are required: `Viewport`'s construct signature checks parameters
 * contravariantly, so optional members would stop the generated module from
 * being assignable to [`WasmModule`].
 */
export interface WasmOwned {
  free(): void;
  [Symbol.dispose](): void;
}

export interface WasmEditor extends WasmOwned {
  handle(requestJson: string): string;
  handleWith(
    requestJson: string,
    attachments: Attachment[],
  ): { reply: string; payload?: Uint8Array };
  /** Events since the last drain; Rust never calls back into JS. */
  drainEvents(): string[];
  /** Outward only: what a save or export wrote, until a backend drains it. */
  takeBytes(key: string): Uint8Array | undefined;
  writtenKeys(): string[];
}

/**
 * Acquired from the first canvas. On WebGL2 the device is that canvas's
 * context and later canvases are fed pixels; WebGPU ignores the canvas.
 */
export interface WasmGpu extends WasmOwned {
  /** `max_texture_dimension_2d`; the viewport goes black silently past it. */
  maxSize(): number;
  /** `"webgpu"` or `"webgl2"`. For display; never branch on it. */
  tier(): string;
}

/** Only `id` is read here; the hints are for whoever decodes the bytes. */
export interface TextureRequest {
  id: TexId;
  encoding: string;
  alpha: string;
}

/** Keyed by hash too: an extension's bytes change under an unchanged key. */
export interface ExtensionRequest {
  key: string;
  hash: string;
}

export interface WasmReplica extends WasmOwned {
  meshDraft(node: NodeId): WasmMeshDraft;
  recordingPairs(node: NodeId, param: ParamId): string;
  recording(
    node: NodeId,
    param: ParamId,
    paramY: string | undefined,
    x: number,
    y: number,
  ): WasmRecording;
  setEditing(editing: boolean): void;
  textureImage(texture: string): ReturnType<WasmReplica["textureThumbnail"]>;
  rev(): number;

  texturesNeeded(structure: Uint8Array): string;
  putTexture(id: TexId, bytes: Uint8Array): void;
  /** JSON extensions are never listed: they ride inline in the structure. */
  extensionsNeeded(structure: Uint8Array): string;
  putExtension(key: string, bytes: Uint8Array): void;
  /**
   * Forward only: a `rev` at or below the current one returns false. Throws a
   * string on a malformed structure or a missing payload, leaving the model
   * untouched.
   */
  applyStructure(structure: Uint8Array, rev: number): boolean;
  /** In-tab handoff from the editor's own model. Returns the rev. */
  syncFromEditor(editor: WasmEditor, session: number): number;

  query(requestJson: string): string;

  setParam(id: ParamId, value: number): boolean;
  paramValue(id: ParamId): number | undefined;
  /** `undefined` clears the control's preview value. */
  setControl(name: string, value: number | undefined): void;
  setExpressionWeight(name: string, weight: number | undefined): void;
  setVisemeWeight(name: string, weight: number | undefined): void;
  clearInputs(): void;
  /** JSON `PreviewInputs`. */
  inputs(): string;
  play(clip: string): boolean;
  stop(): void;
  /** JSON `Playing`; `undefined` when nothing plays. */
  playing(): string | undefined;
  /** Packed `[dx0, dy0, dx1, dy1, ...]`. */
  scratchDeform(node: NodeId, offsets: Float32Array): boolean;
  clearScratchDeform(node: NodeId): boolean;
  /** Absolute values; `NaN` leaves what the fold produced for that field. */
  scratchTransform(
    node: NodeId,
    tx: number,
    ty: number,
    tz: number,
    rx: number,
    ry: number,
    rz: number,
    sx: number,
    sy: number,
    zOrder: number,
    opacity: number,
  ): boolean;
  clearScratchTransform(node: NodeId): boolean;
  clearAllScratch(): void;

  /** 16 floats, column-major. */
  nodeWorldTransform(node: NodeId): Float32Array | undefined;
  pickNode(x: number, y: number): string | undefined;
  /** Flattened world-space pairs. */
  nodeVertices(node: NodeId): Float32Array | undefined;
  nodeLocalTransform(node: NodeId): Float32Array | undefined;
  nodeDeltaFromWorld(
    node: NodeId,
    x: number,
    y: number,
  ): Float32Array | undefined;
  textureThumbnail(texture: string):
    | {
        width: number;
        height: number;
        pixels(): Uint8Array;
        free(): void;
        [Symbol.dispose](): void;
      }
    | undefined;
  /** Result is in the parent's frame. */
  translationAfterWorldDelta(
    node: NodeId,
    dx: number,
    dy: number,
  ): Float32Array | undefined;
  /**
   * Posed world box `[min_x, min_y, max_x, max_y]`, Y-up; `undefined` when
   * nothing draws.
   */
  bounds(): Float32Array | undefined;
}

/** Releasing one never changes the model. */
export interface WasmMeshDraft extends WasmOwned {
  view(): string;
  finish(): string;
  vertices(): Float32Array;
  triangles(): Uint32Array;
  beginGesture(): void;
  endGesture(commit: boolean): void;
  moveVertex(index: number, x: number, y: number): void;
  translateVertices(indices: Uint32Array, dx: number, dy: number): void;
  addVertex(x: number, y: number): number;
  deleteVertices(indices: Uint32Array): void;
  toggleEdge(a: number, b: number): void;
  generateContour(spacing: number, margin: number): void;
  generateGrid(cols: number, rows: number): void;
  undo(): void;
  redo(): void;
}

export interface WasmRecording extends WasmOwned {
  posed(): string;
  patch(json: string, authoredBasis: boolean): string;
  deform(deltas: Float32Array): string;
}

export interface WasmFrame {
  /** Device pixels, not CSS size. */
  width: number;
  height: number;
  /** `width * height * 4` bytes, RGBA whatever the surface prefers. */
  rgba: Uint8Array;
}

export interface WasmViewport extends WasmOwned {
  /** At most one frame follows. */
  invalidate(): void;
  /** Device pixels; the caller has already resized the canvas. */
  resize(width: number, height: number): void;
  setCamera(centerX: number, centerY: number, height: number): void;
  /** Idempotent. */
  start(): void;
  /** Idempotent; `start` works again afterwards. */
  stop(): void;
  /**
   * For browser tests: headless Chromium with WebGPU never composites the
   * canvas, so screenshots are blank.
   */
  readback(): Promise<WasmFrame>;
}

/**
 * `Gpu.acquire` takes a canvas because WebGL2 enumerates adapters from a
 * canvas context; everything after it is synchronous.
 */
export interface WasmModule {
  CatchlightEditor: new () => WasmEditor;
  /** A pure function of the JSON; needs no session. */
  manifestRequirements(json: string): string[];
  Gpu: { acquire(canvas: HTMLCanvasElement): Promise<WasmGpu> };
  Replica: new () => WasmReplica;
  Viewport: new (
    gpu: WasmGpu,
    replica: WasmReplica,
    canvas: HTMLCanvasElement,
  ) => WasmViewport;
}
