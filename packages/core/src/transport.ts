/**
 * How a command reaches the editor: one JSON string in, one JSON string out.
 *
 * The wasm module speaks the protocol as serialized text — the same messages
 * the Unix socket and the CLI already speak — and this is the one place that
 * text is built and read. Everything above it works in typed `Command`s and
 * typed `ResponseBody`s and never sees a string.
 *
 * **Everything here is async even though it resolves synchronously.** The
 * wasm call returns a value directly; this returns a promise anyway. That is
 * deliberate: it is what lets the wasm module move into a Worker later without
 * touching a single caller. It is not a hedge for a remote server — the
 * document lives in this tab's wasm `Editor`, and cloud persistence rides the
 * `Storage` seam rather than this one (see `storage.ts`).
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
 * generated. The transport assigns `id`, which is why callers pass a `Command`.
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

/**
 * The wasm module's surface, as this package needs it. Declared structurally
 * rather than imported so everything above can be tested against a fake
 * without instantiating 2 MiB of WebAssembly.
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

/** Sends typed commands into the wasm editor and hands back typed bodies. */
export class Transport {
  #wasm: WasmEditor | undefined;
  #nextId = 1;

  constructor(wasm: WasmEditor) {
    this.#wasm = wasm;
  }

  /** Sends `command`, resolving with its body or rejecting `ProtocolError`. */
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

  /** The wasm module, for the byte staging that `path` keys are read from. */
  get wasm(): WasmEditor {
    const wasm = this.#wasm;
    if (!wasm) throw new ProtocolError({ code: "closed", message: "transport is closed" });
    return wasm;
  }

  /** Releases the wasm module. Idempotent. */
  close(): void {
    this.#wasm?.free?.();
    this.#wasm = undefined;
  }
}
