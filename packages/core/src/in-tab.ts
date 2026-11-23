/**
 * The editor inside this tab, behind the same [`Backend`] seam so it can move
 * into a Worker untouched. Events are dispatched before `send` resolves, so a
 * `Session`'s bounded wait never fires here.
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

  /** Takes ownership of `editor`: [`close`] frees it. */
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

    // Dispatch first: a caller about to wait on a revision needs this feed in flight.
    this.#dispatch(editor);

    const reply = readReply(answer.reply, command.cmd);
    if (written) await this.#drainBytes(editor, written, reply);
    return answer.payload === undefined ? reply : { ...reply, payload: answer.payload };
  }

  readFile(key: string): Promise<Uint8Array | undefined> {
    this.#live();
    return this.#storage.read(key);
  }

  /** `rev` is unused: the editor already holds that revision or newer. */
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

  /** Idempotent; every later call rejects as closed. */
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
        // An unreadable event is a memory bug; don't blame this command.
        continue;
      }
      // Copy: a listener may unsubscribe while being told.
      for (const listener of [...this.#listeners]) listener(event);
    }
  }

  /**
   * Drains rather than copies, so the map never holds a second copy of every
   * texture. A `saved` reply names its key, since it may overwrite one.
   */
  async #drainBytes(editor: WasmEditor, before: Set<string>, reply: OkReply): Promise<void> {
    const keys = editor.writtenKeys().filter((key) => !before.has(key));
    const saved = reply.body.result === "saved" ? reply.body.path : undefined;
    if (saved !== undefined && !keys.includes(saved)) keys.push(saved);

    for (const key of keys) {
      const bytes = editor.takeBytes(key);
      if (!bytes) {
        if (key !== saved) continue;
        // A write reporting no bytes would make the caller believe it is on disk.
        throw new ProtocolError({
          code: "bad_reply",
          message: `save reported ${key} but staged no bytes for it`,
        });
      }
      await this.#storage.write(key, bytes);
    }
  }
}

function writesBytes(command: Command): boolean {
  return command.cmd === "session_save" || command.cmd === "manifest_export";
}
