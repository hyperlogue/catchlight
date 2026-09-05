/**
 * The one seam under everything: where the editor that owns the model is.
 *
 * A `Backend` is either the wasm editor running in this tab or a local editor
 * process reached over HTTP and a WebSocket. Both answer the same commands,
 * emit the same events, and feed the same kind of replica; nothing above this
 * file may branch on which one it has.
 *
 * **The tab holds a replica, not the model.** A command goes to the
 * backend; what comes back is a body and the revision the session is at
 * *after* it. The replica moves only through [`Backend.feed`], which is driven
 * by the `model_changed` event — one path, so a reply and an event can
 * never race to apply two versions of the same state.
 *
 * **`send` resolves after its events are dispatched.** An in-tab editor
 * produces its events synchronously inside `handle`, so draining them and
 * calling the listeners before resolving is what lets a caller wait for a
 * revision that the event it has not seen yet is what delivers. Ordering it
 * the other way deadlocks the first `node_add`.
 *
 * **Feeds for one session never overlap.** [`FeedQueue`] serializes them and
 * coalesces everything waiting into a single feed at the newest revision, so a
 * burst of edits costs one structure fetch rather than one per event.
 *
 * **Bytes ride with the command that uses them, over HTTP and never the
 * socket.** [`COMMAND_BYTES`] says which commands carry bytes; those go
 * through [`Backend.sendWith`], and passing one to [`Backend.send`] throws
 * before anything is sent. There is nothing to stage and no key to name — a
 * blob nothing ever names cannot exist — and a reply that carries bytes hands
 * them back beside it.
 */

import type { Command, ErrorCode, Event, Reply, ResponseBody } from "./protocol.gen.js";
import { COMMAND_BYTES } from "./protocol.gen.js";
import type { WasmReplica } from "./wasm.js";

/**
 * One request on the wire: a command plus the id its reply is correlated by.
 *
 * The Rust side is `Request { id, #[serde(flatten)] command }`; a flattened
 * field has no declaration of its own, so the intersection is written here
 * rather than generated. A backend assigns `id`; callers pass a `Command`.
 */
export type Request = { id: number } & Command;

/** A successful reply: the body, and the revision the session is at after it. */
export interface OkReply {
  readonly body: ResponseBody;
  /**
   * Absent only for the editor-level commands that name no session, and for a
   * `session_close`, whose session is gone.
   */
  readonly rev?: number | undefined;
}

export type Unsubscribe = () => void;

/**
 * Why a call failed. Most of these are [`ErrorCode`] — the editor's own
 * reasons, generated from Rust.
 *
 * The three client codes exist because a failure can happen on this side of
 * the wire, where the editor never sees it and so has no word for it: the
 * backend was closed, a reply arrived that answers nothing, or the replica
 * could not be brought to the revision a reply promised.
 */
export type ClientErrorCode = "closed" | "bad_reply" | "feed";
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

/** One attachment: the name the command declares it under, and its bytes. */
export type Attachment = readonly [name: string, bytes: Uint8Array];

/** What a [`Backend.sendWith`] call answers with. */
export interface OkReplyWithPayload extends OkReply {
  /** Present only when the command's reply carries bytes back. */
  readonly payload?: Uint8Array | undefined;
}

/** Where an editor is, and how it is reached. */
export interface Backend {
  readonly kind: "in-tab" | "connected";

  /**
   * Sends one command that carries no bytes; resolves with its ok reply,
   * rejects [`ProtocolError`].
   *
   * A command listed in [`COMMAND_BYTES`] belongs to [`sendWith`], and passing
   * one here throws before anything reaches the editor — it would otherwise be
   * refused for a missing attachment, one round trip later and further from
   * the mistake.
   */
  send(command: Command): Promise<OkReply>;

  /**
   * Sends one command with the bytes it declares, and hands back whatever
   * bytes its reply carries.
   *
   * The one way bytes reach the editor. `attachments` are `[name, bytes]`
   * pairs, named as [`COMMAND_BYTES`] spells them; how they are framed is the
   * backend's business and differs between the two.
   */
  sendWith(command: Command, attachments: readonly Attachment[]): Promise<OkReplyWithPayload>;

  /**
   * The bytes this tab's store holds at `key`, or `undefined` when there is no
   * copy here to hand out.
   *
   * The outward half of the in-tab store, and the only direction it has: a
   * command that *needs* bytes is handed them. In-tab a save lands in the
   * browser's own storage, and a model that stays there is one nobody can
   * take to another machine, so the bytes come back and become a download.
   * Connected, the save already landed on the machine the person is sitting
   * at and there is nothing to hand out. A missing key in-tab is an error,
   * not `undefined`: a save that reported a key wrote it.
   */
  readFile(key: string): Promise<Uint8Array | undefined>;

  /**
   * Brings `replica` to at least `rev`, resolving with the revision it holds
   * afterwards — which may be newer than asked. Feeds for one session are
   * serialized and coalesced.
   */
  feed(replica: WasmReplica, session: number, rev: number): Promise<number>;

  /** Every event the editor emitted, in order. */
  onEvent(listener: (event: Event) => void): Unsubscribe;

  close(): void;
}

/**
 * Does this command carry bytes? [`COMMAND_BYTES`] is where that is written
 * down, once, for every language.
 */
export function carriesBytes(command: Command): boolean {
  return COMMAND_BYTES[command.cmd] !== undefined;
}

/**
 * Throws when a byte-bearing command was passed to [`Backend.send`].
 *
 * Caught here rather than by the editor: the editor would refuse it for a
 * missing attachment, which is a true answer to the wrong question and arrives
 * a round trip away from the call that made the mistake.
 */
export function refuseIfItCarriesBytes(command: Command): void {
  if (!carriesBytes(command)) return;
  throw new ProtocolError({
    code: "bad_request",
    message: `${command.cmd} carries bytes; send it with sendWith`,
  });
}

/** Reads a JSON reply, turning anything but an `ok` into a [`ProtocolError`]. */
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

/** The parsed form of [`readReply`], for a transport that already parsed. */
export function takeReply(reply: Reply, cmd: string): OkReply {
  if (reply.reply === "err") throw new ProtocolError(reply);
  if (reply.reply !== "ok") {
    // An unsolicited event answered a request: the correlation is broken, and
    // resolving would hand the caller someone else's data.
    throw new ProtocolError({
      code: "bad_reply",
      message: `expected a reply to ${cmd}, got an event`,
    });
  }
  return reply.rev === null || reply.rev === undefined
    ? { body: reply.body }
    : { body: reply.body, rev: reply.rev };
}

/**
 * Narrows a reply body to the one shape a call asked for.
 *
 * An editor that answered something else is a bug rather than a case to
 * handle, but it has to surface as a [`ProtocolError`] like any other failure
 * instead of as an `undefined` read three frames later.
 */
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

/** Whatever was thrown, as the one error type callers branch on. */
export function asProtocolError(cause: unknown, code: FailureCode = "feed"): ProtocolError {
  if (cause instanceof ProtocolError) return cause;
  // wasm-bindgen throws strings, `fetch` throws `TypeError`; both end up here.
  return new ProtocolError({ code, message: String(cause) });
}

/**
 * Serializes feeds per session and coalesces the ones waiting into one.
 *
 * At most two feeds exist for a session at any moment: the one running and the
 * one queued behind it, whose target revision is the highest anybody asked
 * for. Ten edits during one structure fetch are therefore one more fetch, not
 * ten — and because a replica applies forward only, the skipped revisions were
 * never anything a viewer could have seen.
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
      // A failed feed must not strand the ones behind it: the next event is
      // often exactly what recovers from it.
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
    // Cleared once it settles, and only if nothing has taken its place — a
    // queued feed may have started before this handler ran.
    void running
      .catch(() => undefined)
      .then(() => {
        if (this.#active.get(session) === running) this.#active.delete(session);
      });
    return running;
  }
}
