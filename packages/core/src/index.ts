/**
 * `@catchlight/core` — the web-platform glue between a catchlight editor and a
 * UI.
 *
 * The layering rule, in one line: if a native iOS or Android editor would have
 * to reimplement it, it does not belong here. See `editor.ts`.
 *
 * The shape of the whole package: an [`Editor`] over a [`Backend`], which is
 * either the wasm editor in this tab ([`InTabBackend`]) or a local editor
 * process ([`ConnectedBackend`]). Each open document is a [`Session`] holding
 * a replica — this tab's copy of the model, which answers reads synchronously,
 * is what a [`Viewport`] draws, and moves only when the backend feeds it.
 *
 * The wire types are re-exported whole from `protocol.gen.ts`, which
 * `cargo xtask ts` writes from the Rust enums. A consumer builds commands and
 * reads replies against the same declarations the editor compiles.
 */

export * from "./protocol.gen.js";

export { Editor, fileKey } from "./editor.js";
export { Session } from "./session.js";
export type {
  ScratchTransform,
  SessionCommand,
  SessionDocumentCommand,
  SessionPresenceCommand,
  SessionReplicaQueryCommand,
  SessionServerQueryCommand,
} from "./session.js";
export { FeedQueue, ProtocolError } from "./backend.js";
export type {
  Backend,
  ClientErrorCode,
  FailureCode,
  OkReply,
  ProtocolErrorInfo,
  Request,
  Unsubscribe,
} from "./backend.js";
export { InTabBackend } from "./in-tab.js";
export { ConnectedBackend } from "./connected.js";
export type {
  ConnectOptions,
  FetchInit,
  FetchLike,
  HttpResponse,
  SocketFactory,
  SocketLike,
} from "./connected.js";
export { MemoryStorage, NotFoundError, OpfsStorage } from "./storage.js";
export type { Storage } from "./storage.js";
export { Viewport, devicePixelSize } from "./viewport.js";
export type {
  TextureRequest,
  WasmEditor,
  WasmGpu,
  WasmModule,
  WasmReplica,
  WasmViewport,
} from "./wasm.js";
