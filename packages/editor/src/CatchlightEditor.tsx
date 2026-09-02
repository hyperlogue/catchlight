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
 */

import type { Editor, Session, SessionInfo } from "@catchlight/core";
import {
  EditorProvider,
  FileOpen,
  NodeTree,
  ParamList,
  ParamSlider,
  SelectionProvider,
  SessionList,
  Viewport,
  useEditor,
  useNodeDrag,
  useRevision,
  useSelection,
  useSessions,
  useViewportCamera,
} from "@catchlight/react";
import type { ViewportCamera } from "@catchlight/react";
import { useCallback, useEffect, useState } from "react";
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
  const view = useViewportCamera();

  const opened = useCallback((next: Session): void => {
    setProblem(undefined);
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

  // Whatever the editor already had open: a model named on a server's command
  // line, or a session an agent opened over the socket.
  useEffect(() => {
    if (session !== undefined) return;
    const first = sessions[0];
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

  const save = useCallback((): void => {
    if (!session) return;
    void editor.saveDocument(session).then(() => setProblem(undefined), failed);
  }, [editor, session, failed]);

  const fit = useCallback((): void => {
    if (session) view.fit(session);
  }, [session, view]);

  const info = session ? sessions.find((each) => each.session === session.id) : undefined;

  return (
    <>
      <header data-catchlight-toolbar="">
        <label data-catchlight-open="">
          <span>Open .clm</span>
          <FileOpen.Root onOpened={opened} onError={failed} />
        </label>
        <button
          type="button"
          data-catchlight-save=""
          disabled={session === undefined}
          onClick={save}
        >
          Save
        </button>
        <button
          type="button"
          data-catchlight-fit=""
          disabled={session === undefined}
          onClick={fit}
        >
          Fit
        </button>
      </header>
      {session ? (
        <SelectionProvider session={session}>
          <nav data-catchlight-panel="left">
            <Documents onSelect={choose} />
            <section data-catchlight-section="" data-grow="">
              <h2 data-catchlight-heading="">Nodes</h2>
              <NodeTree.Root session={session} />
            </section>
          </nav>
          <Stage session={session} view={view} />
          <aside data-catchlight-panel="right">
            <section data-catchlight-section="" data-grow="">
              <h2 data-catchlight-heading="">Params</h2>
              {/* The default row is the slider alone, and a column of
                  unlabelled sliders names nothing. */}
              <ParamList.Root session={session}>
                {(param) => (
                  <>
                    <span data-catchlight-param-name="">{param.name}</span>
                    <ParamSlider.Root session={session} param={param} />
                  </>
                )}
              </ParamList.Root>
            </section>
          </aside>
          <Status session={session} info={info} problem={problem} />
        </SelectionProvider>
      ) : (
        <>
          <nav data-catchlight-panel="left">
            <Documents onSelect={choose} />
          </nav>
          <div data-catchlight-stage="" data-empty="">
            <p>No document open. Pick a .clm above, or choose one on the left.</p>
          </div>
          <aside data-catchlight-panel="right" />
          <footer data-catchlight-status="" role="status">
            <span data-catchlight-status-item="">no document</span>
            <Environment />
            <Problem problem={problem} />
          </footer>
        </>
      )}
    </>
  );
}

/** Every document the editor has open, this tab's and everyone else's. */
function Documents({ onSelect }: { onSelect: (info: SessionInfo) => void }): ReactNode {
  return (
    <section data-catchlight-section="">
      <h2 data-catchlight-heading="">Documents</h2>
      <SessionList.Root onSelect={onSelect} />
    </section>
  );
}

/**
 * The canvas and the one gesture layered over it.
 *
 * The drag is here rather than in `Shell` because it needs the selection, and
 * the selection only exists once there is a session to publish it against.
 */
function Stage({ session, view }: { session: Session; view: ViewportCamera }): ReactNode {
  const { node } = useSelection();
  const drag = useNodeDrag(session, node);
  return (
    <div data-catchlight-stage="" data-dragging={drag.dragging ? "" : undefined}>
      <Viewport.Root
        session={session}
        camera={view.camera}
        onCameraChange={view.onCameraChange}
        onResize={view.onResize}
        {...drag.handlers}
      />
    </div>
  );
}

function Status({
  session,
  info,
  problem,
}: {
  session: Session;
  info: SessionInfo | undefined;
  problem: string | undefined;
}): ReactNode {
  const revision = useRevision(session);
  const { node } = useSelection();
  return (
    <footer data-catchlight-status="" role="status">
      <span data-catchlight-status-item="">{info?.title ?? "untitled"}</span>
      <span data-catchlight-status-item="">rev {revision}</span>
      {info?.dirty ? (
        <span data-catchlight-status-item="" data-dirty="">
          unsaved
        </span>
      ) : null}
      <span data-catchlight-status-item="">{node ? `selected ${node}` : "nothing selected"}</span>
      <Environment />
      <Problem problem={problem} />
    </footer>
  );
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
