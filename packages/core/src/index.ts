/** `@catchlight/core`: web-platform glue between a Catchlight editor and a UI. */

export * from "./protocol.gen.js";

export { Editor, fileKey } from "./editor.js";
export type { ExportedModel } from "./editor.js";
export { Session } from "./session.js";
export { MeshDraft, RecordingGesture } from "./authoring.js";
export type {
  MeshDraftView,
  RecordingTarget,
  RecordProperties,
} from "./authoring.js";
export type {
  ScratchTransform,
  SessionCommand,
  SessionEditCommand,
  SessionPresenceCommand,
  SessionReplicaQueryCommand,
  SessionServerQueryCommand,
} from "./session.js";
export { FeedQueue, ProtocolError } from "./backend.js";
export type {
  Backend,
  Attachment,
  ClientErrorCode,
  FailureCode,
  OkReply,
  OkReplyWithPayload,
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
export { READBACK, Viewport, devicePixelSize } from "./viewport.js";
export type {
  TextureRequest,
  WasmEditor,
  WasmFrame,
  WasmGpu,
  WasmModule,
  WasmOwned,
  WasmReplica,
  WasmViewport,
} from "./wasm.js";
