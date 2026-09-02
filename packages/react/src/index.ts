/**
 * `@catchlight/react` — the editor's React layer: hooks over a `Session`, and
 * unstyled parts over the hooks.
 *
 * Layer 3, on `@catchlight/core`'s public surface and nothing below it. The
 * rules it is built to:
 *
 * **Hooks are the primitive, parts are a convenience.** Every part here is a
 * thin wrapper that reads the same hooks a host could call itself, renders one
 * real element, spreads the props it did not use onto it, and says what it is
 * in `data-*` attributes. Nothing in this package ships a class name, a
 * stylesheet or a colour; the only inline styles are the two the viewport
 * needs to behave. A host that wants a different shape writes it and keeps the
 * hooks.
 *
 * **No mirror.** The model is in this tab and answers synchronously, so a
 * panel reads it during render through [`useReplica`], keyed on the session's
 * revision. Copying it into React state would be a second version of the truth
 * that can disagree with the canvas.
 *
 * **TypeScript owns gestures, Rust owns what reads the model.** A drag is
 * pointer events and a captured pointer id on this side; every number in it —
 * where a world delta puts a node's translation — comes from the replica.
 */

export { EditorProvider, useEditor } from "./editor-context.js";
export type { EditorProviderProps } from "./editor-context.js";

export { useNodeInfo, useParams, useReplica, useRevision, useTree } from "./replica.js";

export { useSessions } from "./sessions.js";
export type { Sessions } from "./sessions.js";

export { SelectionProvider, useSelection } from "./selection.js";
export type { Selection, SelectionProviderProps } from "./selection.js";

export { Viewport, ViewportRoot, useViewportCamera } from "./viewport.js";
export type { ViewportCamera, ViewportPointerEvent, ViewportRootProps } from "./viewport.js";
// The arithmetic itself, for a host placing an HTML overlay over the canvas.
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

export { ParamKeys, ParamKeysRoot, ParamSlider, ParamSliderRoot } from "./param-slider.js";
export type { ParamKeysRootProps, ParamSliderRootProps } from "./param-slider.js";
export {
  ParamAdd,
  ParamAddRoot,
  ParamFields,
  ParamFieldsRoot,
  ParamList,
  ParamListRoot,
} from "./param-list.js";
export type { ParamAddRootProps, ParamFieldsRootProps, ParamListRootProps } from "./param-list.js";

export { useNodeActions } from "./node-actions.js";
export type { DropAt, NodeActions } from "./node-actions.js";

export { NODE_KINDS, NodeTree, NodeTreeActions, NodeTreeItem, NodeTreeRoot } from "./node-tree.js";
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
export type { BindingCell, NewParam, ParamActions, ParamPatch } from "./param-actions.js";
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

export type { BindingInfo, BindingParams, Camera, NodeInfo } from "@catchlight/core";
