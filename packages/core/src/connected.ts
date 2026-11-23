/**
 * The editor in a local process, over a WebSocket for JSON and HTTP for bulk.
 * The server pings idle sockets and the platform answers, so this schedules
 * no heartbeat and `SocketLike` models no control frames.
 */

import type { Command, Event, Reply } from "./protocol.gen.js";
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
  refuseIfItCarriesBytes,
  takeReply,
} from "./backend.js";
import type { ExtensionRequest, TextureRequest, WasmReplica } from "./wasm.js";

export type FetchLike = (url: string, init?: FetchInit) => Promise<HttpResponse>;

export interface FetchInit {
  method?: string;
  headers?: Record<string, string>;
  body?: Uint8Array | FormData;
}

export interface HttpResponse {
  ok: boolean;
  status: number;
  statusText: string;
  headers: { get(name: string): string | null };
  arrayBuffer(): Promise<ArrayBuffer>;
  json(): Promise<unknown>;
}

export interface SocketLike {
  send(data: string): void;
  close(): void;
  onopen: ((event: unknown) => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
}

export type SocketFactory = (url: string) => SocketLike;

export interface ConnectOptions {
  fetch?: FetchLike;
  socket?: SocketFactory;
  /** Skips `GET /token`, which a launcher that hands over the token does not serve. */
  token?: string;
}

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
   * A WebSocket cannot send an `Authorization` header, so the token goes in a
   * query parameter.
   */
  static async connect(baseUrl: string, options?: ConnectOptions): Promise<ConnectedBackend> {
    const base = baseUrl.replace(/\/+$/, "");
    const doFetch = options?.fetch ?? defaultFetch;
    const open = options?.socket ?? defaultSocket;

    if (options?.token !== undefined) {
      const url = `${wsBase(base)}/ws?token=${encodeURIComponent(options.token)}`;
      return new ConnectedBackend(base, options.token, doFetch, await openSocket(open, url));
    }
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
    // Rejection, not throw, so a caller has one way to catch this call.
    try {
      refuseIfItCarriesBytes(command);
    } catch (cause) {
      return Promise.reject(asProtocolError(cause, "bad_request"));
    }
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

  /**
   * The socket refuses byte-bearing commands. A byte reply comes back as the
   * response body, with the reply itself in `X-Catchlight-Reply`.
   */
  async sendWith(
    command: Command,
    attachments: readonly Attachment[],
  ): Promise<OkReplyWithPayload> {
    if (this.#closed) {
      throw new ProtocolError({ code: "closed", message: "the editor connection is closed" });
    }
    const request: Request = { id: this.#nextId++, ...command };
    const form = new FormData();
    form.append("request", JSON.stringify(request));
    for (const [name, bytes] of attachments) {
      // A Blob: `FormData` would encode a `.clm` as text.
      form.append(name, new Blob([bytes as BlobPart]));
    }

    const response = await this.#http("/request", "io", { method: "POST", body: form });
    const inHeader = response.headers.get("X-Catchlight-Reply");
    const bytes = new Uint8Array(await response.arrayBuffer());
    const text = inHeader ?? new TextDecoder().decode(bytes);

    let parsed: Reply;
    try {
      parsed = JSON.parse(text) as Reply;
    } catch (cause) {
      throw new ProtocolError({
        code: "bad_reply",
        message: `could not read the reply to ${command.cmd}: ${String(cause)}`,
      });
    }
    const reply = takeReply(parsed, command.cmd);
    if (inHeader === null) return reply;
    return { ...reply, payload: bytes };
  }

  /** `undefined`: the save landed in the editor's own store, which has no read route. */
  readFile(_key: string): Promise<Uint8Array | undefined> {
    return Promise.resolve(undefined);
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
      // Use the header's revision: filing newer bytes under an older number
      // makes the next feed look backwards, and a replica applies forward only.
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

    // A texture is immutable under its Id; an extension's bytes change under its key.
    let extensions: ExtensionRequest[];
    try {
      extensions = JSON.parse(replica.extensionsNeeded(structure)) as ExtensionRequest[];
    } catch (cause) {
      throw asProtocolError(cause);
    }
    for (const extension of extensions) {
      const payload = await this.#http(
        `/sessions/${session}/extensions/${encodeURIComponent(extension.key)}`,
        "feed",
      );
      replica.putExtension(extension.key, new Uint8Array(await payload.arrayBuffer()));
    }

    try {
      // Forward only, so an out-of-order feed is harmless rather than a rollback.
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

  #fail(message: string): void {
    this.#closed = true;
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) {
      request.reject(new ProtocolError({ code: "closed", message }));
    }
  }
}

/** Strips the socket's `reply` tag so listeners match in-tab events. */
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

/** Bound: an unbound `fetch` throws in a browser. */
const defaultFetch: FetchLike = (url, init) =>
  globalThis.fetch(url, init as RequestInit) as unknown as Promise<HttpResponse>;

/**
 * A `MessageEvent` is not assignable to `{ data: unknown }` in the
 * contravariant position, hence the cast.
 */
const defaultSocket: SocketFactory = (url) => new WebSocket(url) as unknown as SocketLike;
