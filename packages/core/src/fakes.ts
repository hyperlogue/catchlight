/** Hand-written stand-ins for the wasm module and the network, for tests. */

import type {
  BindingInfo,
  ClipInfo,
  EditOp,
  BindingTarget,
  ControlMappingInfo,
  ExpressionInfo,
  ExtensionInfo,
  ExtensionRef,
  ExtensionWarningInfo,
  StandardControlInfo,
  Command,
  Event,
  Interpolate,
  NodeInfo,
  ParamInfo,
  Presence,
  Review,
  ReviewThread,
  ResponseBody,
  SessionInfo,
  TexInfo,
  TreeNode,
  VersionInfo,
  VersionViewing,
} from "./protocol.gen.js";
import type {
  Attachment,
  Backend,
  OkReply,
  OkReplyWithPayload,
  Request,
  Unsubscribe,
} from "./backend.js";
import { FeedQueue, ProtocolError } from "./backend.js";
import type { FetchInit, HttpResponse, SocketLike } from "./connected.js";
import type { Playing, LiveInputs } from "./session.js";
import type {
  Needs,
  WasmEditor,
  WasmGpu,
  WasmModule,
  WasmReplica,
  WasmViewport,
} from "./wasm.js";

/** What a fake replica's `scratchTransform` was last given, as plain arrays. */
export interface ScratchFields {
  translate: number[] | undefined;
  rotate: number[] | undefined;
  scale: number[] | undefined;
  zOrder: number | undefined;
  opacity: number | undefined;
}

export interface FakeDoc {
  rev: number;
  title: string;
  file: string | null;
  root: TreeNode;
  params: ParamInfo[];
  textures: TexInfo[];
  /** Albedo per part; a missing entry draws nothing. */
  albedo: Record<string, string>;
  bindings: FakeBinding[];
  /** A byte extension holds only the marker a feed compares. */
  extensions: FakeExtension[];
  /** `catchlight.humanoid`'s mappings, in control name order. */
  humanoid: ControlMappingInfo[];
  /** `catchlight.expressions`' two groups, in author order. */
  expressions: ExpressionInfo[];
  visemes: ExpressionInfo[];
  /** What `extension_warnings` answers; tests set it. */
  extensionWarnings: ExtensionWarningInfo[];
  /** `catchlight.animations`' clips, as `clip_list` answers; tests set them. */
  clips: ClipInfo[];
  /** Keys of official extensions this build cannot read; their reads and edits answer `extension_unread`. */
  unread: string[];
}

/** A few of the standard controls, enough to tell the two kinds apart. */
export const FAKE_CONTROLS: StandardControlInfo[] = [
  { name: "head_yaw", one_sided: false },
  { name: "mouth_smile", one_sided: false },
  { name: "mouth_open", one_sided: true },
];
export const FAKE_EMOTIONS = ["happy", "angry", "sad", "relaxed", "surprised"];
export const FAKE_VISEMES = ["aa", "ee", "oh"];

export type FakeExtension =
  | { key: string; kind: "json"; value: unknown }
  | { key: string; kind: "bytes"; hash: string; size?: number }
  | { key: string; kind: "maintained"; version: number; refs: ExtensionRef[]; value: unknown };

function extensionInfo(e: FakeExtension): ExtensionInfo {
  switch (e.kind) {
    case "json":
      return { key: e.key, value: { kind: "json", value: e.value } };
    case "bytes":
      return { key: e.key, value: { kind: "bytes", size: e.size ?? 0, hash: e.hash } };
    case "maintained":
      return { key: e.key, value: { kind: "maintained", version: e.version, refs: e.refs, value: e.value } };
  }
}

/** Only the cells a command authored, sparse like the model's own. */
export interface FakeBinding {
  node: string;
  target: BindingTarget;
  param: string;
  param_y?: string | null;
  interpolate: Interpolate;
  key_positions: number[][];
  /** `value: null` is authored with no scalar of its own, as a deform cell is. */
  cells: { x: number; y: number; value: number | null }[];
}

function setLive(values: Record<string, number>, name: string, value: number | undefined): void {
  if (value === undefined) delete values[name];
  else values[name] = value;
}

export function emptyDoc(title: string): FakeDoc {
  return {
    rev: 0,
    title,
    file: null,
    root: {
      id: "root",
      name: "root",
      kind: "group",
      z_order: 0,
      enabled: true,
      children: [],
    },
    params: [],
    textures: [],
    albedo: {},
    bindings: [],
    extensions: [],
    humanoid: [],
    expressions: [],
    visemes: [],
    extensionWarnings: [],
    clips: [],
    unread: [],
  };
}

export function structureBytes(doc: FakeDoc): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(doc));
}

export function readStructure(bytes: Uint8Array): FakeDoc {
  return JSON.parse(new TextDecoder().decode(bytes)) as FakeDoc;
}

export class FakeEditor implements WasmEditor {
  docs = new Map<number, FakeDoc>();
  written = new Map<string, Uint8Array>();
  requests: Request[] = [];
  attached: Array<Record<string, Uint8Array>> = [];
  /** Commands to refuse instead of running, keyed by `cmd`. */
  refuse = new Map<string, { code: string; message: string }>();
  presence = new Map<number, Presence>();
  /** Tests may set `agents` directly; polls, fetches, acks and claims move it. */
  reviews = new Map<number, Review>();
  reviewImages = new Map<number, Uint8Array>();
  /** What every review says of how long it is kept. */
  reviewKept: Review["kept"] = "stored";
  /** Each session's versions, with the doc each one saved. */
  versions = new Map<number, { info: VersionInfo; doc: FakeDoc }[]>();
  versionsKept: Review["kept"] = "stored";
  /** Read-only sessions `version_open` made. */
  viewing = new Map<number, VersionViewing>();
  /** Advanced by tests; each version records it as its time. */
  now = 1_700_000_000_000;
  freed = false;

  #events: string[] = [];
  #nextSession = 1;
  #nextNode = 1;
  #nextReviewId = 1;
  #payload: Uint8Array | undefined;

  handleWith(
    requestJson: string,
    attachments: Attachment[],
  ): { reply: string; payload?: Uint8Array } {
    this.attached.push(Object.fromEntries(attachments));
    this.#payload = undefined;
    const reply = this.handle(requestJson);
    const payload = this.#payload;
    this.#payload = undefined;
    return payload ? { reply, payload } : { reply };
  }

  handle(requestJson: string): string {
    const request = JSON.parse(requestJson) as Request;
    this.requests.push(request);
    const id = request.id;
    const refusal = this.refuse.get(request.cmd);
    if (refusal) return JSON.stringify({ reply: "err", id, ...refusal });
    const unread = officialKey(request.cmd);
    if (unread && "session" in request && this.docs.get(request.session as number)?.unread.includes(unread))
      return this.#err(id, "extension_unread", `extension "${unread}" cannot be read by this editor: a newer version`);
    const viewed = "session" in request ? this.viewing.get(request.session as number) : undefined;
    if (viewed && READ_ONLY_REFUSES.has(request.cmd))
      return JSON.stringify({
        reply: "err",
        id,
        code: "read_only",
        message: `this session shows version ${viewed.version} of session ${viewed.session} and takes no edits`,
      });

    switch (request.cmd) {
      case "session_get": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        return this.#ok(
          id,
          {
            result: "status",
            status: {
              title: doc.title,
              node_count: 1 + doc.root.children.length,
              param_count: doc.params.length,
              texture_count: doc.textures.length,
              dirty: false,
              rev: doc.rev,
              undo_steps: Math.max(0, doc.rev - 1),
              redo_steps: 0,
            },
          },
          doc.rev,
        );
      }
      case "session_create":
        return this.#opened(id, request.name ?? "untitled", null, !!request.source);
      case "session_open": {
        if (!this.written.has(request.path)) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "io",
            message: `${JSON.stringify(request.path)} is not in the store`,
          });
        }
        return this.#opened(id, request.path, request.path);
      }
      case "session_list": {
        const sessions: SessionInfo[] = [...this.docs].map(
          ([session, doc]) => ({
            session,
            title: doc.title,
            file: doc.file,
            dirty: false,
            rev: doc.rev,
            node_count: 1 + doc.root.children.length,
            ...(this.viewing.has(session) ? { viewing: this.viewing.get(session)! } : {}),
          }),
        );
        return this.#ok(id, { result: "sessions", sessions, instance: "fake" });
      }
      case "node_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const node = `${request.parent}/${request.kind}-${this.#nextNode++}`;
        doc.root.children.push({
          id: node,
          name: request.name ?? node,
          kind: request.kind,
          z_order: 0,
          enabled: true,
          children: [],
        });
        doc.rev += 1;
        this.#emit({
          event: "model_changed",
          session: request.session,
          rev: doc.rev,
        });
        return this.#ok(id, { result: "node", node }, doc.rev);
      }
      // Albedo states: absent is unchanged, null is "draw none", an Id draws.
      case "node_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.texture === null) delete doc.albedo[request.node];
        else if (request.texture !== undefined)
          doc.albedo[request.node] = request.texture;
        doc.rev += 1;
        this.#emit({
          event: "model_changed",
          session: request.session,
          rev: doc.rev,
        });
        return this.#ok(id, { result: "node", node: request.node }, doc.rev);
      }
      case "texture_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const texture = `tex-${doc.textures.length + 1}`;
        doc.textures.push({ id: texture, width: 4, height: 4, encoding: "png", alpha: "straight", sha256: "0".repeat(64) });
        doc.albedo[request.node] = texture;
        doc.rev += 1;
        this.#emit({
          event: "model_changed",
          session: request.session,
          rev: doc.rev,
        });
        return this.#ok(
          id,
          { result: "texture", texture, dropped: [] },
          doc.rev,
        );
      }
      case "param_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const param = `param-${doc.params.length + 1}`;
        doc.params.push({
          id: param,
          name: request.name,
          min: request.min,
          max: request.max,
          default: request.default,
          initial_positions: [0, 1],
          bindings: 0,
        });
        doc.rev += 1;
        this.#emit({
          event: "model_changed",
          session: request.session,
          rev: doc.rev,
        });
        return this.#ok(id, { result: "param", param }, doc.rev);
      }
      case "param_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        doc.params = doc.params.filter((p) => p.id !== request.param);
        doc.bindings = doc.bindings.filter(
          (b) => b.param !== request.param && b.param_y !== request.param,
        );
        return this.#changed(id, request.session, doc);
      }
      case "extension_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.key.startsWith("catchlight.")) return this.#reserved(id, request.key);
        const value = request.value;
        const bytes = this.attached.at(-1)?.value;
        let stored: FakeExtension;
        if (value.kind === "json") stored = { key: request.key, kind: "json", value: value.value };
        else if (value.kind === "maintained")
          stored = { key: request.key, kind: "maintained", version: value.version, refs: value.refs ?? [], value: value.value };
        else stored = { key: request.key, kind: "bytes", hash: new TextDecoder().decode(bytes), size: bytes?.length ?? 0 };
        doc.extensions = [...doc.extensions.filter((e) => e.key !== request.key), stored].sort((a, b) =>
          a.key < b.key ? -1 : a.key > b.key ? 1 : 0,
        );
        return this.#changed(id, request.session, doc);
      }
      case "extension_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.key.startsWith("catchlight.")) return this.#reserved(id, request.key);
        if (!doc.extensions.some((e) => e.key === request.key))
          return JSON.stringify({ reply: "err", id, code: "no_extension", message: `no extension "${request.key}"` });
        doc.extensions = doc.extensions.filter((e) => e.key !== request.key);
        return this.#changed(id, request.session, doc);
      }
      case "humanoid_map": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const param = doc.params.find((p) => p.id === request.param);
        if (!param) return this.#err(id, "no_param", `no param ${request.param}`);
        const standard = FAKE_CONTROLS.find((c) => c.name === request.control);
        if (!standard && !request.nonstandard)
          return this.#err(id, "nonstandard_control", `${JSON.stringify(request.control)} is not a standard humanoid control`);
        const stops: [number, number, number] =
          request.stops ??
          (standard?.one_sided
            ? [param.default, param.default, param.max]
            : [param.min, param.default, param.max]);
        const mapping = { control: request.control, param: request.param, stops };
        const replaced = doc.humanoid.find((m) => m.control === request.control) ?? null;
        doc.humanoid = [...doc.humanoid.filter((m) => m.control !== request.control), mapping].sort(
          (a, b) => (a.control < b.control ? -1 : a.control > b.control ? 1 : 0),
        );
        return this.#changed(id, request.session, doc, { result: "humanoid_mapped", mapping, replaced });
      }
      case "humanoid_unmap": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const mapping = doc.humanoid.find((m) => m.control === request.control);
        if (!mapping) return this.#err(id, "edit", `no control ${JSON.stringify(request.control)} is mapped`);
        doc.humanoid = doc.humanoid.filter((m) => m !== mapping);
        return this.#changed(id, request.session, doc, { result: "humanoid_unmapped", mapping });
      }
      case "expression_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (!request.name || request.settings.length === 0)
          return this.#err(id, "bad_request", "an expression needs a name and at least one setting");
        const missing = request.settings.find((s) => s.param != null && !doc.params.some((p) => p.id === s.param));
        if (missing) return this.#err(id, "no_param", `no param ${missing.param}`);
        const named = request.settings.find(
          (s) => s.control != null && !request.nonstandard && !FAKE_CONTROLS.some((c) => c.name === s.control),
        );
        if (named)
          return this.#err(id, "nonstandard_control", `${JSON.stringify(named.control)} is not a standard humanoid control`);
        const group = request.group === "viseme" ? "visemes" : "expressions";
        const expression = { name: request.name, settings: request.settings };
        const at = doc[group].findIndex((e) => e.name === request.name);
        const replaced = at < 0 ? null : doc[group][at]!;
        if (at < 0) doc[group].push(expression);
        else doc[group][at] = expression;
        return this.#changed(id, request.session, doc, {
          result: "expression",
          group: request.group,
          expression,
          replaced,
        });
      }
      case "expression_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const group = request.group === "viseme" ? "visemes" : "expressions";
        const expression = doc[group].find((e) => e.name === request.name);
        if (!expression) return this.#err(id, "edit", `there is no ${request.group} ${JSON.stringify(request.name)}`);
        doc[group] = doc[group].filter((e) => e !== expression);
        return this.#changed(id, request.session, doc, { result: "expression_deleted", group: request.group, expression });
      }
      case "viseme_add_standard": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const report = { added: [] as string[], replaced: [] as string[], skipped: [] as string[] };
        for (const name of FAKE_VISEMES) {
          const at = doc.visemes.findIndex((v) => v.name === name);
          const viseme = { name, settings: [{ control: "mouth_open", value: 0.5 }] };
          if (at < 0) {
            report.added.push(name);
            doc.visemes.push(viseme);
          } else if (request.replace) {
            report.replaced.push(name);
            doc.visemes[at] = viseme;
          } else report.skipped.push(name);
        }
        const body = { result: "visemes_added" as const, ...report };
        return report.added.length + report.replaced.length === 0
          ? this.#ok(id, body, doc.rev)
          : this.#changed(id, request.session, doc, body);
      }
      case "binding_add": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        binding(doc, request);
        return this.#changed(id, request.session, doc);
      }
      case "binding_cells_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        applyEdit(doc, { ...request, op: "binding_cells_set" });
        return this.#changed(id, request.session, doc);
      }
      case "edit_apply": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.if_rev !== doc.rev) return JSON.stringify({ reply: "err", id, code: "revision_conflict", message: "The model changed.", rev: doc.rev });
        const next = structuredClone(doc);
        for (const edit of request.edits) applyEdit(next, edit);
        this.docs.set(request.session, next);
        return this.#changed(id, request.session, next);
      }
      case "binding_cells_unset":
      case "binding_key_insert":
      case "binding_key_delete":
      case "binding_key_move": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        if (request.cmd === "binding_key_insert") applyEdit(doc, { ...request, op: "binding_key_insert" });
        else if (request.cmd === "binding_key_delete") applyEdit(doc, { ...request, op: "binding_key_delete" });
        else if (request.cmd === "binding_key_move") applyEdit(doc, { ...request, op: "binding_key_move" });
        else applyEdit(doc, { ...request, op: "binding_cells_unset" });
        return this.#changed(id, request.session, doc);
      }
      case "binding_interpolation_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        binding(doc, request).interpolate = request.mode;
        return this.#changed(id, request.session, doc);
      }
      case "binding_delete": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        doc.bindings = doc.bindings.filter((b) => !addresses(b, request));
        return this.#changed(id, request.session, doc);
      }
      case "session_save": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const key = request.path ?? doc.file ?? "untitled.clm";
        this.written.set(key, structureBytes(doc));
        doc.file = key;
        return this.#ok(id, { result: "saved", path: key }, doc.rev);
      }
      case "manifest_export": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        this.written.set(request.path, new TextEncoder().encode("{}"));
        this.written.set("tex0.png", new TextEncoder().encode("a texture"));
        return this.#ok(id, { result: "saved", path: request.path }, doc.rev);
      }
      case "session_close": {
        if (!this.docs.delete(request.session))
          return this.#noSession(id, request.session);
        this.presence.delete(request.session);
        this.viewing.delete(request.session);
        for (const [view, of] of [...this.viewing])
          if (of.session === request.session) {
            this.viewing.delete(view);
            this.docs.delete(view);
          }
        this.#emit({ event: "sessions_changed" });
        return this.#ok(id, { result: "empty" });
      }
      case "presence_set": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const { cmd: _cmd, id: _id, session, ...presence } = request;
        this.presence.set(session, presence);
        return this.#ok(id, { result: "empty" }, doc.rev);
      }
      case "presence_get": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const presence = this.presence.get(request.session) ?? null;
        return this.#ok(id, { result: "presence", presence }, doc.rev);
      }
      case "review_get":
      case "review_image_add":
      case "review_image_get":
      case "review_thread_add":
      case "review_thread_set":
      case "review_thread_delete":
      case "review_comment_add":
      case "review_comment_delete":
      case "review_submit":
      case "review_poll":
      case "review_fetch":
      case "review_ack":
      case "review_claim":
      case "review_release": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const body = this.#review(request, doc.rev);
        if (!body)
          return JSON.stringify({ reply: "err", id, code: "no_thread", message: "no such thread" });
        if (!["review_get", "review_poll", "review_fetch", "review_image_add", "review_image_get"].includes(request.cmd))
          this.#emit({ event: "review_changed", session: request.session });
        return this.#ok(id, body, doc.rev);
      }
      case "version_save": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const list = this.versions.get(request.session) ?? [];
        this.versions.set(request.session, list);
        const info: VersionInfo = {
          version: list.length + 1,
          author: request.author,
          time: this.now,
          rev: doc.rev,
          byte_length: structureBytes(doc).length,
          sha256: "",
          ...(request.label?.trim() ? { label: request.label.trim() } : {}),
          ...(request.summary?.trim() ? { summary: request.summary.trim() } : {}),
        };
        list.push({ info, doc: readStructure(structureBytes(doc)) });
        this.#emit({ event: "versions_changed", session: request.session });
        return this.#ok(id, { result: "version", version: info }, doc.rev);
      }
      case "version_list": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const versions = (this.versions.get(request.session) ?? []).map((v) => structuredClone(v.info));
        return this.#ok(id, { result: "versions", versions, kept: this.versionsKept }, doc.rev);
      }
      case "version_get":
      case "version_restore":
      case "version_open": {
        const doc = this.docs.get(request.session);
        if (!doc) return this.#noSession(id, request.session);
        const saved = this.versions.get(request.session)?.[request.version - 1];
        if (!saved)
          return JSON.stringify({ reply: "err", id, code: "no_version", message: `no version ${request.version}` });
        if (request.cmd === "version_get") {
          this.#payload = structureBytes(saved.doc);
          return this.#ok(id, { result: "version", version: structuredClone(saved.info) }, doc.rev);
        }
        if (request.cmd === "version_restore") {
          const restored = readStructure(structureBytes(saved.doc));
          doc.root = restored.root;
          doc.params = restored.params;
          return this.#changed(id, request.session, doc, { result: "version", version: structuredClone(saved.info) });
        }
        const viewing = { session: request.session, version: request.version };
        for (const [view, of] of this.viewing)
          if (of.session === viewing.session && of.version === viewing.version)
            return this.#ok(id, { result: "session", session: view }, 0);
        const view = this.#nextSession++;
        const copy = readStructure(structureBytes(saved.doc));
        copy.rev = 0;
        copy.title = `${doc.title} v${request.version}`;
        copy.file = null;
        this.docs.set(view, copy);
        this.viewing.set(view, viewing);
        this.#emit({ event: "sessions_changed" });
        return this.#ok(id, { result: "session", session: view }, 0);
      }
      default: {
        const session =
          "session" in request ? (request.session as number) : undefined;
        return this.#ok(
          id,
          { result: "empty" },
          session === undefined ? undefined : this.docs.get(session)?.rev,
        );
      }
    }
  }

  #changed(id: number, session: number, doc: FakeDoc, body: ResponseBody = { result: "empty" }): string {
    doc.rev += 1;
    this.#emit({ event: "model_changed", session, rev: doc.rev });
    return this.#ok(id, body, doc.rev);
  }

  #err(id: number, code: string, message: string): string {
    return JSON.stringify({ reply: "err", id, code, message });
  }

  #reserved(id: number, key: string): string {
    return JSON.stringify({
      reply: "err",
      id,
      code: "reserved_extension",
      message: `${key}: \`catchlight.\` is the format's own prefix`,
    });
  }

  snapshot(session: number): FakeDoc {
    const doc = this.docs.get(session);
    if (!doc) throw `no session ${session}`;
    return readStructure(structureBytes(doc));
  }

  drainEvents(): string[] {
    const events = this.#events;
    this.#events = [];
    return events;
  }

  takeBytes(key: string): Uint8Array | undefined {
    const bytes = this.written.get(key);
    this.written.delete(key);
    return bytes;
  }

  writtenKeys(): string[] {
    return [...this.written.keys()].sort();
  }

  free(): void {
    this.freed = true;
  }

  [Symbol.dispose](): void {
    this.free();
  }

  #opened(id: number, title: string, file: string | null, imported = false): string {
    const session = this.#nextSession++;
    const doc = emptyDoc(title);
    doc.file = file;
    if (imported) doc.root.children.push({ id: "imported", name: "imported", kind: "part", z_order: 0, enabled: true, children: [] });
    this.docs.set(session, doc);
    this.#emit({ event: "sessions_changed" });
    return this.#ok(id, { result: "session", session }, doc.rev);
  }

  #review(
    request: Extract<Request, { cmd: `review_${string}` }>,
    rev: number,
  ): ResponseBody | undefined {
    let review = this.reviews.get(request.session);
    if (!review) this.reviews.set(request.session, (review = { threads: [], agents: [], kept: "stored" }));
    const find = (thread: number): ReviewThread | undefined =>
      review.threads.find((t) => t.thread === thread);
    const sent = (t: ReviewThread): boolean =>
      t.status === "open" && t.comments.some((c) => c.delivery === "sent");
    const agent = "agent" in request ? request.agent : undefined;
    const waiting = (t: ReviewThread): boolean =>
      sent(t) && (!t.holder || t.holder.id === agent?.id);
    /** `listening` undefined keeps what the agent's own polls last said. */
    const hear = (listening: boolean | undefined): void => {
      if (!agent) return;
      const agents = review.agents;
      const was = agents.find((a) => a.id === agent.id);
      review.agents = [
        ...agents.filter((a) => a.id !== agent.id),
        { ...agent, listening: listening ?? was?.listening ?? false },
      ];
    };
    const claim = (ids: number[] | undefined) =>
      (ids ?? []).map((image) => ({
        image,
        media_type: "image/png",
        byte_length: this.reviewImages.get(image)?.length ?? 0,
      }));
    switch (request.cmd) {
      case "review_get":
        return { result: "review", review: { ...structuredClone(review), kept: this.reviewKept } };
      case "review_image_add": {
        const bytes = this.attached[this.attached.length - 1]?.image ?? new Uint8Array();
        const image = this.#nextReviewId++;
        this.reviewImages.set(image, bytes);
        return {
          result: "review_image",
          image: { image, media_type: "image/png", byte_length: bytes.length },
        };
      }
      case "review_image_get": {
        const bytes = this.reviewImages.get(request.image);
        if (!bytes) return undefined;
        this.#payload = bytes;
        return {
          result: "review_image",
          image: { image: request.image, media_type: "image/png", byte_length: bytes.length },
        };
      }
      case "review_thread_add": {
        const thread: ReviewThread = {
          thread: this.#nextReviewId++,
          target: request.target,
          target_missing: false,
          status: "open",
          pose: request.pose ?? [],
          comments: [{
            comment: this.#nextReviewId++,
            author: request.author,
            body: request.body,
            delivery: request.author.role === "human" ? "unsent" : "delivered",
            rev,
            images: claim(request.images),
            ...(request.version != null ? { version: request.version } : {}),
          }],
        };
        review.threads.push(thread);
        return { result: "review_thread", thread: structuredClone(thread) };
      }
      case "review_comment_add": {
        const thread = find(request.thread);
        if (!thread) return undefined;
        thread.comments.push({
          comment: this.#nextReviewId++,
          author: request.author,
          body: request.body,
          delivery: request.author.role === "human" ? "unsent" : "delivered",
          rev,
          images: claim(request.images),
          ...(request.version != null ? { version: request.version } : {}),
        });
        if (request.author.role === "human") thread.status = "open";
        else if (thread.holder?.id === request.author.id) delete thread.holder;
        return { result: "review_thread", thread: structuredClone(thread) };
      }
      case "review_thread_set": {
        const thread = find(request.thread);
        if (!thread) return undefined;
        thread.status = request.status;
        if (request.status === "resolved") delete thread.holder;
        return { result: "review_thread", thread: structuredClone(thread) };
      }
      case "review_thread_delete": {
        if (!find(request.thread)) return undefined;
        review.threads = review.threads.filter((t) => t.thread !== request.thread);
        return { result: "empty" };
      }
      case "review_comment_delete": {
        const thread = find(request.thread);
        if (!thread) return undefined;
        thread.comments = thread.comments.filter((c) => c.comment !== request.comment);
        if (thread.comments.length === 0)
          review.threads = review.threads.filter((t) => t !== thread);
        return { result: "empty" };
      }
      case "review_submit": {
        for (const thread of review.threads)
          if (thread.status === "open")
            for (const comment of thread.comments)
              if (comment.delivery === "unsent") comment.delivery = "sent";
        return { result: "empty" };
      }
      case "review_poll":
        hear(true);
        return { result: "review_pending", pending: review.threads.filter(waiting).length };
      case "review_fetch": {
        hear(undefined);
        const taken = review.threads.filter(waiting);
        const batch = taken.flatMap((t) => t.comments.filter((c) => c.delivery === "sent").map((c) => c.comment));
        return {
          result: "review_fetched",
          threads: structuredClone(taken),
          ...(batch.length ? { batch: batch.join(".") } : {}),
        };
      }
      case "review_ack": {
        const ids = request.batch.split(".").map(Number);
        const owner = (id: number) =>
          review.threads.find((t) => t.status === "open" && t.comments.some((c) => c.comment === id && c.delivery === "sent"));
        if (!ids.every(owner)) return undefined;
        const claimed: number[] = [];
        const held: { thread: number; holder: { id: string; name: string } }[] = [];
        for (const thread of review.threads) {
          const mine = thread.comments.filter((c) => ids.includes(c.comment));
          if (mine.length === 0) continue;
          for (const comment of mine) comment.delivery = "delivered";
          if (thread.holder && thread.holder.id !== request.agent.id)
            held.push({ thread: thread.thread, holder: thread.holder });
          else {
            thread.holder = request.agent;
            claimed.push(thread.thread);
          }
        }
        hear(false);
        return { result: "review_acked", claimed, held };
      }
      case "review_claim": {
        for (const id of request.threads) if (!find(id)) return undefined;
        for (const id of request.threads) find(id)!.holder = request.agent;
        hear(undefined);
        return { result: "review_claimed", threads: request.threads, lease_seconds: 3600 };
      }
      case "review_release": {
        for (const id of request.threads) {
          const thread = find(id);
          if (thread?.holder?.id === request.agent.id) delete thread.holder;
        }
        hear(undefined);
        return { result: "empty" };
      }
    }
  }

  #noSession(id: number, session: number): string {
    return JSON.stringify({
      reply: "err",
      id,
      code: "no_session",
      message: `no session ${session}`,
    });
  }

  #ok(id: number, body: ResponseBody, rev?: number): string {
    return JSON.stringify(
      rev === undefined
        ? { reply: "ok", id, body }
        : { reply: "ok", id, rev, body },
    );
  }

  #emit(event: Event): void {
    this.#events.push(JSON.stringify(event));
  }
}

/** What a fake read-only session refuses; the real editor refuses every edit. */
function officialKey(cmd: string): string | undefined {
  if (cmd.startsWith("humanoid_")) return "catchlight.humanoid";
  if (cmd.startsWith("expression_") || cmd === "viseme_add_standard") return "catchlight.expressions";
  if (cmd === "clip_list") return "catchlight.animations";
  return undefined;
}

const READ_ONLY_REFUSES = new Set([
  "node_add", "node_set", "node_delete", "param_add", "param_set", "param_delete", "texture_add",
  "edit_apply", "session_save", "binding_add", "review_thread_add", "review_comment_add", "version_save",
  "extension_set", "extension_delete", "humanoid_map", "humanoid_unmap", "expression_set",
  "expression_delete", "viseme_add_standard",
]);

/** Reports a small limit so the clamp is testable. */
export class FakeGpu implements WasmGpu {
  freed = false;
  /** One per editor, however many canvases ask. */
  acquires = 0;
  maxSize(): number {
    return 4096;
  }
  free(): void {
    this.freed = true;
  }
  [Symbol.dispose](): void {
    this.free();
  }
  tierName = "webgpu";
  tier(): string {
    return this.tierName;
  }
  acquiredFrom: HTMLCanvasElement[] = [];
}

export class FakeReplica implements WasmReplica {
  editing = false;
  paused = false;
  setEditing(editing: boolean): void {
    this.editing = editing;
    if (editing) this.clip = undefined;
  }
  setMotionPaused(paused: boolean): void {
    this.paused = paused;
  }
  motionPaused(): boolean {
    return this.paused;
  }
  motionRunning(): boolean {
    return !this.editing && !this.paused;
  }
  recordingPairs(_node: string, _param: string): string {
    return "[]";
  }
  textureImage(texture: string) {
    return this.textureThumbnail(texture);
  }
  meshDraft(_node: string): never {
    throw new Error("The fake does not implement mesh geometry.");
  }
  recording(
    _node: string,
    _param: string,
    _paramY: string | undefined,
    _x: number,
    _y: number,
  ): never {
    throw new Error("The fake does not evaluate recording keys.");
  }
  doc: FakeDoc | undefined;
  applied: Array<{ rev: number; textures: string[] }> = [];
  syncs: number[] = [];
  held = new Set<string>();
  heldExtensions = new Map<string, string>();
  pose = new Map<string, number>();
  scratchDeforms = new Map<string, Float32Array>();
  scratchTransforms = new Map<string, ScratchFields>();
  translations = new Map<string, [number, number, number]>();
  box: [number, number, number, number] | undefined;
  freed = false;
  live: LiveInputs = { controls: {}, expressions: {}, visemes: {} };
  /** Stands still: the fake never advances a clip. */
  clip: Playing | undefined;

  #rev = 0;

  rev(): number {
    return this.#rev;
  }

  needs(structure: Uint8Array): string {
    const doc = readStructure(structure);
    const needs: Needs = {
      textures: doc.textures
        .filter((texture) => !this.held.has(texture.id))
        .map((texture) => ({
          id: texture.id,
          encoding: "png",
          alpha: "straight",
        })),
      extensions: doc.extensions
        .filter(
          (e) => e.kind === "bytes" && this.heldExtensions.get(e.key) !== e.hash,
        )
        .map((e) => ({ key: e.key, hash: e.kind === "bytes" ? e.hash : "" })),
    };
    return JSON.stringify(needs);
  }

  putTexture(id: string): void {
    this.held.add(id);
  }

  putExtension(key: string, bytes: Uint8Array): void {
    this.heldExtensions.set(key, new TextDecoder().decode(bytes));
  }

  applyStructure(structure: Uint8Array, rev: number): boolean {
    const doc = readStructure(structure);
    const missing = doc.textures.filter(
      (texture) => !this.held.has(texture.id),
    );
    if (missing.length > 0)
      throw `missing textures: ${missing.map((t) => t.id).join(", ")}`;
    const unfetched = doc.extensions.filter(
      (e) => e.kind === "bytes" && this.heldExtensions.get(e.key) !== e.hash,
    );
    if (unfetched.length > 0) {
      throw `missing extensions: ${unfetched.map((e) => e.key).join(", ")}`;
    }
    if (this.doc && rev <= this.#rev) return false;
    this.doc = doc;
    this.#rev = rev;
    this.applied.push({ rev, textures: [...this.held] });
    return true;
  }

  syncFromEditor(editor: WasmEditor, session: number): number {
    const doc = (editor as FakeEditor).snapshot(session);
    this.syncs.push(session);
    if (this.doc && doc.rev <= this.#rev) return this.#rev;
    this.doc = doc;
    this.#rev = doc.rev;
    return this.#rev;
  }

  query(requestJson: string): string {
    const request = JSON.parse(requestJson) as Request;
    const id = request.id;
    const doc = this.doc;
    if (!doc) {
      return JSON.stringify({
        reply: "err",
        id,
        code: "no_session",
        message: "nothing fed yet",
      });
    }
    const unread = officialKey(request.cmd);
    if (unread && doc.unread.includes(unread))
      return JSON.stringify({
        reply: "err",
        id,
        code: "extension_unread",
        message: `extension "${unread}" cannot be read by this editor: a newer version`,
      });
    switch (request.cmd) {
      case "slot_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "slots", node: request.node, slots: [] },
        });
      case "node_tree_get":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "tree", root: doc.root },
        });
      case "param_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "params", params: doc.params },
        });
      case "humanoid_get":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "humanoid", mappings: doc.humanoid, controls: FAKE_CONTROLS },
        });
      case "expression_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: {
            result: "expressions",
            expressions: doc.expressions,
            visemes: doc.visemes,
            emotion_names: FAKE_EMOTIONS,
            viseme_names: FAKE_VISEMES,
          },
        });
      case "clip_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "clips", clips: doc.clips },
        });
      case "extension_warnings":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "extension_warnings", warnings: doc.extensionWarnings },
        });
      case "extension_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "extensions", extensions: doc.extensions.map(extensionInfo) },
        });
      case "texture_list":
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "textures", textures: doc.textures },
        });
      case "binding_cells_get": {
        const b = doc.bindings.find((b) => addresses(b, request));
        if (!b) throw new Error("The fake binding is missing.");
        const info = bindingInfo(doc, b);
        const cells = request.cells.map((cell) => {
          const authored = b.cells.find((c) => c.x === cell[0] && c.y === cell[1]);
          const nearest = b.cells.reduce<typeof authored>((best, candidate) => !best || Math.abs(candidate.x - cell[0]) + Math.abs(candidate.y - cell[1]) < Math.abs(best.x - cell[0]) + Math.abs(best.y - cell[1]) ? candidate : best, undefined);
          return { cell, authored: !!authored, value: authored ? { scalar: authored.value } : null,
            ...(request.include_derived && !authored ? { derived: { scalar: nearest?.value ?? 0 } } : {}) };
        });
        return JSON.stringify({ reply: "ok", id, rev: this.#rev, body: { result: "binding_cells", node: request.node, param: b.param, param_y: b.param_y, target: b.target, width: info.width, height: info.height, interpolate: b.interpolate, cells } });
      }
      case "binding_list": {
        if (!holds(doc.root, request.node)) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "no_node",
            message: `no node ${request.node}`,
          });
        }
        const bindings = doc.bindings
          .filter((b) => b.node === request.node)
          .map((b) => bindingInfo(doc, b));
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "bindings", bindings },
        });
      }
      case "node_get": {
        const node = nodeInfo(doc, doc.root, request.node);
        if (!node) {
          return JSON.stringify({
            reply: "err",
            id,
            code: "no_node",
            message: `no node ${request.node}`,
          });
        }
        return JSON.stringify({
          reply: "ok",
          id,
          rev: this.#rev,
          body: { result: "node_info", node },
        });
      }
      default:
        return JSON.stringify({
          reply: "err",
          id,
          code: "bad_request",
          message: `${request.cmd} is not a replica query`,
        });
    }
  }

  setParam(id: string, value: number): boolean {
    this.pose.set(id, value);
    for (const mapping of this.doc?.humanoid ?? []) {
      if (mapping.param === id) delete this.live.controls[mapping.control];
    }
    return true;
  }

  setControl(name: string, value: number | undefined): void {
    setLive(this.live.controls, name, value);
  }

  setExpressionWeight(name: string, weight: number | undefined): void {
    setLive(this.live.expressions, name, weight);
  }

  setVisemeWeight(name: string, weight: number | undefined): void {
    setLive(this.live.visemes, name, weight);
  }

  clearInputs(): void {
    this.live = { controls: {}, expressions: {}, visemes: {} };
  }

  inputs(): string {
    return JSON.stringify(this.live);
  }

  /** Samples heard since the voice started, and their rate. */
  heard: { sampleRate: number; samples: number } | undefined;
  /** What `voice()` reports while there is a voice. */
  speaking = { level: 0, visemes: {} as Record<string, number> };

  pushVoice(samples: Float32Array, sampleRate: number): void {
    const before = this.heard?.sampleRate === sampleRate ? this.heard.samples : 0;
    this.heard = { sampleRate, samples: before + samples.length };
  }

  voice(): string | undefined {
    return this.heard && JSON.stringify(this.speaking);
  }

  clearVoice(): void {
    this.heard = undefined;
  }

  play(clip: string): boolean {
    if (!this.doc?.clips.some((c) => c.name === clip)) return false;
    this.clip = { name: clip, time: 0, frame: 0 };
    return true;
  }

  stop(): void {
    this.clip = undefined;
  }

  playing(): string | undefined {
    return this.clip && JSON.stringify(this.clip);
  }

  paramValue(id: string): number | undefined {
    return this.pose.get(id);
  }

  scratchDeform(node: string, offsets: Float32Array): boolean {
    this.scratchDeforms.set(node, offsets);
    return true;
  }

  clearScratchDeform(node: string): boolean {
    return this.scratchDeforms.delete(node);
  }

  scratchTransform(
    node: string,
    translate: Float32Array | undefined,
    rotate: Float32Array | undefined,
    scale: Float32Array | undefined,
    zOrder: number | undefined,
    opacity: number | undefined,
  ): boolean {
    const list = (v: Float32Array | undefined) => (v ? [...v] : undefined);
    this.scratchTransforms.set(node, {
      translate: list(translate),
      rotate: list(rotate),
      scale: list(scale),
      zOrder,
      opacity,
    });
    return true;
  }

  clearScratchTransform(node: string): boolean {
    return this.scratchTransforms.delete(node);
  }

  pickNode(_x: number, _y: number): string | undefined {
    return undefined;
  }

  nodeVertices(_node: string): Float32Array | undefined {
    return undefined;
  }
  nodeLocalTransform(_node: string): Float32Array | undefined {
    return undefined;
  }
  nodeDeltaFromWorld(_node: string, x: number, y: number): Float32Array {
    return new Float32Array([x, y]);
  }
  textureThumbnail(_texture: string): undefined {
    return undefined;
  }

  nodeWorldTransform(node: string): Float32Array | undefined {
    if (!this.#holds(node)) return undefined;
    const world = new Float32Array(16);
    world[0] = world[5] = world[10] = world[15] = 1;
    return world;
  }

  /** The parent frame is identity. */
  translationAfterWorldDelta(
    node: string,
    dx: number,
    dy: number,
  ): Float32Array | undefined {
    if (!this.#holds(node)) return undefined;
    const [x, y, z] = this.translations.get(node) ?? [0, 0, 0];
    return new Float32Array([x + dx, y + dy, z]);
  }

  #holds(node: string): boolean {
    return this.doc ? holds(this.doc.root, node) : false;
  }

  clearAllScratch(): void {
    this.scratchDeforms.clear();
    this.scratchTransforms.clear();
  }

  bounds(): Float32Array | undefined {
    return this.box ? new Float32Array(this.box) : undefined;
  }

  free(): void {
    this.freed = true;
  }

  [Symbol.dispose](): void {
    this.free();
  }
}

/**
 * Omits the same fields the model does per kind, since panels branch on
 * presence. A meshed kind reports an empty mesh, as an unmeshed real part does.
 */
function nodeInfo(
  doc: FakeDoc,
  tree: TreeNode,
  node: string,
  parent?: string,
): NodeInfo | undefined {
  if (tree.id === node) {
    const drawn = tree.kind === "part" || tree.kind === "composite";
    return {
      id: tree.id,
      kind: tree.kind,
      parent: parent ?? null,
      name: tree.name,
      translate: [0, 0, 0],
      rotate: [0, 0, 0],
      scale: [1, 1],
      z_order: tree.z_order,
      enabled: tree.enabled,
      lock_to_root: false,
      ...(drawn
        ? {
            opacity: 1,
            blend_mode: "normal" as const,
            tint: [1, 1, 1] as [number, number, number],
            screen_tint: [0, 0, 0] as [number, number, number],
            mask_threshold: 0.5,
          }
        : {}),
      ...(tree.kind === "part" && doc.albedo[tree.id] !== undefined
        ? { texture: doc.albedo[tree.id] }
        : {}),
      ...(tree.kind === "part" || tree.kind === "mesh_group"
        ? { vertex_count: 0, triangle_count: 0 }
        : {}),
      ...(tree.kind === "composite" ? { propagate_meshgroup: false } : {}),
      ...(tree.kind === "mesh_group" ? { mg_translate_children: true } : {}),
    };
  }
  for (const child of tree.children) {
    const found = nodeInfo(doc, child, node, tree.id);
    if (found) return found;
  }
  return undefined;
}

function holds(tree: TreeNode, node: string): boolean {
  return tree.id === node || tree.children.some((child) => holds(child, node));
}

function addresses(
  binding: FakeBinding,
  at: {
    node: string;
    target: BindingTarget;
    param: string;
    param_y?: string | null;
  },
): boolean {
  return (
    binding.node === at.node &&
    binding.target === at.target &&
    binding.param === at.param &&
    (binding.param_y ?? null) === (at.param_y ?? null)
  );
}

function binding(
  doc: FakeDoc,
  at: {
    node: string;
    target: BindingTarget;
    param: string;
    param_y?: string | null;
    key_positions?: number[][] | null;
  },
): FakeBinding {
  const found = doc.bindings.find((b) => addresses(b, at));
  if (found) return found;
  const made: FakeBinding = {
    node: at.node,
    target: at.target,
    param: at.param,
    param_y: at.param_y ?? null,
    interpolate: "linear",
    key_positions: at.key_positions ?? (at.param_y ? [[0, 1], [0, 1]] : [[0, 1]]),
    cells: [],
  };
  doc.bindings.push(made);
  return made;
}

/** Derived, not stored: the real model stores only authored cells. */
function bindingInfo(doc: FakeDoc, b: FakeBinding): BindingInfo {
  const width = b.key_positions[0]!.length;
  const height = b.key_positions[1]?.length ?? 1;
  const keys: (number | null)[][] = [];
  const authored: boolean[][] = [];
  for (let y = 0; y < height; y++) {
    keys.push(new Array<number | null>(width).fill(null));
    authored.push(new Array<boolean>(width).fill(false));
  }
  for (const cell of b.cells) {
    const row = keys[cell.y];
    const flags = authored[cell.y];
    if (!row || !flags || cell.x >= width) continue;
    row[cell.x] = cell.value;
    flags[cell.x] = true;
  }
  return {
    key_positions: b.key_positions,
    identity: b.target === "deform" ? { offset: [0, 0], vertex_count: 0 } : { scalar: ["sx", "sy", "opacity", "tintr", "tintg", "tintb"].includes(b.target) ? 1 : 0 },
    target: b.target,
    param: b.param,
    param_y: b.param_y ?? null,
    interpolate: b.interpolate,
    width,
    height,
    keys,
    authored,
  };
}

export class FakeViewport implements WasmViewport {
  started = 0;
  stopped = 0;
  invalidated = 0;
  freed = 0;
  size: [number, number] | undefined;
  camera: [number, number, number] | undefined;
  frame = {
    width: 1,
    height: 1,
    rgba: new Uint8Array([0x80, 0x80, 0x80, 0xff]),
  };

  readback(): Promise<{ width: number; height: number; rgba: Uint8Array }> {
    return Promise.resolve(this.frame);
  }

  start(): void {
    this.started += 1;
  }
  stop(): void {
    this.stopped += 1;
  }
  invalidate(): void {
    this.invalidated += 1;
  }
  resize(width: number, height: number): void {
    this.size = [width, height];
  }
  setCamera(x: number, y: number, height: number): void {
    this.camera = [x, y, height];
  }
  free(): void {
    this.freed += 1;
  }
  [Symbol.dispose](): void {
    this.free();
  }
}

export interface FakeModule {
  module: WasmModule;
  gpu: FakeGpu;
  replicas: FakeReplica[];
  viewports: FakeViewport[];
  failNextAcquire: string | undefined;
}

export function fakeWasm(): FakeModule {
  const gpu = new FakeGpu();
  const replicas: FakeReplica[] = [];
  const viewports: FakeViewport[] = [];

  class TrackedReplica extends FakeReplica {
    constructor() {
      super();
      replicas.push(this);
    }
  }

  class TrackedViewport extends FakeViewport {
    constructor(
      _gpu: WasmGpu,
      _replica: WasmReplica,
      _canvas: HTMLCanvasElement,
    ) {
      super();
      viewports.push(this);
    }
  }

  const made: FakeModule = {
    gpu,
    replicas,
    viewports,
    failNextAcquire: undefined,
    module: {
      CatchlightEditor: FakeEditor,
      manifestRequirements: (json: string): string[] => {
        const doc = JSON.parse(json) as { textures?: Array<{ path?: string }> };
        return (doc.textures ?? []).flatMap((t) =>
          t.path === undefined ? [] : [t.path],
        );
      },
      Gpu: {
        acquire: (canvas: HTMLCanvasElement) => {
          const failure = made.failNextAcquire;
          if (failure !== undefined) {
            made.failNextAcquire = undefined;
            return Promise.reject(new Error(failure));
          }
          gpu.acquires += 1;
          gpu.acquiredFrom.push(canvas);
          return Promise.resolve(gpu);
        },
      },
      Replica: TrackedReplica,
      Viewport: TrackedViewport,
    },
  };
  return made;
}

/**
 * A backend a test steps by hand, so a reply can arrive before or after its
 * event — both orders are real and `Session` must resolve the same way.
 */
export class ScriptedBackend implements Backend {
  readonly kind = "in-tab";
  sent: Command[] = [];
  attached: Array<readonly Attachment[]> = [];
  stored = new Map<string, Uint8Array>();
  feeds: Array<{ session: number; rev: number }> = [];
  /** Feeds that actually ran, after coalescing. */
  runs: number[] = [];
  revs = new Map<number, number>();
  hold: Promise<void> | undefined;
  failFeed: string | undefined;
  replies = new Map<string, OkReplyWithPayload>();

  #listeners = new Set<(event: Event) => void>();
  #queue = new FeedQueue();

  send(command: Command): Promise<OkReply> {
    this.sent.push(command);
    const scripted = this.replies.get(command.cmd);
    if (scripted) return Promise.resolve(scripted);
    const session =
      "session" in command ? (command.session as number) : undefined;
    const rev = session === undefined ? undefined : this.revs.get(session);
    return Promise.resolve(
      rev === undefined
        ? { body: { result: "empty" } }
        : { body: { result: "empty" }, rev },
    );
  }

  async sendWith(
    command: Command,
    attachments: readonly Attachment[],
  ): Promise<OkReplyWithPayload> {
    this.attached.push(attachments);
    return this.send(command);
  }

  readFile(key: string): Promise<Uint8Array | undefined> {
    return Promise.resolve(this.stored.get(key));
  }

  feed(replica: WasmReplica, session: number, rev: number): Promise<number> {
    this.feeds.push({ session, rev });
    return this.#queue.run(session, rev, async (target) => {
      this.runs.push(target);
      if (this.hold) await this.hold;
      if (this.failFeed !== undefined) {
        const message = this.failFeed;
        this.failFeed = undefined;
        throw new ProtocolError({ code: "feed", message });
      }
      const doc = emptyDoc(`session ${session}`);
      doc.rev = target;
      replica.applyStructure(structureBytes(doc), target);
      return replica.rev();
    });
  }

  onEvent(listener: (event: Event) => void): Unsubscribe {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  close(): void {
    this.#listeners.clear();
  }

  changed(session: number, rev: number): void {
    this.revs.set(session, rev);
    this.emit({ event: "model_changed", session, rev });
  }

  emit(event: Event): void {
    for (const listener of [...this.#listeners]) listener(event);
  }
}

export class FakeSocket implements SocketLike {
  static opened: FakeSocket[] = [];
  url: string;
  sent: string[] = [];
  closed = false;
  onopen: ((event: unknown) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: unknown) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeSocket.opened.push(this);
    queueMicrotask(() => this.onopen?.({}));
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
    this.onclose?.({});
  }

  request(index: number): Request {
    return JSON.parse(this.sent[index] ?? "{}") as Request;
  }

  deliver(message: unknown): void {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
}

export function httpResponse(
  body: Uint8Array | unknown,
  init?: { status?: number; headers?: Record<string, string> },
): HttpResponse {
  const status = init?.status ?? 200;
  const headers = new Map(
    Object.entries(init?.headers ?? {}).map(([k, v]) => [k.toLowerCase(), v]),
  );
  const bytes =
    body instanceof Uint8Array
      ? body
      : new TextEncoder().encode(JSON.stringify(body));
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status === 200 ? "OK" : "no",
    headers: { get: (name: string) => headers.get(name.toLowerCase()) ?? null },
    arrayBuffer: () =>
      Promise.resolve(
        bytes.buffer.slice(
          bytes.byteOffset,
          bytes.byteOffset + bytes.byteLength,
        ) as ArrayBuffer,
      ),
    json: () =>
      Promise.resolve(JSON.parse(new TextDecoder().decode(bytes)) as unknown),
  };
}

export function fakeFetch(routes: Record<string, () => HttpResponse>): {
  fetch: (url: string, init?: FetchInit) => Promise<HttpResponse>;
  calls: Array<{ url: string; init: FetchInit | undefined }>;
} {
  const calls: Array<{ url: string; init: FetchInit | undefined }> = [];
  return {
    calls,
    fetch: (url, init) => {
      calls.push({ url, init });
      const route = routes[new URL(url, "http://editor.invalid").pathname];
      if (!route)
        return Promise.resolve(httpResponse({ error: url }, { status: 404 }));
      return Promise.resolve(route());
    },
  };
}

/** Throws on every call after `free`, as wasm-bindgen does. */
export class GuardedReplica extends FakeReplica {
  usedAfterFree: string[] = [];

  override rev(): number {
    this.#live("rev");
    return super.rev();
  }

  override needs(structure: Uint8Array): string {
    this.#live("needs");
    return super.needs(structure);
  }

  override putExtension(key: string, bytes: Uint8Array): void {
    this.#live("putExtension");
    super.putExtension(key, bytes);
  }

  override putTexture(id: string): void {
    this.#live("putTexture");
    super.putTexture(id);
  }

  override applyStructure(structure: Uint8Array, rev: number): boolean {
    this.#live("applyStructure");
    return super.applyStructure(structure, rev);
  }

  override syncFromEditor(editor: WasmEditor, session: number): number {
    this.#live("syncFromEditor");
    return super.syncFromEditor(editor, session);
  }

  /** wasm-bindgen throws a string. */
  #live(call: string): void {
    if (!this.freed) return;
    this.usedAfterFree.push(call);
    throw "null pointer passed to rust";
  }
}

function applyEdit(doc: FakeDoc, edit: EditOp): void {
  if (edit.op === "binding_add") { binding(doc, edit); return; }
  if (edit.op === "binding_cells_set") {
    const b = binding(doc, edit);
    for (const { cell: [x, y], value } of edit.cells) {
      const scalar = "scalar" in value ? value.scalar : null;
      const old = b.cells.find((cell) => cell.x === x && cell.y === y);
      if (old) old.value = scalar; else b.cells.push({ x, y, value: scalar });
    }
    return;
  }
  if (edit.op === "binding_cells_unset") {
    const b = binding(doc, edit);
    b.cells = b.cells.filter((cell) => !edit.cells.some(([x, y]) => cell.x === x && cell.y === y)); return;
  }
  if (edit.op === "binding_key_insert" || edit.op === "binding_key_delete" || edit.op === "binding_key_move") {
    const b = binding(doc, edit), axis = edit.axis === b.param ? 0 : 1, points = b.key_positions[axis]!;
    if (edit.op === "binding_key_insert") {
      let index = points.findIndex((p) => p >= edit.value);
      if (index < 0) index = points.length;
      if (points[index] === edit.value) return;
      points.splice(index, 0, edit.value);
      for (const cell of b.cells) { if (axis === 0 && cell.x >= index) cell.x++; if (axis === 1 && cell.y >= index) cell.y++; }
    } else if (edit.op === "binding_key_move") points[edit.index] = edit.value;
    else {
      points.splice(edit.index, 1);
      b.cells = b.cells.filter((cell) => (axis === 0 ? cell.x : cell.y) !== edit.index);
      for (const cell of b.cells) { if (axis === 0 && cell.x > edit.index) cell.x--; if (axis === 1 && cell.y > edit.index) cell.y--; }
    }
    return;
  }
  if (edit.op === "node_reparent" || edit.op === "node_reorder") {
    const find = (root: TreeNode, id: string): TreeNode | undefined => root.id === id ? root : root.children.map((child) => find(child, id)).find(Boolean);
    const parent = (root: TreeNode, id: string): TreeNode | undefined => root.children.some((child) => child.id === id) ? root : root.children.map((child) => parent(child, id)).find(Boolean);
    const from = parent(doc.root, edit.node), to = edit.op === "node_reparent" ? find(doc.root, edit.to) : from;
    if (!from || !to) throw new Error("The node or parent is missing.");
    const [node] = from.children.splice(from.children.findIndex((child) => child.id === edit.node), 1);
    to.children.splice(edit.op === "node_reorder" ? edit.index : to.children.length, 0, node!); return;
  }
  throw new Error(`The fake does not implement ${edit.op}.`);
}
