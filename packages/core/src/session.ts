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
 * **A drag is not a revision.** The presence path (`scratch_deform`,
 * `presence_set`) deliberately does not bump `rev`, so a gesture of any length
 * re-renders nothing and costs no undo entries. One command on release moves
 * `rev` and the panels catch up. That split is what keeps drags smooth, and it
 * is why `send` and `sendQuiet` are different methods rather than one method
 * with a flag nobody would remember to pass.
 *
 * **Two channels, because they run at different rates.** `subscribe` is the
 * document's revision, and React reads it — once per commit. `onInvalidate` is
 * "the picture changed", and the viewport reads it — possibly on every pointer
 * move, because a presence command redraws without being a revision. Every
 * command fires the second, queries included: telling a renderer to repaint
 * something that did not change costs one coalesced frame, and failing to tell
 * it leaves a stale viewport with no other symptom.
 */

import type { Command, ResponseBody, SessionId } from "./protocol.gen.js";
import type { Transport } from "./transport.js";

/** `Omit` that distributes over a union instead of collapsing it. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/**
 * A command aimed at this session: any generated [`Command`] arm that carries
 * a `session`, minus that field. Filling it in is the session's own job, so a
 * caller cannot address the wrong document by accident, and a command that
 * names no session (`session_new`, `session_list`) will not typecheck here —
 * it belongs on the `Editor`.
 */
export type SessionCommand = DistributiveOmit<Extract<Command, { session: SessionId }>, "session">;

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
   * subscribers re-read.
   */
  async send(command: SessionCommand): Promise<ResponseBody> {
    const body = await this.#transport.send(this.#address(command));
    this.#bump();
    return body;
  }

  /** Tells every viewport on this session that the picture changed. */
  invalidate(): void {
    for (const listener of [...this.#redraw]) listener();
  }

  /**
   * Runs a command that does not change the document — a query, or anything on
   * the presence path. Subscribers are not notified, so a drag never reaches
   * React.
   */
  async sendQuiet(command: SessionCommand): Promise<ResponseBody> {
    const body = await this.#transport.send(this.#address(command));
    this.invalidate();
    return body;
  }

  #address(command: SessionCommand): Command {
    // Putting `session` back on an arm that `SessionCommand` took it off of
    // reconstitutes that arm, but TypeScript cannot see a spread that way: it
    // widens the union rather than tracking which arm it came from. The type
    // of `SessionCommand` is what makes this sound.
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
