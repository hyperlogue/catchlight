/**
 * `@catchlight/react`: hooks over a `Session`, and unstyled parts over the
 * hooks. Parts ship no class names or stylesheets; state is in `data-*`.
 */

export { EditorProvider, useEditor } from "./editor-context.js";
export type { EditorProviderProps } from "./editor-context.js";

export {
  useNodeInfo,
  useParams,
  useReplica,
  useRevision,
  useTree,
} from "./replica.js";

export { useSessions } from "./sessions.js";
export type { Sessions } from "./sessions.js";

export { SelectionProvider, useSelection } from "./selection.js";
export type { Selection, SelectionProviderProps } from "./selection.js";

export { Viewport, ViewportRoot, useViewportCamera } from "./viewport.js";
export type {
  ViewportCamera,
  ViewportPointerEvent,
  ViewportRootProps,
} from "./viewport.js";
export {
  DEFAULT_CAMERA,
  fitCamera,
  panTo,
  screenAt,
  wheelNotches,
  worldAt,
  worldPerPixel,
  zoomAbout,
} from "./camera.js";
export type { Bounds, Point, Size } from "./camera.js";

export { useNodeDrag } from "./node-drag.js";
export type { NodeDrag } from "./node-drag.js";

export {
  ParamKeys,
  ParamKeysRoot,
  ParamSlider,
  ParamSliderRoot,
} from "./param-slider.js";
export type {
  ParamKeysRootProps,
  ParamSliderRootProps,
} from "./param-slider.js";
export {
  ParamAdd,
  ParamAddRoot,
  ParamFields,
  ParamFieldsRoot,
  ParamList,
  ParamListRoot,
} from "./param-list.js";
export type {
  ParamAddRootProps,
  ParamFieldsRootProps,
  ParamListRootProps,
} from "./param-list.js";

export { useNodeActions } from "./node-actions.js";
export type { DropAt, NodeActions } from "./node-actions.js";

export {
  NODE_KINDS,
  NodeTree,
  NodeTreeActions,
  NodeTreeItem,
  NodeTreeRoot,
} from "./node-tree.js";
export type {
  NodeTreeActionsProps,
  NodeTreeItemProps,
  NodeTreeRootProps,
} from "./node-tree.js";

export { SessionList, SessionListRoot } from "./session-list.js";
export type { SessionListRootProps } from "./session-list.js";

export { FileOpen, FileOpenRoot } from "./file-open.js";
export type { FileOpenRootProps } from "./file-open.js";

export {
  BINDING_TARGETS,
  INTERPOLATE_MODES,
  bindingsOfParam,
  keyIndexNear,
  normalizedValue,
  useBindings,
  valueAtKey,
} from "./bindings.js";
export { useParamActions } from "./param-actions.js";
export type {
  BindingCell,
  BindingAddress,
  NewParam,
  ParamActions,
  ParamPatch,
} from "./param-actions.js";
export { BindingGrid, BindingGridRoot } from "./binding-grid.js";
export type { BindingGridRootProps } from "./binding-grid.js";

export { useNodePatch } from "./node-patch.js";
export type { NodePatchFn } from "./node-patch.js";
export { BLEND_MODES, Inspector, InspectorRoot } from "./inspector.js";
export type { InspectorRootProps } from "./inspector.js";

export { POSE_INTERVAL_MS, PresenceProvider, usePose } from "./presence.js";
export type { Pose, PoseSource, PresenceProviderProps } from "./presence.js";
export { readPose, usePosePublisher, useResetPose } from "./pose.js";
export {
  downloadBytes,
  downloadName,
  FileSave,
  FileSaveRoot,
  saveKey,
  useFileSave,
} from "./file-save.js";
export type { FileSaver, FileSaveRootProps, SaveOutcome } from "./file-save.js";

export type {
  BindingInfo,
  BindingParams,
  Camera,
  NodeInfo,
} from "@catchlight/core";

export {
  Icon,
  IconButton,
  Field,
  SearchField,
  NumberField,
  TextField,
  Disclosure,
  EmptyState,
  Modal,
} from "./controls.js";
export type { IconName } from "./controls.js";
export {
  useCommand,
  useSessionStatus,
  useModelQuery,
  importArtwork,
  flattenTree,
  kindLabels,
  kindIcons,
} from "./authoring.js";
export type { ErrorHandler } from "./authoring.js";
export {
  MeshPanel,
  SpinePanel,
  MotionResponsePanel,
  RiggingEmpty,
} from "./rigging.js";
export {
  MaskPanel,
  SlotsPanel,
  WeldsPanel,
  ModelHealth,
  ExtensionsPanel,
} from "./model-panels.js";
export {
  ParamsPanel,
  BindingsPanel,
  usePoseSweep,
  DeformPanel,
} from "./posing-panel.js";
export {
  useWorkspaceActions,
  useCanvasSelection,
  useSelectionVertices,
} from "./workspace.js";
export {
  PropertiesPanel,
  AssetsPanel,
  TextureThumbnail,
} from "./properties-panel.js";
export {
  usePanelSize,
  useDismissMenus,
  usePreviewExport,
  captureCanvas,
} from "./workspace-layout.js";
export { SelectionOverlay } from "./gizmo.js";

export { EditingProvider, useEditing } from "./editing.js";
export type { Editing, EditingMode } from "./editing.js";
export {
  MeshCanvas,
  MeshTools,
  MeshInspector,
  meshBounds,
} from "./mesh-editor.js";
export {
  WorkspaceModes,
  EditingBar,
  RecordingInspector,
  RecordingKeys,
} from "./recording-panel.js";
export {
  ReviewProvider,
  ReviewPanel,
  ReviewStatus,
  ReviewBadge,
  ReviewCommentButton,
  AgentPrompt,
  useReview,
  sameTarget,
  targetLabel,
} from "./review.js";
export type {
  AgentPhase,
  ReviewPanelProps,
  ReviewProviderProps,
  ReviewState,
  Reviewer,
  TargetLabel,
} from "./review.js";
export {
  useVersions,
  useVersionView,
  VersionSelect,
  VersionBanner,
  versionName,
  relativeTime,
} from "./versions.js";
export type {
  VersionsState,
  VersionView,
  VersionSelectProps,
  VersionBannerProps,
} from "./versions.js";
