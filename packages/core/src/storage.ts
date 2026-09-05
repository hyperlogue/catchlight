/**
 * Where document bytes live in the browser.
 *
 * A store is not a transport, and the split is deliberate: a backend carries
 * small JSON messages and answers in milliseconds, a store carries a rig whose
 * textures are most of its size and wants progress, resumability and a quota.
 * So "the file lives somewhere else" is a `Storage`, never a second command
 * channel.
 *
 * Keys are opaque to everything but their reader, which reads one thing out of
 * them: `/` separates segments, so a manifest's texture references resolve
 * relative to the manifest ([`parentKey`], [`joinKey`]).
 *
 * Bytes reach the editor as attachments on the command that uses them, so a
 * store is never in that path: what a caller does is read the key here and
 * hand the bytes over, and nothing above has to know an order.
 */

/** A byte store addressed by opaque keys. */
export interface Storage {
  read(key: string): Promise<Uint8Array>;
  write(key: string, bytes: Uint8Array): Promise<void>;
  /** Every key, sorted. `prefix` filters by string prefix, not by directory. */
  list(prefix?: string): Promise<string[]>;
  /** Removes `key`. Removing one that is not there is not an error. */
  delete(key: string): Promise<void>;
}

/**
 * The key a relative reference inside `key`'s document resolves against —
 * everything before the last `/`, or `""` when the key has no separator.
 */
export function parentKey(key: string): string {
  const at = key.lastIndexOf("/");
  return at < 0 ? "" : key.slice(0, at);
}

/** `name` resolved against `base`, the way a manifest's references resolve. */
export function joinKey(base: string, name: string): string {
  const trimmed = base.replace(/\/+$/, "");
  return trimmed === "" ? name : `${trimmed}/${name}`;
}

/** A key that is not in the store. Distinguishable from a backend failure. */
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

  list(prefix = ""): Promise<string[]> {
    return Promise.resolve([...this.#entries.keys()].filter((k) => k.startsWith(prefix)).sort());
  }

  delete(key: string): Promise<void> {
    this.#entries.delete(key);
    return Promise.resolve();
  }
}

/**
 * The origin private file system: the browser's own disk, per origin.
 *
 * What a self-contained web editor keeps documents in — no picker, no upload,
 * survives a reload, and the only quota that applies is the origin's. A key's
 * `/` segments become directories, which is what makes a manifest and its
 * textures land next to each other the way the editor resolves them.
 *
 * [`OpfsStorage.open`] is the only constructor, because the root handle is a
 * promise and a store that might not be usable yet is a store every caller has
 * to check. Under bun, or any host without the API, it refuses up front rather
 * than failing on the first read.
 */
export class OpfsStorage implements Storage {
  #root: DirectoryHandle;

  private constructor(root: DirectoryHandle) {
    this.#root = root;
  }

  /** Whether this host has OPFS at all. False under bun and in a plain Node. */
  static available(): boolean {
    return typeof navigator !== "undefined" && !!opfs(navigator)?.storage?.getDirectory;
  }

  static async open(): Promise<OpfsStorage> {
    const storage = opfs(globalThis.navigator)?.storage;
    if (!storage?.getDirectory) {
      throw new Error("this host has no origin private file system");
    }
    return new OpfsStorage(await storage.getDirectory());
  }

  async read(key: string): Promise<Uint8Array> {
    const file = await this.#file(key, false);
    if (!file) throw new NotFoundError(key);
    return new Uint8Array(await (await file.getFile()).arrayBuffer());
  }

  async write(key: string, bytes: Uint8Array): Promise<void> {
    const file = await this.#file(key, true);
    if (!file) throw new NotFoundError(key);
    const stream = await file.createWritable();
    try {
      await stream.write(bytes);
    } finally {
      await stream.close();
    }
  }

  async list(prefix = ""): Promise<string[]> {
    const keys: string[] = [];
    await walk(this.#root, "", keys);
    return keys.filter((key) => key.startsWith(prefix)).sort();
  }

  async delete(key: string): Promise<void> {
    const segments = key.split("/");
    const name = segments.pop();
    if (!name) return;
    const dir = await this.#dir(segments, false);
    // Missing is not a failure: delete's promise is "it is gone", and it is.
    if (!dir) return;
    await dir.removeEntry(name).catch(() => undefined);
  }

  async #file(key: string, create: boolean): Promise<FileHandle | undefined> {
    const segments = key.split("/");
    const name = segments.pop();
    if (!name) return undefined;
    const dir = await this.#dir(segments, create);
    if (!dir) return undefined;
    try {
      return await dir.getFileHandle(name, { create });
    } catch {
      return undefined;
    }
  }

  async #dir(segments: string[], create: boolean): Promise<DirectoryHandle | undefined> {
    let dir = this.#root;
    for (const segment of segments) {
      if (!segment || segment === ".") continue;
      try {
        dir = await dir.getDirectoryHandle(segment, { create });
      } catch {
        return undefined;
      }
    }
    return dir;
  }
}

async function walk(dir: DirectoryHandle, prefix: string, out: string[]): Promise<void> {
  for await (const [name, handle] of dir.entries()) {
    const key = prefix ? `${prefix}/${name}` : name;
    if (handle.kind === "directory") await walk(handle, key, out);
    else out.push(key);
  }
}

/**
 * The slice of the file system access API this store calls, declared here
 * rather than taken from `lib.dom` — the directory iteration is newer than the
 * types some toolchains ship, and a fake needs four methods, not the API.
 */
interface DirectoryHandle {
  kind: "directory";
  getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<DirectoryHandle>;
  removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
  entries(): AsyncIterableIterator<[string, DirectoryHandle | FileHandle]>;
}

interface FileHandle {
  kind: "file";
  getFile(): Promise<{ arrayBuffer(): Promise<ArrayBuffer> }>;
  createWritable(): Promise<{ write(data: Uint8Array): Promise<void>; close(): Promise<void> }>;
}

/** `navigator.storage.getDirectory`, without asserting the host has either. */
function opfs(
  nav: unknown,
): { storage?: { getDirectory?(): Promise<DirectoryHandle> } } | undefined {
  return nav as { storage?: { getDirectory?(): Promise<DirectoryHandle> } } | undefined;
}
