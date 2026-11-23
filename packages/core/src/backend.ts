/**
 * Where the editor that owns the model is: in this tab or a local process.
 * Nothing above this file may branch on which.
 */

import type { Command, ErrorCode, Event, LimitInfo, Reply, ResponseBody } from "./protocol.gen.js";
import { COMMAND_BYTES } from "./protocol.gen.js";
import type { WasmReplica } from "./wasm.js";

/** Hand-written: Rust's `#[serde(flatten)] command` has no generated declaration. */
export type Request = { id: number } & Command;

export interface OkReply {
  readonly body: ResponseBody;
  /** Absent for editor-level commands and for `session_close`. */
  readonly rev?: number | undefined;
}

export type Unsubscribe = () => void;

/** [`ErrorCode`] plus client-side failures the editor never sees. */
export type ClientErrorCode = "closed" | "bad_reply" | "feed";
export type FailureCode = ErrorCode | ClientErrorCode;

export interface ProtocolErrorInfo {
  readonly code: FailureCode;
  readonly message: string;
  readonly rev?: number | null;
  readonly op_index?: number | null;
  readonly limit?: LimitInfo | null;
}

/** Branch on `code`; `message` is for a person. */
export class ProtocolError extends Error {
  readonly code: FailureCode;
  readonly rev: number | undefined;
  readonly op_index: number | undefined;
  readonly limit: LimitInfo | undefined;

  constructor(info: ProtocolErrorInfo) {
    super(info.message);
    this.name = "ProtocolError";
    this.code = info.code;
    this.rev = info.rev ?? undefined;
    this.op_index = info.op_index ?? undefined;
    this.limit = info.limit ?? undefined;
  }
}

export type Attachment = readonly [name: string, bytes: Uint8Array];

export interface OkReplyWithPayload extends OkReply {
  readonly payload?: Uint8Array | undefined;
}

export interface Backend {
  readonly kind: "in-tab" | "connected";

  /**
   * Rejects [`ProtocolError`]. A [`COMMAND_BYTES`] command throws before
   * sending; it belongs to [`sendWith`].
   */
  send(command: Command): Promise<OkReply>;

  /** `attachments` are named as [`COMMAND_BYTES`] spells them. */
  sendWith(command: Command, attachments: readonly Attachment[]): Promise<OkReplyWithPayload>;

  /**
   * `undefined` when there is no copy in this tab (connected). A missing key
   * in-tab is an error, since a save that reported a key wrote it.
   */
  readFile(key: string): Promise<Uint8Array | undefined>;

  /** Resolves with the revision held afterwards, which may be newer than asked. */
  feed(replica: WasmReplica, session: number, rev: number): Promise<number>;

  onEvent(listener: (event: Event) => void): Unsubscribe;

  close(): void;
}

export function carriesBytes(command: Command): boolean {
  if (command.cmd === "session_create") return command.source != null;
  if (command.cmd === "extension_set") return command.value.kind === "bytes";
  return COMMAND_BYTES[command.cmd] !== undefined;
}

export function refuseIfItCarriesBytes(command: Command): void {
  if (!carriesBytes(command)) return;
  throw new ProtocolError({
    code: "bad_request",
    message: `${command.cmd} carries bytes; send it with sendWith`,
  });
}

export function readReply(text: string, cmd: string): OkReply {
  let reply: Reply;
  try {
    reply = JSON.parse(text) as Reply;
  } catch (cause) {
    throw new ProtocolError({
      code: "bad_reply",
      message: `could not read the reply to ${cmd}: ${String(cause)}`,
    });
  }
  return takeReply(reply, cmd);
}

export function takeReply(reply: Reply, cmd: string): OkReply {
  if (reply.reply === "err") throw new ProtocolError(reply);
  if (reply.reply !== "ok") {
    // An event answered a request: correlation is broken.
    throw new ProtocolError({
      code: "bad_reply",
      message: `expected a reply to ${cmd}, got an event`,
    });
  }
  return reply.rev === null || reply.rev === undefined
    ? { body: reply.body }
    : { body: reply.body, rev: reply.rev };
}

export function expectResult<R extends ResponseBody["result"]>(
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

export function asProtocolError(cause: unknown, code: FailureCode = "feed"): ProtocolError {
  if (cause instanceof ProtocolError) return cause;
  // wasm-bindgen throws strings, `fetch` throws `TypeError`.
  return new ProtocolError({ code, message: String(cause) });
}

/**
 * Serializes feeds per session and coalesces waiting ones into one at the
 * highest requested revision; safe because a replica applies forward only.
 */
export class FeedQueue {
  #active = new Map<number, Promise<number>>();
  #queued = new Map<number, Promise<number>>();
  #wanted = new Map<number, { rev: number; feed: (rev: number) => Promise<number> }>();

  run(session: number, rev: number, feed: (rev: number) => Promise<number>): Promise<number> {
    const active = this.#active.get(session);
    if (!active) return this.#start(session, rev, feed);

    const wanted = this.#wanted.get(session);
    this.#wanted.set(session, { rev: Math.max(wanted?.rev ?? 0, rev), feed });

    const queued = this.#queued.get(session);
    if (queued) return queued;

    const next = active
      // A failure must not strand queued feeds; the next often recovers.
      .catch(() => undefined)
      .then(() => {
        this.#queued.delete(session);
        const target = this.#wanted.get(session);
        this.#wanted.delete(session);
        if (!target) return 0;
        return this.#start(session, target.rev, target.feed);
      });
    this.#queued.set(session, next);
    return next;
  }

  #start(session: number, rev: number, feed: (rev: number) => Promise<number>): Promise<number> {
    const running = (async () => feed(rev))();
    this.#active.set(session, running);
    // Only if nothing replaced it: a queued feed may have started already.
    void running
      .catch(() => undefined)
      .then(() => {
        if (this.#active.get(session) === running) this.#active.delete(session);
      });
    return running;
  }
}
