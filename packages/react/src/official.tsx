/**
 * Reads, preview state and edits shared by the panels over the official
 * extensions: humanoid controls, expressions and visemes, and animations.
 */
import { ProtocolError } from "@catchlight/core";
import type {
  ExtensionRef,
  ExtensionWarningInfo,
  ParamId,
  Playing,
  PreviewInputs,
  ResponseBody,
  Session,
  SessionEditCommand,
  SessionReplicaQueryCommand,
} from "@catchlight/core";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import type { ErrorHandler } from "./authoring.js";
import { Icon, Modal } from "./controls.js";
import { useRevision } from "./replica.js";

export const HUMANOID = "catchlight.humanoid";
export const EXPRESSIONS = "catchlight.expressions";
export const ANIMATIONS = "catchlight.animations";

/** A read of one official extension, or why this build cannot read its data. */
export type OfficialRead<R extends ResponseBody["result"]> =
  | { body: Extract<ResponseBody, { result: R }>; unread?: undefined }
  | { body?: undefined; unread: string };

export function useOfficialRead<R extends ResponseBody["result"]>(
  session: Session,
  command: SessionReplicaQueryCommand,
  result: R,
): OfficialRead<R> {
  const rev = useRevision(session);
  const key = JSON.stringify(command);
  return useMemo((): OfficialRead<R> => {
    let body: ResponseBody;
    try {
      body = session.query(JSON.parse(key) as SessionReplicaQueryCommand);
    } catch (cause) {
      if (cause instanceof ProtocolError && cause.code === "extension_unread")
        return { unread: cause.message };
      throw cause;
    }
    if (body.result !== result) throw new Error(`Could not read ${result}.`);
    return { body: body as Extract<ResponseBody, { result: R }> };
  }, [session, rev, key, result]);
}

export function useHumanoid(session: Session) {
  return useOfficialRead(session, { cmd: "humanoid_get" }, "humanoid");
}

export function useExpressions(session: Session) {
  return useOfficialRead(session, { cmd: "expression_list" }, "expressions");
}

export function useClips(session: Session) {
  return useOfficialRead(session, { cmd: "clip_list" }, "clips");
}

/** The findings about the extension under `key`, and the params they concern. */
export function useExtensionWarnings(session: Session, key: string) {
  const rev = useRevision(session);
  return useMemo(() => {
    const body = session.query({ cmd: "extension_warnings" });
    const warnings =
      body.result === "extension_warnings" ? body.warnings.filter((w) => w.key === key) : [];
    const params = new Map<ParamId, string[]>();
    for (const warning of warnings) {
      for (const subject of warning.subjects) {
        const param = subjectParam(subject);
        if (param !== undefined) params.set(param, [...(params.get(param) ?? []), warning.message]);
      }
    }
    return { warnings, params };
  }, [session, rev, key]);
}

function subjectParam(subject: ExtensionRef): ParamId | undefined {
  if ("param" in subject) return subject.param;
  if ("removed" in subject) return subjectParam(subject.removed);
  return undefined;
}

/** The messages of the warnings about any of `params`; empty when none. */
export function warningsOn(warned: Map<ParamId, string[]>, params: Iterable<ParamId>): string[] {
  const out = new Set<string>();
  for (const param of params) for (const message of warned.get(param) ?? []) out.add(message);
  return [...out];
}

export function usePreviewInputs(session: Session): PreviewInputs {
  const read = useCallback(() => session.inputs(), [session]);
  return useSyncExternalStore(session.onInvalidate, read, read);
}

/**
 * A playing clip moves params without invalidating, so this re-reads every
 * animation frame while one plays and stops once none does. `select` keeps
 * re-renders to what a caller shows: an unchanged value renders nothing.
 */
function usePlayingSelect<T>(
  session: Session | undefined,
  select: (playing: Playing | undefined) => T,
  same: (a: T, b: T) => boolean,
): T {
  const latest = useRef({ select, same });
  latest.current = { select, same };
  const [value, setValue] = useState(() => select(session?.playing()));
  useEffect(() => {
    let frame = 0;
    const read = () => {
      const playing = session?.playing();
      const next = latest.current.select(playing);
      setValue((previous) => (latest.current.same(previous, next) ? previous : next));
      if (playing && !frame) frame = requestAnimationFrame(tick);
    };
    const tick = () => {
      frame = 0;
      read();
    };
    read();
    if (!session) return;
    const off = session.onInvalidate(read);
    return () => {
      off();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [session]);
  return value;
}

/** The name of the clip that plays; `undefined` when none does. */
export function usePlayingClip(session: Session | undefined): string | undefined {
  return usePlayingSelect(session, (p) => p?.name, Object.is);
}

/** Where the playing clip is, updated every frame while it plays. */
export function usePlayhead(session: Session): Playing | undefined {
  return usePlayingSelect(session, (p) => p, samePlaying);
}

const samePlaying = (a: Playing | undefined, b: Playing | undefined) =>
  a === b || (!!a && !!b && a.name === b.name && a.time === b.time && a.frame === b.frame);

type Nonstandard = Extract<SessionEditCommand, { nonstandard: boolean }>;

/**
 * Edits that may name a control outside the humanoid standard. A refusal for
 * that waits in `asking` until the person keeps the name (resent with
 * `nonstandard`) or cancels; every other failure goes to `onError`.
 */
export function useOfficialCommand(session: Session, onError: ErrorHandler) {
  const [pending, setPending] = useState(0);
  const [asking, setAsking] = useState<{ command: Nonstandard; message: string; done?: () => void }>();
  const live = useRef({ session, onError });
  live.current = { session, onError };
  const run = useCallback(
    async (command: SessionEditCommand, done?: () => void): Promise<ResponseBody | undefined> => {
      setPending((n) => n + 1);
      try {
        const body = await live.current.session.send(command);
        done?.();
        return body;
      } catch (cause) {
        if (
          cause instanceof ProtocolError &&
          cause.code === "nonstandard_control" &&
          "nonstandard" in command &&
          !command.nonstandard
        ) {
          setAsking({ command, message: cause.message, ...(done ? { done } : {}) });
        } else live.current.onError(cause);
        return undefined;
      } finally {
        setPending((n) => n - 1);
      }
    },
    [],
  );
  const keep = useCallback(() => {
    if (!asking) return;
    setAsking(undefined);
    void run({ ...asking.command, nonstandard: true }, asking.done);
  }, [asking, run]);
  const dismiss = useCallback(() => setAsking(undefined), []);
  return { run, busy: pending > 0, asking, keep, dismiss };
}

export function NonstandardDialog({
  asking,
  onKeep,
  onCancel,
}: {
  asking: { message: string } | undefined;
  onKeep: () => void;
  onCancel: () => void;
}) {
  if (!asking) return null;
  return (
    <Modal title="Keep a control outside the standard?" onClose={onCancel}>
      <p data-catchlight-hint="">
        {capitalize(asking.message)}. Apps that speak the humanoid standard will not drive it.
      </p>
      <div data-catchlight-dialog-actions="">
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" data-primary="" onClick={onKeep}>
          Keep the name
        </button>
      </div>
    </Modal>
  );
}

/** In the style of the Model tab's health list. */
export function ExtensionWarnings({ warnings }: { warnings: ExtensionWarningInfo[] }) {
  if (!warnings.length) return null;
  return (
    <div data-catchlight-extension-warnings="">
      {warnings.map((w, i) => (
        <p key={i} data-catchlight-warning="" data-code={w.code}>
          <Icon name="warning" />
          {capitalize(w.message)}
        </p>
      ))}
    </div>
  );
}

export function UnreadNotice({ what, why }: { what: string; why: string }) {
  return (
    <div data-catchlight-extension-unread="" role="status">
      <p data-catchlight-warning="">
        <Icon name="warning" />
        This build cannot read the model’s {what}, so they cannot be edited here. They are kept
        and saved as they are.
      </p>
      <p data-catchlight-hint="">{capitalize(why)}</p>
    </div>
  );
}

export function capitalize(text: string): string {
  return text.charAt(0).toLocaleUpperCase() + text.slice(1);
}

export function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** Marks a card or row that a warning concerns; the messages are its label. */
export function WarningMark({ warnings }: { warnings: string[] }) {
  if (!warnings.length) return null;
  const label = warnings.map(capitalize).join("\n");
  return (
    <span data-catchlight-warning-mark="" role="img" aria-label={label} title={label}>
      <Icon name="warning" width="13" height="13" />
    </span>
  );
}
