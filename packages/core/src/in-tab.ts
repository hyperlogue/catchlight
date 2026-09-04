/**
 * The editor running inside this tab.
 *
 * `handle` is a synchronous call into wasm, so every promise here resolves
 * without ever yielding to the network. The class exists anyway, and exists
 * behind the same [`Backend`] interface as the connected one, so that nothing
 * above it can be written against "the document is right here" — and so the
 * whole thing can move into a Worker without touching a caller.
 *
 * **Two things it does that a caller must not have to think about.**
 *
 * A command that names a storage key gets that key staged first. The editor
 * reads keys synchronously and the browser produces bytes asynchronously, so
 * the staging map is where the asynchrony stops; putting the read here means
 * `openDocument` is one command and not a three-step ritual anybody could get
 * out of order. A key that is staged already is left alone, which is what lets
 * a caller stage bytes it holds ([`Backend.putBytes`]) and then open them
 * under a key the store has never heard of.
 *
 * A command that writes bytes gets them drained into the store. Draining
 * rather than copying is the point: a staging map that is never emptied holds
 * a second copy of every texture in the model. `save` names its one key in the
 * reply; `export_manifest` writes a manifest and a file per texture, so what
 * it staged is whatever appeared while it ran.
 *
 * The mirror of that holds for the keys a command *reads*: staging is where
 * the asynchrony stops, not a cache. A `session_open` decodes the `.clm` into
 * a model that owns its own copy, so leaving the bytes staged keeps a second
 * whole document — textures and all — alive for the life of the tab. So a
 * command that read a key into a document discards it once the reply is good;
 * a query that merely read one (`manifest_requirements`) does not, because the
 * command it is asked ahead of is about to want the same bytes.
 *
 * **Events are dispatched before `send` resolves.** They come out of the same
 * synchronous `handle` call, and a caller waiting for the revision its reply
 * promised is waiting for the feed that this dispatch starts. Nothing here can
 * lose one either, which is why a `Session`'s bounded wait never fires against
 * this backend: the replica is already at the revision when `send` returns.
 */

import type { Command, Event } from "./protocol.gen.js";
import type { Backend, OkReply, Request, Unsubscribe } from "./backend.js";
import { asProtocolError, FeedQueue, ProtocolError, readReply } from "./backend.js";
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
    const key = readsKey(command);
    if (key !== undefined) await this.stageKey(key);

    const editor = this.#live();
    const staged = writesBytes(command) ? new Set(editor.stagedKeys()) : undefined;
    const request: Request = { id: this.#nextId++, ...command };

    let text: string;
    try {
      text = editor.handle(JSON.stringify(request));
    } catch (cause) {
      throw asProtocolError(cause, "bad_reply");
    }

    // Before the reply is read, and before it resolves: an `err` is still an
    // event's cause, and a caller about to wait on a revision needs the feed
    // this starts to be in flight already.
    this.#dispatch(editor);

    const reply = readReply(text, command.cmd);
    if (staged) await this.#drainBytes(editor, staged, reply);
    // After the reply is read, so a command that failed keeps its bytes: the
    // caller is entitled to retry it without staging them a second time.
    if (key !== undefined && consumesKey(command)) editor.takeBytes(key);
    return reply;
  }

  putBytes(key: string, bytes: Uint8Array): Promise<void> {
    this.#live().putBytes(key, bytes);
    return Promise.resolve();
  }

  async stageKey(key: string): Promise<void> {
    if (this.#live().stagedKeys().includes(key)) return;
    const bytes = await this.#storage.read(key);
    this.#live().putBytes(key, bytes);
  }

  /** Drops what is staged under `key`. Nothing staged is not an error. */
  discardKey(key: string): Promise<void> {
    this.#live().takeBytes(key);
    return Promise.resolve();
  }

  /**
   * Reads `key` out of the store — never out of staging, which `send` drains
   * into the store before it resolves, so what is there is what was written.
   */
  readBytes(key: string): Promise<Uint8Array | undefined> {
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
   * Moves what the command staged into the store, emptying staging as it goes.
   *
   * A `saved` reply names its key outright — it may have overwritten a key
   * that was already staged, which no before/after difference would show — and
   * anything else that appeared came from the same command.
   */
  async #drainBytes(editor: WasmEditor, before: Set<string>, reply: OkReply): Promise<void> {
    const keys = editor.stagedKeys().filter((key) => !before.has(key));
    const saved = reply.body.result === "saved" ? reply.body.path : undefined;
    if (saved !== undefined && !keys.includes(saved)) keys.push(saved);

    for (const key of keys) {
      const bytes = editor.takeBytes(key);
      if (!bytes) {
        if (key !== saved) continue;
        // Reporting a write that produced no bytes is the worst outcome an
        // authoring tool has: the caller believes the document is on disk.
        throw new ProtocolError({
          code: "bad_reply",
          message: `save reported ${key} but staged no bytes for it`,
        });
      }
      await this.#storage.write(key, bytes);
    }
  }
}

/** The storage key a command reads, if it names one directly. */
function readsKey(command: Command): string | undefined {
  switch (command.cmd) {
    case "session_open":
      return command.path;
    // `texture_add` names a key or attaches its bytes; only the first stages.
    case "texture_add":
      return command.path ?? undefined;
    case "session_import":
    case "manifest_requirements":
      return command.manifest_path;
    default:
      return undefined;
  }
}

/**
 * Whether a command reads its key *into* the document, so that what is staged
 * under it is a second copy the moment the command returns.
 *
 * `manifest_requirements` reads a manifest and answers a question about it,
 * which is why it is not here: `importManifest` asks it and then imports the
 * same path, and discarding in between would be a trip back to storage for
 * bytes that were already in hand.
 */
function consumesKey(command: Command): boolean {
  return (
    command.cmd === "session_open" ||
    command.cmd === "session_import" ||
    command.cmd === "texture_add"
  );
}

/** Whether a command puts bytes into the editor's store rather than reading them. */
function writesBytes(command: Command): boolean {
  return command.cmd === "save" || command.cmd === "export_manifest";
}
