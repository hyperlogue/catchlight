/**
 * The public surface of `@catchlight/core`. Only web-platform glue belongs in
 * this package; anything a native editor would also need belongs in Rust.
 */

import type { Command, ResponseBody, SessionId, SessionInfo } from "./protocol.gen.js";
import type { Attachment, Backend, OkReply, OkReplyWithPayload, Unsubscribe } from "./backend.js";
import { asProtocolError, expectResult, ProtocolError } from "./backend.js";
import { Session } from "./session.js";
import { joinKey, parentKey } from "./storage.js";
import type { WasmGpu, WasmModule } from "./wasm.js";
import { Viewport } from "./viewport.js";

export type ExportedModel = Omit<Extract<ResponseBody, { result: "model_export" }>, "result"> & {
  readonly bytes: Uint8Array;
  readonly rev: number;
};

export class Editor {
  #wasm: WasmModule;
  #backend: Backend;
  #gpu: WasmGpu | undefined;
  #acquiring: Promise<WasmGpu> | undefined;
  #sessions = new Map<SessionId, Session>();
  #adopting = new Map<SessionId, Promise<Session>>();
  #gpuListeners = new Set<() => void>();
  #closed = false;

  private constructor(wasm: WasmModule, backend: Backend) {
    this.#wasm = wasm;
    this.#backend = backend;
  }

  /**
   * Touches no GPU until the first [`attach`]. The wasm module is injected so
   * this package never forces WebAssembly into a types-only bundle.
   */
  static create(wasm: WasmModule, backend: Backend): Promise<Editor> {
    return Promise.resolve(new Editor(wasm, backend));
  }

  /** For display only; behaviour must not branch on it. */
  backendKind(): Backend["kind"] {
    return this.#backend.kind;
  }

  /** `undefined` until the first [`attach`]. For display only. */
  gpuTier(): string | undefined {
    return this.#gpu?.tier();
  }

  /** Fires once: a device is acquired once per editor and never swapped. */
  onGpuChanged(listener: () => void): Unsubscribe {
    this.#gpuListeners.add(listener);
    return () => {
      this.#gpuListeners.delete(listener);
    };
  }

  async newSession(name?: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_create", name: name ?? null }));
  }

  /**
   * A read-only session showing one of `source`'s saved versions; the editor
   * answers the same session for the same version, and closes it with
   * `source`. Every edit to it is refused with `read_only`.
   */
  async openVersion(source: Session, version: number): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "version_open", session: source.id, version }));
  }

  /** Opens a file in the *editor's* store; page-held bytes go to [`openFile`]. */
  async openSession(key: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_open", path: key }));
  }

  async forkSession(source: Session, name?: string): Promise<Session> {
    const reply = await this.#backend.send({
      cmd: "session_fork", session: source.id, if_rev: source.getRevision(), name: name ?? null,
    });
    const body = expectResult(reply.body, "session_fork");
    return this.#adoptId(body.session, reply.rev ?? 0);
  }

  async exportSession(session: Session): Promise<ExportedModel> {
    const rev = session.getRevision();
    const reply = await session.queryServerWith({ cmd: "model_export", if_rev: rev });
    const { format, byte_length, sha256 } = expectResult(reply.body, "model_export");
    if (!reply.payload || reply.payload.byteLength !== byte_length || reply.rev !== rev) {
      throw new ProtocolError({ code: "bad_reply", message: "model_export receipt does not match its snapshot" });
    }
    return { bytes: reply.payload, format, byte_length, sha256, rev };
  }

  /** The source is validated before the session becomes visible; it has no store path until saved. */
  async openFile(bytes: Uint8Array, name: string): Promise<Session> {
    return this.#adopt(await this.#backend.sendWith(
      { cmd: "session_create", name: fileKey(name), source: { format: "clm" } },
      [["model", bytes]],
    ));
  }

  /**
   * References resolve against the manifest's own key and attach under the
   * name the manifest spells them with.
   */
  async importManifest(key: string): Promise<Session> {
    const manifest = await this.#read(key);
    let required: string[];
    try {
      required = this.#wasm.manifestRequirements(new TextDecoder().decode(manifest));
    } catch (cause) {
      throw asProtocolError(cause, "manifest");
    }
    const attachments: Attachment[] = [["manifest", manifest]];
    for (const reference of required) {
      attachments.push([`texture:${reference}`, await this.#read(joinKey(parentKey(key), reference))]);
    }

    return this.#adopt(await this.#backend.sendWith(
      { cmd: "session_create", name: key, source: { format: "manifest" } }, attachments,
    ));
  }

  /** Returns the key the save landed under. */
  async saveSession(session: Session, key?: string): Promise<string> {
    const body = await session.send({ cmd: "session_save", path: key ?? null });
    return expectResult(body, "saved").path;
  }

  /**
   * `undefined` when the bytes are not in this tab: a connected editor wrote
   * the file on its own machine.
   */
  readFile(key: string): Promise<Uint8Array | undefined> {
    return this.#backend.readFile(key);
  }

  /** By id, because the list names sessions this tab may never have attached. */
  async closeSession(id: SessionId): Promise<void> {
    await this.#backend.send({ cmd: "session_close", session: id });
    const held = this.#sessions.get(id);
    if (!held) return;
    this.#sessions.delete(id);
    held.close();
  }

  async listSessions(): Promise<SessionInfo[]> {
    const reply = await this.#backend.send({ cmd: "session_list" });
    return expectResult(reply.body, "sessions").sessions;
  }

  async attachSession(info: SessionInfo): Promise<Session> {
    return this.#sessions.get(info.session) ?? this.#adoptId(info.session, info.rev);
  }

  session(id: SessionId): Session | undefined {
    return this.#sessions.get(id);
  }

  /** Fires on open, close, edit, or save anywhere. */
  onSessionsChanged(listener: () => void): Unsubscribe {
    return this.#backend.onEvent((event) => {
      if (event.event === "sessions_changed" || event.event === "model_changed") listener();
    });
  }

  /**
   * The first call acquires the device from this canvas; on WebGL2 that is the
   * canvas it can present into. Rejects when the browser has neither WebGPU
   * nor WebGL2, with a message meant for the user.
   */
  async attach(session: Session, canvas: HTMLCanvasElement): Promise<Viewport> {
    const gpu = await this.#device(canvas);
    let view;
    try {
      view = new this.#wasm.Viewport(gpu, session.replica, canvas);
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }
    return new Viewport(view, canvas, session, gpu.maxSize());
  }

  #device(canvas: HTMLCanvasElement): Promise<WasmGpu> {
    if (this.#gpu) return Promise.resolve(this.#gpu);
    // Same-tick mounts share one acquisition, from the first canvas.
    this.#acquiring ??= this.#wasm.Gpu.acquire(canvas).then(
      (gpu) => {
        this.#gpu = gpu;
        // Copy: a listener may unsubscribe while being told.
        for (const listener of [...this.#gpuListeners]) listener();
        return gpu;
      },
      (cause: unknown) => {
        // Not cached: a StrictMode remount must be able to retry.
        this.#acquiring = undefined;
        throw cause;
      },
    );
    return this.#acquiring;
  }

  /** Unstable escape hatch: waits for no replica and notifies no session. */
  send(command: Command): Promise<OkReply> {
    return this.#backend.send(command);
  }

  sendWith(command: Command, attachments: readonly Attachment[] = []): Promise<OkReplyWithPayload> {
    return this.#backend.sendWith(command, attachments);
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const session of this.#sessions.values()) session.close();
    this.#sessions.clear();
    this.#gpu?.free();
    this.#gpu = undefined;
    this.#acquiring = undefined;
    this.#gpuListeners.clear();
    this.#backend.close();
  }

  async #read(key: string): Promise<Uint8Array> {
    const bytes = await this.#backend.readFile(key);
    if (bytes) return bytes;
    throw new ProtocolError({
      code: "io",
      message: `${key} is not in this tab's store`,
    });
  }

  async #adopt(reply: OkReply): Promise<Session> {
    const id = expectResult(reply.body, "session").session;
    return this.#sessions.get(id) ?? this.#adoptId(id, reply.rev ?? 0);
  }

  async #adoptId(id: SessionId, rev: number): Promise<Session> {
    const pending = this.#adopting.get(id);
    if (pending) return pending;
    // Opening and session-list notifications can attach the same model together.
    const adopting = this.#loadSession(id, rev);
    this.#adopting.set(id, adopting);
    try {
      return await adopting;
    } finally {
      this.#adopting.delete(id);
    }
  }

  async #loadSession(id: SessionId, rev: number): Promise<Session> {
    const session = new Session(this.#backend, id, new this.#wasm.Replica());
    try {
      await session.catchUp(rev);
    } catch (cause) {
      session.close();
      throw asProtocolError(cause);
    }
    this.#sessions.set(id, session);
    return session;
  }
}

/**
 * Keeps only the last path segment, so a save never writes back into a tree
 * the user did not name, and maps characters outside the key charset to `_`.
 */
export function fileKey(name: string): string {
  const last = name.split(/[\\/]/).pop() ?? "";
  const safe = last.replace(/[^A-Za-z0-9._-]/g, "_").replace(/^[.-]+/, "");
  return safe.length > 0 ? safe : "untitled.clm";
}
