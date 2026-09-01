/**
 * The editor, as the layers above it see it.
 *
 * This is the whole public surface of `@catchlight/core`: everything a React
 * hook or a host application needs, and nothing about how it is served. In
 * particular nothing above this file may reach the transport, the wasm module
 * or the store directly — that is what keeps "no mirror" (see `session.ts`) and
 * "where the bytes live" (see `storage.ts`) decisions changeable in one
 * package.
 *
 * **The document is the wasm editor in this tab.** Every command goes into it
 * and every picture comes out of it; cloud persistence, when it comes, is a
 * `Storage` that reads and writes whole `.clm` files plus a lock, not a server
 * that sees commands. So an `Editor` always has a renderer to point at a
 * canvas, and there is no configuration in which `attach` cannot work.
 *
 * The rule this package lives by: **it contains only web-platform glue.** File
 * pickers, OPFS, canvas lifecycle, pointer marshalling, this store adapter.
 * Anything a native iOS or Android editor would also need — what a drag means,
 * what a tool does, how a hit test resolves — belongs in Rust, because that is
 * the half a native build reuses verbatim and the half TypeScript would have to
 * write twice.
 */

import type { Command, ResponseBody, SessionInfo } from "./protocol.gen.js";
import { Session } from "./session.js";
import type { Storage } from "./storage.js";
import { ProtocolError, Transport } from "./transport.js";
import type { WasmEditor } from "./transport.js";
import { Viewport } from "./viewport.js";

export class Editor {
  #transport: Transport;
  #storage: Storage;

  /**
   * An editor over the wasm module in this page, backed by `storage`.
   *
   * The module is passed in rather than imported so this package never forces
   * 2 MiB of WebAssembly into a bundle that only wanted the types — and so the
   * whole thing is testable against a fake.
   */
  constructor(wasm: WasmEditor, storage: Storage) {
    this.#transport = new Transport(wasm);
    this.#storage = storage;
  }

  /** Draws `session` on `canvas`, until the returned viewport is disposed. */
  async attach(session: Session, canvas: HTMLCanvasElement): Promise<Viewport> {
    const view = await this.#transport.wasm.attach(canvas, session.id);
    return new Viewport(view, canvas, session);
  }

  /** An empty document. */
  async newDocument(name?: string): Promise<Session> {
    const body = await this.#transport.send({ cmd: "session_new", name: name ?? null });
    return new Session(this.#transport, expect(body, "session").session);
  }

  /**
   * Opens the document stored at `key`.
   *
   * The three steps — read the bytes, stage them where the synchronous wasm
   * side can see them, then name the key in the command — are this method's
   * whole reason to exist. Nothing above should have to know that order.
   */
  async openDocument(key: string): Promise<Session> {
    const bytes = await this.#storage.read(key);
    this.#stage(key, bytes);
    const body = await this.#transport.send({ cmd: "session_open", path: key });
    return new Session(this.#transport, expect(body, "session").session);
  }

  /**
   * Writes `session` back to the store, at `key` or wherever it was opened
   * from.
   *
   * The mirror image of `openDocument`: the command stages the bytes, and this
   * drains them into the store. Draining rather than copying matters — a
   * staging map that is never emptied holds a whole second copy of the model's
   * textures.
   */
  async saveDocument(session: Session, key?: string): Promise<string> {
    const body = await session.send({ cmd: "save", path: key ?? null });
    const savedKey = expect(body, "saved").path;
    const bytes = this.#unstage(savedKey);
    if (!bytes) {
      throw new ProtocolError({
        code: "bad_reply",
        message: `save reported ${savedKey} but staged no bytes for it`,
      });
    }
    await this.#storage.write(savedKey, bytes);
    return savedKey;
  }

  /** Every open document. */
  async listSessions(): Promise<SessionInfo[]> {
    const body = await this.#transport.send({ cmd: "session_list" });
    return expect(body, "sessions").sessions;
  }

  /**
   * Sends a command that names no session. Present so a host is never stuck
   * waiting for this class to grow a method; anything used twice should get
   * one.
   */
  send(command: Command): Promise<ResponseBody> {
    return this.#transport.send(command);
  }

  close(): void {
    this.#transport.close();
  }

  /**
   * The staging map is where the browser's asynchrony stops: bytes are put
   * here after `await`, and the synchronous wasm side reads them by key.
   */
  #stage(key: string, bytes: Uint8Array): void {
    this.#transport.wasm.putBytes(key, bytes);
  }

  #unstage(key: string): Uint8Array | undefined {
    return this.#transport.wasm.takeBytes(key);
  }
}

/**
 * Narrows a reply to the one shape this call asked for.
 *
 * A server that answered something else is a bug, not a case to handle, but it
 * has to surface as a `ProtocolError` like any other failure rather than as an
 * `undefined` read three frames later.
 */
function expect<R extends ResponseBody["result"]>(
  body: ResponseBody,
  result: R,
): Extract<ResponseBody, { result: R }> {
  if (body.result !== result) {
    throw new ProtocolError({
      code: "bad_reply",
      message: `expected a ${result} reply, got ${JSON.stringify(body)}`,
    });
  }
  return body as Extract<ResponseBody, { result: R }>;
}
