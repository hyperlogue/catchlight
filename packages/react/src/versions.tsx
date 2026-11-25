/**
 * A session's saved versions: numbered snapshots of its model kept beside it.
 * Not revisions — a revision counts edits and ends with the session. Looking
 * at a version opens it as a read-only session of its own, so the stage shows
 * it through the same replica path as any model while the live one stays put.
 */

import type { Kept, Session, VersionInfo, VersionList } from "@catchlight/core";
import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import type { KeyboardEvent, ReactNode } from "react";

import type { ErrorHandler } from "./authoring.js";
import { Icon } from "./controls.js";
import { useEditor } from "./editor-context.js";
import { useLatest } from "./latest.js";
import type { Reviewer } from "./review.js";

const NO_VERSIONS: VersionList = { versions: [], kept: "stored" };
const neverChanges = (): (() => void) => () => {};
const noVersions = (): VersionList => NO_VERSIONS;

export interface VersionsState {
  /** Oldest first. */
  versions: readonly VersionInfo[];
  kept: Kept;
  keptProblem: string | undefined;
  busy: boolean;
  /** The version saved, or `undefined` when the editor refused. */
  save(label?: string): Promise<VersionInfo | undefined>;
  restore(version: number): Promise<boolean>;
}

/** `reviewer` signs what this tab saves, as it signs comments. */
export function useVersions(
  session: Session | undefined,
  onError: ErrorHandler,
  reviewer?: Reviewer,
): VersionsState {
  const list = useSyncExternalStore(
    session?.subscribeVersions ?? neverChanges,
    session?.getVersions ?? noVersions,
    session?.getVersions ?? noVersions,
  );
  const [pending, setPending] = useState(0);
  const live = useLatest({ session, onError, reviewer });
  const run = useCallback(
    async <T,>(work: (session: Session) => Promise<T>): Promise<T | undefined> => {
      const { session, onError } = live.current;
      if (!session) return undefined;
      setPending((n) => n + 1);
      try {
        return await work(session);
      } catch (cause) {
        onError(cause);
        return undefined;
      } finally {
        setPending((n) => n - 1);
      }
    },
    [live],
  );
  const save = useCallback(
    (label?: string) =>
      run(async (session) => {
        const who = live.current.reviewer;
        const body = await session.sendVersion({
          cmd: "version_save",
          author: who ? { role: "human", id: who.id, name: who.name } : { role: "human" },
          ...(label?.trim() ? { label: label.trim() } : {}),
        });
        return body.result === "version" ? body.version : undefined;
      }),
    [run, live],
  );
  const restore = useCallback(
    async (version: number) =>
      (await run(async (session) => {
        await session.send({ cmd: "version_restore", version });
        return true;
      })) ?? false,
    [run],
  );
  return {
    versions: list.versions,
    kept: list.kept,
    keptProblem: list.kept_problem ?? undefined,
    busy: pending > 0,
    save,
    restore,
  };
}

export interface VersionView {
  /** The read-only session on show, and which version it is. */
  viewing: { session: Session; version: VersionInfo } | undefined;
  open(version: VersionInfo): void;
  /** Shows the live model again and closes the read-only session. */
  back(): void;
  /** Restores the version on show into the live session, then goes back. */
  restore(): Promise<boolean>;
}

/**
 * Which of `live`'s versions is on show instead of it. Changing `live` drops
 * the view; the editor closes a version's session with the one it came from.
 */
export function useVersionView(
  live: Session | undefined,
  versions: VersionsState,
  onError: ErrorHandler,
): VersionView {
  const editor = useEditor();
  const [view, setView] = useState<{ of: Session; session: Session; version: VersionInfo }>();
  const [closing, setClosing] = useState<Session[]>([]);
  const request = useRef(0);
  const shown = view && view.of === live ? view : undefined;

  useEffect(() => {
    if (view && view.of !== live) {
      setView(undefined);
      setClosing((was) => [...was, view.session]);
    }
  }, [view, live]);
  // After the render that stopped showing it, so no viewport holds its replica.
  useEffect(() => {
    if (closing.length === 0) return;
    setClosing([]);
    for (const session of closing)
      void editor.closeSession(session.id).catch(() => undefined);
  }, [closing, editor]);

  const open = useCallback(
    (version: VersionInfo) => {
      if (!live) return;
      const generation = ++request.current;
      void editor.openVersion(live, version.version).then((session) => {
        if (generation !== request.current) return;
        setView((was) => {
          if (was && was.session !== session) setClosing((c) => [...c, was.session]);
          return { of: live, session, version };
        });
      }, onError);
    },
    [editor, live, onError],
  );
  const back = useCallback(() => {
    ++request.current;
    setView((was) => {
      if (was) setClosing((c) => [...c, was.session]);
      return undefined;
    });
  }, []);
  const restore = useCallback(async () => {
    if (!shown) return false;
    const done = await versions.restore(shown.version.version);
    if (done) back();
    return done;
  }, [shown, versions, back]);

  return {
    viewing: shown ? { session: shown.session, version: shown.version } : undefined,
    open,
    back,
    restore,
  };
}

/** "v3 · label", or "v3" without a label. */
export function versionName(version: VersionInfo): string {
  return version.label ? `v${version.version} · ${version.label}` : `v${version.version}`;
}

/** Coarse and short: "just now", "5 min ago", "3 h ago", "2 d ago", else the date. */
export function relativeTime(ms: number, now = Date.now()): string {
  if (ms <= 0) return "";
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days < 14) return `${days} d ago`;
  return new Date(ms).toLocaleDateString();
}

function keptNote(kept: Kept): string | undefined {
  switch (kept) {
    case "session":
      return "Versions last until this editor closes.";
    case "unsaved":
      return "Save the model to keep its versions.";
    case "unreadable":
      return "Versions kept with this model could not be read and were left untouched. New ones last until this editor closes.";
    case "stored":
      return undefined;
  }
}

export interface VersionSelectProps {
  versions: VersionsState;
  /** The version on show, or `undefined` for the live model. */
  current: number | undefined;
  /** `undefined` picks the live model. */
  onSelect: (version: VersionInfo | undefined) => void;
  reviewer?: Reviewer | undefined;
  /** Bumped to open the list with the save field focused. */
  saveRequested?: number;
  disabled?: boolean;
  now?: number;
}

/**
 * A trigger naming what the stage shows and a list of versions, newest
 * first, with a field to save the model as the next one. Unstyled: every
 * part carries a `data-catchlight-version-*` attribute.
 */
export function VersionSelect({
  versions,
  current,
  onSelect,
  reviewer,
  saveRequested = 0,
  disabled = false,
  now,
}: VersionSelectProps): ReactNode {
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const listId = useId();
  const shown = versions.versions.find((v) => v.version === current);
  const newestFirst = [...versions.versions].reverse();

  useEffect(() => {
    if (saveRequested === 0 || disabled) return;
    setOpen(true);
    setSaving(true);
  }, [saveRequested, disabled]);
  useEffect(() => {
    if (!open) return;
    if (saving) field.current?.focus();
    else
      (
        list.current?.querySelector<HTMLElement>('[role="option"][aria-selected="true"]') ??
        list.current?.querySelector<HTMLElement>('[role="option"]')
      )?.focus();
  }, [open, saving]);
  useEffect(() => {
    if (!open) return;
    const away = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) close(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  });

  const close = (refocus: boolean) => {
    setOpen(false);
    setSaving(false);
    if (refocus) trigger.current?.focus();
  };
  const pick = (version: VersionInfo | undefined) => {
    close(true);
    if (version?.version !== current) onSelect(version);
  };
  const submit = () => {
    void versions.save(field.current?.value).then((saved) => {
      if (saved) close(true);
    });
  };
  const onListKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const options = [...(list.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];
    const at = options.indexOf(document.activeElement as HTMLElement);
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? options.length - 1
          : event.key === "ArrowDown"
            ? (at + 1) % options.length
            : event.key === "ArrowUp"
              ? (at - 1 + options.length) % options.length
              : undefined;
    if (next === undefined) return;
    event.preventDefault();
    options[next]?.focus();
  };
  const note = keptNote(versions.kept);
  return (
    <div
      ref={root}
      data-catchlight-version-select=""
      data-open={open ? "" : undefined}
      data-viewing={shown ? "" : undefined}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.preventDefault();
          event.stopPropagation();
          close(true);
        }
      }}
    >
      <button
        ref={trigger}
        type="button"
        data-catchlight-version-trigger=""
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-label={`Version: ${shown ? versionName(shown) : "Current"}`}
        title="Saved versions"
        disabled={disabled}
        onClick={() => (open ? close(false) : setOpen(true))}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <Icon name="history" width="14" height="14" />
        {shown ? (
          <>
            <span data-catchlight-version-badge="" data-active="">
              v{shown.version}
            </span>
            {shown.label && <span data-catchlight-version-label="">{shown.label}</span>}
          </>
        ) : (
          <span data-catchlight-version-label="">Current</span>
        )}
        <Icon name="down" width="11" height="11" />
      </button>
      {open && (
        <div data-catchlight-version-popover="">
          <div
            ref={list}
            id={listId}
            role="listbox"
            aria-label="Saved versions"
            data-catchlight-version-list=""
            onKeyDown={onListKey}
          >
            <button
              type="button"
              role="option"
              aria-selected={current === undefined}
              tabIndex={current === undefined ? 0 : -1}
              data-catchlight-version-option="current"
              onClick={() => pick(undefined)}
            >
              <span data-catchlight-version-badge="" data-active={current === undefined ? "" : undefined}>
                Now
              </span>
              <span data-catchlight-version-row="">
                <span data-catchlight-version-label="">Current</span>
                <small>What you are editing</small>
              </span>
            </button>
            {newestFirst.map((version) => {
              const selected = version.version === current;
              const agent = version.author.role === "agent";
              const who =
                version.author.role === "agent"
                  ? version.author.name
                  : (version.author.id ?? undefined) === reviewer?.id
                    ? "You"
                    : (version.author.name ?? "Reviewer");
              return (
                <button
                  key={version.version}
                  type="button"
                  role="option"
                  aria-selected={selected}
                  tabIndex={selected ? 0 : -1}
                  data-catchlight-version-option={version.version}
                  aria-label={versionName(version)}
                  onClick={() => pick(version)}
                >
                  <span data-catchlight-version-badge="" data-active={selected ? "" : undefined}>
                    v{version.version}
                  </span>
                  <span data-catchlight-version-row="">
                    <span data-catchlight-version-label="">
                      {version.label ?? `Version ${version.version}`}
                    </span>
                    <small>
                      <span data-catchlight-version-author="" data-role={version.author.role}>
                        {agent && <Icon name="spark" width="11" height="11" />}
                        {who}
                      </span>
                      {version.time > 0 && (
                        <time dateTime={new Date(version.time).toISOString()}>
                          {relativeTime(version.time, now)}
                        </time>
                      )}
                    </small>
                    {version.summary && <span data-catchlight-version-summary="">{version.summary}</span>}
                  </span>
                </button>
              );
            })}
            {newestFirst.length === 0 && (
              <p data-catchlight-version-empty="">No saved versions yet.</p>
            )}
          </div>
          {saving ? (
            <form
              data-catchlight-version-save=""
              onSubmit={(event) => {
                event.preventDefault();
                submit();
              }}
            >
              <input
                ref={field}
                type="text"
                maxLength={80}
                placeholder="Label (optional)"
                aria-label="Version label"
              />
              <button type="submit" data-primary="" disabled={versions.busy}>
                Save
              </button>
            </form>
          ) : (
            <button
              type="button"
              data-catchlight-version-save-open=""
              disabled={disabled}
              onClick={() => setSaving(true)}
            >
              <Icon name="plus" width="14" height="14" />
              Save version…
            </button>
          )}
          {note && (
            <p
              data-catchlight-version-kept={versions.kept}
              role="status"
              title={versions.keptProblem}
            >
              {note}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

export interface VersionBannerProps {
  version: VersionInfo;
  busy?: boolean;
  onRestore: () => void;
  onBack: () => void;
}

/** Says the stage shows a saved version, not the model being edited. */
export function VersionBanner({ version, busy = false, onRestore, onBack }: VersionBannerProps): ReactNode {
  return (
    <div data-catchlight-version-banner="" role="status">
      <Icon name="history" width="14" height="14" />
      <span>
        Viewing <strong>{versionName(version)}</strong>
        <small> · read-only</small>
      </span>
      <button type="button" data-catchlight-version-restore="" disabled={busy} onClick={onRestore}>
        Restore this version
      </button>
      <button type="button" data-primary="" data-catchlight-version-back="" onClick={onBack}>
        Back to current
      </button>
    </div>
  );
}
