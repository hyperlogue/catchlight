/**
 * A session's review: threads a person leaves on things in the model for a
 * listening agent. Comments stay `unsent` until the person sends the review;
 * the agent's replies arrive through `review_changed`.
 */

import type {
  ParamInfo,
  Review,
  ReviewAgent,
  ReviewAuthor,
  ReviewComment,
  ReviewImage,
  ReviewTarget,
  ReviewThread,
  Session,
  ThreadId,
  ThreadStatus,
  TreeNode,
} from "@catchlight/core";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { ClipboardEvent, DragEvent, KeyboardEvent, ReactNode } from "react";

import { kindIcons } from "./authoring.js";
import { useLatest } from "./latest.js";
import type { ErrorHandler } from "./authoring.js";
import { Icon, IconButton, Modal } from "./controls.js";
import type { IconName } from "./controls.js";
import { useEditing } from "./editing.js";
import { ImageMarkup } from "./image-markup.js";
import { findNode } from "./node-actions.js";
import { readPose } from "./pose.js";
import { useSelection } from "./presence.js";
import { useRevision } from "./replica.js";

const NO_REVIEW: Review = { threads: [], agents: [], kept: "stored" };
const neverChanges = (): (() => void) => () => {};
const noReview = (): Review => NO_REVIEW;

/**
 * `absent`: no agent has been heard from lately. `away`: some were, and none
 * is waiting or holding a thread. `working`, any agent holding an open
 * thread, wins over `listening`, any agent waiting.
 */
export type AgentPhase = "absent" | "away" | "listening" | "working";

/** The person at this tab, as the host knows them. */
export interface Reviewer {
  id: string;
  name: string;
}

export interface ReviewState {
  session: Session | undefined;
  review: Review;
  /** Recently heard from, most recent last. */
  agents: readonly ReviewAgent[];
  phase: AgentPhase;
  reviewer: Reviewer | undefined;
  /** Human comments on open threads still held back from the agent. */
  unsent: number;
  /** Threads sent and not yet taken by an agent. */
  waiting: number;
  /** Open threads whose last word is the agent's. */
  replied: number;
  busy: boolean;
  /** What the composer is aimed at; `undefined` while it is closed. */
  composing: ReviewTarget | undefined;
  compose(target: ReviewTarget | undefined): void;
  focused: ThreadId | undefined;
  focus(thread: ThreadId | undefined): void;
  open(target: ReviewTarget, body: string, images?: readonly Blob[]): Promise<boolean>;
  reply(thread: ThreadId, body: string, images?: readonly Blob[]): Promise<boolean>;
  /** Reports a failure the way a failed command is reported. */
  fail(cause: unknown): void;
  setStatus(thread: ThreadId, status: ThreadStatus): Promise<boolean>;
  removeThread(thread: ThreadId): Promise<boolean>;
  removeComment(thread: ThreadId, comment: number): Promise<boolean>;
  send(): Promise<boolean>;
  /** Shows a saved version a comment points at; absent when the host shows none. */
  openVersion: ((version: number) => void) | undefined;
}

const ReviewContext = createContext<ReviewState | undefined>(undefined);

export interface ReviewProviderProps {
  session: Session | undefined;
  onError: ErrorHandler;
  /**
   * Stamped on the comments this tab writes. Without one, every comment from
   * a person reads as the viewer's own.
   */
  reviewer?: Reviewer;
  /** A thread was focused or a comment started from outside the panel. */
  onAttention?: () => void;
  /** Shows a saved version a comment points at, such as the one holding a fix. */
  onOpenVersion?: (version: number) => void;
  children?: ReactNode;
}

export function ReviewProvider({
  session,
  onError,
  reviewer,
  onAttention,
  onOpenVersion,
  children,
}: ReviewProviderProps) {
  const review = useSyncExternalStore(
    session?.subscribeReview ?? neverChanges,
    session?.getReview ?? noReview,
    session?.getReview ?? noReview,
  );
  const [pending, setPending] = useState(0);
  const [aim, setAim] = useState<{ session: Session | undefined; target?: ReviewTarget; thread?: ThreadId }>({
    session,
  });
  const here = aim.session === session ? aim : { session };
  // By value: a host passing a fresh object each render must not churn the context.
  const reviewerId = reviewer?.id;
  const reviewerName = reviewer?.name;
  const live = useRef({ session, onError, onAttention, reviewer });
  live.current = { session, onError, onAttention, reviewer };
  const person = (): ReviewAuthor => {
    const who = live.current.reviewer;
    return who ? { role: "human", id: who.id, name: who.name } : { role: "human" };
  };

  const run = useCallback(async (work: (session: Session) => Promise<unknown>): Promise<boolean> => {
    const { session, onError } = live.current;
    if (!session) return false;
    setPending((n) => n + 1);
    try {
      await work(session);
      return true;
    } catch (cause) {
      onError(cause);
      return false;
    } finally {
      setPending((n) => n - 1);
    }
  }, []);

  const actions = useMemo(
    () => ({
      fail(cause: unknown): void {
        live.current.onError(cause);
      },
      compose(target: ReviewTarget | undefined): void {
        setAim((was) => ({
          session: live.current.session,
          ...(target ? { target } : {}),
          ...(was.thread !== undefined && was.session === live.current.session ? { thread: was.thread } : {}),
        }));
        if (target) live.current.onAttention?.();
      },
      focus(thread: ThreadId | undefined): void {
        setAim((was) => ({
          session: live.current.session,
          ...(was.target && was.session === live.current.session ? { target: was.target } : {}),
          ...(thread !== undefined ? { thread } : {}),
        }));
        if (thread !== undefined) live.current.onAttention?.();
      },
      open: (target: ReviewTarget, body: string, images: readonly Blob[] = []) =>
        run(async (session) => {
          const reply = await session.sendReview({
            cmd: "review_thread_add",
            target,
            author: person(),
            body,
            pose: readPose(session),
            images: await upload(session, images),
          });
          setAim({
            session,
            ...(reply.result === "review_thread" ? { thread: reply.thread.thread } : {}),
          });
        }),
      reply: (thread: ThreadId, body: string, images: readonly Blob[] = []) =>
        run(async (session) =>
          session.sendReview({
            cmd: "review_comment_add",
            thread,
            author: person(),
            body,
            images: await upload(session, images),
          }),
        ),
      setStatus: (thread: ThreadId, status: ThreadStatus) =>
        run((session) => session.sendReview({ cmd: "review_thread_set", thread, status })),
      removeThread: (thread: ThreadId) =>
        run((session) => session.sendReview({ cmd: "review_thread_delete", thread })),
      removeComment: (thread: ThreadId, comment: number) =>
        run((session) => session.sendReview({ cmd: "review_comment_delete", thread, comment })),
      send: () => run((session) => session.sendReview({ cmd: "review_submit" })),
    }),
    [run],
  );

  const value = useMemo<ReviewState>(() => {
    const open = review.threads.filter((thread) => thread.status === "open");
    const agents = review.agents;
    return {
      session,
      review,
      agents,
      reviewer: reviewerId === undefined ? undefined : { id: reviewerId, name: reviewerName ?? "" },
      phase: open.some((thread) => thread.holder)
        ? "working"
        : agents.some((agent) => agent.listening)
          ? "listening"
          : agents.length > 0
            ? "away"
            : "absent",
      unsent: open.reduce((n, thread) => n + thread.comments.filter(isUnsent).length, 0),
      waiting: open.filter((thread) => thread.comments.some((c) => c.delivery === "sent")).length,
      replied: open.filter(agentSpokeLast).length,
      busy: pending > 0,
      composing: here.target,
      focused: here.thread,
      openVersion: onOpenVersion,
      ...actions,
    };
  }, [session, review, pending, here.target, here.thread, actions, reviewerId, reviewerName, onOpenVersion]);

  return <ReviewContext.Provider value={value}>{children}</ReviewContext.Provider>;
}

async function upload(session: Session, images: readonly Blob[]): Promise<number[]> {
  const ids: number[] = [];
  // In order: a comment shows its images as they were attached.
  for (const image of images) {
    const held = await session.addReviewImage(new Uint8Array(await image.arrayBuffer()));
    ids.push(held.image);
  }
  return ids;
}

/** `undefined` outside a [`ReviewProvider`], so a part can be used without review. */
export function useReview(): ReviewState | undefined {
  return useContext(ReviewContext);
}

function isUnsent(comment: ReviewComment): boolean {
  return comment.author.role === "human" && comment.delivery === "unsent";
}

function agentSpokeLast(thread: ReviewThread): boolean {
  return thread.comments[thread.comments.length - 1]?.author.role === "agent";
}

/** Whether a person's comment is the viewer's: same id, or neither has one. */
export function isViewer(author: ReviewAuthor, reviewer: Reviewer | undefined): boolean {
  return author.role === "human" && (author.id ?? undefined) === reviewer?.id;
}

/** Who is attending, in a few words: one agent by name, several by count. */
export function agentsLabel(agents: readonly ReviewAgent[]): string | undefined {
  if (agents.length > 1) return `${agents.length} agents`;
  return agents[0]?.name;
}

export function sameTarget(a: ReviewTarget, b: ReviewTarget): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case "model":
      return true;
    case "node":
      return a.node === (b as typeof a).node;
    case "param":
      return a.param === (b as typeof a).param;
    case "texture":
      return a.texture === (b as typeof a).texture;
    case "slot":
      return a.node === (b as typeof a).node && a.slot === (b as typeof a).slot;
  }
}

/** A slot's thread counts toward its node, so a tree row shows it. */
function isAbout(thread: ReviewThread, target: ReviewTarget): boolean {
  if (sameTarget(thread.target, target)) return true;
  return target.kind === "node" && thread.target.kind === "slot" && thread.target.node === target.node;
}

export interface TargetLabel {
  icon: IconName;
  kind: string;
  name: string;
}

export function targetLabel(
  target: ReviewTarget,
  tree: TreeNode | undefined,
  params: readonly ParamInfo[],
): TargetLabel {
  switch (target.kind) {
    case "model":
      return { icon: "composite", kind: "Model", name: "Whole model" };
    case "node": {
      const node = tree && findNode(tree, target.node);
      return {
        icon: node ? kindIcons[node.kind] : "group",
        kind: "Node",
        name: node?.name ?? target.node,
      };
    }
    case "param":
      return {
        icon: "settings",
        kind: "Param",
        name: params.find((param) => param.id === target.param)?.name ?? target.param,
      };
    case "texture":
      return { icon: "part", kind: "Texture", name: target.texture };
    case "slot": {
      const node = tree && findNode(tree, target.node);
      return { icon: "link", kind: "Vertex slot", name: `${node?.name ?? target.node} · ${target.slot}` };
    }
  }
}

/**
 * How many open threads are about `target`; renders nothing at zero. Clicking
 * focuses the newest. `data-attention` marks an agent reply waiting to be read.
 */
export function ReviewBadge({ target, label }: { target: ReviewTarget; label: string }) {
  const review = useReview();
  if (!review) return null;
  const threads = review.review.threads.filter(
    (thread) => thread.status === "open" && isAbout(thread, target),
  );
  const newest = threads[threads.length - 1];
  if (!newest) return null;
  return (
    <button
      type="button"
      data-catchlight-review-badge=""
      data-attention={threads.some(agentSpokeLast) ? "" : undefined}
      data-working={threads.some((thread) => thread.holder) ? "" : undefined}
      aria-label={`${threads.length} open ${threads.length === 1 ? "thread" : "threads"} on ${label}`}
      title={`${threads.length} open ${threads.length === 1 ? "thread" : "threads"}`}
      onClick={(event) => {
        event.stopPropagation();
        review.focus(newest.thread);
      }}
    >
      <Icon name="comment" width="12" height="12" />
      {threads.length}
    </button>
  );
}

/** Starts a comment on `target`; renders nothing outside a [`ReviewProvider`]. */
export function ReviewCommentButton({ target, label }: { target: ReviewTarget; label: string }) {
  const review = useReview();
  if (!review) return null;
  return (
    <IconButton
      icon="comment"
      label={`Comment on ${label}`}
      data-catchlight-review-comment-on=""
      onClick={(event) => {
        event.stopPropagation();
        review.compose(target);
      }}
    />
  );
}

const PHASE_TEXT: Record<AgentPhase, string> = {
  absent: "No agent connected",
  away: "Agent away",
  listening: "Agent listening",
  working: "Agent working",
};

/** The agent's state and the send action, small enough for a toolbar. */
export function ReviewStatus({ onOpen }: { onOpen?: () => void }) {
  const review = useReview();
  if (!review?.session) return null;
  const { phase, agents, unsent, replied } = review;
  const who = agentsLabel(agents);
  return (
    <div data-catchlight-review-status="" data-phase={phase}>
      <button
        type="button"
        data-catchlight-review-agent=""
        title={who ? `${agents.map((agent) => agent.name).join(", ")}: ${PHASE_TEXT[phase].toLowerCase()}` : PHASE_TEXT[phase]}
        onClick={onOpen}
      >
        <i data-catchlight-review-dot="" />
        <span>{phase === "absent" ? "Review" : (who ?? "Agent")}</span>
        <small>{phase === "absent" ? "" : PHASE_TEXT[phase].replace("Agent ", "")}</small>
        {replied > 0 && <b data-catchlight-review-count="">{replied}</b>}
      </button>
      {unsent > 0 && (
        <button
          type="button"
          data-primary=""
          data-catchlight-review-send=""
          disabled={review.busy}
          onClick={() => void review.send()}
        >
          <Icon name="send" width="14" height="14" />
          Send · {unsent}
        </button>
      )}
    </div>
  );
}

function useLabels(session: Session): (target: ReviewTarget) => TargetLabel {
  const revision = useRevision(session);
  return useMemo(() => {
    const tree = session.tree();
    const params = session.params();
    return (target: ReviewTarget) => targetLabel(target, tree, params);
  }, [session, revision]);
}

export interface ReviewPanelProps {
  session: Session;
  /**
   * The host's way to bring an agent in, shown while none is listening or
   * working: instructions for the person's own agent, a control that starts a
   * hosted one, or both. How an agent reaches the editor is the host's to know.
   */
  connect?: ReactNode;
  /** A picture of what the person is looking at, offered as an attachment. */
  capture?: () => Promise<Blob>;
}

export function ReviewPanel({ session, connect, capture }: ReviewPanelProps) {
  const review = useReview();
  const { node, select } = useSelection();
  const editing = useEditing();
  const label = useLabels(session);
  const [tab, setTab] = useState<ThreadStatus>("open");
  const list = useRef<HTMLOListElement>(null);

  const focused = review?.focused;
  const focusedStatus = useLatest(
    review?.review.threads.find((thread) => thread.thread === focused)?.status,
  );
  // On focus only: resolving the focused thread must not drag the tab along.
  useEffect(() => {
    if (focusedStatus.current) setTab(focusedStatus.current);
  }, [focused, focusedStatus]);
  useEffect(() => {
    if (focused === undefined) return;
    list.current
      ?.querySelector(`[data-thread="${focused}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [focused, tab]);

  if (!review) return null;
  const { phase, agents, unsent, waiting } = review;
  const threads = review.review.threads
    .filter((thread) => thread.status === tab)
    .sort((a, b) => lastComment(b) - lastComment(a));
  const count = (status: ThreadStatus) =>
    review.review.threads.filter((thread) => thread.status === status).length;
  const selected: ReviewTarget = node ? { kind: "node", node } : { kind: "model" };
  const texture = node ? session.nodeInfo(node)?.texture : undefined;
  const offers: ReviewTarget[] = [
    ...(node ? [{ kind: "node", node } as const] : []),
    ...(texture ? [{ kind: "texture", texture } as const] : []),
    ...(editing?.param ? [{ kind: "param", param: editing.param } as const] : []),
    { kind: "model" },
  ];

  const locate = (thread: ReviewThread): void => {
    review.focus(thread.thread);
    if (thread.target_missing) return;
    const target = thread.target;
    if (target.kind === "node" || target.kind === "slot") select(target.node);
    else if (target.kind === "param") editing?.selectParam(target.param);
  };
  const restorePose = (thread: ReviewThread): void => {
    for (const { param, value } of thread.pose) session.setParam(param, value);
  };

  return (
    <section data-catchlight-review="" data-phase={phase} aria-label="Review">
      <header data-catchlight-review-header="">
        <div data-catchlight-review-agent="" data-phase={phase}>
          <i data-catchlight-review-dot="" />
          <span>
            <strong title={agents.map((agent) => agent.name).join(", ")}>
              {agentsLabel(agents) ?? "No agent connected"}
            </strong>
            <small>
              {phase === "absent"
                ? "Comments wait here until one joins"
                : phase === "working"
                  ? "Working on your comments"
                  : phase === "listening"
                    ? waiting > 0
                      ? "Picking up your comments…"
                      : "Listening for comments"
                    : waiting > 0
                      ? `${waiting} ${waiting === 1 ? "thread" : "threads"} waiting to be picked up`
                      : "Not listening right now"}
            </small>
          </span>
        </div>
        <button
          type="button"
          data-primary=""
          data-catchlight-review-send=""
          disabled={unsent === 0 || review.busy}
          title={unsent === 0 ? "Nothing new to send" : "Send your comments to the agent"}
          onClick={() => void review.send()}
        >
          <Icon name="send" width="14" height="14" />
          {unsent > 0 ? `Send to agent · ${unsent}` : "Send to agent"}
        </button>
      </header>
      <ReviewKeptNote review={review.review} />
      {connect && (phase === "absent" || phase === "away") && (
        <div data-catchlight-review-connect="">{connect}</div>
      )}
      <div data-catchlight-review-tabs="" role="tablist">
        {(["open", "resolved"] as const).map((status) => (
          <button
            key={status}
            type="button"
            role="tab"
            aria-selected={tab === status}
            data-active={tab === status ? "" : undefined}
            onClick={() => setTab(status)}
          >
            {status === "open" ? "Open" : "Resolved"}
            <small>{count(status)}</small>
          </button>
        ))}
      </div>
      {review.composing ? (
        <ReviewComposer
          key={JSON.stringify(review.composing)}
          target={review.composing}
          offers={offers}
          label={label}
          busy={review.busy}
          capture={capture}
          onError={review.fail}
          onTarget={review.compose}
          onCancel={() => review.compose(undefined)}
          onSubmit={(body, images) => review.open(review.composing!, body, images)}
        />
      ) : (
        <button
          type="button"
          data-catchlight-review-new=""
          onClick={() => review.compose(selected)}
        >
          <Icon name="comment" width="15" height="15" />
          <span>
            Comment on <strong>{label(selected).name}</strong>
          </span>
          <kbd>C</kbd>
        </button>
      )}
      <ol data-catchlight-review-threads="" ref={list}>
        {threads.map((thread) => (
          <ReviewThreadCard
            key={thread.thread}
            thread={thread}
            label={label(thread.target)}
            session={session}
            reviewer={review.reviewer}
            focused={thread.thread === focused}
            busy={review.busy}
            capture={capture}
            onError={review.fail}
            onLocate={() => locate(thread)}
            onPose={thread.pose.length ? () => restorePose(thread) : undefined}
            onReply={(body, images) => review.reply(thread.thread, body, images)}
            onStatus={(status) => void review.setStatus(thread.thread, status)}
            onDelete={() => void review.removeThread(thread.thread)}
            onDeleteComment={(comment) => void review.removeComment(thread.thread, comment)}
            onOpenVersion={review.openVersion}
          />
        ))}
      </ol>
      {threads.length === 0 && !review.composing && (
        <p data-catchlight-review-empty="">
          {tab === "open"
            ? "Select anything in the model and leave a comment. Send them together when you are ready."
            : "Resolved threads collect here."}
        </p>
      )}
    </section>
  );
}

function lastComment(thread: ReviewThread): number {
  return thread.comments[thread.comments.length - 1]?.comment ?? 0;
}

/** How long the comments last, said only when it is not "with the model". */
function ReviewKeptNote({ review }: { review: Review }) {
  if (review.kept === "unreadable")
    return (
      <p data-catchlight-review-kept="unreadable" role="status" title={review.kept_problem ?? undefined}>
        The comments saved with this model could not be read and were left untouched. New ones last
        until this editor closes.
      </p>
    );
  if (review.kept === "stored" || review.threads.length === 0) return null;
  return (
    <p data-catchlight-review-kept={review.kept} role="status">
      {review.kept === "unsaved"
        ? "Save the model to keep these comments."
        : "Comments last until this editor closes."}
    </p>
  );
}

/** A message for the person to hand their own agent, with a copy button. */
export function AgentPrompt({ children, prompt }: { children?: ReactNode; prompt: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div data-catchlight-agent-prompt="">
      {children}
      <code>{prompt}</code>
      <button
        type="button"
        onClick={() => {
          void navigator.clipboard?.writeText(prompt).then(() => setCopied(true));
        }}
      >
        <Icon name={copied ? "check" : "duplicate"} width="14" height="14" />
        {copied ? "Copied" : "Copy prompt"}
      </button>
    </div>
  );
}

// A controlled field needs `onChange`; the native input event is handled once
// through `onInput`.
function noop(): void {}

function submitOnModEnter(event: KeyboardEvent<HTMLTextAreaElement>, submit: () => void): void {
  if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
    event.preventDefault();
    submit();
  }
}

function ReviewComposer({
  target,
  offers,
  label,
  busy,
  capture,
  onError,
  onTarget,
  onCancel,
  onSubmit,
}: {
  target: ReviewTarget;
  offers: ReviewTarget[];
  label: (target: ReviewTarget) => TargetLabel;
  busy: boolean;
  capture: (() => Promise<Blob>) | undefined;
  onError: (cause: unknown) => void;
  onTarget: (target: ReviewTarget) => void;
  onCancel: () => void;
  onSubmit: (body: string, images: readonly Blob[]) => Promise<boolean>;
}) {
  const [body, setBody] = useState("");
  const attached = useAttachments(capture, onError);
  const choices = offers.some((offer) => sameTarget(offer, target)) ? offers : [target, ...offers];
  const empty = body.trim() === "" && attached.blobs.length === 0;
  const submit = (): void => {
    if (empty || busy) return;
    void onSubmit(body.trim(), attached.blobs);
  };
  return (
    <form
      data-catchlight-review-composer=""
      onDrop={attached.onDrop}
      onDragOver={allowImageDrop}
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div data-catchlight-review-targets="" role="radiogroup" aria-label="Comment on">
        {choices.map((choice) => {
          const text = label(choice);
          const active = sameTarget(choice, target);
          return (
            <button
              key={JSON.stringify(choice)}
              type="button"
              role="radio"
              aria-checked={active}
              data-active={active ? "" : undefined}
              title={`${text.kind}: ${text.name}`}
              onClick={() => onTarget(choice)}
            >
              <Icon name={text.icon} width="13" height="13" />
              {text.name}
            </button>
          );
        })}
      </div>
      <textarea
        autoFocus
        rows={3}
        aria-label={`Comment on ${label(target).name}`}
        placeholder="What should change?"
        value={body}
        onInput={(event) => setBody(event.currentTarget.value)}
        onChange={noop}
        onPaste={attached.onPaste}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onCancel();
          } else submitOnModEnter(event, submit);
        }}
      />
      <AttachmentStrip attached={attached} />
      <div data-catchlight-review-composer-actions="">
        <AttachButtons attached={attached} />
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" data-primary="" disabled={empty || busy}>
          Comment
        </button>
      </div>
    </form>
  );
}

const DELIVERY_TEXT = { unsent: "Not sent", sent: "Sent", delivered: "Seen" } as const;

function ReviewThreadCard({
  thread,
  label,
  session,
  reviewer,
  focused,
  busy,
  capture,
  onError,
  onLocate,
  onPose,
  onReply,
  onStatus,
  onDelete,
  onDeleteComment,
  onOpenVersion,
}: {
  thread: ReviewThread;
  label: TargetLabel;
  session: Session;
  reviewer: Reviewer | undefined;
  focused: boolean;
  busy: boolean;
  capture: (() => Promise<Blob>) | undefined;
  onError: (cause: unknown) => void;
  onLocate: () => void;
  onPose: (() => void) | undefined;
  onReply: (body: string, images: readonly Blob[]) => Promise<boolean>;
  onStatus: (status: ThreadStatus) => void;
  onDelete: () => void;
  onDeleteComment: (comment: number) => void;
  onOpenVersion: ((version: number) => void) | undefined;
}) {
  const [reply, setReply] = useState("");
  const [replying, setReplying] = useState(false);
  const attached = useAttachments(capture, onError);
  const resolved = thread.status === "resolved";
  const empty = reply.trim() === "" && attached.blobs.length === 0;
  const submit = (): void => {
    if (empty || busy) return;
    void onReply(reply.trim(), attached.blobs).then((sent) => {
      if (!sent) return;
      setReply("");
      attached.clear();
      setReplying(false);
    });
  };
  return (
    <li
      data-catchlight-review-thread=""
      data-thread={thread.thread}
      data-status={thread.status}
      data-focused={focused ? "" : undefined}
      data-working={thread.holder ? "" : undefined}
      data-attention={!resolved && agentSpokeLast(thread) ? "" : undefined}
      data-missing={thread.target_missing ? "" : undefined}
    >
      <header>
        <button
          type="button"
          data-catchlight-review-target=""
          title={thread.target_missing ? `${label.name} is no longer in the model` : `Show ${label.name}`}
          onClick={onLocate}
        >
          <Icon name={label.icon} width="14" height="14" />
          <span>{label.name}</span>
          <small>{thread.target_missing ? "removed" : label.kind}</small>
        </button>
        {onPose && (
          <IconButton icon="reset" label="Restore the pose this was written at" onClick={onPose} />
        )}
        <IconButton icon="trash" label="Delete thread" disabled={busy} onClick={onDelete} />
      </header>
      {thread.comments.map((comment) => (
        <article
          key={comment.comment}
          data-catchlight-review-comment=""
          data-role={comment.author.role}
          data-delivery={comment.delivery}
        >
          <header>
            {comment.author.role === "agent" ? (
              <>
                <Icon name="spark" width="13" height="13" />
                <strong>{comment.author.name}</strong>
              </>
            ) : (
              <>
                <strong>
                  {isViewer(comment.author, reviewer) ? "You" : (comment.author.name ?? "Reviewer")}
                </strong>
                <small data-catchlight-review-delivery="">{DELIVERY_TEXT[comment.delivery]}</small>
              </>
            )}
            {isUnsent(comment) && (
              <IconButton
                icon="close"
                label="Delete comment"
                disabled={busy}
                onClick={() => onDeleteComment(comment.comment)}
              />
            )}
          </header>
          {comment.body && <p>{comment.body}</p>}
          {comment.version != null && (
            <VersionChip version={comment.version} onOpen={onOpenVersion} />
          )}
          {!!comment.images?.length && <ReviewImages session={session} images={comment.images} />}
        </article>
      ))}
      {thread.holder && (
        <p data-catchlight-review-working="" role="status" title={thread.holder.id}>
          <Icon name="spark" width="13" height="13" />
          {thread.holder.name} is working on this…
        </p>
      )}
      {replying ? (
        <form
          data-catchlight-review-reply=""
          onDrop={attached.onDrop}
          onDragOver={allowImageDrop}
          onSubmit={(event) => {
            event.preventDefault();
            submit();
          }}
        >
          <textarea
            autoFocus
            rows={2}
            aria-label={`Reply on ${label.name}`}
            placeholder="Reply…"
            value={reply}
            onInput={(event) => setReply(event.currentTarget.value)}
            onChange={noop}
            onPaste={attached.onPaste}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setReplying(false);
              } else submitOnModEnter(event, submit);
            }}
          />
          <AttachmentStrip attached={attached} />
          <div data-catchlight-review-composer-actions="">
            <AttachButtons attached={attached} />
            <button type="button" onClick={() => setReplying(false)}>
              Cancel
            </button>
            <button type="submit" data-primary="" disabled={empty || busy}>
              Reply
            </button>
          </div>
        </form>
      ) : (
        <footer>
          <button type="button" data-catchlight-review-reply-open="" onClick={() => setReplying(true)}>
            Reply
          </button>
          <button
            type="button"
            data-catchlight-review-resolve=""
            disabled={busy}
            onClick={() => onStatus(resolved ? "open" : "resolved")}
          >
            <Icon name={resolved ? "undo" : "check"} width="14" height="14" />
            {resolved ? "Reopen" : "Resolve"}
          </button>
        </footer>
      )}
    </li>
  );
}

/** What the editor stores; anything else a clipboard offers is refused. */
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp"];
export const MAX_COMMENT_IMAGES = 4;

interface Attachments {
  blobs: readonly Blob[];
  urls: readonly string[];
  full: boolean;
  add(files: Iterable<Blob>): void;
  replace(index: number, blob: Blob): void;
  remove(index: number): void;
  fail(cause: unknown): void;
  clear(): void;
  capture: (() => void) | undefined;
  onPaste(event: ClipboardEvent<HTMLElement>): void;
  onDrop(event: DragEvent<HTMLElement>): void;
}

function useAttachments(
  capture: (() => Promise<Blob>) | undefined,
  onError: (cause: unknown) => void,
): Attachments {
  const [held, setHeld] = useState<{ blob: Blob; url: string }[]>([]);
  const latest = useLatest(held);
  useEffect(
    () => () => {
      for (const { url } of latest.current) URL.revokeObjectURL(url);
    },
    [latest],
  );
  return useMemo<Attachments>(() => {
    const add = (files: Iterable<Blob>): void => {
      const images = [...files];
      const usable = images.filter((file) => IMAGE_TYPES.includes(file.type));
      if (usable.length < images.length) onError(new Error("Attach PNG, JPEG or WebP images."));
      const room = MAX_COMMENT_IMAGES - latest.current.length;
      if (usable.length > room)
        onError(new Error(`A comment holds up to ${MAX_COMMENT_IMAGES} images.`));
      const taken = usable.slice(0, Math.max(0, room));
      if (taken.length === 0) return;
      setHeld((was) => [
        ...was,
        ...taken.map((blob) => ({ blob, url: URL.createObjectURL(blob) })),
      ]);
    };
    return {
      blobs: held.map((each) => each.blob),
      urls: held.map((each) => each.url),
      full: held.length >= MAX_COMMENT_IMAGES,
      add,
      fail: onError,
      replace(index, blob) {
        setHeld((was) =>
          was.map((each, at) => {
            if (at !== index) return each;
            URL.revokeObjectURL(each.url);
            return { blob, url: URL.createObjectURL(blob) };
          }),
        );
      },
      remove(index) {
        setHeld((was) => {
          const gone = was[index];
          if (gone) URL.revokeObjectURL(gone.url);
          return was.filter((_, at) => at !== index);
        });
      },
      clear() {
        setHeld((was) => {
          for (const { url } of was) URL.revokeObjectURL(url);
          return [];
        });
      },
      capture: capture && (() => void capture().then((blob) => add([blob]), onError)),
      onPaste(event) {
        const files = [...event.clipboardData.files].filter((file) => file.type.startsWith("image/"));
        if (files.length === 0) return;
        event.preventDefault();
        add(files);
      },
      onDrop(event) {
        const files = [...event.dataTransfer.files].filter((file) => file.type.startsWith("image/"));
        if (files.length === 0) return;
        event.preventDefault();
        event.stopPropagation();
        add(files);
      },
    };
  }, [held, latest, capture, onError]);
}

function allowImageDrop(event: DragEvent<HTMLElement>): void {
  if (!event.dataTransfer.types.includes("Files")) return;
  event.preventDefault();
  event.stopPropagation();
}

function AttachmentStrip({ attached }: { attached: Attachments }) {
  const [marking, setMarking] = useState<number>();
  const blob = marking === undefined ? undefined : attached.blobs[marking];
  if (attached.urls.length === 0) return null;
  return (
    <ul data-catchlight-review-attachments="">
      {attached.urls.map((url, index) => (
        <li key={url}>
          <button
            type="button"
            data-catchlight-review-markup-open=""
            title="Mark up: box, arrow, draw or crop"
            aria-label={`Mark up attachment ${index + 1}`}
            onClick={() => setMarking(index)}
          >
            <img src={url} alt="" />
          </button>
          <IconButton
            icon="close"
            label={`Remove attachment ${index + 1}`}
            onClick={() => attached.remove(index)}
          />
        </li>
      ))}
      {blob && marking !== undefined && (
        <ImageMarkup
          image={blob}
          onCancel={() => setMarking(undefined)}
          onError={attached.fail}
          onDone={(marked) => {
            attached.replace(marking, marked);
            setMarking(undefined);
          }}
        />
      )}
    </ul>
  );
}

function AttachButtons({ attached }: { attached: Attachments }) {
  const file = useRef<HTMLInputElement>(null);
  return (
    <span data-catchlight-review-attach="">
      <input
        ref={file}
        type="file"
        hidden
        multiple
        accept={IMAGE_TYPES.join(",")}
        aria-label="Attach images"
        onChange={(event) => {
          attached.add(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
        }}
      />
      <IconButton
        icon="part"
        label="Attach an image (or paste one)"
        disabled={attached.full}
        onClick={() => file.current?.click()}
      />
      {attached.capture && (
        <IconButton
          icon="fit"
          label="Attach a capture of the canvas"
          data-catchlight-review-capture=""
          disabled={attached.full}
          onClick={attached.capture}
        />
      )}
    </span>
  );
}

/** The saved version a comment points at; opens it where the host can show it. */
function VersionChip({ version, onOpen }: { version: number; onOpen: ((version: number) => void) | undefined }) {
  if (!onOpen)
    return (
      <span data-catchlight-version-chip="" title={`Points at saved version ${version}`}>
        v{version}
      </span>
    );
  return (
    <button
      type="button"
      data-catchlight-version-chip=""
      title={`Open saved version ${version}`}
      aria-label={`Open version ${version}`}
      onClick={() => onOpen(version)}
    >
      <Icon name="history" width="12" height="12" />v{version}
    </button>
  );
}

function ReviewImages({ session, images }: { session: Session; images: readonly ReviewImage[] }) {
  const [open, setOpen] = useState<string>();
  return (
    <ul data-catchlight-review-images="">
      {images.map((image) => (
        <li key={image.image}>
          <ReviewImageThumb session={session} image={image} onOpen={setOpen} />
        </li>
      ))}
      {open && (
        <Modal title="Attached image" wide onClose={() => setOpen(undefined)}>
          <img data-catchlight-review-image-full="" src={open} alt="Attached image, full size" />
        </Modal>
      )}
    </ul>
  );
}

function ReviewImageThumb({
  session,
  image,
  onOpen,
}: {
  session: Session;
  image: ReviewImage;
  onOpen: (url: string) => void;
}) {
  const [url, setUrl] = useState<string>();
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let live = true;
    session.reviewImageUrl(image).then(
      (made) => {
        if (live) setUrl(made);
      },
      () => {
        if (live) setFailed(true);
      },
    );
    return () => {
      live = false;
    };
  }, [session, image]);
  if (failed) return <small data-catchlight-review-image-missing="">Image unavailable</small>;
  return (
    <button
      type="button"
      data-catchlight-review-image=""
      data-image={image.image}
      disabled={!url}
      aria-label="Open attached image"
      onClick={() => url && onOpen(url)}
    >
      {url && <img src={url} alt="" />}
    </button>
  );
}
