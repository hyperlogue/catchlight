/**
 * The editor running inside this tab.
 *
 * `handle` is a synchronous call into wasm, so every promise here resolves
 * without ever yielding to the network. The class exists anyway, and exists
 * behind the same [`Backend`] interface as the connected one, so that nothing
 * above it can be written against "the model is right here" — and so the
 * whole thing can move into a Worker without touching a caller.
 *
 * **Bytes go in with the command and come out of the tab's store.** A command
 * that needs bytes is handed them ([`sendWith`]), so nothing is parked under a
 * key first and the browser's asynchrony stops in the caller, which awaits its
 * own file read and then calls in synchronously.
 *
 * The other direction is the one thing this class still drains. A `save` or an
 * `export_manifest` leaves what it wrote in the editor's own map, and this
 * moves it into the tab's [`Storage`] — draining rather than copying, because
 * a map nothing empties holds a second copy of every texture in the model.
 * `save` names its one key in the reply; `export_manifest` writes a manifest
 * and a file per texture, so what it left is whatever appeared while it ran.
 *
 * **Events are dispatched before `send` resolves.** They come out of the same
 * synchronous `handle` call, and a caller waiting for the revision its reply
 * promised is waiting for the feed that this dispatch starts. Nothing here can
 * lose one either, which is why a `Session`'s bounded wait never fires against
 * this backend: the replica is already at the revision when `send` returns.
 */

import type { Command, Event } from "./protocol.gen.js";
import type {
  Attachment,
  Backend,
  OkReply,
  OkReplyWithPayload,
  Request,
  Unsubscribe,
} from "./backend.js";
import {
  asProtocolError,
  FeedQueue,
  ProtocolError,
  readReply,
  refuseIfItCarriesBytes,
} from "./backend.js";
import type { Storage } from "./storage.js";
import type { WasmEditor, WasmReplica } from "./wasm.js";

export class InTabBackend implements Backend {
  readonly kind = "in-tab";

  #editor: WasmEditor | undefined;
  #storage: Storage;
  #listeners = new Set<(event: Event) => void>();
  #feeds = new FeedQueue();
  #nextId = 1;

  /**
   * Takes ownership of `editor`: [`close`] frees it. `storage` is where the
   * keys commands name are read from and written back to.
   */
  constructor(editor: WasmEditor, storage: Storage) {
    this.#editor = editor;
    this.#storage = storage;
  }

  async send(command: Command): Promise<OkReply> {
    refuseIfItCarriesBytes(command);
    return this.sendWith(command, []);
  }

  async sendWith(
    command: Command,
    attachments: readonly Attachment[],
  ): Promise<OkReplyWithPayload> {
    const editor = this.#live();
    const written = writesBytes(command) ? new Set(editor.writtenKeys()) : undefined;
    const request: Request = { id: this.#nextId++, ...command };

    let answer: { reply: string; payload?: Uint8Array };
    try {
      answer = editor.handleWith(JSON.stringify(request), [...attachments]);
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }

    // Before the reply is read, and before it resolves: an `err` is still an
    // event's cause, and a caller about to wait on a revision needs the feed
    // this starts to be in flight already.
    this.#dispatch(editor);

    const reply = readReply(answer.reply, command.cmd);
    if (written) await this.#drainBytes(editor, written, reply);
    return answer.payload === undefined ? reply : { ...reply, payload: answer.payload };
  }

  /**
   * The bytes the tab's store holds at `key`, or `undefined` when it has
   * none.
   *
   * Read out of the store rather than out of the editor: [`sendWith`] moves
   * what a command wrote into the store before it resolves, so what is there
   * is what was written.
   */
  readFile(key: string): Promise<Uint8Array | undefined> {
    this.#live();
    return this.#storage.read(key);
  }

  /**
   * Hands the editor's own model to the replica.
   *
   * `rev` is not passed on: the event that asked for this came from the editor
   * itself, so its session is already at or past that revision, and what the
   * replica should hold is whatever the editor holds now.
   */
  feed(replica: WasmReplica, session: number, rev: number): Promise<number> {
    return this.#feeds.run(session, rev, () => {
      try {
        return Promise.resolve(replica.syncFromEditor(this.#live(), session));
      } catch (cause) {
        return Promise.reject(asProtocolError(cause));
      }
    });
  }

  onEvent(listener: (event: Event) => void): Unsubscribe {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  /** Frees the wasm editor. Idempotent; every later call rejects as closed. */
  close(): void {
    this.#editor?.free();
    this.#editor = undefined;
    this.#listeners.clear();
  }

  #live(): WasmEditor {
    const editor = this.#editor;
    if (!editor) throw new ProtocolError({ code: "closed", message: "the editor is closed" });
    return editor;
  }

  #dispatch(editor: WasmEditor): void {
    for (const text of editor.drainEvents()) {
      let event: Event;
      try {
        event = JSON.parse(text) as Event;
      } catch {
        // serde wrote it; an unreadable one is a memory bug, and failing the
        // command that happened to drain it would blame the wrong caller.
        continue;
      }
      // Copy: a listener that unsubscribes while being told must not shift the
      // set out from under this loop.
      for (const listener of [...this.#listeners]) listener(event);
    }
  }

  /**
   * Moves what the command wrote into the store, emptying the editor's map as
   * it goes.
   *
   * A `saved` reply names its key outright — it may have overwritten a key
   * that was already there, which no before/after difference would show — and
   * anything else that appeared came from the same command.
   */
  async #drainBytes(editor: WasmEditor, before: Set<string>, reply: OkReply): Promise<void> {
    const keys = editor.writtenKeys().filter((key) => !before.has(key));
    const saved = reply.body.result === "saved" ? reply.body.path : undefined;
    if (saved !== undefined && !keys.includes(saved)) keys.push(saved);

    for (const key of keys) {
      const bytes = editor.takeBytes(key);
      if (!bytes) {
        if (key !== saved) continue;
        // Reporting a write that produced no bytes is the worst outcome an
        // authoring tool has: the caller believes the model is on disk.
        throw new ProtocolError({
          code: "bad_reply",
          message: `save reported ${key} but staged no bytes for it`,
        });
      }
      await this.#storage.write(key, bytes);
    }
  }
}

/** Whether a command leaves bytes in the editor's store for the tab to drain. */
function writesBytes(command: Command): boolean {
  return command.cmd === "save" || command.cmd === "export_manifest";
}
