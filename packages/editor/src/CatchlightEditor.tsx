/**
 * The editor, assembled: a toolbar, two panels, a canvas and a status line.
 *
 * Layer 4, and the only layer with an opinion about how the editor *looks*.
 * What it is allowed to contain is exactly this: layout, and the wiring one
 * screen needs to hold together — which document is the current one, and what
 * a failed call says. Every behaviour is a part or a hook from
 * `@catchlight/react`; nothing here reads a replica, builds a command, or
 * touches the GPU.
 *
 * **The current session is this component's one piece of state.** `Session` is
 * an object with a replica behind it, and `useSessions` reports `SessionInfo`
 * — a description. Turning one into the other is `attachSession`, which is a
 * round trip, so the choice cannot be derived during render. The first
 * document the editor lists wins until something is opened or picked, which
 * is what makes a page that was handed a model on the command line come up
 * showing it.
 *
 * **A failure is shown, never swallowed.** The parts report through `onError`
 * callbacks and the editor's promises reject; both land in one line at the
 * bottom, because a browser console is not part of the product.
 *
 * **The camera is held here, not in the canvas.** The viewport frames a model
 * by itself the first time it draws one, but "Fit" is a toolbar button and the
 * toolbar is not inside the stage — so the one piece of view state that two
 * cells share lives in the component that contains both.
 *
 * **The stage is never unmounted.** The grid's cells are rendered whether or
 * not a document is open; with none, the panels are empty and the canvas
 * draws nothing under a hint. That is what keeps one canvas element alive for
 * the life of the screen — on the WebGL2 tier the device draws on exactly the
 * canvas it was acquired from, so a stage that came and went with its
 * documents could never draw a second one. The presence provider sits above
 * the cells for the same reason, and tolerates having no session.
 *
 * **Closing the current document moves the screen off it first.** The panels
 * under it read its replica, and the close frees that replica — so the next
 * document is attached and made current (or the session is dropped, when it
 * was the last), the commit that does so re-attaches the viewport on the same
 * canvas, and only an effect after that commit sends the close. The closed
 * id is remembered so the automatic pick below does not take it back off a
 * list that has not refreshed yet.
 */

import type { Editor, ParamId, Session, SessionId, SessionInfo } from "@catchlight/core";
import {
  BindingGrid,
  EditorProvider,
  FileOpen,
  FileSave,
  Inspector,
  NodeTree,
  ParamAdd,
  ParamFields,
  ParamKeys,
  ParamList,
  ParamSlider,
  PresenceProvider,
  SessionList,
  Viewport,
  downloadName,
  useEditor,
  useFileSave,
  useNodeDrag,
  useParams,
  usePosePublisher,
  useResetPose,
  useRevision,
  useSelection,
  useSessions,
  useViewportCamera,
} from "@catchlight/react";
import type { SaveOutcome, ViewportCamera } from "@catchlight/react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

export interface CatchlightEditorProps {
  editor: Editor;
  /** Added to the `catchlight` class, for a host theming one instance. */
  className?: string;
}

export function CatchlightEditor({ editor, className }: CatchlightEditorProps): ReactNode {
  return (
    <div className={className ? `catchlight ${className}` : "catchlight"}>
      <EditorProvider editor={editor}>
        <Shell />
      </EditorProvider>
    </div>
  );
}

/** Everything under the provider: the grid's five cells, and what fills them. */
function Shell(): ReactNode {
  const editor = useEditor();
  const { sessions } = useSessions();
  const [session, setSession] = useState<Session | undefined>(undefined);
  const [problem, setProblem] = useState<string | undefined>(undefined);
  /** What the last save did, for the status line. Cleared by the next switch. */
  const [notice, setNotice] = useState<string | undefined>(undefined);
  /** Sessions this screen closed, which the automatic pick must not take back. */
  const dismissed = useRef(new Set<SessionId>());
  const view = useViewportCamera();

  const opened = useCallback((next: Session): void => {
    setProblem(undefined);
    setNotice(undefined);
    setSession(next);
  }, []);

  const failed = useCallback((cause: unknown): void => {
    setProblem(describe(cause));
  }, []);

  const choose = useCallback(
    (info: SessionInfo): void => {
      void editor.attachSession(info).then(opened, failed);
    },
    [editor, opened, failed],
  );

  const create = useCallback((): void => {
    void editor.newDocument().then(opened, failed);
  }, [editor, opened, failed]);

  /** The document to close once the screen has moved off it. */
  const [closing, setClosing] = useState<SessionId | undefined>(undefined);

  const close = useCallback(
    (info: SessionInfo): void => {
      if (session?.id !== info.session) {
        void editor.closeDocument(info.session).then(() => setProblem(undefined), failed);
        return;
      }
      const next = sessions.find(
        (each) => each.session !== info.session && !dismissed.current.has(each.session),
      );
      if (!next) {
        dismissed.current.add(info.session);
        setSession(undefined);
        setNotice(undefined);
        setClosing(info.session);
        return;
      }
      void editor.attachSession(next).then((attached) => {
        dismissed.current.add(info.session);
        setProblem(undefined);
        setNotice(undefined);
        setSession(attached);
        setClosing(info.session);
      }, failed);
    },
    [editor, session, sessions, failed],
  );

  // After the commit that moved the screen off the closing document: nothing
  // under the stage reads its replica any more, and the viewport was disposed
  // and re-attached on the same canvas rather than replaced.
  useEffect(() => {
    if (closing === undefined) return;
    setClosing(undefined);
    void editor.closeDocument(closing).then(() => setProblem(undefined), failed);
  }, [closing, editor, failed]);

  // Whatever the editor already had open: a model named on a server's command
  // line, or a session an agent opened over the socket.
  useEffect(() => {
    if (session !== undefined) return;
    const first = sessions.find((each) => !dismissed.current.has(each.session));
    if (!first) return;
    let live = true;
    void editor.attachSession(first).then(
      (attached) => {
        if (live) setSession(attached);
      },
      (cause: unknown) => {
        if (live) setProblem(describe(cause));
      },
    );
    return () => {
      live = false;
    };
  }, [editor, session, sessions]);

  const saved = useCallback((outcome: SaveOutcome): void => {
    setProblem(undefined);
    setNotice(
      outcome.downloaded ? `downloaded ${downloadName(outcome.key)}` : `saved to ${outcome.key}`,
    );
  }, []);

  const fit = useCallback((): void => {
    if (session) view.fit(session);
  }, [session, view]);

  const info = session ? sessions.find((each) => each.session === session.id) : undefined;

  return (
    <>
      <header data-catchlight-toolbar="">
        <button type="button" data-catchlight-new="" onClick={create}>
          New
        </button>
        <label data-catchlight-open="">
          <span>Open .clm</span>
          <FileOpen.Root onOpened={opened} onError={failed} />
        </label>
        {session ? (
          <SaveTools session={session} info={info} onSaved={saved} onError={failed} />
        ) : (
          <button type="button" data-catchlight-save="" disabled>
            Save
          </button>
        )}
        <button
          type="button"
          data-catchlight-fit=""
          disabled={session === undefined}
          onClick={fit}
        >
          Fit
        </button>
        <span data-catchlight-zoom="" title="Zoom, relative to the fitted view">
          {zoomLabel(view.zoom)}
        </span>
        <button
          type="button"
          data-catchlight-camera-reset=""
          disabled={session === undefined}
          onClick={fit}
        >
          Reset
        </button>
        {session ? (
          <ResetPose session={session} />
        ) : (
          <button type="button" data-catchlight-pose-reset="" disabled>
            Reset pose
          </button>
        )}
      </header>
      <PresenceProvider session={session}>
        <nav data-catchlight-panel="left">
          <Documents onSelect={choose} onClose={close} current={session?.id} />
          {session ? (
            <section data-catchlight-section="" data-grow="">
              <h2 data-catchlight-heading="">Nodes</h2>
              <NodeTree.Actions session={session} onError={failed} />
              <NodeTree.Root session={session} onError={failed} />
            </section>
          ) : null}
        </nav>
        <Stage session={session} view={view} />
        <aside data-catchlight-panel="right">
          {session ? (
            <>
              <section data-catchlight-section="">
                <h2 data-catchlight-heading="">Inspector</h2>
                <Inspector.Root session={session} onError={failed} />
              </section>
              <section data-catchlight-section="" data-grow="">
                <h2 data-catchlight-heading="">Params</h2>
                <ParamAdd.Root session={session} onError={failed} />
                {/* The default row is the slider alone, and a column of
                    unlabelled sliders names nothing. */}
                <ParamList.Root session={session}>
                  {(param) => (
                    <>
                      <ParamFields.Root session={session} param={param} onError={failed} />
                      <ParamSlider.Root session={session} param={param} />
                      <ParamKeys.Root session={session} param={param} onError={failed} />
                    </>
                  )}
                </ParamList.Root>
              </section>
              <section data-catchlight-section="">
                <h2 data-catchlight-heading="">Bindings</h2>
                <Bindings session={session} onError={failed} />
              </section>
            </>
          ) : null}
        </aside>
        {session ? (
          <>
            <PosePublisher session={session} />
            <Status session={session} info={info} notice={notice} problem={problem} />
          </>
        ) : (
          <footer data-catchlight-status="" role="status">
            <span data-catchlight-status-item="">no document</span>
            <Environment />
            <Problem problem={problem} />
          </footer>
        )}
      </PresenceProvider>
    </>
  );
}

/**
 * What the selected node's bindings on one param look like.
 *
 * The param is picked here rather than taken from the selection because a node
 * is usually driven by several, and a panel that showed all of them at once
 * would be a wall of grids. Which node is a different question, and the
 * selection already answers it — so this reads the selection and lets a person
 * choose the axis.
 */
function Bindings({
  session,
  onError,
}: {
  session: Session;
  onError: (cause: unknown) => void;
}): ReactNode {
  const { node } = useSelection();
  const params = useParams(session);
  const [param, setParam] = useState<ParamId | undefined>(undefined);
  // Whatever is still there: a param deleted from under this falls back to the
  // first one rather than leaving the panel pointing at nothing.
  const showing = params.some((each) => each.id === param) ? param : params[0]?.id;

  if (!node) {
    return <p data-catchlight-empty="">Select a node to see what drives it.</p>;
  }
  if (params.length === 0) {
    return <p data-catchlight-empty="">This document has no params yet.</p>;
  }

  return (
    <>
      <select
        data-catchlight-binding-param=""
        aria-label="Param"
        value={showing ?? ""}
        onChange={(event) => setParam(event.currentTarget.value)}
      >
        {params.map((each) => (
          <option key={each.id} value={each.id}>
            {each.name}
          </option>
        ))}
      </select>
      <BindingGrid.Root session={session} node={node} param={showing} onError={onError} />
    </>
  );
}

/**
 * Save and Save As, for the document that is open.
 *
 * Its own component because the hook wants a session, and the toolbar exists
 * before there is one. A document that was never saved has no path to save
 * to, so a plain Save on it goes under its title — which is what a person who
 * pressed New and then Save expects to find in their downloads.
 */
function SaveTools({
  session,
  info,
  onSaved,
  onError,
}: {
  session: Session;
  info: SessionInfo | undefined;
  onSaved: (outcome: SaveOutcome) => void;
  onError: (cause: unknown) => void;
}): ReactNode {
  const { save } = useFileSave(session);
  const handleSave = (): void => {
    void save(info?.file ? undefined : info?.title).then(onSaved, onError);
  };
  return (
    <>
      <button type="button" data-catchlight-save="" onClick={handleSave}>
        Save
      </button>
      {/* Keyed so the name input starts over with each document. */}
      <FileSave.Root
        key={session.id}
        session={session}
        defaultName={info?.file ? downloadName(info.file) : undefined}
        onSaved={onSaved}
        onError={onError}
      />
    </>
  );
}

function ResetPose({ session }: { session: Session }): ReactNode {
  const reset = useResetPose(session);
  return (
    <button type="button" data-catchlight-pose-reset="" onClick={reset}>
      Reset pose
    </button>
  );
}

/** Every document the editor has open, this tab's and everyone else's. */
function Documents({
  onSelect,
  onClose,
  current,
}: {
  onSelect: (info: SessionInfo) => void;
  onClose: (info: SessionInfo) => void;
  current: SessionId | undefined;
}): ReactNode {
  return (
    <section data-catchlight-section="">
      <h2 data-catchlight-heading="">Documents</h2>
      <SessionList.Root onSelect={onSelect} onClose={onClose} current={current} />
    </section>
  );
}

/**
 * The canvas and the one gesture layered over it.
 *
 * Mounted with or without a document: the canvas element has to be the same
 * one for the life of the screen (see the header), so with no session it is
 * drawn on by nothing and the hint sits over it. The drag is here rather than
 * in `Shell` because it needs the selection, which the provider above the
 * cells supplies.
 */
function Stage({
  session,
  view,
}: {
  session: Session | undefined;
  view: ViewportCamera;
}): ReactNode {
  const { node } = useSelection();
  const drag = useNodeDrag(session, node);
  return (
    <div
      data-catchlight-stage=""
      data-empty={session ? undefined : ""}
      data-dragging={drag.dragging ? "" : undefined}
    >
      <Viewport.Root
        session={session}
        camera={view.camera}
        onCameraChange={view.onCameraChange}
        onFit={view.onFit}
        onResize={view.onResize}
        {...drag.handlers}
      />
      {session ? null : (
        <p data-catchlight-stage-hint="">
          No document open. Pick a .clm above, or choose one on the left.
        </p>
      )}
    </div>
  );
}

/**
 * Publishes the pose of the open document through the provider. A component
 * rather than a hook call in `Stage`, because the stage also exists with no
 * document and the publisher wants one.
 */
function PosePublisher({ session }: { session: Session }): ReactNode {
  usePosePublisher(session);
  return null;
}

function Status({
  session,
  info,
  notice,
  problem,
}: {
  session: Session;
  info: SessionInfo | undefined;
  notice: string | undefined;
  problem: string | undefined;
}): ReactNode {
  const revision = useRevision(session);
  const { node } = useSelection();
  return (
    <footer data-catchlight-status="" role="status">
      <span data-catchlight-status-item="">{info?.title ?? "untitled"}</span>
      <span data-catchlight-status-item="" data-catchlight-file="">
        {info?.file ?? "not saved yet"}
      </span>
      <span data-catchlight-status-item="">rev {revision}</span>
      {info?.dirty ? (
        <span data-catchlight-status-item="" data-dirty="">
          unsaved
        </span>
      ) : null}
      <span data-catchlight-status-item="">{node ? `selected ${node}` : "nothing selected"}</span>
      {notice ? (
        <span data-catchlight-status-item="" data-catchlight-notice="">
          {notice}
        </span>
      ) : null}
      <Environment />
      <Problem problem={problem} />
    </footer>
  );
}

/** The zoom as a person reads it: relative to the fit, once there has been one. */
function zoomLabel(zoom: number | undefined): string {
  if (zoom === undefined) return "–";
  return `${Math.round(zoom * 100)}%`;
}

/**
 * Where the document is, and which graphics API is drawing it.
 *
 * Both are things a person cannot otherwise tell by looking, and both change
 * what a bug report means: an in-tab editor and a connected one fail
 * differently, and so do WebGPU and the WebGL2 fallback.
 */
function Environment(): ReactNode {
  const editor = useEditor();
  const gpu = useGpuBackend(editor);
  return (
    <>
      <span data-catchlight-status-item="" data-catchlight-backend="">
        {editor.backendKind()}
      </span>
      <span data-catchlight-status-item="" data-catchlight-gpu="">
        {gpu ?? "no device"}
      </span>
    </>
  );
}

/**
 * Which graphics API this tab came up on, once a canvas has asked for one.
 *
 * A device is acquired at the first viewport and never swapped, so the
 * subscription fires once and the state settles for the life of the editor.
 */
function useGpuBackend(editor: Editor): string | undefined {
  const [backend, setBackend] = useState<string | undefined>(() => editor.gpuBackend());
  useEffect(() => {
    setBackend(editor.gpuBackend());
    return editor.onGpuChanged(() => setBackend(editor.gpuBackend()));
  }, [editor]);
  return backend;
}

function Problem({ problem }: { problem: string | undefined }): ReactNode {
  if (!problem) return null;
  return (
    <span data-catchlight-problem="" role="alert">
      {problem}
    </span>
  );
}

/** What a rejected promise or a part's `onError` says, as one line. */
function describe(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  return String(cause);
}
