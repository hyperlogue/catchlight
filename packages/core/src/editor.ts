/**
 * The editor, as everything above it sees it.
 *
 * This is the whole public surface of `@catchlight/core`: what a React hook or
 * a host application needs, and nothing about where the document is served
 * from. Nothing above this file may reach the backend, the wasm module or the
 * store directly — that is what keeps "the tab holds a replica" (see
 * `session.ts`) and "where the bytes live" (see `storage.ts`) changeable in
 * one package.
 *
 * **One device, acquired once.** [`Editor.create`] is asynchronous for exactly
 * that reason: a WebGPU adapter is a promise, and every replica and viewport
 * under this editor shares the device it hands back. After that, opening a
 * document, reading a tree and running a drag are all synchronous or one
 * round trip.
 *
 * **Every session is fed before it is handed out.** A `Session` a caller holds
 * has already been brought to the revision its reply named, so the first thing
 * a panel does with it is a read that answers.
 *
 * The rule this package lives by: **it contains only web-platform glue.** File
 * pickers, OPFS, canvas lifecycle, pointer marshalling, the fetch and socket
 * adapters. Anything a native iOS or Android editor would also need — what a
 * drag means, what a tool does, how a hit test resolves — belongs in Rust,
 * because that is the half a native build reuses verbatim and the half
 * TypeScript would otherwise write twice.
 */

import type { Command, SessionId, SessionInfo } from "./protocol.gen.js";
import type { Backend, OkReply, Unsubscribe } from "./backend.js";
import { asProtocolError, expectResult } from "./backend.js";
import { Session } from "./session.js";
import type { WasmGpu, WasmModule } from "./wasm.js";
import { Viewport } from "./viewport.js";

export class Editor {
  #wasm: WasmModule;
  #backend: Backend;
  #gpu: WasmGpu;
  #sessions = new Map<SessionId, Session>();
  #closed = false;

  private constructor(wasm: WasmModule, backend: Backend, gpu: WasmGpu) {
    this.#wasm = wasm;
    this.#backend = backend;
    this.#gpu = gpu;
  }

  /**
   * Acquires the GPU and returns an editor over `backend`.
   *
   * The wasm module is passed in rather than imported so this package never
   * forces 2 MiB of WebAssembly into a bundle that only wanted the types — and
   * so the whole thing is testable against a fake. The backend is built from
   * the same module when it is the in-tab one.
   */
  static async create(wasm: WasmModule, backend: Backend): Promise<Editor> {
    const gpu = await wasm.Gpu.acquire();
    return new Editor(wasm, backend, gpu);
  }

  /** An empty document. */
  async newDocument(name?: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_new", name: name ?? null }));
  }

  /**
   * Opens the document the store holds at `key`.
   *
   * Whether the bytes have to be staged first is the backend's business, not
   * this method's: in-tab it reads them out of the store and stages them,
   * connected the key already names a file the editor can read.
   */
  async openDocument(key: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_open", path: key }));
  }

  /**
   * Opens bytes the page already holds — a dropped file, a picked one, a fetch
   * — under a key derived from `name`.
   *
   * The key is what the document is then addressed by, including by a later
   * save, so it is sanitized down to what a storage key is read for: `/`
   * separates segments and the tail after the last `.` picks a decoder.
   */
  async openFile(bytes: Uint8Array, name: string): Promise<Session> {
    const key = fileKey(name);
    await this.#backend.putBytes(key, bytes);
    return this.#adopt(await this.#backend.send({ cmd: "session_open", path: key }));
  }

  /**
   * Opens a manifest and the textures it references.
   *
   * The editor is asked what the manifest needs before it is imported, because
   * an in-tab editor cannot go looking for a file: every key it will read has
   * to be resolvable first, and only the manifest itself is named by the
   * command. A connected editor already has its own store, so the staging is
   * a no-op there.
   */
  async importManifest(key: string): Promise<Session> {
    const asked = await this.#backend.send({ cmd: "manifest_requirements", manifest_path: key });
    for (const required of expectResult(asked.body, "manifest_requirements").textures) {
      await this.#backend.stageKey(required);
    }
    return this.#adopt(await this.#backend.send({ cmd: "session_import", manifest_path: key }));
  }

  /**
   * Writes `session` back to the store, at `key` or wherever it was opened
   * from, and returns the key it landed under.
   */
  async saveDocument(session: Session, key?: string): Promise<string> {
    const body = await session.send({ cmd: "save", path: key ?? null });
    return expectResult(body, "saved").path;
  }

  /** Every document the editor has open, including ones this tab did not open. */
  async listSessions(): Promise<SessionInfo[]> {
    const reply = await this.#backend.send({ cmd: "session_list" });
    return expectResult(reply.body, "sessions").sessions;
  }

  /**
   * Follows a session that already exists on the backend — one an agent or
   * another tab opened, as [`listSessions`] reports it.
   */
  async attachSession(info: SessionInfo): Promise<Session> {
    return this.#sessions.get(info.session) ?? this.#adoptId(info.session, info.rev);
  }

  /** The session this editor already holds for `id`, if any. */
  session(id: SessionId): Session | undefined {
    return this.#sessions.get(id);
  }

  /** Registers `listener`, called when a document is opened or closed anywhere. */
  onSessionsChanged(listener: () => void): Unsubscribe {
    return this.#backend.onEvent((event) => {
      if (event.event === "sessions_changed") listener();
    });
  }

  /**
   * Draws `session` on `canvas` until the returned viewport is disposed.
   *
   * Asynchronous only to keep the option: the device already exists, so the
   * renderer is built synchronously underneath.
   */
  async attach(session: Session, canvas: HTMLCanvasElement): Promise<Viewport> {
    let view;
    try {
      view = new this.#wasm.Viewport(this.#gpu, session.replica, canvas);
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }
    return new Viewport(view, canvas, session, this.#gpu.maxSize());
  }

  /**
   * Sends a command directly.
   *
   * The escape hatch, and deliberately unstable: it takes the wire type, it
   * does not wait for a replica to catch up, and it tells no session that
   * anything changed. Anything reached for twice should become a method here
   * instead.
   */
  send(command: Command): Promise<OkReply> {
    return this.#backend.send(command);
  }

  /** Frees every replica, the device, and the backend. Idempotent. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const session of this.#sessions.values()) session.close();
    this.#sessions.clear();
    this.#gpu.free();
    this.#backend.close();
  }

  /** Turns a `session` reply into a session whose replica is already current. */
  async #adopt(reply: OkReply): Promise<Session> {
    const id = expectResult(reply.body, "session").session;
    return this.#sessions.get(id) ?? this.#adoptId(id, reply.rev ?? 0);
  }

  async #adoptId(id: SessionId, rev: number): Promise<Session> {
    const session = new Session(this.#backend, id, new this.#wasm.Replica(this.#gpu));
    try {
      await session.catchUp(rev);
    } catch (cause) {
      // A replica that never loaded is not a session anybody can use, and it
      // holds GPU memory until something frees it.
      session.close();
      throw asProtocolError(cause);
    }
    this.#sessions.set(id, session);
    return session;
  }
}

/**
 * A storage key for a file the page holds.
 *
 * Only the last segment is kept — a picked file's `webkitRelativePath` or a
 * dropped one's name can carry a whole tree, and a document that writes itself
 * back somewhere the user did not name is worse than one with a flattened
 * name. Everything outside the key charset becomes `_` so the key survives a
 * round trip through a URL path.
 */
export function fileKey(name: string): string {
  const last = name.split(/[\\/]/).pop() ?? "";
  const safe = last.replace(/[^A-Za-z0-9._-]/g, "_").replace(/^[.-]+/, "");
  return safe.length > 0 ? safe : "untitled.clm";
}
