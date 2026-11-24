/**
 * A session's review: threads a person leaves on things in the model for a
 * listening agent. Comments stay `unsent` until the person sends the review;
 * the agent's replies arrive through `review_changed`.
 */

import type {
  ParamInfo,
  Review,
  ReviewAgent,
  ReviewComment,
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
import type { KeyboardEvent, ReactNode } from "react";

import { kindIcons } from "./authoring.js";
import { useLatest } from "./latest.js";
import type { ErrorHandler } from "./authoring.js";
import { Icon, IconButton } from "./controls.js";
import type { IconName } from "./controls.js";
import { useEditing } from "./editing.js";
import { useEditor } from "./editor-context.js";
import { findNode } from "./node-actions.js";
import { readPose } from "./pose.js";
import { useSelection } from "./presence.js";
import { useRevision } from "./replica.js";

const NO_REVIEW: Review = { threads: [] };
const neverChanges = (): (() => void) => () => {};
const noReview = (): Review => NO_REVIEW;

/**
 * `absent`: no agent has been heard from. `away`: one was, and it is neither
 * waiting nor holding a thread. `working` wins over `listening`.
 */
export type AgentPhase = "absent" | "away" | "listening" | "working";

export interface ReviewState {
  session: Session | undefined;
  review: Review;
  agent: ReviewAgent | undefined;
  phase: AgentPhase;
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
  open(target: ReviewTarget, body: string): Promise<boolean>;
  reply(thread: ThreadId, body: string): Promise<boolean>;
  setStatus(thread: ThreadId, status: ThreadStatus): Promise<boolean>;
  removeThread(thread: ThreadId): Promise<boolean>;
  removeComment(thread: ThreadId, comment: number): Promise<boolean>;
  send(): Promise<boolean>;
}

const ReviewContext = createContext<ReviewState | undefined>(undefined);

export interface ReviewProviderProps {
  session: Session | undefined;
  onError: ErrorHandler;
  /** A thread was focused or a comment started from outside the panel. */
  onAttention?: () => void;
  children?: ReactNode;
}

export function ReviewProvider({ session, onError, onAttention, children }: ReviewProviderProps) {
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
  const live = useRef({ session, onError, onAttention });
  live.current = { session, onError, onAttention };

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
      open: (target: ReviewTarget, body: string) =>
        run(async (session) => {
          const reply = await session.sendReview({
            cmd: "review_thread_add",
            target,
            author: { role: "human" },
            body,
            pose: readPose(session),
          });
          setAim({
            session,
            ...(reply.result === "review_thread" ? { thread: reply.thread.thread } : {}),
          });
        }),
      reply: (thread: ThreadId, body: string) =>
        run((session) =>
          session.sendReview({ cmd: "review_comment_add", thread, author: { role: "human" }, body }),
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
    const agent = review.agent ?? undefined;
    return {
      session,
      review,
      agent,
      phase: open.some((thread) => thread.working)
        ? "working"
        : agent?.listening
          ? "listening"
          : agent
            ? "away"
            : "absent",
      unsent: open.reduce((n, thread) => n + thread.comments.filter(isUnsent).length, 0),
      waiting: open.filter((thread) => thread.comments.some((c) => c.delivery === "sent")).length,
      replied: open.filter(agentSpokeLast).length,
      busy: pending > 0,
      composing: here.target,
      focused: here.thread,
      ...actions,
    };
  }, [session, review, pending, here.target, here.thread, actions]);

  return <ReviewContext.Provider value={value}>{children}</ReviewContext.Provider>;
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
      return { icon: "link", kind: "Slot", name: `${node?.name ?? target.node} · ${target.slot}` };
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
      data-working={threads.some((thread) => thread.working) ? "" : undefined}
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
  const { phase, agent, unsent, replied } = review;
  return (
    <div data-catchlight-review-status="" data-phase={phase}>
      <button
        type="button"
        data-catchlight-review-agent=""
        title={agent ? `${agent.name}: ${PHASE_TEXT[phase].toLowerCase()}` : PHASE_TEXT[phase]}
        onClick={onOpen}
      >
        <i data-catchlight-review-dot="" />
        <span>{phase === "absent" ? "Review" : (agent?.name ?? "Agent")}</span>
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
}

export function ReviewPanel({ session }: ReviewPanelProps) {
  const review = useReview();
  const editor = useEditor();
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
  const { phase, agent, unsent, waiting } = review;
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
            <strong>{agent?.name ?? "No agent connected"}</strong>
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
      {phase === "absent" && <ReviewConnect session={session} connected={editor.backendKind() === "connected"} />}
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
          onTarget={review.compose}
          onCancel={() => review.compose(undefined)}
          onSubmit={(body) => review.open(review.composing!, body)}
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
            focused={thread.thread === focused}
            busy={review.busy}
            onLocate={() => locate(thread)}
            onPose={thread.pose.length ? () => restorePose(thread) : undefined}
            onReply={(body) => review.reply(thread.thread, body)}
            onStatus={(status) => void review.setStatus(thread.thread, status)}
            onDelete={() => void review.removeThread(thread.thread)}
            onDeleteComment={(comment) => void review.removeComment(thread.thread, comment)}
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

function ReviewConnect({ session, connected }: { session: Session; connected: boolean }) {
  const [copied, setCopied] = useState(false);
  const prompt = `Join catchlight session ${session.id} and work through my review comments: run \`catchlight-editor guide\`, then \`catchlight-editor review watch --session ${session.id}\`.`;
  if (!connected)
    return (
      <p data-catchlight-review-connect="">
        This model lives in this tab. To bring an agent in, open it with{" "}
        <code>catchlight-editor</code> so both of you share the session.
      </p>
    );
  return (
    <div data-catchlight-review-connect="">
      <p>Bring an agent into this session by telling it:</p>
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
  onTarget,
  onCancel,
  onSubmit,
}: {
  target: ReviewTarget;
  offers: ReviewTarget[];
  label: (target: ReviewTarget) => TargetLabel;
  busy: boolean;
  onTarget: (target: ReviewTarget) => void;
  onCancel: () => void;
  onSubmit: (body: string) => Promise<boolean>;
}) {
  const [body, setBody] = useState("");
  const choices = offers.some((offer) => sameTarget(offer, target)) ? offers : [target, ...offers];
  const submit = (): void => {
    if (body.trim() === "" || busy) return;
    void onSubmit(body.trim());
  };
  return (
    <form
      data-catchlight-review-composer=""
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
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onCancel();
          } else submitOnModEnter(event, submit);
        }}
      />
      <div data-catchlight-review-composer-actions="">
        <small>
          <kbd>⌘ ↵</kbd> to add
        </small>
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" data-primary="" disabled={body.trim() === "" || busy}>
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
  focused,
  busy,
  onLocate,
  onPose,
  onReply,
  onStatus,
  onDelete,
  onDeleteComment,
}: {
  thread: ReviewThread;
  label: TargetLabel;
  focused: boolean;
  busy: boolean;
  onLocate: () => void;
  onPose: (() => void) | undefined;
  onReply: (body: string) => Promise<boolean>;
  onStatus: (status: ThreadStatus) => void;
  onDelete: () => void;
  onDeleteComment: (comment: number) => void;
}) {
  const [reply, setReply] = useState("");
  const [replying, setReplying] = useState(false);
  const resolved = thread.status === "resolved";
  const submit = (): void => {
    if (reply.trim() === "" || busy) return;
    void onReply(reply.trim()).then((sent) => {
      if (!sent) return;
      setReply("");
      setReplying(false);
    });
  };
  return (
    <li
      data-catchlight-review-thread=""
      data-thread={thread.thread}
      data-status={thread.status}
      data-focused={focused ? "" : undefined}
      data-working={thread.working ? "" : undefined}
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
                <strong>You</strong>
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
          <p>{comment.body}</p>
        </article>
      ))}
      {thread.working && (
        <p data-catchlight-review-working="" role="status">
          <Icon name="spark" width="13" height="13" />
          {thread.working} is working on this…
        </p>
      )}
      {replying ? (
        <form
          data-catchlight-review-reply=""
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
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setReplying(false);
              } else submitOnModEnter(event, submit);
            }}
          />
          <div data-catchlight-review-composer-actions="">
            <button type="button" onClick={() => setReplying(false)}>
              Cancel
            </button>
            <button type="submit" data-primary="" disabled={reply.trim() === "" || busy}>
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
