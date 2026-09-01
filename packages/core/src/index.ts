/**
 * `@catchlight/core` — the web-platform glue between a wasm editor and a UI.
 *
 * The layering rule, in one line: if a native iOS or Android editor would have
 * to reimplement it, it does not belong here. See `client.ts`.
 */

export { Editor } from "./client.js";
export type { EditorOptions, SessionInfo } from "./client.js";
export { Session } from "./session.js";
export type { SessionCommand, Unsubscribe } from "./session.js";
export { FetchStorage, MemoryStorage, NotFoundError } from "./storage.js";
export type { Storage } from "./storage.js";
export { LocalTransport, ProtocolError } from "./transport.js";
export type {
  Command,
  ProtocolErrorInfo,
  ResponseBody,
  Transport,
  WasmEditor,
} from "./transport.js";
