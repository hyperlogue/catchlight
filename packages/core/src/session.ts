/**
 * One open model: the backend-owned replica React subscribes to. Nothing here
 * mutates the model; only the `model_changed` feed moves the replica, and an
 * edit resolves once the replica reaches the revision its reply named.
 */

import type {
  BindingInfo,
  Command,
  EditCommand,
  Event,
  NodeId,
  NodeInfo,
  MeshInfo,
  ParamId,
  ParamInfo,
  PresenceCommand,
  ReplicaQueryCommand,
  ResponseBody,
  Review,
  ReviewCommand,
  ReviewImage,
  ServerQueryCommand,
  SessionId,
  TexInfo,
  TreeNode,
} from "./protocol.gen.js";
import type { Attachment, Backend, OkReplyWithPayload, Request, Unsubscribe } from "./backend.js";
import {
  asProtocolError,
  expectResult,
  ProtocolError,
  readReply,
} from "./backend.js";
import type { WasmReplica } from "./wasm.js";

/** `Omit` that distributes over a union instead of collapsing it. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown
  ? Omit<T, K>
  : never;

/** Arms of `T` that address a session; the session fills `session` in. */
type OnSession<T> = DistributiveOmit<
  Extract<T, { session: SessionId }>,
  "session"
>;

export type SessionEditCommand = OnSession<EditCommand>;
export type SessionPresenceCommand = OnSession<PresenceCommand>;
export type SessionReviewCommand = OnSession<ReviewCommand>;
export type SessionReplicaQueryCommand = OnSession<ReplicaQueryCommand>;
export type SessionServerQueryCommand = OnSession<ServerQueryCommand>;

export type SessionCommand =
  | SessionEditCommand
  | SessionPresenceCommand
  | SessionReviewCommand
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
 * How often the review is re-read while an agent is listening. A lapsed
 * heartbeat raises no event, so only a re-read can notice the agent left.
 */
export const REVIEW_LISTENING_POLL_MS = 4000;

const NO_REVIEW: Review = { threads: [], agents: [], kept: "stored" };

/** How long a `send` waits for its `model_changed` frame before re-feeding. */
export const CATCH_UP_TIMEOUT_MS = 4000;

interface Waiter {
  rev: number;
  resolve(): void;
  reject(error: ProtocolError): void;
  timer: ReturnType<typeof setTimeout> | undefined;
}

export class Session {
  readonly id: SessionId;
  /** Treat as gone once [`close`] has run. */
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
  #review: Review = NO_REVIEW;
  #reviewListeners = new Set<() => void>();
  #reviewRead = 0;
  #reviewTimer: ReturnType<typeof setTimeout> | undefined;
  #reviewImages = new Map<number, Promise<string>>();
  #closed = false;
  #freed = false;
  #timeout: number;

  constructor(
    backend: Backend,
    id: SessionId,
    replica: WasmReplica,
    timeout?: number,
  ) {
    this.#backend = backend;
    this.id = id;
    this.replica = replica;
    this.#revision = replica.rev();
    this.#timeout = timeout ?? CATCH_UP_TIMEOUT_MS;
    this.#offEvents = backend.onEvent((event) => this.#observe(event));
  }

  /** Resolves once the replica is at the revision the reply named. */
  async send(command: SessionEditCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    if (reply.rev !== undefined) await this.#reached(reply.rev);
    this.#advance();
    return reply.body;
  }

  async sendWith(
    command: SessionEditCommand,
    attachments: readonly Attachment[],
  ): Promise<ResponseBody> {
    const reply = await this.#backend.sendWith(
      this.#address(command),
      attachments,
    );
    if (reply.rev !== undefined) await this.#reached(reply.rev);
    this.#advance();
    return reply.body;
  }

  /** Moves no revision, repaints nothing, and adds no undo entry. */
  async sendPresence(command: SessionPresenceCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    return reply.body;
  }

  /** Moves no revision; the review follows from its `review_changed` event. */
  async sendReview(
    // An image carries bytes, so it goes up through `addReviewImage` first.
    command: Exclude<SessionReviewCommand, { cmd: "review_image_add" }>,
  ): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    // Not every transport pushes events back to the sender before the reply.
    await this.refreshReview();
    return reply.body;
  }

  /** Holds an image for the next comment to claim by its id. */
  async addReviewImage(bytes: Uint8Array): Promise<ReviewImage> {
    const reply = await this.#backend.sendWith(
      this.#address({ cmd: "review_image_add" }),
      [["image", bytes]],
    );
    return expectResult(reply.body, "review_image").image;
  }

  /** An object URL for the image, fetched once and revoked on [`close`]. */
  reviewImageUrl(image: ReviewImage): Promise<string> {
    let url = this.#reviewImages.get(image.image);
    if (!url) {
      url = this.#backend
        .sendWith(this.#address({ cmd: "review_image_get", image: image.image }), [])
        .then((reply) => {
          if (!reply.payload)
            throw new ProtocolError({
              code: "bad_reply",
              message: `review image ${image.image} came back without bytes`,
            });
          const blob = new Blob([reply.payload as BlobPart], { type: image.media_type });
          const made = URL.createObjectURL(blob);
          if (this.#closed) URL.revokeObjectURL(made);
          return made;
        });
      this.#reviewImages.set(image.image, url);
      // A failed read may be retried.
      url.catch(() => this.#reviewImages.delete(image.image));
    }
    return url;
  }

  /** Empty until the first subscriber's read lands. */
  getReview = (): Review => this.#review;

  subscribeReview = (listener: () => void): Unsubscribe => {
    const first = this.#reviewListeners.size === 0;
    this.#reviewListeners.add(listener);
    if (first) void this.refreshReview().catch(() => undefined);
    return () => {
      this.#reviewListeners.delete(listener);
      if (this.#reviewListeners.size === 0) this.#stopReviewTimer();
    };
  };

  async refreshReview(): Promise<void> {
    if (this.#closed) return;
    const read = ++this.#reviewRead;
    const body = await this.queryServer({ cmd: "review_get" });
    // A slower, older read must not overwrite a newer one.
    if (this.#closed || read !== this.#reviewRead) return;
    const next = expectResult(body, "review").review;
    this.#stopReviewTimer();
    if (next.agents.some((agent) => agent.listening) && this.#reviewListeners.size > 0) {
      this.#reviewTimer = setTimeout(() => {
        this.#reviewTimer = undefined;
        void this.refreshReview().catch(() => undefined);
      }, REVIEW_LISTENING_POLL_MS);
    }
    if (JSON.stringify(next) === JSON.stringify(this.#review)) return;
    this.#review = next;
    for (const listener of [...this.#reviewListeners]) listener();
  }

  #stopReviewTimer(): void {
    if (this.#reviewTimer === undefined) return;
    clearTimeout(this.#reviewTimer);
    this.#reviewTimer = undefined;
  }

  query(command: SessionReplicaQueryCommand): ResponseBody {
    const request = {
      id: this.#nextQueryId++,
      ...this.#address(command),
    } as Request;
    let text: string;
    try {
      text = this.replica.query(JSON.stringify(request));
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }
    return readReply(text, command.cmd).body;
  }

  async queryServer(command: SessionServerQueryCommand): Promise<ResponseBody> {
    const reply = await this.#backend.send(this.#address(command));
    return reply.body;
  }

  queryServerWith(command: SessionServerQueryCommand, attachments: readonly Attachment[] = []): Promise<OkReplyWithPayload> {
    return this.#backend.sendWith(this.#address(command), attachments);
  }

  params(): ParamInfo[] {
    return expectResult(this.query({ cmd: "param_list" }), "params").params;
  }

  tree(): TreeNode {
    return expectResult(this.query({ cmd: "node_tree_get" }), "tree").root;
  }

  /** `undefined` when the node is gone: a selection can outlive its node. */
  nodeInfo(node: NodeId): NodeInfo | undefined {
    let body: ResponseBody;
    try {
      body = this.query({ cmd: "node_get", node });
    } catch (cause) {
      if (cause instanceof ProtocolError && cause.code === "no_node")
        return undefined;
      throw cause;
    }
    return expectResult(body, "node_info").node;
  }

  /**
   * `[]` when the node is gone. A binding's grid is indexed `[y][x]`, the
   * transpose of the `cell: [x, y]` every binding command takes.
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

  mesh(node: NodeId): MeshInfo {
    return expectResult(this.query({ cmd: "mesh_get", node }), "mesh_info").mesh;
  }

  textures(): TexInfo[] {
    return expectResult(this.query({ cmd: "texture_list" }), "textures")
      .textures;
  }

  /** Existing parameter pairs, keeping each pair's original axis order. */
  recordingPairs(node: NodeId, param: ParamId): [string, string][] {
    return JSON.parse(this.replica.recordingPairs(node, param)) as [
      string,
      string,
    ][];
  }

  setEditing(editing: boolean): void {
    if (this.#closed) return;
    this.replica.setEditing(editing);
    this.invalidate();
  }

  /** Full-resolution straight-alpha artwork. */
  textureImage(
    texture: string,
  ): { width: number; height: number; rgba: Uint8Array } | undefined {
    if (this.#closed) return undefined;
    const image = this.replica.textureImage(texture);
    if (!image) return undefined;
    try {
      return { width: image.width, height: image.height, rgba: image.pixels() };
    } finally {
      image.free();
    }
  }

  /** Repaints; authors nothing. */
  setParam(param: ParamId, value: number): boolean {
    const moved = this.replica.setParam(param, value);
    this.invalidate();
    return moved;
  }

  paramValue(param: ParamId): number | undefined {
    return this.replica.paramValue(param);
  }

  /** Offsets are packed `[dx0, dy0, dx1, dy1, ...]`. */
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

  /** Absolute `node_set` values; an absent field keeps what the fold produced. */
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

  /** World transform after the last tick: 16 floats, column-major. */
  nodeWorldTransform(node: NodeId): Float32Array | undefined {
    return this.replica.nodeWorldTransform(node);
  }

  /** `(x, y)` is Y-up world space. Texture transparency and clipping are ignored. */
  pickNode(x: number, y: number): NodeId | undefined {
    if (this.#closed) return undefined;
    return this.replica.pickNode(x, y);
  }

  /** Y-up world positions, packed x/y. A group concatenates its enabled parts. */
  nodeVertices(node: NodeId): Float32Array | undefined {
    if (this.#closed) return undefined;
    return this.replica.nodeVertices(node);
  }

  get closed(): boolean {
    return this.#closed;
  }

  /** Evaluated local translate xyz, rotate xyz, scale xy. */
  nodeLocalTransform(node: NodeId): Float32Array | undefined {
    return this.#closed ? undefined : this.replica.nodeLocalTransform(node);
  }

  /** `undefined` for a missing node or a transform with no inverse. */
  nodeDeltaFromWorld(
    node: NodeId,
    x: number,
    y: number,
  ): Float32Array | undefined {
    return this.#closed
      ? undefined
      : this.replica.nodeDeltaFromWorld(node, x, y);
  }

  /** Straight-alpha pixels; the caller owns these bytes, never a wasm object. */
  textureThumbnail(
    texture: string,
  ): { width: number; height: number; rgba: Uint8Array } | undefined {
    if (this.#closed) return undefined;
    const thumbnail = this.replica.textureThumbnail(texture);
    if (!thumbnail) return undefined;
    try {
      return {
        width: thumbnail.width,
        height: thumbnail.height,
        rgba: thumbnail.pixels(),
      };
    } finally {
      thumbnail.free();
    }
  }

  /** The authored translation after a world move of `(dx, dy)`, in the parent frame. */
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
   * Posed Y-up world box `[min_x, min_y, max_x, max_y]` from the last tick;
   * `undefined` until a viewport has drawn a frame or when nothing draws.
   */
  bounds(): [number, number, number, number] | undefined {
    const box = this.replica.bounds();
    if (!box) return undefined;
    const [minX, minY, maxX, maxY] = box;
    if (minX === undefined || minY === undefined) return undefined;
    if (maxX === undefined || maxY === undefined) return undefined;
    return [minX, minY, maxX, maxY];
  }

  clearAllScratch(): void {
    this.replica.clearAllScratch();
    this.invalidate();
  }

  invalidate(): void {
    for (const listener of [...this.#redraw]) listener();
  }

  getRevision = (): number => this.#revision;

  subscribe = (listener: () => void): Unsubscribe => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  onInvalidate = (listener: () => void): Unsubscribe => {
    this.#redraw.add(listener);
    return () => {
      this.#redraw.delete(listener);
    };
  };

  /** Feed failures, including ones no command was waiting on. */
  onError = (listener: (error: ProtocolError) => void): Unsubscribe => {
    this.#errors.add(listener);
    return () => {
      this.#errors.delete(listener);
    };
  };

  /** The only path into the replica; the backend serializes and coalesces feeds. */
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
      // A close during the feed deferred freeing to here.
      if (this.#closed) this.#free();
    }
    this.#failure = undefined;
    if (this.#closed) return;
    this.#advance();
    this.#release();
  }

  /**
   * The model stays open on the backend. A feed in flight holds a pointer into
   * the replica, so it frees the replica on exit instead (wasm use-after-free).
   */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#offEvents();
    this.#failWaiters(
      new ProtocolError({
        code: "closed",
        message: `session ${this.id} is closed`,
      }),
    );
    this.#listeners.clear();
    this.#redraw.clear();
    this.#errors.clear();
    this.#reviewListeners.clear();
    this.#stopReviewTimer();
    for (const url of this.#reviewImages.values())
      void url.then((made) => URL.revokeObjectURL(made)).catch(() => undefined);
    this.#reviewImages.clear();
    this.#free();
  }

  #free(): void {
    if (this.#freed || this.#feeding > 0) return;
    this.#freed = true;
    this.replica.free();
  }

  #address(command: SessionCommand): Command {
    // TypeScript widens a spread over a union; `OnSession` makes this sound.
    return { ...command, session: this.id } as Command;
  }

  #observe(event: Event): void {
    if (event.event === "review_changed") {
      if (event.session === this.id && this.#reviewListeners.size > 0)
        void this.refreshReview().catch((cause: unknown) => {
          if (!this.#closed) this.#report(asProtocolError(cause));
        });
      return;
    }
    if (event.event !== "model_changed" || event.session !== this.id) return;
    // An unawaited feed failure still rejects waiters, unless closed.
    void this.catchUp(event.rev).catch((cause: unknown) => {
      if (this.#closed) return;
      const error = asProtocolError(cause);
      this.#failWaiters(error);
      this.#report(error);
    });
  }

  /**
   * Rejects rather than waiting when no feed is in flight and the last one
   * failed; the timer bounds a wait for a lost frame.
   */
  #reached(rev: number): Promise<void> {
    if (this.replica.rev() >= rev) return Promise.resolve();
    if (this.#closed) {
      return Promise.reject(
        new ProtocolError({
          code: "closed",
          message: `session ${this.id} is closed`,
        }),
      );
    }
    if (this.#feeding === 0 && this.#failure)
      return Promise.reject(this.#failure);
    return new Promise<void>((resolve, reject) => {
      const waiter: Waiter = { rev, resolve, reject, timer: undefined };
      waiter.timer = setTimeout(() => void this.#refeed(waiter), this.#timeout);
      this.#waiters.push(waiter);
    });
  }

  /** One attempt: a replica still short after a whole feed won't recover by retrying. */
  async #refeed(waiter: Waiter): Promise<void> {
    waiter.timer = undefined;
    if (!this.#waiters.includes(waiter)) return;
    try {
      await this.catchUp(waiter.rev);
    } catch (cause) {
      if (this.#drop(waiter)) waiter.reject(asProtocolError(cause));
      return;
    }
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

  #drop(waiter: Waiter): boolean {
    const at = this.#waiters.indexOf(waiter);
    if (at < 0) return false;
    this.#waiters.splice(at, 1);
    if (waiter.timer !== undefined) clearTimeout(waiter.timer);
    return true;
  }

  #advance(): void {
    const rev = this.replica.rev();
    const moved = rev !== this.#revision;
    this.#revision = rev;
    this.invalidate();
    if (!moved) return;
    // Copy: a listener may unsubscribe during notification.
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
      // A replica that silently stopped following is invisible to the user.
      console.error(`session ${this.id}: ${error.message}`);
      return;
    }
    for (const listener of [...this.#errors]) listener(error);
  }
}

const NO_VEC3: [number, number, number] = [NaN, NaN, NaN];
const NO_VEC2: [number, number] = [NaN, NaN];
