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
 * **One device, acquired at the first attach, from that attach's canvas.**
 * Not at creation: opening a document, reading a tree and running a drag need
 * no GPU, and a tab that only ever lists documents should not hold one. So
 * [`attach`] acquires it, once, and every later viewport and replica shares
 * it. The canvas goes with the request because the fallback tier needs it: a
 * WebGL2 device is a canvas's rendering context, so the first element to ask
 * is the one that device can present into, and later canvases there are drawn
 * another way. WebGPU ignores it. Which tier answered is [`gpuTier`], a fact
 * to report and not one to branch on.
 *
 * **That acquisition is the only await between a canvas and a frame.** Every
 * later attach resolves from the device already in hand, and everything below
 * it — a viewport, a resize, a camera, a frame — is synchronous.
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
  #gpuListeners = new Set<() => void>();
  #closed = false;

  private constructor(wasm: WasmModule, backend: Backend) {
    this.#wasm = wasm;
    this.#backend = backend;
  }

  /**
   * An editor over `backend`. Touches no GPU: the device waits for the first
   * [`attach`].
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

  /**
   * Where the document lives: the wasm editor in this tab, or a process this
   * tab is connected to.
   *
   * The one thing above this class that may legitimately branch on which
   * backend it has — a status line saying which of the two a person is
   * looking at. Nothing about *behaviour* may read it; that is the seam's
   * whole point.
   */
  backendKind(): Backend["kind"] {
    return this.#backend.kind;
  }

  /**
   * Which graphics tier the device came up on: `"webgpu"`, or `"webgl2"` on
   * the fallback.
   *
   * `undefined` until the first [`attach`] has acquired a device, because
   * until then nothing has an answer. [`onGpuChanged`] is how a label finds
   * out that it now does. Like [`backendKind`], for a person to read and for
   * nothing to branch on: what the tier changes is handled inside the wasm
   * module.
   */
  gpuTier(): string | undefined {
    return this.#gpu?.tier();
  }

  /**
   * Registers `listener`, called once the device exists.
   *
   * One notification, at the one moment the answer to [`gpuTier`] changes: a
   * device is acquired once per editor and never swapped.
   */
  onGpuChanged(listener: () => void): Unsubscribe {
    this.#gpuListeners.add(listener);
    return () => {
      this.#gpuListeners.delete(listener);
    };
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
   * command. A connected editor already has its own store, so the staging and
   * the discard that follows it are both no-ops there.
   */
  async importManifest(key: string): Promise<Session> {
    const asked = await this.#backend.send({ cmd: "manifest_requirements", manifest_path: key });
    const required = expectResult(asked.body, "manifest_requirements").textures;
    for (const texture of required) await this.#backend.stageKey(texture);
    const reply = await this.#backend.send({ cmd: "session_import", manifest_path: key });
    // The import decoded every one of them into a model that owns its copy, so
    // holding the encoded bytes as well is the whole model twice. The backend
    // discards the keys a command named for itself; these it never saw.
    for (const texture of required) await this.#backend.discardKey(texture);
    return this.#adopt(reply);
  }

  /**
   * Writes `session` back to the store, at `key` or wherever it was opened
   * from, and returns the key it landed under.
   */
  async saveDocument(session: Session, key?: string): Promise<string> {
    const body = await session.send({ cmd: "save", path: key ?? null });
    return expectResult(body, "saved").path;
  }

  /**
   * The bytes the store holds at `key`, or `undefined` when they are not in
   * this tab to be had.
   *
   * What makes a save in a tab worth anything: the store an in-tab editor
   * writes to is the browser's own, and a document that never leaves it is
   * one the person cannot take anywhere. So a host saves, reads the key back
   * through here, and hands the bytes to the browser as a download. A
   * connected editor wrote the file where it was asked to and hands back
   * nothing — the host then says where it went rather than downloading it.
   */
  readDocument(key: string): Promise<Uint8Array | undefined> {
    return this.#backend.readBytes(key);
  }

  /**
   * Closes the document on the editor and frees this tab's replica of it.
   *
   * By id rather than by `Session`, because the list a person closes from
   * names sessions this tab may never have attached. Closing one it did hold
   * frees the replica too; anything still waiting on that session rejects.
   */
  async closeDocument(id: SessionId): Promise<void> {
    await this.#backend.send({ cmd: "session_close", session: id });
    const held = this.#sessions.get(id);
    if (!held) return;
    this.#sessions.delete(id);
    held.close();
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

  /**
   * Registers `listener`, called whenever what [`listSessions`] reports may
   * have changed: a document opened or closed anywhere, and a document that
   * moved — a `SessionInfo` carries the revision, the node count and whether
   * there is anything unsaved, and every one of those follows an edit or a
   * save.
   */
  onSessionsChanged(listener: () => void): Unsubscribe {
    return this.#backend.onEvent((event) => {
      if (event.event === "sessions_changed" || event.event === "document_changed") listener();
    });
  }

  /**
   * Draws `session` on `canvas` until the returned viewport is disposed.
   *
   * The first call acquires the device, from this canvas; everything after it
   * is synchronous. Any number of canvases may be attached, to one session or
   * to several: they share the device, and a session's replica keeps one
   * renderer and one render cache however many viewports draw it.
   *
   * Rejects when this browser has neither WebGPU nor WebGL2, with the message
   * the wasm module writes — the one thing a host has to show a person rather
   * than log.
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
    // the second one waits on the first one's promise, and the first one's
    // canvas is the one the device is made from.
    this.#acquiring ??= this.#wasm.Gpu.acquire(canvas).then(
      (gpu) => {
        this.#gpu = gpu;
        // Copy: a listener that unsubscribes while being told must not shift
        // the set out from under this loop.
        for (const listener of [...this.#gpuListeners]) listener();
        return gpu;
      },
      (cause: unknown) => {
        // A failure is not a verdict: a transient one must not be the answer
        // every later attach gets. Remembering it would make a StrictMode
        // remount unrecoverable.
        this.#acquiring = undefined;
        throw cause;
      },
    );
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
    this.#gpuListeners.clear();
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
