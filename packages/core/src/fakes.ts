/**
 * Hand-written stand-ins for the wasm module and the network, for the tests.
 *
 * Deliberately not the real module: the suites assert the *contract* — that a
 * document command resolves only once the replica can answer for it, that a
 * drag never becomes a revision, that a feed fetches exactly the textures a
 * structure named — and each of those is a few lines here against 2 MiB of
 * WebAssembly, a GPU and a fixture model. The real module gets its own thin
 * integration suite; that one proves the wiring, these prove the rules.
 *
 * The fake editor keeps one small document per session and emits the events
 * the real one would. The fake replica holds whatever it was last fed and
 * answers the two reads the panels want. A "structure" here is JSON, because
 * the only thing these tests care about is that the right bytes reached the
 * right call.
 */

import type {
  BindingInfo,
  Command,
  Event,
  NodeInfo,
  ParamInfo,
  Presence,
  ResponseBody,
  SessionInfo,
  TexInfo,
  TreeNode,
} from "./protocol.gen.js";
import type { Backend, OkReply, Request, Unsubscribe } from "./backend.js";
import { FeedQueue, ProtocolError } from "./backend.js";
import type { FetchInit, HttpResponse, SocketLike } from "./connected.js";
import type { TextureRequest, WasmEditor, WasmGpu, WasmModule, WasmReplica, WasmViewport } from "./wasm.js";

/** One document, as both the fake editor and the fake replica hold it. */
export interface FakeDoc {
  rev: number;
  title: string;
  file: string | null;
  root: TreeNode;
  params: ParamInfo[];
  textures: TexInfo[];
  /**
   * What each part draws, keyed by node Id — the model's albedo, which is the
   * one node field this fake tracks. A part missing here draws nothing, and
   * that is a state a panel renders differently from an unmapped kind.
   */
  albedo: Record<string, string>;
  bindings: FakeBinding[];
}

/**
 * A binding as the fake stores it: the cells a command authored, and nothing
 * derived.
 *
 * Sparse like the model's own, so `binding_list` has holes to report — but
 * *only* the cells a command named. The real model also authors the identity
 * at the rest cell alongside a binding's first key; that is a model rule the
 * Rust suite pins, and a fake that copied it would make these tests depend on
 * it.
 */
export interface FakeBinding {
  node: string;
  target: string;
  param: string;
  param_y?: string | null;
  interpolate: string;
  /** `value: null` is authored with no scalar of its own, as a deform cell is. */
  cells: { x: number; y: number; value: number | null }[];
}

export function emptyDoc(title: string): FakeDoc {
  return {
    rev: 1,
    title,
    file: null,
    root: { id: "root", name: "root", kind: "group", z_order: 0, enabled: true, children: [] },
    params: [],
    textures: [],
    albedo: {},
    bindings: [],
  };
}

/** A document as bytes, the way a structure container travels. */
export function structureBytes(doc: FakeDoc): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(doc));
}

export function readStructure(bytes: Uint8Array): FakeDoc {
  return JSON.parse(new TextDecoder().decode(bytes)) as FakeDoc;
}

/** The wasm editor, answering the commands these tests send. */
export class FakeEditor implements WasmEditor {
  docs = new Map<number, FakeDoc>();
  staged = new Map<string, Uint8Array>();
  requests: Request[] = [];
  /** What `manifest_requirements` reports. */
  requirements: string[] = [];
  /** Commands to refuse instead of running, keyed by `cmd`. */
  refuse = new Map<string, { code: string; message: string }>();
  /** What `presence_set` last stored per session, and `presence_get` answers. */
  presence = new Map<number, Presence>();
  freed = false;

  #events: string[] = [];
  #nextSession = 1;
  #nextNode = 1;

  handle(requestJson: string): string {
    const request = JSON.parse(requestJson) as Request;
    this.requests.push(request);
    const id = request.id;
    const refusal = this.refuse.get(request.cmd);
    if (refusal) return JSON.stringify({ reply: "err", id, ...refusal });

    switch (request.cmd) {
      case "session_new":
        return this.#opened(id, request.name ?? "untitled", null);
      case "session_open": {
        if (!this.staged.has(request.path)) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "io",
            message: `${JSON.stringify(request.path)} was not staged`,
          });
        }
        return this.#opened(id, request.path, request.path);
      }
      case "session_import": {
        const missing = this.requirements.filter((key) => !this.staged.has(key));
        if (missing.length > 0) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "io",
            message: `not staged: ${missing.join(", ")}`,
          });
        }
        return this.#opened(id, request.manifest_path, request.manifest_path);
      }
      case "manifest_requirements":
        return this.#ok(id, { result: "manifest_requirements", textures: this.requirements });
      case "session_list": {
        const sessions: SessionInfo[] = [...this.docs].map(([session, doc]) => ({
          session,
          title: doc.title,
          file: doc.file,
          dirty: false,
          rev: doc.rev,
          node_count: 1 + doc.root.children.length,
        }));
        return this.#ok(id, { result: "sessions", sessions });
      }
      case "node_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const node = `${request.parent}/${request.kind}-${this.#nextNode++}`;
        doc.root.children.push({
          id: node,
          name: request.name ?? node,
          kind: request.kind,
          z_order: 0,
          enabled: true,
          children: [],
        });
        doc.rev += 1;
        this.#emit({ event: "document_changed", session: request.session, rev: doc.rev });
        return this.#ok(id, { result: "node", node }, doc.rev);
      }
      // Only the albedo: it is the one node field the fake holds, because
      // `texture` and `clear_texture` are two spellings of it and a panel has
      // to be able to tell them apart.
      case "node_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.clear_texture) delete doc.albedo[request.node];
        else if (request.texture) doc.albedo[request.node] = request.texture;
        doc.rev += 1;
        this.#emit({ event: "document_changed", session: request.session, rev: doc.rev });
        return this.#ok(id, { result: "node", node: request.node }, doc.rev);
      }
      // A texture belongs to a part: it arrives named by the node that draws
      // it, and the model gains both in one edit.
      case "texture_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const texture = `tex-${doc.textures.length + 1}`;
        doc.textures.push({ id: texture, width: 4, height: 4 });
        doc.albedo[request.node] = texture;
        doc.rev += 1;
        this.#emit({ event: "document_changed", session: request.session, rev: doc.rev });
        return this.#ok(id, { result: "texture", texture, dropped: [] }, doc.rev);
      }
      case "param_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const param = `param-${doc.params.length + 1}`;
        doc.params.push({
          id: param,
          name: request.name,
          min: request.min,
          max: request.max,
          default: request.default,
          key_positions: request.key_positions,
          bindings: 0,
        });
        doc.rev += 1;
        this.#emit({ event: "document_changed", session: request.session, rev: doc.rev });
        return this.#ok(id, { result: "param", param }, doc.rev);
      }
      case "param_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        doc.params = doc.params.filter((p) => p.id !== request.param);
        doc.bindings = doc.bindings.filter(
          (b) => b.param !== request.param && b.param_y !== request.param,
        );
        return this.#changed(id, request.session, doc);
      }
      case "binding_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        binding(doc, request);
        return this.#changed(id, request.session, doc);
      }
      case "binding_key": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const b = binding(doc, request);
        const [x, y] = request.cell;
        const at = b.cells.find((c) => c.x === x && c.y === y);
        if (at) at.value = request.value;
        else b.cells.push({ x, y, value: request.value });
        return this.#changed(id, request.session, doc);
      }
      case "binding_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        doc.bindings = doc.bindings.filter((b) => !addresses(b, request));
        return this.#changed(id, request.session, doc);
      }
      case "save": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const key = request.path ?? doc.file ?? "untitled.clm";
        this.staged.set(key, structureBytes(doc));
        doc.file = key;
        return this.#ok(id, { result: "saved", path: key }, doc.rev);
      }
      case "export_manifest": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        this.staged.set(request.path, new TextEncoder().encode("{}"));
        this.staged.set("tex0.png", new TextEncoder().encode("a texture"));
        return this.#ok(id, { result: "saved", path: request.path }, doc.rev);
      }
      case "session_close": {
        if (!this.docs.delete(request.session)) return this.#noSession(id, request.session);
        this.presence.delete(request.session);
        this.#emit({ event: "sessions_changed" });
        // No rev: the session it would have named is gone.
        return this.#ok(id, { result: "empty" });
      }
      case "presence_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const { cmd: _cmd, id: _id, session, ...presence } = request;
        this.presence.set(session, presence);
        return this.#ok(id, { result: "empty" }, doc.rev);
      }
      case "presence_get": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const presence = this.presence.get(request.session) ?? null;
        return this.#ok(id, { result: "presence", presence }, doc.rev);
      }
      default: {
        const session = "session" in request ? (request.session as number) : undefined;
        return this.#ok(
          id,
          { result: "empty" },
          session === undefined ? undefined : this.docs.get(session)?.rev,
        );
      }
    }
  }

  /** Moves the document on and answers `empty`, as an edit with no Id does. */
  #changed(id: number, session: number, doc: FakeDoc): string {
    doc.rev += 1;
    this.#emit({ event: "document_changed", session, rev: doc.rev });
    return this.#ok(id, { result: "empty" }, doc.rev);
  }

  /** What a replica gets when it syncs from this editor. */
  snapshot(session: number): FakeDoc {
    const doc = this.docs.get(session);
    if (!doc) throw `no session ${session}`;
    return readStructure(structureBytes(doc));
  }

  drainEvents(): string[] {
    const events = this.#events;
    this.#events = [];
    return events;
  }

  putBytes(key: string, bytes: Uint8Array): void {
    this.staged.set(key, bytes);
  }

  takeBytes(key: string): Uint8Array | undefined {
    const bytes = this.staged.get(key);
    this.staged.delete(key);
    return bytes;
  }

  stagedKeys(): string[] {
    return [...this.staged.keys()].sort();
  }

  free(): void {
    this.freed = true;
  }

  [Symbol.dispose](): void {
    this.free();
  }

  #opened(id: number, title: string, file: string | null): string {
    const session = this.#nextSession++;
    const doc = emptyDoc(title);
    doc.file = file;
    this.docs.set(session, doc);
    this.#emit({ event: "sessions_changed" });
    return this.#ok(id, { result: "session", session }, doc.rev);
  }

  #noSession(id: number, session: number): string {
    return JSON.stringify({ reply: "err", id, code: "no_session", message: `no session ${session}` });
  }

  #ok(id: number, body: ResponseBody, rev?: number): string {
    return JSON.stringify(rev === undefined ? { reply: "ok", id, body } : { reply: "ok", id, rev, body });
  }

  #emit(event: Event): void {
    this.#events.push(JSON.stringify(event));
  }
}

/** One device. Reports a small limit so the clamp is testable. */
export class FakeGpu implements WasmGpu {
  freed = false;
  /** How many times `acquire` ran. One per editor, however many canvases ask. */
  acquires = 0;
  maxSize(): number {
    return 4096;
  }
  free(): void {
    this.freed = true;
  }
  [Symbol.dispose](): void {
    this.free();
  }
}

/** This tab's copy of one document, and what it was told. */
export class FakeReplica implements WasmReplica {
  doc: FakeDoc | undefined;
  applied: Array<{ rev: number; textures: string[] }> = [];
  syncs: number[] = [];
  held = new Set<string>();
  pose = new Map<string, number>();
  scratchDeforms = new Map<string, Float32Array>();
  scratchTransforms = new Map<string, number[]>();
  /** Authored local translations, which `translationAfterWorldDelta` reads. */
  translations = new Map<string, [number, number, number]>();
  /** What `bounds()` answers: `[min_x, min_y, max_x, max_y]`, or nothing drawn. */
  box: [number, number, number, number] | undefined;
  freed = false;

  #rev = 0;

  rev(): number {
    return this.#rev;
  }

  texturesNeeded(structure: Uint8Array): string {
    const doc = readStructure(structure);
    const needed: TextureRequest[] = doc.textures
      .filter((texture) => !this.held.has(texture.id))
      .map((texture) => ({ id: texture.id, encoding: "png", alpha: "straight" }));
    return JSON.stringify(needed);
  }

  putTexture(id: string): void {
    this.held.add(id);
  }

  applyStructure(structure: Uint8Array, rev: number): boolean {
    const doc = readStructure(structure);
    const missing = doc.textures.filter((texture) => !this.held.has(texture.id));
    if (missing.length > 0) throw `missing textures: ${missing.map((t) => t.id).join(", ")}`;
    if (rev <= this.#rev) return false;
    this.doc = doc;
    this.#rev = rev;
    this.applied.push({ rev, textures: [...this.held] });
    return true;
  }

  syncFromEditor(editor: WasmEditor, session: number): number {
    const doc = (editor as FakeEditor).snapshot(session);
    this.syncs.push(session);
    if (doc.rev <= this.#rev) return this.#rev;
    this.doc = doc;
    this.#rev = doc.rev;
    return this.#rev;
  }

  query(requestJson: string): string {
    const request = JSON.parse(requestJson) as Request;
    const id = request.id;
    const doc = this.doc;
    if (!doc) {
      return JSON.stringify({ reply: "err", id, code: "no_session", message: "nothing fed yet" });
    }
    switch (request.cmd) {
      case "node_tree":
        return JSON.stringify({ reply: "ok", id, rev: this.#rev, body: { result: "tree", root: doc.root } });
      case "param_list":
        return JSON.stringify({ reply: "ok", id, rev: this.#rev, body: { result: "params", params: doc.params } });
      case "texture_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "textures", textures: doc.textures },
        });
      case "binding_list": {
        if (!holds(doc.root, request.node)) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "no_node",
            message: `no node ${request.node}`,
          });
        }
        const bindings = doc.bindings
          .filter((b) => b.node === request.node)
          .map((b) => bindingInfo(doc, b));
        return JSON.stringify({ reply: "ok", id, rev: this.#rev, body: { result: "bindings", bindings } });
      }
      case "node_info": {
        const node = nodeInfo(doc, doc.root, request.node);
        if (!node) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "no_node",
            message: `no node ${request.node}`,
          });
        }
        return JSON.stringify({ reply: "ok", id, rev: this.#rev, body: { result: "node_info", node } });
      }
      default:
        return JSON.stringify({
          reply: "err",
          id,
          code: "bad_request",
          message: `${request.cmd} is not a replica query`,
        });
    }
  }

  setParam(id: string, value: number): boolean {
    this.pose.set(id, value);
    return true;
  }

  paramValue(id: string): number | undefined {
    return this.pose.get(id);
  }

  scratchDeform(node: string, offsets: Float32Array): boolean {
    this.scratchDeforms.set(node, offsets);
    return true;
  }

  clearScratchDeform(node: string): boolean {
    return this.scratchDeforms.delete(node);
  }

  scratchTransform(node: string, ...values: number[]): boolean {
    this.scratchTransforms.set(node, values);
    return true;
  }

  clearScratchTransform(node: string): boolean {
    return this.scratchTransforms.delete(node);
  }

  /** Identity: the fake has no fold, and nothing here reads the rotation. */
  nodeWorldTransform(node: string): Float32Array | undefined {
    if (!this.#holds(node)) return undefined;
    const world = new Float32Array(16);
    world[0] = world[5] = world[10] = world[15] = 1;
    return world;
  }

  /** The stored local translation plus the delta, the parent frame being identity. */
  translationAfterWorldDelta(node: string, dx: number, dy: number): Float32Array | undefined {
    if (!this.#holds(node)) return undefined;
    const [x, y, z] = this.translations.get(node) ?? [0, 0, 0];
    return new Float32Array([x + dx, y + dy, z]);
  }

  /** Whether the fed document names `node` anywhere in its tree. */
  #holds(node: string): boolean {
    return this.doc ? holds(this.doc.root, node) : false;
  }

  clearAllScratch(): void {
    this.scratchDeforms.clear();
    this.scratchTransforms.clear();
  }

  /**
   * Whatever a test set, and `undefined` until one does — the fake has no
   * geometry, and a replica that has drawn nothing is exactly the case a fit
   * has to survive.
   */
  bounds(): Float32Array | undefined {
    return this.box ? new Float32Array(this.box) : undefined;
  }

  free(): void {
    this.freed = true;
  }

  [Symbol.dispose](): void {
    this.free();
  }
}

/**
 * The tree row for `node`, as the `node_info` reply carries it.
 *
 * A fake document holds a tree and its albedo, so the transform is the rest
 * pose and the values are defaults. Which fields are *present* is not a
 * default: a panel decides what to draw from the kind carrying a field or not,
 * so the fake omits the same ones the model does — colour on anything but a
 * part or a composite, `mg_*` off a mesh group, and `texture` off a part that
 * draws none.
 */
function nodeInfo(
  doc: FakeDoc,
  tree: TreeNode,
  node: string,
  parent?: string,
): NodeInfo | undefined {
  if (tree.id === node) {
    const drawn = tree.kind === "part" || tree.kind === "composite";
    return {
      id: tree.id,
      kind: tree.kind,
      parent: parent ?? null,
      name: tree.name,
      translate: [0, 0, 0],
      rotate: [0, 0, 0],
      scale: [1, 1],
      z_order: tree.z_order,
      enabled: tree.enabled,
      lock_to_root: false,
      ...(drawn
        ? {
            opacity: 1,
            blend_mode: "Normal",
            tint: [1, 1, 1] as [number, number, number],
            screen_tint: [0, 0, 0] as [number, number, number],
            mask_threshold: 0.5,
          }
        : {}),
      ...(tree.kind === "part" && doc.albedo[tree.id] !== undefined
        ? { texture: doc.albedo[tree.id] }
        : {}),
      ...(tree.kind === "composite" ? { propagate_meshgroup: false } : {}),
      ...(tree.kind === "mesh_group"
        ? { mg_dynamic: false, mg_translate_children: true }
        : {}),
    };
  }
  for (const child of tree.children) {
    const found = nodeInfo(doc, child, node, tree.id);
    if (found) return found;
  }
  return undefined;
}

/** Whether `tree` names `node` anywhere under it. */
function holds(tree: TreeNode, node: string): boolean {
  return tree.id === node || tree.children.some((child) => holds(child, node));
}

/** Whether `binding` is the one a command addresses. */
function addresses(
  binding: FakeBinding,
  at: { node: string; target: string; param: string; param_y?: string | null },
): boolean {
  return (
    binding.node === at.node &&
    binding.target === at.target &&
    binding.param === at.param &&
    (binding.param_y ?? null) === (at.param_y ?? null)
  );
}

/** The binding a command addresses, created if this is the first mention. */
function binding(
  doc: FakeDoc,
  at: { node: string; target: string; param: string; param_y?: string | null },
): FakeBinding {
  const found = doc.bindings.find((b) => addresses(b, at));
  if (found) return found;
  const made: FakeBinding = {
    node: at.node,
    target: at.target,
    param: at.param,
    param_y: at.param_y ?? null,
    interpolate: "linear",
    cells: [],
  };
  doc.bindings.push(made);
  return made;
}

/**
 * A binding's grid, as `binding_list` reports it: the product of its params'
 * key positions, with the cells nobody authored left null.
 *
 * Built here rather than stored, because the grid is sized by the params and
 * the model stores only the authored cells — a fake that stored a rectangle
 * would answer a question the real one derives.
 */
function bindingInfo(doc: FakeDoc, b: FakeBinding): BindingInfo {
  const keyCount = (param: string): number =>
    Math.max(1, doc.params.find((p) => p.id === param)?.key_positions.length ?? 1);
  const width = keyCount(b.param);
  const height = b.param_y ? keyCount(b.param_y) : 1;
  const keys: (number | null)[][] = [];
  const authored: boolean[][] = [];
  for (let y = 0; y < height; y++) {
    keys.push(new Array<number | null>(width).fill(null));
    authored.push(new Array<boolean>(width).fill(false));
  }
  for (const cell of b.cells) {
    const row = keys[cell.y];
    const flags = authored[cell.y];
    if (!row || !flags || cell.x >= width) continue;
    row[cell.x] = cell.value;
    flags[cell.x] = true;
  }
  return {
    target: b.target,
    param: b.param,
    param_y: b.param_y ?? null,
    interpolate: b.interpolate,
    width,
    height,
    keys,
    authored,
  };
}

/** A renderer that counts what it was told, and draws nothing. */
export class FakeViewport implements WasmViewport {
  started = 0;
  stopped = 0;
  invalidated = 0;
  freed = 0;
  size: [number, number] | undefined;
  camera: [number, number, number] | undefined;
  /** What `readback` resolves with. One grey pixel unless a test says more. */
  frame = { width: 1, height: 1, rgba: new Uint8Array([0x80, 0x80, 0x80, 0xff]) };

  readback(): Promise<{ width: number; height: number; rgba: Uint8Array }> {
    return Promise.resolve(this.frame);
  }

  start(): void {
    this.started += 1;
  }
  stop(): void {
    this.stopped += 1;
  }
  invalidate(): void {
    this.invalidated += 1;
  }
  resize(width: number, height: number): void {
    this.size = [width, height];
  }
  setCamera(x: number, y: number, height: number): void {
    this.camera = [x, y, height];
  }
  free(): void {
    this.freed += 1;
  }
  [Symbol.dispose](): void {
    this.free();
  }
}

export interface FakeModule {
  module: WasmModule;
  gpu: FakeGpu;
  replicas: FakeReplica[];
  viewports: FakeViewport[];
  /** Set to make the next `Gpu.acquire` reject with this message, once. */
  failNextAcquire: string | undefined;
}

/** The four classes `@catchlight/wasm` exports, in memory. */
export function fakeWasm(): FakeModule {
  const gpu = new FakeGpu();
  const replicas: FakeReplica[] = [];
  const viewports: FakeViewport[] = [];

  class TrackedReplica extends FakeReplica {
    constructor() {
      super();
      replicas.push(this);
    }
  }

  class TrackedViewport extends FakeViewport {
    constructor(_gpu: WasmGpu, _replica: WasmReplica, _canvas: HTMLCanvasElement) {
      super();
      viewports.push(this);
    }
  }

  const made: FakeModule = {
    gpu,
    replicas,
    viewports,
    failNextAcquire: undefined,
    module: {
      CatchlightEditor: FakeEditor,
      Gpu: {
        acquire: () => {
          const failure = made.failNextAcquire;
          if (failure !== undefined) {
            made.failNextAcquire = undefined;
            return Promise.reject(new Error(failure));
          }
          gpu.acquires += 1;
          return Promise.resolve(gpu);
        },
      },
      Replica: TrackedReplica,
      Viewport: TrackedViewport,
    },
  };
  return made;
}

/**
 * A backend whose every step a test drives by hand.
 *
 * What the in-tab and connected backends cannot show between them: a reply
 * that arrives before its event, and one that arrives after. Both orders are
 * real — an in-tab editor emits inside the same call, a connected one over a
 * socket that may deliver either way round — and `Session` has to resolve the
 * same way for both.
 */
export class ScriptedBackend implements Backend {
  readonly kind = "in-tab";
  sent: Command[] = [];
  staged = new Map<string, Uint8Array>();
  stagedKeys: string[] = [];
  discardedKeys: string[] = [];
  feeds: Array<{ session: number; rev: number }> = [];
  /** The target of each feed that actually ran, coalescing included. */
  runs: number[] = [];
  /** The rev each session is at, as far as the editor is concerned. */
  revs = new Map<number, number>();
  /** Set to make a feed wait; resolve it to let the feed finish. */
  hold: Promise<void> | undefined;
  /** Set to make the next feed fail. */
  failFeed: string | undefined;
  /** What `send` answers, by command. */
  replies = new Map<string, OkReply>();

  #listeners = new Set<(event: Event) => void>();
  #queue = new FeedQueue();

  send(command: Command): Promise<OkReply> {
    this.sent.push(command);
    const scripted = this.replies.get(command.cmd);
    if (scripted) return Promise.resolve(scripted);
    const session = "session" in command ? (command.session as number) : undefined;
    const rev = session === undefined ? undefined : this.revs.get(session);
    return Promise.resolve(rev === undefined ? { body: { result: "empty" } } : { body: { result: "empty" }, rev });
  }

  putBytes(key: string, bytes: Uint8Array): Promise<void> {
    this.staged.set(key, bytes);
    return Promise.resolve();
  }

  stageKey(key: string): Promise<void> {
    this.stagedKeys.push(key);
    return Promise.resolve();
  }

  discardKey(key: string): Promise<void> {
    this.discardedKeys.push(key);
    this.staged.delete(key);
    return Promise.resolve();
  }

  /** Whatever a test staged under `key`; nothing there is "not in this tab". */
  readBytes(key: string): Promise<Uint8Array | undefined> {
    return Promise.resolve(this.staged.get(key));
  }

  feed(replica: WasmReplica, session: number, rev: number): Promise<number> {
    this.feeds.push({ session, rev });
    return this.#queue.run(session, rev, async (target) => {
      this.runs.push(target);
      if (this.hold) await this.hold;
      if (this.failFeed !== undefined) {
        const message = this.failFeed;
        this.failFeed = undefined;
        throw new ProtocolError({ code: "feed", message });
      }
      const doc = emptyDoc(`session ${session}`);
      doc.rev = target;
      replica.applyStructure(structureBytes(doc), target);
      return replica.rev();
    });
  }

  onEvent(listener: (event: Event) => void): Unsubscribe {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  close(): void {
    this.#listeners.clear();
  }

  /** The editor moved a session forward, and said so. */
  changed(session: number, rev: number): void {
    this.revs.set(session, rev);
    this.emit({ event: "document_changed", session, rev });
  }

  emit(event: Event): void {
    for (const listener of [...this.#listeners]) listener(event);
  }
}

/** A WebSocket a test writes both ends of. */
export class FakeSocket implements SocketLike {
  static opened: FakeSocket[] = [];
  url: string;
  sent: string[] = [];
  closed = false;
  onopen: ((event: unknown) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: unknown) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeSocket.opened.push(this);
    // Open on the next microtask, the way a real one never opens inline.
    queueMicrotask(() => this.onopen?.({}));
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
    this.onclose?.({});
  }

  /** The request the client sent at `index`, parsed. */
  request(index: number): Request {
    return JSON.parse(this.sent[index] ?? "{}") as Request;
  }

  /** Delivers a frame from the server. */
  deliver(message: unknown): void {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
}

/** One canned HTTP response. */
export function httpResponse(
  body: Uint8Array | unknown,
  init?: { status?: number; headers?: Record<string, string> },
): HttpResponse {
  const status = init?.status ?? 200;
  const headers = new Map(Object.entries(init?.headers ?? {}).map(([k, v]) => [k.toLowerCase(), v]));
  const bytes =
    body instanceof Uint8Array ? body : new TextEncoder().encode(JSON.stringify(body));
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status === 200 ? "OK" : "no",
    headers: { get: (name: string) => headers.get(name.toLowerCase()) ?? null },
    arrayBuffer: () =>
      Promise.resolve(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer),
    json: () => Promise.resolve(JSON.parse(new TextDecoder().decode(bytes)) as unknown),
  };
}

/** A `fetch` that answers from a table and records every call. */
export function fakeFetch(routes: Record<string, () => HttpResponse>): {
  fetch: (url: string, init?: FetchInit) => Promise<HttpResponse>;
  calls: Array<{ url: string; init: FetchInit | undefined }>;
} {
  const calls: Array<{ url: string; init: FetchInit | undefined }> = [];
  return {
    calls,
    fetch: (url, init) => {
      calls.push({ url, init });
      const route = routes[new URL(url, "http://editor.invalid").pathname];
      if (!route) return Promise.resolve(httpResponse({ error: url }, { status: 404 }));
      return Promise.resolve(route());
    },
  };
}

/**
 * A replica that behaves like a freed wasm object: every call into it after
 * `free` throws the way wasm-bindgen does, and says so.
 *
 * [`FakeReplica`] keeps answering after `free`, which is what a fake should do
 * for the suites that only care about the rules. This one is for the one rule
 * that is about the pointer: a feed still reading a replica the session closed
 * out from under it is a use-after-free nobody sees until it reaches Rust.
 */
export class GuardedReplica extends FakeReplica {
  /** Every call that reached this replica after it was freed. */
  usedAfterFree: string[] = [];

  override rev(): number {
    this.#live("rev");
    return super.rev();
  }

  override texturesNeeded(structure: Uint8Array): string {
    this.#live("texturesNeeded");
    return super.texturesNeeded(structure);
  }

  override putTexture(id: string): void {
    this.#live("putTexture");
    super.putTexture(id);
  }

  override applyStructure(structure: Uint8Array, rev: number): boolean {
    this.#live("applyStructure");
    return super.applyStructure(structure, rev);
  }

  override syncFromEditor(editor: WasmEditor, session: number): number {
    this.#live("syncFromEditor");
    return super.syncFromEditor(editor, session);
  }

  /** wasm-bindgen throws a string, so this does too. */
  #live(call: string): void {
    if (!this.freed) return;
    this.usedAfterFree.push(call);
    throw "null pointer passed to rust";
  }
}
