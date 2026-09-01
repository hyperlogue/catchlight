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
 */

/** A protocol request, minus the correlation id the transport assigns. */
export type Command = { cmd: string } & Record<string, unknown>;

/** A successful reply's payload. */
export type ResponseBody = { result: string } & Record<string, unknown>;

/** What a refused command tells a client to branch on. */
export interface ProtocolErrorInfo {
  readonly code: string;
  readonly message: string;
}

/**
 * A command the editor refused. `code` is the machine-readable reason and the
 * thing to branch on; `message` is for a person.
 */
export class ProtocolError extends Error {
  readonly code: string;

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
  free?(): void;
}

type Reply =
  | { reply: "ok"; id: number; body: ResponseBody }
  | { reply: "err"; id: number; code: string; message: string }
  | { reply: "event"; [k: string]: unknown };

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

    const id = this.#nextId++;
    let reply: Reply;
    try {
      reply = JSON.parse(wasm.handle(JSON.stringify({ id, ...command }))) as Reply;
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
