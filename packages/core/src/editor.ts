/**
 * The editor, as everything above it sees it.
 *
 * This is the whole public surface of `@catchlight/core`: what a React hook or
 * a host application needs, and nothing about where the model is served
 * from. Nothing above this file may reach the backend, the wasm module or the
 * store directly — that is what keeps "the tab holds a replica" (see
 * `session.ts`) and "where the bytes live" (see `storage.ts`) changeable in
 * one package.
 *
 * **One device, acquired at the first attach, from that attach's canvas.**
 * Not at creation: opening a model, reading a tree and running a drag need
 * no GPU, and a tab that only ever lists models should not hold one. So
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
import type { Attachment, Backend, OkReply, Unsubscribe } from "./backend.js";
import { asProtocolError, expectResult, ProtocolError } from "./backend.js";
import { Session } from "./session.js";
import { joinKey, parentKey } from "./storage.js";
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
   * Where the model lives: the wasm editor in this tab, or a process this
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

  /** An empty model. */
  async newSession(name?: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_new", name: name ?? null }));
  }

  /**
   * Opens the model the *editor's* store holds at `key`.
   *
   * A file of the editor's, which the session can then save back over. Bytes
   * this page holds are [`openFile`] instead.
   */
  async openSession(key: string): Promise<Session> {
    return this.#adopt(await this.#backend.send({ cmd: "session_open", path: key }));
  }

  /**
   * Opens bytes the page already holds — a dropped file, a picked one, a
   * fetch — as a model named `name`.
   *
   * A fresh session and then the file imported into it, because the two mean
   * different things: nothing named a file of the editor's, so the session
   * gets none and a later save has to be told where to go.
   */
  async openFile(bytes: Uint8Array, name: string): Promise<Session> {
    const made = await this.#backend.send({ cmd: "session_new", name: fileKey(name) });
    const session = expectResult(made.body, "session").session;
    return this.#fill(session, { cmd: "import_file", session, parent: null }, [["model", bytes]]);
  }

  /**
   * Opens a manifest and the textures it references, all of which this page
   * reads itself.
   *
   * What the manifest needs is a pure function of its JSON, so this asks the
   * wasm module rather than the editor: there is no session yet, and nothing
   * to ask one about. Each reference is resolved against the manifest's own
   * key — which is what a reference in a manifest means — and attached under
   * the name the import matches it by, spelled exactly as the manifest spells
   * it.
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

    const made = await this.#backend.send({ cmd: "session_new", name: key });
    const session = expectResult(made.body, "session").session;
    return this.#fill(session, { cmd: "import_manifest", session }, attachments);
  }

  /**
   * Writes `session` back to the store, at `key` or wherever it was opened
   * from, and returns the key it landed under.
   */
  async saveSession(session: Session, key?: string): Promise<string> {
    const body = await session.send({ cmd: "save", path: key ?? null });
    return expectResult(body, "saved").path;
  }

  /**
   * The bytes the store holds at `key`, or `undefined` when they are not in
   * this tab to be had.
   *
   * What makes a save in a tab worth anything: the store an in-tab editor
   * writes to is the browser's own, and a model that never leaves it is
   * one the person cannot take anywhere. So a host saves, reads the key back
   * through here, and hands the bytes to the browser as a download. A
   * connected editor wrote the file where it was asked to and hands back
   * nothing — the host then says where it went rather than downloading it.
   */
  readFile(key: string): Promise<Uint8Array | undefined> {
    return this.#backend.readFile(key);
  }

  /**
   * Closes the model on the editor and frees this tab's replica of it.
   *
   * By id rather than by `Session`, because the list a person closes from
   * names sessions this tab may never have attached. Closing one it did hold
   * frees the replica too; anything still waiting on that session rejects.
   */
  async closeSession(id: SessionId): Promise<void> {
    await this.#backend.send({ cmd: "session_close", session: id });
    const held = this.#sessions.get(id);
    if (!held) return;
    this.#sessions.delete(id);
    held.close();
  }

  /** Every model the editor has open, including ones this tab did not open. */
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
   * have changed: a model opened or closed anywhere, and a model that
   * moved — a `SessionInfo` carries the revision, the node count and whether
   * there is anything unsaved, and every one of those follows an edit or a
   * save.
   */
  onSessionsChanged(listener: () => void): Unsubscribe {
    return this.#backend.onEvent((event) => {
      if (event.event === "sessions_changed" || event.event === "model_changed") listener();
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

  /**
   * One key out of this tab's store, or a refusal naming it.
   *
   * A connected editor holds no store here — its files are on its own machine
   * — so this is what makes [`importManifest`] a tab-side operation and says
   * so plainly rather than sending an import with nothing attached.
   */
  async #read(key: string): Promise<Uint8Array> {
    const bytes = await this.#backend.readFile(key);
    if (bytes) return bytes;
    throw new ProtocolError({
      code: "io",
      message: `${key} is not in this tab's store`,
    });
  }

  /** Turns a `session` reply into a session whose replica is already current. */
  async #adopt(reply: OkReply): Promise<Session> {
    const id = expectResult(reply.body, "session").session;
    return this.#sessions.get(id) ?? this.#adoptId(id, reply.rev ?? 0);
  }

  /**
   * Runs the import that gives a just-made session its model, and adopts it at
   * the revision that import produced.
   *
   * A session that stays empty is worse than none — it would sit in every
   * list as a model nobody opened — so a refused import takes it with it.
   */
  async #fill(
    session: SessionId,
    command: Command,
    attachments: readonly Attachment[],
  ): Promise<Session> {
    let reply;
    try {
      reply = await this.#backend.sendWith(command, attachments);
    } catch (cause) {
      await this.#backend.send({ cmd: "session_close", session }).catch(() => undefined);
      throw asProtocolError(cause);
    }
    return this.#adoptId(session, reply.rev ?? 0);
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
 * dropped one's name can carry a whole tree, and a model that writes itself
 * back somewhere the user did not name is worse than one with a flattened
 * name. Everything outside the key charset becomes `_` so the key survives a
 * round trip through a URL path.
 */
export function fileKey(name: string): string {
  const last = name.split(/[\\/]/).pop() ?? "";
  const safe = last.replace(/[^A-Za-z0-9._-]/g, "_").replace(/^[.-]+/, "");
  return safe.length > 0 ? safe : "untitled.clm";
}
