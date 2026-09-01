/**
 * Where document bytes come from and go.
 *
 * This is the seam cloud persistence rides on — the whole of it. The document
 * itself lives in this tab's wasm editor; a server holds saved `.clm` files
 * and a lock, and never sees a command. So "remote" is a `Storage` that talks
 * HTTP, not a second transport, and it is deliberately kept off the command
 * channel: a transport is a request/reply stream of small JSON messages, a
 * store is bytes that want progress and resumability because a rig's textures
 * are most of its size.
 *
 * Running self-contained, a store is OPFS or a file the user picked. Running
 * against a cloud project, it is HTTP — and nothing above this file changes.
 *
 * The wasm editor reads keys synchronously (see the staging note in
 * `catchlight-editor-wasm`), so a caller resolves bytes here first and stages
 * them before naming the key in a command. `openDocument` in `client.ts` is
 * that sequence; nothing else should have to know it.
 */

/** A byte store addressed by opaque keys. */
export interface Storage {
  read(key: string): Promise<Uint8Array>;
  write(key: string, bytes: Uint8Array): Promise<void>;
}

/** A key that is not in the store. Distinguishable from a transport failure. */
export class NotFoundError extends Error {
  readonly key: string;
  constructor(key: string) {
    super(`no stored value for ${JSON.stringify(key)}`);
    this.name = "NotFoundError";
    this.key = key;
  }
}

/** A store held in this tab and nowhere else. The default for a scratch document. */
export class MemoryStorage implements Storage {
  #entries = new Map<string, Uint8Array>();

  read(key: string): Promise<Uint8Array> {
    const bytes = this.#entries.get(key);
    return bytes ? Promise.resolve(bytes) : Promise.reject(new NotFoundError(key));
  }

  write(key: string, bytes: Uint8Array): Promise<void> {
    this.#entries.set(key, bytes);
    return Promise.resolve();
  }

  keys(): string[] {
    return [...this.#entries.keys()].sort();
  }
}

/**
 * Bytes fetched from the page's own origin, read-only.
 *
 * What a bundled demo model loads through, and what a cloud project's
 * signed-URL reads will look like. Writes are refused rather than silently
 * dropped: a caller that thinks it saved and did not is the worst outcome an
 * authoring tool has.
 */
export class FetchStorage implements Storage {
  #base: string;

  constructor(base = "") {
    this.#base = base;
  }

  async read(key: string): Promise<Uint8Array> {
    const response = await fetch(this.#base + key);
    if (response.status === 404) throw new NotFoundError(key);
    if (!response.ok) {
      throw new Error(`fetching ${key}: ${response.status} ${response.statusText}`);
    }
    return new Uint8Array(await response.arrayBuffer());
  }

  write(key: string): Promise<void> {
    return Promise.reject(new Error(`${this.constructor.name} is read-only; cannot write ${key}`));
  }
}
