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
 * **One device, acquired at the first canvas.** Not at creation: on WebGL2 an
 * adapter is enumerated from a canvas context and presents into that same
 * canvas, so no device can exist before something asks to be drawn. So
 * [`attach`] acquires it, once, and every later viewport and replica shares
 * it — and on the WebGL2 fallback the canvas that acquired it is the only one
 * that can be drawn on. Opening a document, reading a tree and running a drag
 * need no device at all.
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
  #gpu: WasmGpu | undefined;
  /** The one acquisition in flight, so two first attaches share a device. */
  #acquiring: Promise<WasmGpu> | undefined;
  #sessions = new Map<SessionId, Session>();
  #closed = false;

  private constructor(wasm: WasmModule, backend: Backend) {
    this.#wasm = wasm;
    this.#backend = backend;
  }

  /**
   * An editor over `backend`. Touches no GPU: the device waits for a canvas.
   *
   * The wasm module is passed in rather than imported so this package never
   * forces 2 MiB of WebAssembly into a bundle that only wanted the types — and
   * so the whole thing is testable against a fake. The backend is built from
   * the same module when it is the in-tab one.
   *
   * Asynchronous only to keep the option: a host that already awaits this must
   * not have to change when something here does need to be awaited.
   */
  static create(wasm: WasmModule, backend: Backend): Promise<Editor> {
    return Promise.resolve(new Editor(wasm, backend));
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
   * The first call acquires the device, from this canvas. Everything after it
   * is synchronous — and on the WebGL2 fallback every later canvas is drawn by
   * a device that belongs to the first one, which is a limit of the backend
   * rather than of this method.
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

  /** The device, acquired from `canvas` if this is the first one to ask. */
  #device(canvas: HTMLCanvasElement): Promise<WasmGpu> {
    if (this.#gpu) return Promise.resolve(this.#gpu);
    // Two canvases mounting in the same tick must not race for two devices:
    // the second one waits on the first one's promise.
    this.#acquiring ??= this.#wasm.Gpu.acquire(canvas).then((gpu) => {
      this.#gpu = gpu;
      return gpu;
    });
    return this.#acquiring;
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

  /** Frees every replica, the device if one was ever acquired, and the backend. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const session of this.#sessions.values()) session.close();
    this.#sessions.clear();
    this.#gpu?.free();
    this.#gpu = undefined;
    this.#acquiring = undefined;
    this.#backend.close();
  }

  /** Turns a `session` reply into a session whose replica is already current. */
  async #adopt(reply: OkReply): Promise<Session> {
    const id = expectResult(reply.body, "session").session;
    return this.#sessions.get(id) ?? this.#adoptId(id, reply.rev ?? 0);
  }

  async #adoptId(id: SessionId, rev: number): Promise<Session> {
    const session = new Session(this.#backend, id, new this.#wasm.Replica());
    try {
      await session.catchUp(rev);
    } catch (cause) {
      // A replica that never loaded is not a session anybody can use, and it
      // holds its model until something frees it.
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
