/**
 * The editor running in a local process, reached over HTTP and a WebSocket.
 *
 * The socket carries the protocol verbatim — one JSON object per text frame,
 * a `Request` up, a `Reply` down — so the same messages the Unix socket and
 * the CLI speak are what a tab sends. Correlation is by the request's `id`,
 * because replies may arrive out of order and events arrive unbidden between
 * them.
 *
 * **Bulk goes over HTTP, never the socket.** A structure is megabytes and a
 * texture more; a frame that large blocks every command behind it, and a
 * `fetch` gets range requests, caching and a progress bar for free. So the
 * socket is small JSON only, and [`feed`] is three plain GETs.
 *
 * **The revision a feed applies is the one the response header names.** The
 * editor may have moved on between the event and the GET, and the bytes are
 * what they are; trusting the asked-for revision would file newer state under
 * an older number and the next feed would be skipped as backwards.
 *
 * **A dead socket fails everything.** Every pending request rejects with
 * `closed` the moment the socket does, rather than hanging until a caller's
 * own timeout — an editor that cannot be reached is not a slow editor.
 *
 * **The server keeps the socket warm, and nothing here does.** It pings an
 * idle connection and hangs up on one that has stayed silent for two of its
 * intervals. The platform's `WebSocket` answers a ping on its own, underneath
 * [`SocketLike`], which is why this class schedules no heartbeat and why
 * [`SocketLike`] models no control frames.
 *
 * `fetch` and the socket constructor are injected so the whole class can be
 * tested against fakes, with no network and no DOM.
 */

import type { Command, Event, Reply } from "./protocol.gen.js";
import type { Backend, OkReply, Request, Unsubscribe } from "./backend.js";
import { asProtocolError, FeedQueue, ProtocolError, takeReply } from "./backend.js";
import type { TextureRequest, WasmReplica } from "./wasm.js";

/** The `fetch` surface this backend calls. The platform's own satisfies it. */
export type FetchLike = (url: string, init?: FetchInit) => Promise<HttpResponse>;

export interface FetchInit {
  method?: string;
  headers?: Record<string, string>;
  body?: Uint8Array;
}

export interface HttpResponse {
  ok: boolean;
  status: number;
  statusText: string;
  headers: { get(name: string): string | null };
  arrayBuffer(): Promise<ArrayBuffer>;
  json(): Promise<unknown>;
}

/**
 * The WebSocket surface, as callbacks rather than listeners: a fake is then a
 * dozen lines, and this backend never needs more than one handler each.
 */
export interface SocketLike {
  send(data: string): void;
  close(): void;
  onopen: ((event: unknown) => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
}

/** Opens one socket. A factory rather than a class, so the platform's own
 *  `WebSocket` and a fake are named the same way. */
export type SocketFactory = (url: string) => SocketLike;

export interface ConnectOptions {
  fetch?: FetchLike;
  socket?: SocketFactory;
}

/** A request that has gone out and not been answered. */
interface Pending {
  cmd: string;
  resolve(reply: OkReply): void;
  reject(error: ProtocolError): void;
}

export class ConnectedBackend implements Backend {
  readonly kind = "connected";

  #base: string;
  #token: string;
  #fetch: FetchLike;
  #socket: SocketLike;
  #pending = new Map<number, Pending>();
  #listeners = new Set<(event: Event) => void>();
  #feeds = new FeedQueue();
  #nextId = 1;
  #closed = false;

  private constructor(base: string, token: string, doFetch: FetchLike, socket: SocketLike) {
    this.#base = base;
    this.#token = token;
    this.#fetch = doFetch;
    this.#socket = socket;
    socket.onmessage = (event) => this.#receive(String(event.data));
    socket.onclose = () => this.#fail("the editor connection closed");
    socket.onerror = () => this.#fail("the editor connection failed");
  }

  /**
   * Asks `baseUrl` for a token, then opens the socket with it.
   *
   * The token request is a plain `fetch`: whatever authorizes this tab —
   * a cookie, an origin allowance — is what CORS already carries, and a
   * WebSocket cannot send an `Authorization` header, so the query parameter is
   * the only place a socket can be given one.
   */
  static async connect(baseUrl: string, options?: ConnectOptions): Promise<ConnectedBackend> {
    const base = baseUrl.replace(/\/+$/, "");
    const doFetch = options?.fetch ?? defaultFetch;
    const open = options?.socket ?? defaultSocket;

    const response = await doFetch(`${base}/token`).catch((cause: unknown) => {
      throw asProtocolError(cause, "closed");
    });
    if (!response.ok) {
      throw new ProtocolError({
        code: "closed",
        message: `asking ${base} for a token: ${response.status} ${response.statusText}`,
      });
    }
    const token = (await response.json()) as { token?: unknown };
    if (typeof token.token !== "string") {
      throw new ProtocolError({ code: "closed", message: `${base} answered no token` });
    }

    const url = `${wsBase(base)}/ws?token=${encodeURIComponent(token.token)}`;
    const socket = await openSocket(open, url);
    return new ConnectedBackend(base, token.token, doFetch, socket);
  }

  send(command: Command): Promise<OkReply> {
    if (this.#closed) {
      return Promise.reject(
        new ProtocolError({ code: "closed", message: "the editor connection is closed" }),
      );
    }
    const request: Request = { id: this.#nextId++, ...command };
    return new Promise<OkReply>((resolve, reject) => {
      this.#pending.set(request.id, { cmd: command.cmd, resolve, reject });
      try {
        this.#socket.send(JSON.stringify(request));
      } catch (cause) {
        this.#pending.delete(request.id);
        reject(asProtocolError(cause, "closed"));
      }
    });
  }

  /** Puts bytes into the editor's own store, under `key`. */
  async putBytes(key: string, bytes: Uint8Array): Promise<void> {
    await this.#http(`/files/${encodeURIComponent(key)}`, "io", {
      method: "PUT",
      body: bytes,
    });
  }

  /**
   * Nothing: a connected editor reads its own store, which is where a key
   * already points. The staging in-tab callers need has no counterpart here.
   */
  stageKey(_key: string): Promise<void> {
    return Promise.resolve();
  }

  /**
   * Nothing, for the same reason: there is no staging map to empty, and the
   * key names a file in the editor's store that a caller did not ask to
   * delete.
   */
  discardKey(_key: string): Promise<void> {
    return Promise.resolve();
  }

  feed(replica: WasmReplica, session: number, rev: number): Promise<number> {
    return this.#feeds.run(session, rev, () => this.#fetchInto(replica, session));
  }

  onEvent(listener: (event: Event) => void): Unsubscribe {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  close(): void {
    if (this.#closed) return;
    try {
      this.#socket.close();
    } finally {
      this.#fail("the editor connection was closed");
      this.#listeners.clear();
    }
  }

  async #fetchInto(replica: WasmReplica, session: number): Promise<number> {
    const response = await this.#http(`/sessions/${session}/structure`, "feed");
    const header = response.headers.get("X-Catchlight-Rev");
    const at = header === null || header.trim() === "" ? NaN : Number(header);
    if (!Number.isFinite(at)) {
      // Filing newer bytes under an older number makes the next feed look
      // backwards, and a replica applies forward only: it would never load.
      throw new ProtocolError({
        code: "feed",
        message: `the structure of session ${session} named no revision`,
      });
    }
    const structure = new Uint8Array(await response.arrayBuffer());

    let needed: TextureRequest[];
    try {
      needed = JSON.parse(replica.texturesNeeded(structure)) as TextureRequest[];
    } catch (cause) {
      throw asProtocolError(cause);
    }
    for (const texture of needed) {
      const payload = await this.#http(
        `/sessions/${session}/textures/${encodeURIComponent(texture.id)}`,
        "feed",
      );
      replica.putTexture(texture.id, new Uint8Array(await payload.arrayBuffer()));
    }

    try {
      // Forward only: an older structure than the replica holds is ignored,
      // which is what makes an out-of-order feed harmless rather than a
      // rollback.
      replica.applyStructure(structure, at);
    } catch (cause) {
      throw asProtocolError(cause);
    }
    return replica.rev();
  }

  async #http(path: string, code: "feed" | "io", init?: FetchInit): Promise<HttpResponse> {
    const headers = { ...init?.headers, Authorization: `Bearer ${this.#token}` };
    let response: HttpResponse;
    try {
      response = await this.#fetch(`${this.#base}${path}`, { ...init, headers });
    } catch (cause) {
      throw asProtocolError(cause, code);
    }
    if (!response.ok) {
      throw new ProtocolError({
        code,
        message: `${path}: ${response.status} ${response.statusText}`,
      });
    }
    return response;
  }

  #receive(text: string): void {
    let reply: Reply;
    try {
      reply = JSON.parse(text) as Reply;
    } catch {
      // A frame that is not a reply answers no request and moves no replica;
      // there is nobody to reject and nothing to apply.
      return;
    }

    if (reply.reply === "ok" || reply.reply === "err") {
      const pending = this.#pending.get(reply.id);
      if (!pending) return;
      this.#pending.delete(reply.id);
      try {
        pending.resolve(takeReply(reply, pending.cmd));
      } catch (cause) {
        pending.reject(asProtocolError(cause, "bad_reply"));
      }
      return;
    }

    const event = asEvent(reply);
    for (const listener of [...this.#listeners]) listener(event);
  }

  /** Rejects everything in flight. What a closed socket does to its requests. */
  #fail(message: string): void {
    this.#closed = true;
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) {
      request.reject(new ProtocolError({ code: "closed", message }));
    }
  }
}

/**
 * An event frame as the event itself. The `reply` tag is how a socket frame
 * says which of the three it is; in-tab events arrive without it, and a
 * listener must not have to know which backend it is behind.
 */
function asEvent(frame: Reply): Event {
  const { reply: _tag, ...event } = frame as { reply: string } & Record<string, unknown>;
  return event as unknown as Event;
}

function wsBase(base: string): string {
  if (base.startsWith("https:")) return `wss:${base.slice("https:".length)}`;
  if (base.startsWith("http:")) return `ws:${base.slice("http:".length)}`;
  return base;
}

function openSocket(open: SocketFactory, url: string): Promise<SocketLike> {
  return new Promise<SocketLike>((resolve, reject) => {
    let socket: SocketLike;
    try {
      socket = open(url);
    } catch (cause) {
      reject(asProtocolError(cause, "closed"));
      return;
    }
    const settle = (error?: string): void => {
      socket.onopen = null;
      socket.onerror = null;
      socket.onclose = null;
      if (error) reject(new ProtocolError({ code: "closed", message: error }));
      else resolve(socket);
    };
    socket.onopen = () => settle();
    socket.onerror = () => settle(`could not open ${url}`);
    socket.onclose = () => settle(`${url} closed before it opened`);
  });
}

/** The platform's `fetch`, bound — an unbound one throws in a browser. */
const defaultFetch: FetchLike = (url, init) =>
  globalThis.fetch(url, init as RequestInit) as unknown as Promise<HttpResponse>;

/**
 * The platform's `WebSocket`. The cast is the one place a real event type is
 * adapted to the callback shapes above; a `MessageEvent` is not assignable to
 * `{ data: unknown }` in the contravariant position, and widening the
 * interface to `any` to say so would lose more than it buys.
 */
const defaultSocket: SocketFactory = (url) => new WebSocket(url) as unknown as SocketLike;
