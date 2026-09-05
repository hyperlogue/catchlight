/**
 * One open model: the replica it is drawn from, and the only thing React
 * subscribes to.
 *
 * **The replica is a copy the backend owns.** Nothing here mutates the model.
 * A command goes to the editor, the editor emits `model_changed`, and the
 * feed that event starts is the only thing that moves the replica forward.
 * Applying an edit locally as well would be a second source of truth that can
 * disagree with the picture on the canvas, and every new command would need a
 * patch handler written twice.
 *
 * **An edit resolves once the replica can answer for it.** The
 * reply carries the revision the session reached; [`send`] waits until the
 * replica is at least there before it resolves. That is what makes the Id a
 * `node_add` minted readable the moment the promise settles — a caller never
 * has to poll, and never reads a tree that predates its own edit. The wait
 * ends when the feed completes, never by feeding from `send`: two paths into
 * the replica is how two versions of one revision get applied.
 *
 * **That wait is bounded.** A `model_changed` frame can be lost on a socket
 * that stays up, and nothing else would ever bring the replica to the revision
 * a caller is waiting for. So a wait that reaches [`CATCH_UP_TIMEOUT_MS`]
 * re-feeds — through [`catchUp`], the same path the missing event would have
 * taken, so the backend's queue still serializes it and there is still one way
 * into the replica. The send then resolves when the replica lands, and rejects
 * when that feed cannot bring it there. In-tab the timer never fires: that
 * editor emits its events inside the `send` it is answering, so the replica is
 * already at the revision when `send` looks at it.
 *
 * **One method per kind of command, and the type picks it.** The split is
 * generated from Rust (`CommandKind` in `catchlight-editor-protocol`), so
 * passing `scratch_deform` to [`send`] does not typecheck. An edit
 * moves the revision; a presence command publishes view state to other
 * clients and moves nothing; a replica query is answered here, synchronously,
 * with no round trip at all; a server query is the only read that has to go
 * over the wire.
 *
 * **Scratch has no `send`.** A drag is a typed call on the replica — pose,
 * scratch deform, scratch transform — because those run per pointer move and
 * a JSON round trip per move is exactly the cost this split exists to avoid.
 * The editor never learns about them, so nothing is undoable and nothing is
 * saved until an edit authors it.
 *
 * **Two channels, because they run at different rates.** [`subscribe`] is the
 * model's revision, and React reads it once per commit. [`onInvalidate`] is
 * "the picture changed", and a viewport reads it, possibly on every pointer
 * move.
 */

import type {
  BindingInfo,
  Command,
  EditCommand,
  Event,
  NodeId,
  NodeInfo,
  ParamId,
  ParamInfo,
  PresenceCommand,
  ReplicaQueryCommand,
  ResponseBody,
  ServerQueryCommand,
  SessionId,
  TexInfo,
  TreeNode,
} from "./protocol.gen.js";
import type { Attachment, Backend, Request, Unsubscribe } from "./backend.js";
import { asProtocolError, expectResult, ProtocolError, readReply } from "./backend.js";
import type { WasmReplica } from "./wasm.js";

/** `Omit` that distributes over a union instead of collapsing it. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/**
 * The arms of `T` that address a session, with the `session` field removed.
 *
 * Filling it in is the session's own job, so a caller cannot address the wrong
 * model by accident — and a command that names no session (`session_new`,
 * `session_list`) drops out of the union entirely, because it belongs on the
 * `Editor` rather than here.
 */
type OnSession<T> = DistributiveOmit<Extract<T, { session: SessionId }>, "session">;

/** A command that changes this session's model. Goes to [`Session#send`]. */
export type SessionEditCommand = OnSession<EditCommand>;
/** A command that publishes shared view state. Goes to [`Session#sendPresence`]. */
export type SessionPresenceCommand = OnSession<PresenceCommand>;
/** A read the replica answers. Goes to [`Session#query`], synchronously. */
export type SessionReplicaQueryCommand = OnSession<ReplicaQueryCommand>;
/** A read only the editor can answer. Goes to [`Session#queryServer`]. */
export type SessionServerQueryCommand = OnSession<ServerQueryCommand>;

/** Any command aimed at a session, whatever it does. */
export type SessionCommand =
  | SessionEditCommand
  | SessionPresenceCommand
  | SessionReplicaQueryCommand
  | SessionServerQueryCommand;

/** What a scratch transform sets. An absent field keeps what the fold produced. */
export interface ScratchTransform {
  translate?: [number, number, number];
  rotate?: [number, number, number];
  scale?: [number, number];
  zOrder?: number;
  opacity?: number;
}

/**
 * How long a `send` waits for the frame that moves the replica before it
 * stops trusting the socket and re-feeds.
 *
 * Long enough that a slow structure fetch settles it rather than the timer,
 * short enough that a lost frame does not read as a hung editor.
 */
export const CATCH_UP_TIMEOUT_MS = 4000;

/** A `send` waiting for the replica to reach the revision its reply named. */
interface Waiter {
  rev: number;
  resolve(): void;
  reject(error: ProtocolError): void;
  /** Armed until the waiter settles: what turns a lost frame into a re-feed. */
  timer: ReturnType<typeof setTimeout> | undefined;
}

/** A model the editor has open, and this tab's replica of it. */
export class Session {
  readonly id: SessionId;
  /**
   * The model, puppet and render cache this session draws from. Public
   * because a viewport is built on it; read through the methods below rather
   * than reaching for it, and treat it as gone once [`close`] has run.
   */
  readonly replica: WasmReplica;

  #backend: Backend;
  #revision: number;
  #listeners = new Set<() => void>();
  #redraw = new Set<() => void>();
  #errors = new Set<(error: ProtocolError) => void>();
  #waiters: Waiter[] = [];
  #feeding = 0;
  #failure: ProtocolError | undefined;
  #offEvents: Unsubscribe;
  #nextQueryId = 1;
  #closed = false;
  #freed = false;
  #timeout: number;

  /**
   * `timeout` is how long a [`send`] waits for the frame that should move the
   * replica before it re-feeds; it defaults to [`CATCH_UP_TIMEOUT_MS`] and is
   * a parameter so a test does not have to wait out a real one.
   */
  constructor(backend: Backend, id: SessionId, replica: WasmReplica, timeout?: number) {
    this.#backend = backend;
    this.id = id;
    this.replica = replica;
    this.#revision = replica.rev();
    this.#timeout = timeout ?? CATCH_UP_TIMEOUT_MS;
    this.#offEvents = backend.onEvent((event) => this.#observe(event));
  }

  /**
   * Runs a command that changes the model, resolving once the replica is at
   * the revision the reply named — so the body it hands back describes a model
   * a caller can immediately read.
   */
  async send(command: SessionEditCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    if (reply.rev !== undefined) await this.#reached(reply.rev);
    this.#advance();
    return reply.body;
  }

  /**
   * [`send`] for an edit that carries bytes — an image, a `.clm`,
   * a manifest and its textures.
   *
   * The same contract: the attachments go beside the command, and the promise
   * resolves once the replica is at the revision the reply named.
   */
  async sendWith(
    command: SessionEditCommand,
    attachments: readonly Attachment[],
  ): Promise<ResponseBody> {
    const reply = await this.#backend.sendWith(this.#address(command), attachments);
    if (reply.rev !== undefined) await this.#reached(reply.rev);
    this.#advance();
    return reply.body;
  }

  /**
   * Publishes shared view state: pose, camera, selection.
   *
   * Nothing local changes — this tab's own picture is moved by [`setParam`]
   * and the scratch calls — so no revision, no repaint, and no undo entry. It
   * is what other clients read to follow along.
   */
  async sendPresence(command: SessionPresenceCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    return reply.body;
  }

  /**
   * Answers a read from the replica, synchronously.
   *
   * No promise, because there is nothing to wait for: the model is in this
   * tab's memory and the answer is a pure function of it. That is what lets a
   * React render call this directly instead of holding a mirrored store.
   */
  query(command: SessionReplicaQueryCommand): ResponseBody {
    const request = { id: this.#nextQueryId++, ...this.#address(command) } as Request;
    let text: string;
    try {
      text = this.replica.query(JSON.stringify(request));
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }
    return readReply(text, command.cmd).body;
  }

  /** Runs a read only the editor can answer: its sessions, its store, a preview. */
  async queryServer(command: SessionServerQueryCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    return reply.body;
  }

  /** Every param, as the panels list them. */
  params(): ParamInfo[] {
    return expectResult(this.query({ cmd: "param_list" }), "params").params;
  }

  /** The node tree, from the root down. */
  tree(): TreeNode {
    return expectResult(this.query({ cmd: "node_tree" }), "tree").root;
  }

  /**
   * One node in full: what a `node_set` can change, under the field names it
   * takes, plus the node's kind and parent.
   *
   * `undefined` when the model carries no such node, because a selection can
   * outlive the node it names — a deleted node is a panel that renders
   * nothing, not a failed read.
   */
  nodeInfo(node: NodeId): NodeInfo | undefined {
    let body: ResponseBody;
    try {
      body = this.query({ cmd: "node_info", node });
    } catch (cause) {
      if (cause instanceof ProtocolError && cause.code === "no_node") return undefined;
      throw cause;
    }
    return expectResult(body, "node_info").node;
  }

  /**
   * Every binding on one node: the params driving it, the property it drives,
   * and the grid of authored keypoints.
   *
   * `[]` when the model carries no such node, for the same reason
   * [`nodeInfo`] answers `undefined` — a selection outlives the node it names,
   * and a panel drawing nothing is not a failed read.
   *
   * A binding's grid is indexed `[y][x]`, the transpose of the `cell: [x, y]`
   * every binding command takes.
   */
  bindings(node: NodeId): BindingInfo[] {
    let body: ResponseBody;
    try {
      body = this.query({ cmd: "binding_list", node });
    } catch (cause) {
      if (cause instanceof ProtocolError && cause.code === "no_node") return [];
      throw cause;
    }
    return expectResult(body, "bindings").bindings;
  }

  /** Every texture the model carries, with its dimensions. */
  textures(): TexInfo[] {
    return expectResult(this.query({ cmd: "texture_list" }), "textures").textures;
  }

  /** Poses one param. Repaints; authors nothing. */
  setParam(param: ParamId, value: number): boolean {
    const moved = this.replica.setParam(param, value);
    this.invalidate();
    return moved;
  }

  /** What a param is posed at right now, or `undefined` if there is no such param. */
  paramValue(param: ParamId): number | undefined {
    return this.replica.paramValue(param);
  }

  /** Shows per-vertex offsets on a node, as pairs `[dx0, dy0, dx1, dy1, ...]`. */
  scratchDeform(node: NodeId, offsets: Float32Array): boolean {
    const shown = this.replica.scratchDeform(node, offsets);
    this.invalidate();
    return shown;
  }

  clearScratchDeform(node: NodeId): boolean {
    const cleared = this.replica.clearScratchDeform(node);
    this.invalidate();
    return cleared;
  }

  /**
   * Shows a transform on a node without authoring it — the absolute values a
   * `node_set` would commit. An absent field leaves what the fold produced.
   */
  scratchTransform(node: NodeId, patch: ScratchTransform): boolean {
    const [tx, ty, tz] = patch.translate ?? NO_VEC3;
    const [rx, ry, rz] = patch.rotate ?? NO_VEC3;
    const [sx, sy] = patch.scale ?? NO_VEC2;
    const shown = this.replica.scratchTransform(
      node,
      tx,
      ty,
      tz,
      rx,
      ry,
      rz,
      sx,
      sy,
      patch.zOrder ?? NaN,
      patch.opacity ?? NaN,
    );
    this.invalidate();
    return shown;
  }

  clearScratchTransform(node: NodeId): boolean {
    const cleared = this.replica.clearScratchTransform(node);
    this.invalidate();
    return cleared;
  }

  /**
   * The node's evaluated world transform after the last tick: 16 floats,
   * column-major, or `undefined` when there is no such node.
   *
   * Handed back as the typed array Rust wrote, because a caller reads two of
   * the sixteen — the translation columns — and copying the rest into a tuple
   * per call buys nothing.
   */
  nodeWorldTransform(node: NodeId): Float32Array | undefined {
    return this.replica.nodeWorldTransform(node);
  }

  /**
   * What `[x, y, z]` this node's authored translation becomes if the world
   * moves by `(dx, dy)` — the delta resolved into the node's parent frame.
   *
   * The whole of a translate drag on this side is: this, then a
   * [`scratchTransform`] to show it, then one `node_set` to author it. The
   * arithmetic that turns a screen drag into a local translation is Rust's,
   * because a native editor needs the same answer.
   */
  translationAfterWorldDelta(
    node: NodeId,
    dx: number,
    dy: number,
  ): [number, number, number] | undefined {
    const moved = this.replica.translationAfterWorldDelta(node, dx, dy);
    if (!moved) return undefined;
    const [x, y, z] = moved;
    if (x === undefined || y === undefined || z === undefined) return undefined;
    return [x, y, z];
  }

  /**
   * The world-space box the last tick left the drawn geometry in:
   * `[min_x, min_y, max_x, max_y]`, Y-up. `undefined` when the model draws
   * nothing.
   *
   * A read like [`tree`], answered off the replica with no round trip, so a
   * "frame the model" button is one synchronous call. The box is **posed** —
   * it covers where the last tick left every drawn vertex, not the rest pose —
   * which also means it is `undefined` until a viewport has drawn a frame.
   */
  bounds(): [number, number, number, number] | undefined {
    const box = this.replica.bounds();
    if (!box) return undefined;
    const [minX, minY, maxX, maxY] = box;
    if (minX === undefined || minY === undefined) return undefined;
    if (maxX === undefined || maxY === undefined) return undefined;
    return [minX, minY, maxX, maxY];
  }

  /** Drops every scratch edit at once. What cancelling a gesture runs. */
  clearAllScratch(): void {
    this.replica.clearAllScratch();
    this.invalidate();
  }

  /** Tells every viewport on this session that the picture changed. */
  invalidate(): void {
    for (const listener of [...this.#redraw]) listener();
  }

  /**
   * The value `useSyncExternalStore` compares between renders: the revision
   * the replica holds. A number, so comparison is identity and no snapshot has
   * to be memoized to stay stable.
   */
  getRevision = (): number => this.#revision;

  /** Registers `listener`, called after the revision moves. */
  subscribe = (listener: () => void): Unsubscribe => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** Registers `listener`, called whenever the picture may have changed. */
  onInvalidate = (listener: () => void): Unsubscribe => {
    this.#redraw.add(listener);
    return () => {
      this.#redraw.delete(listener);
    };
  };

  /**
   * Registers `listener` for a failure nobody asked for: a feed that could not
   * be applied. The commands waiting on it reject; this is how the ones that
   * were not waiting find out.
   */
  onError = (listener: (error: ProtocolError) => void): Unsubscribe => {
    this.#errors.add(listener);
    return () => {
      this.#errors.delete(listener);
    };
  };

  /**
   * Brings the replica to `rev` and tells everyone what it now holds.
   *
   * The one path into the replica: the event handler runs it, and the editor
   * runs it once before handing a new session out, so a `Session` is never
   * observed holding an empty model. Feeds are serialized and coalesced by
   * the backend, so calling it twice for one session is one feed.
   */
  async catchUp(rev: number): Promise<void> {
    if (this.#closed) return;
    this.#feeding += 1;
    try {
      await this.#backend.feed(this.replica, this.id, rev);
    } catch (cause) {
      this.#failure = asProtocolError(cause);
      throw this.#failure;
    } finally {
      this.#feeding -= 1;
      // A close during the feed left the freeing to whoever was still reading
      // the replica, which was this.
      if (this.#closed) this.#free();
    }
    this.#failure = undefined;
    if (this.#closed) return;
    this.#advance();
    this.#release();
  }

  /**
   * Frees the replica and stops following the model.
   *
   * The model itself stays open on the backend — `session_close` is a
   * command, and this is not it. Anything still waiting on a revision rejects
   * rather than hanging on a replica that is gone.
   *
   * Synchronous, and the replica can outlive it by one feed: a feed that is
   * mid-fetch holds a pointer into it, and freeing under that hand is the
   * use-after-free wasm reports as a null pointer passed to Rust. Such a feed
   * finishes writing into a replica nobody will read and frees it on the way
   * out. No event can start another one — this unsubscribes first.
   */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#offEvents();
    this.#failWaiters(
      new ProtocolError({ code: "closed", message: `session ${this.id} is closed` }),
    );
    this.#listeners.clear();
    this.#redraw.clear();
    this.#errors.clear();
    this.#free();
  }

  /** Frees the replica once nothing is reading it. Runs at most once. */
  #free(): void {
    if (this.#freed || this.#feeding > 0) return;
    this.#freed = true;
    this.replica.free();
  }

  #address(command: SessionCommand): Command {
    // Putting `session` back on an arm that `OnSession` took it off of
    // reconstitutes that arm, but TypeScript cannot see a spread that way: it
    // widens the union rather than tracking which arm it came from. The types
    // above are what make this sound.
    return { ...command, session: this.id } as Command;
  }

  #observe(event: Event): void {
    if (event.event !== "model_changed" || event.session !== this.id) return;
    // A feed nobody awaited still has to fail loudly: the commands waiting on
    // this revision reject, and anyone watching is told. Unless the session was
    // closed while it ran — then nobody asked for the model any more, and
    // the failure is the close.
    void this.catchUp(event.rev).catch((cause: unknown) => {
      if (this.#closed) return;
      const error = asProtocolError(cause);
      this.#failWaiters(error);
      this.#report(error);
    });
  }

  /**
   * Resolves once the replica holds `rev`, or rejects if it never will.
   *
   * The event that carries a revision can beat its own reply, and the feed it
   * started can have failed before this was ever called — so a replica that is
   * behind with nothing in flight and a failure behind it is not waited for.
   * Waiting there is a promise that never settles, which is the one outcome a
   * command must not have. Nor is a lost frame: the timer this arms is what
   * bounds the one wait that is otherwise open-ended.
   */
  #reached(rev: number): Promise<void> {
    if (this.replica.rev() >= rev) return Promise.resolve();
    if (this.#closed) {
      return Promise.reject(
        new ProtocolError({ code: "closed", message: `session ${this.id} is closed` }),
      );
    }
    if (this.#feeding === 0 && this.#failure) return Promise.reject(this.#failure);
    return new Promise<void>((resolve, reject) => {
      const waiter: Waiter = { rev, resolve, reject, timer: undefined };
      waiter.timer = setTimeout(() => void this.#refeed(waiter), this.#timeout);
      this.#waiters.push(waiter);
    });
  }

  /**
   * Fetches the model once more for a waiter whose frame never came.
   *
   * A feed rather than anything cleverer because a feed is the only way into
   * the replica; the backend's queue is what keeps it from overlapping one
   * already running, at the cost of one extra structure fetch when the frame
   * was merely slow. One attempt: a replica still short of the revision after
   * a whole feed is an editor answering for state older than the reply it
   * sent, which no amount of asking again fixes.
   */
  async #refeed(waiter: Waiter): Promise<void> {
    waiter.timer = undefined;
    if (!this.#waiters.includes(waiter)) return;
    try {
      await this.catchUp(waiter.rev);
    } catch (cause) {
      if (this.#drop(waiter)) waiter.reject(asProtocolError(cause));
      return;
    }
    // Gone from the list means the feed released it, or a close rejected it.
    if (!this.#drop(waiter)) return;
    if (this.replica.rev() >= waiter.rev) {
      waiter.resolve();
      return;
    }
    waiter.reject(
      new ProtocolError({
        code: "feed",
        message: `session ${this.id} never reached revision ${waiter.rev}`,
      }),
    );
  }

  /** Takes `waiter` off the list, answering whether it was still on it. */
  #drop(waiter: Waiter): boolean {
    const at = this.#waiters.indexOf(waiter);
    if (at < 0) return false;
    this.#waiters.splice(at, 1);
    if (waiter.timer !== undefined) clearTimeout(waiter.timer);
    return true;
  }

  /** Takes the revision from the replica and tells whoever is watching. */
  #advance(): void {
    const rev = this.replica.rev();
    const moved = rev !== this.#revision;
    this.#revision = rev;
    this.invalidate();
    if (!moved) return;
    // Copy first: a listener that unsubscribes during notification must not
    // shift the set out from under this loop.
    for (const listener of [...this.#listeners]) listener();
  }

  #release(): void {
    const rev = this.replica.rev();
    const waiting = this.#waiters;
    this.#waiters = waiting.filter((waiter) => waiter.rev > rev);
    for (const waiter of waiting) {
      if (waiter.rev > rev) continue;
      if (waiter.timer !== undefined) clearTimeout(waiter.timer);
      waiter.resolve();
    }
  }

  #failWaiters(error: ProtocolError): void {
    const waiting = this.#waiters;
    this.#waiters = [];
    for (const waiter of waiting) {
      if (waiter.timer !== undefined) clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  }

  #report(error: ProtocolError): void {
    if (this.#errors.size === 0) {
      // Nobody is listening, and a replica that silently stopped following the
      // model is the one failure a user cannot see for themselves.
      console.error(`session ${this.id}: ${error.message}`);
      return;
    }
    for (const listener of [...this.#errors]) listener(error);
  }
}

const NO_VEC3: [number, number, number] = [NaN, NaN, NaN];
const NO_VEC2: [number, number] = [NaN, NaN];
