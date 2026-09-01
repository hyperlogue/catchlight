/**
 * `@catchlight/core` — the web-platform glue between a wasm editor and a UI.
 *
 * The layering rule, in one line: if a native iOS or Android editor would have
 * to reimplement it, it does not belong here. See `client.ts`.
 *
 * The wire types are re-exported whole from `protocol.gen.ts`, which
 * `cargo xtask ts` writes from the Rust enums. A consumer builds commands and
 * reads replies against the same declarations the server compiles.
 */

export type * from "./protocol.gen.js";

export { Editor } from "./client.js";
export type { EditorOptions } from "./client.js";
export { Session } from "./session.js";
export type { SessionCommand, Unsubscribe } from "./session.js";
export { FetchStorage, MemoryStorage, NotFoundError } from "./storage.js";
export type { Storage } from "./storage.js";
export { LocalTransport, ProtocolError } from "./transport.js";
export type {
  ClientErrorCode,
  FailureCode,
  ProtocolErrorInfo,
  Request,
  Transport,
  WasmEditor,
} from "./transport.js";
