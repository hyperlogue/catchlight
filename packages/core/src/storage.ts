/** Where model bytes live in the browser. Keys use `/` to separate segments. */

export interface Storage {
  read(key: string): Promise<Uint8Array>;
  write(key: string, bytes: Uint8Array): Promise<void>;
  /** Every key, sorted. `prefix` filters by string prefix, not by directory. */
  list(prefix?: string): Promise<string[]>;
  /** Removing a missing key is not an error. */
  delete(key: string): Promise<void>;
}

/** Everything before the last `/`, or `""`. */
export function parentKey(key: string): string {
  const at = key.lastIndexOf("/");
  return at < 0 ? "" : key.slice(0, at);
}

export function joinKey(base: string, name: string): string {
  const trimmed = base.replace(/\/+$/, "");
  return trimmed === "" ? name : `${trimmed}/${name}`;
}

export class NotFoundError extends Error {
  readonly key: string;
  constructor(key: string) {
    super(`no stored value for ${JSON.stringify(key)}`);
    this.name = "NotFoundError";
    this.key = key;
  }
}

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
 * The origin private file system. Key segments become directories.
 * [`OpfsStorage.open`] refuses up front on hosts without OPFS.
 */
export class OpfsStorage implements Storage {
  #root: DirectoryHandle;

  private constructor(root: DirectoryHandle) {
    this.#root = root;
  }

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

/** Declared here: some toolchains' `lib.dom` lacks directory iteration. */
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

function opfs(
  nav: unknown,
): { storage?: { getDirectory?(): Promise<DirectoryHandle> } } | undefined {
  return nav as { storage?: { getDirectory?(): Promise<DirectoryHandle> } } | undefined;
}
