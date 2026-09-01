/**
 * How a command reaches an editor.
 *
 * Two implementations are planned and layers above must never learn which one
 * is active: `LocalTransport` calls into the wasm module in this page;
 * `RemoteTransport` (not yet written) speaks the same messages over a
 * WebSocket. The protocol was built transport-agnostic — the same requests
 * already travel over a Unix socket natively — so this seam costs almost
 * nothing to keep and is what makes a server-backed mode possible later.
 *
 * **Everything here is async even where it resolves synchronously.** The local
 * transport could return a value directly; it returns a promise instead. That
 * is deliberate: it is what lets the wasm module move into a Worker, or a
 * remote server appear behind the same calls, without touching a single
 * caller.
 *
 * The message types come from `protocol.gen.ts`, which `cargo xtask ts`
 * writes from the Rust enums. Nothing in this package declares a wire shape of
 * its own — the envelope below is the one exception, and only because
 * `#[serde(flatten)]` has no name on this side.
 */

import type { Command, ErrorCode, Reply, ResponseBody } from "./protocol.gen.js";
import type { WasmViewport } from "./viewport.js";

/**
 * One request on the wire: a command plus the id its reply is correlated by.
 *
 * The Rust side is `Request { id, #[serde(flatten)] command }`; flattening has
 * no declaration of its own, so the intersection is written here rather than
 * generated. A transport assigns `id`, which is why callers pass a `Command`.
 */
export type Request = { id: number } & Command;

/**
 * Why a call failed. Most of these are [`ErrorCode`] — the server's own
 * reasons, generated from Rust.
 *
 * The two client codes exist because a failure can happen on this side of the
 * wire, where the server never sees it and so has no word for it: the
 * transport was closed, or a reply arrived that does not answer the request.
 * Callers branch on one union rather than on a code plus a separate "did it
 * even get there" flag.
 */
export type ClientErrorCode = "closed" | "bad_reply";
export type FailureCode = ErrorCode | ClientErrorCode;

/** What a refused command tells a client to branch on. */
export interface ProtocolErrorInfo {
  readonly code: FailureCode;
  readonly message: string;
}

/**
 * A command that failed. `code` is the machine-readable reason and the thing
 * to branch on; `message` is for a person.
 */
export class ProtocolError extends Error {
  readonly code: FailureCode;

  constructor(info: ProtocolErrorInfo) {
    super(info.message);
    this.name = "ProtocolError";
    this.code = info.code;
  }
}

/** A transport moves one request and returns its reply body. */
export interface Transport {
  /** Sends `command`, resolving with its body or rejecting `ProtocolError`. */
  send(command: Command): Promise<ResponseBody>;
  /** Releases whatever the transport holds. Idempotent. */
  close(): void;
}

/**
 * The wasm module's surface, as this package needs it. Declared structurally
 * rather than imported so the transport can be tested against a fake without
 * instantiating 2 MiB of WebAssembly.
 */
export interface WasmEditor {
  handle(requestJson: string): string;
  putBytes(key: string, bytes: Uint8Array): void;
  takeBytes(key: string): Uint8Array | undefined;
  stagedKeys(): string[];
  /**
   * The one asynchronous call on this surface, because WebGPU's adapter and
   * device are promises. Everything after it — a resize, a camera move, a
   * frame — is synchronous.
   */
  attach(canvas: HTMLCanvasElement, session: number): Promise<WasmViewport>;
  free?(): void;
}

/** Commands go straight into a wasm editor living in this page. */
export class LocalTransport implements Transport {
  #wasm: WasmEditor | undefined;
  #nextId = 1;

  constructor(wasm: WasmEditor) {
    this.#wasm = wasm;
  }

  send(command: Command): Promise<ResponseBody> {
    // Rejecting rather than throwing keeps every failure on one path for
    // callers, including this one — a closed transport is not special.
    const wasm = this.#wasm;
    if (!wasm) {
      return Promise.reject(
        new ProtocolError({
          code: "closed",
          message: "transport is closed",
        }),
      );
    }

    const request: Request = { id: this.#nextId++, ...command };
    let reply: Reply;
    try {
      reply = JSON.parse(wasm.handle(JSON.stringify(request))) as Reply;
    } catch (cause) {
      return Promise.reject(
        new ProtocolError({
          code: "bad_reply",
          message: `could not read the reply to ${command.cmd}: ${String(cause)}`,
        }),
      );
    }

    if (reply.reply === "err") {
      return Promise.reject(new ProtocolError(reply));
    }
    if (reply.reply !== "ok") {
      // An unsolicited event answered a request: the correlation is broken,
      // and silently resolving would hand the caller someone else's data.
      return Promise.reject(
        new ProtocolError({
          code: "bad_reply",
          message: `expected a reply to ${command.cmd}, got an event`,
        }),
      );
    }
    return Promise.resolve(reply.body);
  }

  /** The staging map the wasm editor reads `path` keys out of. */
  get staging(): WasmEditor {
    const wasm = this.#wasm;
    if (!wasm) throw new ProtocolError({ code: "closed", message: "transport is closed" });
    return wasm;
  }

  close(): void {
    this.#wasm?.free?.();
    this.#wasm = undefined;
  }
}
