/** The assembled workspace. Its canvas survives every session switch. A close
 * first moves the screen to a live session; only then does an effect free the
 * old replica. Authoring behavior lives in the public React parts and hooks. */
import type { Editor, ParamId, Session, SessionInfo } from "@catchlight/core";
import {
  AssetsPanel,
  EditingProvider,
  useEditing,
  WorkspaceModes,
  EditingBar,
  MeshTools,
  MeshInspector,
  RecordingInspector,
  RecordingKeys,
  meshBounds,
  fitCamera,
  BindingsPanel,
  Disclosure,
  EditorProvider,
  EmptyState,
  ExtensionsPanel,
  FileOpen,
  FileSave,
  Icon,
  IconButton,
  Modal,
  ModelHealth,
  NodeTree,
  ParamsPanel,
  PresenceProvider,
  PropertiesPanel,
  PhysicsSettings,
  SessionList,
  WeldsPanel,
  downloadName,
  kindIcons,
  kindLabels,
  useEditor,
  useFileSave,
  usePosePublisher,
  usePoseSweep,
  useSelection,
  useSessions,
  useViewportCamera,
  useWorkspaceActions,
  usePanelSize,
  useDismissMenus,
  usePreviewExport,
} from "@catchlight/react";
import type { IconName, SaveOutcome } from "@catchlight/react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { Stage, Environment } from "./stage.js";
import { CommandPalette, Shortcuts } from "./dialogs.js";
import { Notification } from "./notification.js";
import type { PaletteAction } from "./dialogs.js";

const logoUrl = new URL("../../../assets/logo-transparent.svg", import.meta.url)
  .href;

export interface CatchlightEditorProps {
  editor: Editor;
  className?: string;
  onOpenStarter?: () => Promise<Session>;
}
export function CatchlightEditor({
  editor,
  className,
  onOpenStarter,
}: CatchlightEditorProps) {
  return (
    <div className={className ? `catchlight ${className}` : "catchlight"}>
      <EditorProvider editor={editor}>
        <Shell {...(onOpenStarter ? { onOpenStarter } : {})} />
      </EditorProvider>
    </div>
  );
}
function Shell({ onOpenStarter }: { onOpenStarter?: () => Promise<Session> }) {
  const editor = useEditor();
  const { sessions } = useSessions();
  const [session, setSession] = useState<Session>();
  const [problem, setProblem] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [closing, setClosing] = useState<number>();
  const [confirmClose, setConfirmClose] = useState<SessionInfo>();
  const dismissed = useRef(new Set<number>());
  const request = useRef(0);
  const failed = useCallback(
    (cause: unknown) =>
      setProblem(cause instanceof Error ? cause.message : String(cause)),
    [],
  );
  const opened = useCallback((next: Session) => {
    ++request.current;
    setProblem(undefined);
    setNotice(undefined);
    setSession(next);
  }, []);
  const choose = useCallback(
    (info: SessionInfo) => {
      const generation = ++request.current;
      void editor.attachSession(info).then((next) => {
        if (generation === request.current) opened(next);
      }, failed);
    },
    [editor, opened, failed],
  );
  const create = useCallback(() => {
    void editor.newSession().then(opened, failed);
  }, [editor, opened, failed]);
  const close = useCallback(
    (info: SessionInfo) => {
      setConfirmClose(undefined);
      if (session?.id !== info.session) {
        void editor.closeSession(info.session).catch(failed);
        return;
      }
      const next = sessions.find(
        (s) => s.session !== info.session && !dismissed.current.has(s.session),
      );
      const finish = (attached?: Session) => {
        dismissed.current.add(info.session);
        ++request.current;
        setSession(attached);
        setNotice(undefined);
        setClosing(info.session);
      };
      if (next) void editor.attachSession(next).then(finish, failed);
      else finish();
    },
    [session, sessions, editor, failed],
  );
  useEffect(() => {
    if (closing === undefined) return;
    setClosing(undefined);
    void editor.closeSession(closing).catch(failed);
  }, [closing, editor, failed]);
  useEffect(() => {
    if (session) return;
    const first = sessions.find((s) => !dismissed.current.has(s.session));
    if (!first) return;
    let live = true;
    const generation = request.current;
    void editor.attachSession(first).then((next) => {
      if (live && generation === request.current) setSession(next);
    }, failed);
    return () => {
      live = false;
    };
  }, [session, sessions, editor, failed]);
  useEffect(() => session?.onError(failed), [session, failed]);
  useEffect(() => {
    const before = (e: BeforeUnloadEvent) => {
      if (sessions.some((s) => s.dirty)) {
        e.preventDefault();
        e.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", before);
    return () => window.removeEventListener("beforeunload", before);
  }, [sessions]);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(undefined), 4500);
    return () => clearTimeout(timer);
  }, [notice]);
  return (
    <PresenceProvider session={session}>
      <EditingProvider session={session} onError={failed} onNotice={setNotice}>
        <Workspace
          session={session}
          info={sessions.find((s) => s.session === session?.id)}
          onOpened={opened}
          onNew={create}
          onChoose={choose}
          onClose={(s) => (s.dirty ? setConfirmClose(s) : close(s))}
          onError={failed}
          onNotice={setNotice}
          {...(onOpenStarter ? { onOpenStarter } : {})}
        />
      </EditingProvider>
      {(problem || notice) && (
        <Notification>
          <div
            data-catchlight-toast=""
            data-error={problem ? "" : undefined}
            role={problem ? "alert" : "status"}
          >
            <Icon name={problem ? "warning" : "check"} />
            <span
              {...(problem
                ? { "data-catchlight-problem": "" }
                : { "data-catchlight-notice": "" })}
            >
              {problem ?? notice}
            </span>
            <IconButton
              icon="close"
              label="Dismiss notification"
              onClick={() => {
                setProblem(undefined);
                setNotice(undefined);
              }}
            />
          </div>
        </Notification>
      )}
      {confirmClose && (
        <CloseDialog
          info={confirmClose}
          onCancel={() => setConfirmClose(undefined)}
          onClose={() => close(confirmClose)}
          onError={failed}
        />
      )}
    </PresenceProvider>
  );
}
function CloseDialog({
  info,
  onCancel,
  onClose,
  onError,
}: {
  info: SessionInfo;
  onCancel: () => void;
  onClose: () => void;
  onError: (cause: unknown) => void;
}) {
  const editor = useEditor();
  const [session, setSession] = useState<Session>();
  useEffect(() => {
    let live = true;
    void editor.attachSession(info).then((s) => {
      if (live) setSession(s);
    }, onError);
    return () => {
      live = false;
    };
  }, [editor, info, onError]);
  return (
    <Modal title="Save your changes?" onClose={onCancel}>
      <p>“{info.title}” has changes that haven’t been saved.</p>
      {session && (
        <FileSave.Root
          session={session}
          defaultName={downloadName(info.file ?? info.title)}
          onSaved={onClose}
          onError={onError}
        >
          Save and close
        </FileSave.Root>
      )}
      <div data-catchlight-dialog-actions="">
        <button type="button" onClick={onCancel}>
          Keep editing
        </button>
        <button type="button" data-catchlight-danger="" onClick={onClose}>
          Close without saving
        </button>
      </div>
    </Modal>
  );
}
type WorkspaceProps = {
  session: Session | undefined;
  info: SessionInfo | undefined;
  onOpened: (s: Session) => void;
  onNew: () => void;
  onChoose: (s: SessionInfo) => void;
  onClose: (s: SessionInfo) => void;
  onError: (e: unknown) => void;
  onNotice: (s: string) => void;
  onOpenStarter?: () => Promise<Session>;
};
function Workspace({
  session,
  info,
  onOpened,
  onNew,
  onChoose,
  onClose,
  onError,
  onNotice,
  onOpenStarter,
}: WorkspaceProps) {
  const editor = useEditor();
  const { node, select } = useSelection();
  const editing = useEditing()!;
  const actions = useWorkspaceActions(session, onError);
  const modelView = useViewportCamera();
  const meshView = useViewportCamera();
  const view = editing.mode === "mesh" ? meshView : modelView;
  const { save } = useFileSave(session);
  const [tool, setTool] = useState<"select" | "hand">("select");
  const mesh = editing.mode === "mesh";
  const setMesh = (next: boolean) =>
    editing.requestMode(next ? "mesh" : "arrange");
  const [grid, setGrid] = useState(false);
  const [left, setLeft] = useState<"structure" | "assets">("structure");
  const [right, setRight] = useState<"properties" | "model">("properties");
  const [dock, setDock] = useState<"params" | "bindings">("params");
  const [dockOpen, setDockOpen] = useState(true);
  const [focus, setFocus] = useState(false);
  const [search, setSearch] = useState("");
  const { param, selectParam: setParam } = editing;
  const [dialog, setDialog] = useState<"save" | "commands" | "help">();
  const [dragOver, setDragOver] = useState(false);
  const [mobilePanel, setMobilePanel] = useState<"structure" | "properties">();
  const file = useRef<HTMLInputElement>(null);
  const art = useRef<HTMLInputElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const workspace = useRef<HTMLDivElement>(null);
  useDismissMenus(workspace);
  useEffect(() => {
    workspace.current
      ?.querySelector(
        '[data-catchlight-panel="right"] [data-catchlight-panel-scroll]',
      )
      ?.scrollTo({ top: 0 });
  }, [node, session, right, editing.mode]);
  const structureSize = usePanelSize("structure", 232, 180, 360, "x");
  const propertiesSize = usePanelSize("properties", 288, 248, 400, "x", -1);
  const dockSize = usePanelSize("posing tools", 300, 140, 420, "y", -1);
  const capture = usePreviewExport(canvas);
  const sweep = usePoseSweep(session, param);
  const fit = useCallback(() => {
    if (editing.mesh) {
      const element = workspace.current?.querySelector(
        "[data-catchlight-mesh-canvas]",
      );
      if (element) {
        const next = fitCamera(
          meshBounds(editing.mesh),
          { width: element.clientWidth, height: element.clientHeight },
          0.35,
        );
        if (next) {
          view.setCamera(next);
          view.onFit(next);
        }
      }
    } else if (session) view.fit(session);
  }, [session, view, editing.mesh]);
  useEffect(() => {
    sweep.setPlaying(false);
    if (editing.mode === "record") {
      setDock("bindings");
      setDockOpen(true);
    } else if (editing.mode === "arrange") setDock("params");
  }, [editing.mode]);
  const saved = useCallback(
    (outcome: SaveOutcome) => {
      onNotice(
        outcome.downloaded
          ? `Downloaded ${downloadName(outcome.key)}`
          : `Saved ${downloadName(outcome.key)}`,
      );
      setDialog(undefined);
    },
    [onNotice],
  );
  const quickSave = useCallback(() => {
    const run = () => {
      if (session)
        void save(info?.file ? undefined : info?.title).then(saved, onError);
    };
    if (editing.mesh?.dirty) editing.guard(run);
    else run();
  }, [session, info, save, saved, onError, editing]);
  useEffect(() => {
    setSearch("");
  }, [session]);
  const zoom = (factor: number) =>
    view.setCamera({
      ...view.camera,
      height: Math.min(1e7, Math.max(0.001, view.camera.height / factor)),
    });
  const exportPng = () =>
    editing.guard(
      () =>
        void capture(downloadName(info?.title ?? "model")).then(
          () => onNotice("Preview exported as PNG"),
          onError,
        ),
    );
  const openModel = () => editing.guard(() => file.current?.click());
  const newModel = () => editing.guard(onNew);
  const importImages = () => editing.guard(() => art.current?.click());
  const saveCopy = () => editing.guard(() => setDialog("save"));
  const commands: PaletteAction[] = [
    {
      name: "Open model…",
      detail: "Load a .clm file",
      icon: "group",
      key: "⌘ O",
      run: openModel,
    },
    {
      name: "New model",
      detail: "Start with an empty canvas",
      icon: "plus",
      run: newModel,
    },
    {
      name: "Save model",
      detail: "Download a .clm file",
      icon: "save",
      key: "⌘ S",
      run: quickSave,
      disabled: !session,
    },
    {
      name: "Import artwork…",
      detail: "PNG or TGA images",
      icon: "upload",
      run: importImages,
      disabled: !session,
    },
    {
      name: "Export preview",
      detail: "Save the current view as PNG",
      icon: "download",
      run: () => void exportPng(),
      disabled: !session,
    },
    {
      name: "Fit model in view",
      detail: "Frame the complete model",
      icon: "fit",
      key: "F",
      run: fit,
      disabled: !session,
    },
    {
      name: "Reset pose",
      detail: "Restore every param to its default",
      icon: "reset",
      run: actions.resetPose,
      disabled: !session || mesh,
    },
    {
      name: "Undo",
      detail: "Undo the last model edit",
      icon: "undo",
      key: "⌘ Z",
      run: actions.undo,
      disabled: !actions.canUndo,
    },
    {
      name: "Redo",
      detail: "Restore an undone edit",
      icon: "redo",
      key: "⇧ ⌘ Z",
      run: actions.redo,
      disabled: !actions.canRedo,
    },
    {
      name: "Edit mesh on artwork",
      detail: "Place vertices without stretching the image",
      icon: "mesh",
      key: "M",
      run: () => setMesh(!mesh),
      disabled:
        !session ||
        (!mesh && (editing.info?.kind !== "part" || !editing.info.texture)),
    },
    {
      name: "Toggle focus mode",
      detail: "Make room for the canvas",
      icon: "panel",
      key: "⇧ F",
      run: () => setFocus(!focus),
    },
  ];
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const target = e.target as HTMLElement | null;
      const typing =
        target?.matches("input,textarea,select") || target?.isContentEditable;
      const mod = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      if (dialog || document.querySelector("dialog[open]")) return;
      if (mod && key === "k") {
        e.preventDefault();
        setDialog("commands");
        return;
      }
      if (mod && key === "s") {
        e.preventDefault();
        if (e.shiftKey) saveCopy();
        else quickSave();
        return;
      }
      if (mod && key === "o") {
        e.preventDefault();
        openModel();
        return;
      }
      if (typing) return;
      if (mod && key === "z") {
        e.preventDefault();
        void (e.shiftKey ? actions.redo() : actions.undo());
      } else if (mod && key === "d") {
        e.preventDefault();
        void actions.duplicate();
      } else if (key === "delete" || key === "backspace") {
        e.preventDefault();
        void actions.remove();
      } else if (key === "v") setTool("select");
      else if (key === "h") setTool("hand");
      else if (key === "f") {
        e.preventDefault();
        if (e.shiftKey) setFocus(!focus);
        else fit();
      } else if (key === "m") setMesh(!mesh);
      else if (key === "r") {
        if (editing.mode !== "record") editing.requestMode("record");
        else if (editing.recording) editing.stop();
        else editing.arm();
      } else if (key === "g") setGrid(!grid);
      else if (key === "?") setDialog("help");
      else if (key === "escape") {
        if (mesh) editing.requestMode("arrange");
        else if (editing.recording) editing.stop();
        else select(undefined);
        setMobilePanel(undefined);
      } else if (key === "+" || key === "=") {
        e.preventDefault();
        zoom(1.2);
      } else if (key === "-") {
        e.preventDefault();
        zoom(1 / 1.2);
      } else if (node && target?.closest("[data-catchlight-stage]")) {
        const n = e.shiftKey ? 10 : 1;
        const delta: Record<string, [number, number]> = {
          arrowleft: [-n, 0],
          arrowright: [n, 0],
          arrowup: [0, n],
          arrowdown: [0, -n],
        };
        if (delta[key]) {
          e.preventDefault();
          void actions.nudge(...delta[key]);
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });
  const menuItem = (
    label: string,
    icon: IconName,
    action: () => unknown,
    disabled = false,
    shortcut?: string,
  ) => (
    <button
      type="button"
      disabled={disabled}
      onClick={(e) => {
        e.currentTarget.closest("details")?.removeAttribute("open");
        action();
      }}
    >
      <Icon name={icon} />
      <span>{label}</span>
      {shortcut && <kbd>{shortcut}</kbd>}
    </button>
  );
  return (
    <div
      ref={workspace}
      style={
        {
          "--cl-panel-width": `${structureSize.size}px`,
          "--cl-properties-width": `${propertiesSize.size}px`,
          "--cl-dock-height": `${dockSize.size}px`,
        } as CSSProperties
      }
      data-catchlight-workspace=""
      data-mode={editing.mode}
      data-recording={editing.recording ? "" : undefined}
      data-focus={focus ? "" : undefined}
      data-dock-open={dockOpen ? "" : undefined}
      data-mobile-panel={mobilePanel}
    >
      <header data-catchlight-toolbar="">
        <a
          data-catchlight-brand=""
          href="#"
          aria-label="Catchlight home"
          onClick={(e) => {
            e.preventDefault();
            setDialog("commands");
          }}
        >
          <img
            data-catchlight-logo=""
            src={logoUrl}
            alt=""
            width="28"
            height="28"
          />
          catchlight<span data-catchlight-product="">STUDIO</span>
        </a>
        <div data-catchlight-main-menus="">
          <details data-catchlight-menu="">
            <summary>
              File
              <Icon name="down" width="11" height="11" />
            </summary>
            <div role="group" aria-label="File actions">
              {menuItem("New model", "plus", newModel)}
              {menuItem("Open model…", "group", openModel, false, "⌘ O")}
              {menuItem("Import artwork…", "upload", importImages, !session)}
              <hr />
              {menuItem("Save", "save", quickSave, !session, "⌘ S")}
              {menuItem("Save as…", "save", saveCopy, !session)}
              {menuItem(
                "Export preview…",
                "download",
                () => void exportPng(),
                !session,
              )}
            </div>
          </details>
          <details data-catchlight-menu="">
            <summary>
              Edit
              <Icon name="down" width="11" height="11" />
            </summary>
            <div>
              {menuItem("Undo", "undo", actions.undo, !actions.canUndo, "⌘ Z")}
              {menuItem(
                "Redo",
                "redo",
                actions.redo,
                !actions.canRedo,
                "⇧ ⌘ Z",
              )}
              <hr />
              {menuItem(
                "Duplicate",
                "duplicate",
                actions.duplicate,
                !actions.canDuplicate,
                "⌘ D",
              )}
              {menuItem(
                "Delete",
                "trash",
                actions.remove,
                !actions.canRemove,
                "⌫",
              )}
            </div>
          </details>
        </div>
        <div data-catchlight-history-tools="">
          <IconButton
            icon="undo"
            label="Undo (Ctrl/⌘ Z)"
            disabled={!actions.canUndo || actions.busy || editing.busy}
            onClick={actions.undo}
          />
          <IconButton
            icon="redo"
            label="Redo (Ctrl/⌘ Shift Z)"
            disabled={!actions.canRedo || actions.busy || editing.busy}
            onClick={actions.redo}
          />
        </div>
        <button
          type="button"
          data-catchlight-command-trigger=""
          onClick={() => setDialog("commands")}
        >
          <Icon name="search" width="15" height="15" />
          <span>Search anything…</span>
          <kbd>⌘ K</kbd>
        </button>
        <span data-catchlight-save-state="">
          <i data-dirty={info?.dirty || editing.mesh?.dirty ? "" : undefined} />
          {session
            ? editing.mesh?.dirty
              ? "Mesh draft · not applied"
              : info?.dirty
                ? "Unsaved changes"
                : "All changes saved"
            : "Your next creation"}
        </span>
        <button
          type="button"
          data-catchlight-save=""
          data-primary=""
          disabled={!session}
          onClick={quickSave}
        >
          <Icon name="save" width="15" height="15" />
          Save<span data-catchlight-save-label=""> model</span>
        </button>
        <label data-catchlight-hidden-file="">
          <FileOpen.Root ref={file} onOpened={onOpened} onError={onError} />
        </label>
        <input
          ref={art}
          type="file"
          accept=".png,.tga"
          multiple
          aria-label="Import artwork"
          data-catchlight-hidden-file=""
          onChange={(e) => {
            const files = Array.from(e.currentTarget.files ?? []);
            e.currentTarget.value = "";
            editing.guard(() => {
              void actions.importImages(files);
            });
          }}
        />
      </header>
      <nav data-catchlight-model-tabs="" aria-label="Open models">
        <SessionList.Root
          current={session?.id}
          onSelect={(s) => editing.guard(() => onChoose(s))}
          onClose={(s) => editing.guard(() => onClose(s))}
        />
        <IconButton
          data-catchlight-new=""
          icon="plus"
          label="New model"
          onClick={newModel}
        />
      </nav>
      <aside data-catchlight-panel="left" inert={mesh || editing.busy}>
        <div data-catchlight-resize="left" {...structureSize.handle} />
        <div data-catchlight-panel-tabs="">
          <button
            type="button"
            data-active={left === "structure" ? "" : undefined}
            onClick={() => setLeft("structure")}
          >
            Structure
          </button>
          <button
            type="button"
            data-active={left === "assets" ? "" : undefined}
            onClick={() => setLeft("assets")}
          >
            Artwork
          </button>
          <IconButton
            icon="close"
            label="Close structure panel"
            data-catchlight-mobile-only=""
            onClick={() => setMobilePanel(undefined)}
          />
        </div>
        {left === "structure" ? (
          <>
            <div data-catchlight-tree-tools="">
              <label data-catchlight-search="">
                <Icon name="search" width="14" height="14" />
                <input
                  aria-label="Search nodes"
                  placeholder="Find a node…"
                  value={search}
                  onChange={(e) => setSearch(e.currentTarget.value)}
                />
              </label>
              <details data-catchlight-menu="">
                <summary aria-label="Add node">
                  <Icon name="plus" />
                </summary>
                <div>
                  {(
                    [
                      "group",
                      "part",
                      "composite",
                      "mesh_group",
                      "spine",
                      "physics",
                    ] as const
                  ).map((kind) => (
                    <button
                      type="button"
                      key={kind}
                      disabled={!session}
                      onClick={(e) => {
                        e.currentTarget
                          .closest("details")
                          ?.removeAttribute("open");
                        void actions.add(kind);
                      }}
                    >
                      <Icon name={kindIcons[kind]} />
                      <span>{kindLabels[kind]}</span>
                    </button>
                  ))}
                </div>
              </details>
            </div>
            <div data-catchlight-tree-scroll="">
              {session ? (
                <NodeTree.Root
                  session={session}
                  filter={search}
                  onError={onError}
                />
              ) : (
                <EmptyState icon="group" title="A place for every part">
                  Your model’s structure will appear here.
                </EmptyState>
              )}
            </div>
            <div data-catchlight-tree-footer="">
              <button
                type="button"
                data-catchlight-import-artwork=""
                onClick={importImages}
                disabled={!session}
              >
                <Icon name="upload" width="15" height="15" />
                Import artwork
              </button>
              <IconButton
                icon="duplicate"
                label="Duplicate selected node"
                disabled={!actions.canDuplicate}
                onClick={actions.duplicate}
              />
              <IconButton
                icon="trash"
                label="Delete selected node"
                disabled={!actions.canRemove}
                onClick={actions.remove}
              />
            </div>
          </>
        ) : (
          <div data-catchlight-panel-scroll="">
            {session ? (
              <AssetsPanel session={session} />
            ) : (
              <EmptyState icon="part" title="No artwork yet">
                Open a model or create a new one to import your images.
              </EmptyState>
            )}
            <button
              type="button"
              data-catchlight-action=""
              data-catchlight-import-artwork=""
              disabled={!session}
              onClick={importImages}
            >
              <Icon name="upload" />
              Import artwork
            </button>
          </div>
        )}
      </aside>
      <main data-catchlight-center="">
        <div data-catchlight-canvas-toolbar="">
          <div data-catchlight-tool-group="">
            <IconButton
              icon="select"
              label="Select and move (V)"
              aria-pressed={tool === "select"}
              onClick={() => setTool("select")}
            />
            <IconButton
              icon="hand"
              label="Pan (H)"
              aria-pressed={tool === "hand"}
              onClick={() => setTool("hand")}
            />
            <span data-catchlight-separator="" />
            <IconButton
              icon="grid"
              label="Show grid (G)"
              aria-pressed={grid}
              disabled={mesh}
              onClick={() => setGrid(!grid)}
            />
          </div>
          <WorkspaceModes />
          <div data-catchlight-zoom-tools="">
            <IconButton
              icon="minus"
              label="Zoom out"
              onClick={() => zoom(1 / 1.2)}
            />
            <button
              type="button"
              data-catchlight-zoom=""
              onClick={fit}
              title="Fit model in view"
            >
              {view.zoom === undefined
                ? "100%"
                : `${Math.round(view.zoom * 100)}%`}
            </button>
            <IconButton icon="plus" label="Zoom in" onClick={() => zoom(1.2)} />
            <IconButton
              data-catchlight-fit=""
              icon="fit"
              label="Fit model (F)"
              onClick={fit}
              disabled={!session}
            />
            <IconButton
              icon="panel"
              label="Focus mode (Shift F)"
              aria-pressed={focus}
              onClick={() => setFocus(!focus)}
            />
          </div>
        </div>
        <EditingBar />
        {mesh && <MeshTools />}
        <div
          data-catchlight-stage-wrap=""
          data-drop={dragOver ? "" : undefined}
          onDragOver={(e) => {
            if (e.dataTransfer.types.includes("Files")) {
              e.preventDefault();
              setDragOver(true);
            }
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node))
              setDragOver(false);
          }}
          onDrop={(e) => {
            e.preventDefault();
            setDragOver(false);
            const files = Array.from(e.dataTransfer.files);
            const model = files.find((f) => /\.clm$/i.test(f.name));
            editing.guard(() => {
              if (model)
                void model
                  .arrayBuffer()
                  .then((bytes) =>
                    editor.openFile(new Uint8Array(bytes), model.name),
                  )
                  .then(onOpened, onError);
              else if (session) void actions.importImages(files);
              else
                onError(
                  new Error(
                    "Create a model first, then drop your artwork here.",
                  ),
                );
            });
          }}
        >
          <Stage
            session={session}
            view={modelView}
            meshView={meshView}
            tool={tool}
            grid={grid}
            canvas={canvas}
            onError={onError}
          />
          {!session && (
            <div data-catchlight-welcome="">
              <span data-catchlight-welcome-symbol="">
                <img
                  data-catchlight-logo=""
                  src={logoUrl}
                  alt=""
                  width="56"
                  height="56"
                />
              </span>
              <p data-catchlight-eyebrow="">A LITTLE ART. A LOT OF LIFE.</p>
              <h1>
                Make something
                <br />
                <em>move you.</em>
              </h1>
              <p>
                A home for your next character.
                <br />
                Build, shape, and bring it to life.
              </p>
              <div>
                <button type="button" data-primary="" onClick={newModel}>
                  <Icon name="plus" />
                  Create a model
                </button>
                <button type="button" onClick={openModel}>
                  <Icon name="group" />
                  Open a model
                </button>
              </div>
              {onOpenStarter && (
                <button
                  type="button"
                  data-catchlight-text-button=""
                  onClick={() => void onOpenStarter().then(onOpened, onError)}
                >
                  Explore the starter character{" "}
                  <Icon name="arrow" width="14" height="14" />
                </button>
              )}
              <small>Or drop a .clm file anywhere on the canvas</small>
            </div>
          )}
          {dragOver && (
            <div data-catchlight-drop-hint="">
              <Icon name="upload" width="30" height="30" />
              <strong>Drop to bring it in</strong>
              <span>Model files or PNG / TGA artwork</span>
            </div>
          )}
          {session && (
            <div data-catchlight-canvas-hint="">
              <kbd>Space</kbd> + drag to pan<span>·</span>Scroll to zoom
            </div>
          )}
        </div>
        <section
          data-catchlight-dock=""
          data-open={dockOpen ? "" : undefined}
          aria-label="Posing tools"
        >
          {dockOpen && (
            <div data-catchlight-resize="dock" {...dockSize.handle} />
          )}
          <div data-catchlight-dock-header="">
            <div data-catchlight-panel-tabs="">
              <button
                type="button"
                data-active={dock === "params" ? "" : undefined}
                onClick={() => {
                  setDock("params");
                  setDockOpen(true);
                }}
              >
                <Icon name="settings" width="14" height="14" />
                Params
              </button>
              <button
                type="button"
                data-active={dock === "bindings" ? "" : undefined}
                onClick={() => {
                  setDock("bindings");
                  setDockOpen(true);
                }}
              >
                <Icon name="link" width="14" height="14" />
                {editing.mode === "record" ? "Keypoints" : "Bindings"}
              </button>
            </div>
            <div data-catchlight-dock-actions="">
              <button
                type="button"
                disabled={!session || !param || editing.mode !== "arrange"}
                aria-pressed={sweep.playing}
                onClick={() => sweep.setPlaying(!sweep.playing)}
              >
                <Icon
                  name={sweep.playing ? "pause" : "play"}
                  width="13"
                  height="13"
                />
                {sweep.playing ? "Stop sweep" : "Preview sweep"}
              </button>
              <IconButton
                data-catchlight-pose-reset=""
                icon="reset"
                label="Reset pose"
                disabled={!session || mesh}
                onClick={actions.resetPose}
              />
              <IconButton
                icon={dockOpen ? "down" : "chevron"}
                label={
                  dockOpen ? "Collapse posing tools" : "Expand posing tools"
                }
                onClick={() => setDockOpen(!dockOpen)}
              />
            </div>
          </div>
          {dockOpen && (
            <div data-catchlight-dock-body="">
              {mesh ? (
                <p data-catchlight-mesh-pose-held="">
                  Pose held while editing topology. Apply or cancel your mesh to
                  return.
                </p>
              ) : session ? (
                dock === "params" ? (
                  <ParamsPanel
                    key={session.id}
                    session={session}
                    selected={param}
                    onSelect={setParam}
                    onError={onError}
                  />
                ) : editing.mode === "record" ? (
                  <RecordingKeys />
                ) : (
                  <BindingsPanel
                    session={session}
                    node={node}
                    param={param}
                    onError={onError}
                  />
                )
              ) : (
                <p data-catchlight-empty="">
                  Open a model to explore its controls.
                </p>
              )}
            </div>
          )}
        </section>
      </main>
      <aside data-catchlight-panel="right">
        <div data-catchlight-resize="right" {...propertiesSize.handle} />
        <div data-catchlight-panel-tabs="">
          <button
            type="button"
            data-active={
              right === "properties" || editing.mode !== "arrange"
                ? ""
                : undefined
            }
            onClick={() => setRight("properties")}
          >
            Properties
          </button>
          <button
            type="button"
            disabled={editing.mode !== "arrange"}
            data-active={right === "model" ? "" : undefined}
            onClick={() => setRight("model")}
          >
            Model
          </button>
          <IconButton
            icon="close"
            label="Close properties panel"
            data-catchlight-mobile-only=""
            onClick={() => setMobilePanel(undefined)}
          />
        </div>
        <div data-catchlight-panel-scroll="">
          {session ? (
            mesh ? (
              <MeshInspector />
            ) : editing.mode === "record" ? (
              <RecordingInspector />
            ) : right === "properties" ? (
              <>
                {!!editing.emptiedSlots.length && (
                  <div data-catchlight-warning="" role="status">
                    Mesh applied. Reassign slots:{" "}
                    {editing.emptiedSlots.join(", ")}. Open the Slots section
                    below.
                  </div>
                )}
                <PropertiesPanel session={session} onError={onError} />
              </>
            ) : (
              <>
                <ModelHealth session={session} />
                <PhysicsSettings session={session} onError={onError} />
                <Disclosure title="Connections">
                  <WeldsPanel session={session} onError={onError} />
                </Disclosure>
                <Disclosure title="Metadata" defaultOpen={false}>
                  <ExtensionsPanel session={session} onError={onError} />
                </Disclosure>
              </>
            )
          ) : (
            <EmptyState title="Room for possibility">
              Everything you need to shape your character, right here when you
              need it.
            </EmptyState>
          )}
        </div>
      </aside>
      <footer data-catchlight-status="">
        <div>
          <button
            type="button"
            data-catchlight-mobile-only=""
            onClick={() =>
              setMobilePanel(
                mobilePanel === "structure" ? undefined : "structure",
              )
            }
          >
            <Icon name="group" />
            Structure
          </button>
          <span data-catchlight-status-title="">
            {info?.title ?? "No model open"}
          </span>
          <span data-catchlight-status-selection="">
            {node ? `selected ${node.split("/").pop()}` : "Nothing selected"}
          </span>
        </div>
        <div>
          <Environment />
          <button
            type="button"
            data-catchlight-mobile-only=""
            onClick={() =>
              setMobilePanel(
                mobilePanel === "properties" ? undefined : "properties",
              )
            }
          >
            <Icon name="settings" />
            Properties
          </button>
          <IconButton
            icon="help"
            label="Keyboard shortcuts (?)"
            onClick={() => setDialog("help")}
          />
        </div>
      </footer>
      {session && <PosePublisher session={session} />}
      {dialog === "save" && session && (
        <Modal title="Save a copy" onClose={() => setDialog(undefined)}>
          <p data-catchlight-hint="">
            Your .clm file contains the complete model and its artwork.
          </p>
          <FileSave.Root
            session={session}
            defaultName={downloadName(
              info?.file ?? info?.title ?? "untitled.clm",
            )}
            onSaved={saved}
            onError={onError}
          >
            Save model
          </FileSave.Root>
        </Modal>
      )}
      {dialog === "commands" && (
        <CommandPalette
          session={session}
          actions={commands}
          onClose={() => setDialog(undefined)}
        />
      )}
      {dialog === "help" && <Shortcuts onClose={() => setDialog(undefined)} />}
    </div>
  );
}
function PosePublisher({ session }: { session: Session }) {
  usePosePublisher(session);
  return null;
}
