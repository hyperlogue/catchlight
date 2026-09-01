/**
 * One open document, and the only thing React subscribes to.
 *
 * **There is no mirror of the model here.** Panels pull a snapshot when the
 * session's revision moves, through `subscribe` / `getRevision` — the pair
 * `useSyncExternalStore` wants. The model already has a generation clock and
 * the server already has `rev`; keeping a second copy in JavaScript would be a
 * second source of truth that can disagree with what the renderer draws, and
 * every new command would need a patch handler written twice.
 *
 * Whether that stays true is this package's business alone. Moving to a
 * mirrored store later changes `snapshot` and nothing above it — which is why
 * nothing above it may reach past this file to the transport.
 *
 * **One method per kind of command, and the type picks it.** A command either
 * changes the document, changes only what is drawn, or changes nothing; the
 * three are different enough that a caller must not be able to confuse them,
 * and remembering which calls are "quiet" is exactly the thing a caller
 * forgets. So the split is not a convention here — it is generated from Rust
 * (`CommandKind` in `catchlight-editor-protocol`, checked against the enum by
 * `cargo xtask ts` and against the dispatch by a debug assertion in the
 * editor server). Passing `scratch_deform` to `send` does not typecheck.
 *
 * **A drag is not a revision.** [`sendPresence`] deliberately does not bump
 * `rev`, so a gesture of any length re-renders nothing and costs no undo
 * entries. One [`send`] on release moves `rev` and the panels catch up. That
 * split is what keeps drags smooth.
 *
 * **Two channels, because they run at different rates.** `subscribe` is the
 * document's revision, and React reads it — once per commit. `onInvalidate` is
 * "the picture changed", and the viewport reads it — possibly on every pointer
 * move, because a presence command redraws without being a revision.
 *
 * [`send`]: Session#send
 * [`sendPresence`]: Session#sendPresence
 */

import type {
  Command,
  DocumentCommand,
  PresenceCommand,
  QueryCommand,
  ResponseBody,
  SessionId,
} from "./protocol.gen.js";
import type { Transport } from "./transport.js";

/** `Omit` that distributes over a union instead of collapsing it. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/**
 * The arms of `T` that address a session, with the `session` field removed.
 *
 * Filling it in is the session's own job, so a caller cannot address the wrong
 * document by accident — and a command that names no session (`session_new`,
 * `session_list`) drops out of the union entirely, because it belongs on the
 * `Editor` rather than here.
 */
type OnSession<T> = DistributiveOmit<Extract<T, { session: SessionId }>, "session">;

/** A command that changes this session's document. Goes to [`Session#send`]. */
export type SessionDocumentCommand = OnSession<DocumentCommand>;
/** A command on the presence path. Goes to [`Session#sendPresence`]. */
export type SessionPresenceCommand = OnSession<PresenceCommand>;
/** A command that only reads. Goes to [`Session#query`]. */
export type SessionQueryCommand = OnSession<QueryCommand>;

/** Any command aimed at a session, whatever it does. */
export type SessionCommand =
  | SessionDocumentCommand
  | SessionPresenceCommand
  | SessionQueryCommand;

export type Unsubscribe = () => void;

/** A document the editor has open. */
export class Session {
  readonly id: number;
  #transport: Transport;
  #revision = 0;
  #listeners = new Set<() => void>();
  #redraw = new Set<() => void>();

  constructor(transport: Transport, id: number) {
    this.#transport = transport;
    this.id = id;
  }

  /**
   * Runs a command that changes the document, then bumps the revision so
   * subscribers re-read and repaints every viewport on this session.
   */
  async send(command: SessionDocumentCommand): Promise<ResponseBody> {
    const body = await this.#transport.send(this.#address(command));
    this.#bump();
    return body;
  }

  /**
   * Runs a command on the presence path: the picture changes, the document
   * does not. Viewports repaint; React is never told.
   *
   * This is what a drag runs, once per pointer move.
   */
  async sendPresence(command: SessionPresenceCommand): Promise<ResponseBody> {
    const body = await this.#transport.send(this.#address(command));
    this.invalidate();
    return body;
  }

  /**
   * Runs a read. Nothing is notified, because by construction nothing changed
   * — the server asserts as much in debug builds.
   */
  async query(command: SessionQueryCommand): Promise<ResponseBody> {
    return this.#transport.send(this.#address(command));
  }

  /** Tells every viewport on this session that the picture changed. */
  invalidate(): void {
    for (const listener of [...this.#redraw]) listener();
  }

  #address(command: SessionCommand): Command {
    // Putting `session` back on an arm that `OnSession` took it off of
    // reconstitutes that arm, but TypeScript cannot see a spread that way: it
    // widens the union rather than tracking which arm it came from. The types
    // above are what make this sound.
    return { ...command, session: this.id } as Command;
  }

  /**
   * The value `useSyncExternalStore` compares between renders. A number, so
   * comparison is identity and no snapshot has to be memoized to stay stable.
   */
  getRevision = (): number => this.#revision;

  /** Registers `listener`, called after every revision bump. */
  subscribe = (listener: () => void): Unsubscribe => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** Registers `listener`, called whenever the picture may have changed. */
  onInvalidate = (listener: () => void): Unsubscribe => {
    this.#redraw.add(listener);
    return () => {
      this.#redraw.delete(listener);
    };
  };

  #bump(): void {
    this.#revision += 1;
    this.invalidate();
    // Copy first: a listener that unsubscribes during notification must not
    // shift the set out from under this loop.
    for (const listener of [...this.#listeners]) listener();
  }
}
