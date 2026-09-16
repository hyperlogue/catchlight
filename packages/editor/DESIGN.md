# Catchlight studio

The editor should make a character the centre of the workspace. Structure,
properties, and posing stay close to the canvas, while less frequent rigging
operations live in contextual disclosures. The design follows the familiar
canvas-and-panels organisation of [Photopea](https://www.photopea.com/learn/workspace)
and [Penpot](https://help.penpot.app/user-guide/first-steps/the-interface/),
using catchlight's own model vocabulary throughout.

## Visual system

`src/theme.css` contains the default theme under `@layer catchlight`, scoped
to `.catchlight`. Hosts can override its custom properties without changing
components or fighting selector specificity.

The brand mark comes from `assets/logo.svg`. The editor and site use
`assets/logo-transparent.svg`: the same circle, highlight and gradient with
the outer white rectangle removed and the viewBox tightened to the circle.
The original white-backed asset remains available for standalone artwork.
Its gradient provides the accent palette: `#72bffb` for
primary actions and selection, `#9bd3fd` for hover, and `#68b7f7` for stronger
emphasis. Transparent accent treatments derive from those tokens.

| Role       | Tokens and treatment                                                                        |
| ---------- | ------------------------------------------------------------------------------------------- |
| Workspace  | `--cl-bg`, `--cl-mantle`, `--cl-crust`: neutral graphite surfaces                           |
| Controls   | `--cl-surface`, `--cl-surface-raised`, `--cl-surface-active`                                |
| Text       | `--cl-text`, `--cl-text-secondary`, `--cl-text-dim`                                         |
| Intent     | `--cl-accent`, `--cl-accent-soft`: the logo's blue for selection, focus and primary actions |
| Feedback   | `--cl-warn`, `--cl-danger`; readable messages alongside colour                              |
| Density    | 4/8/12/16/24/32 spacing, 30px controls, 32px tree rows                                      |
| Typography | System sans, 12px controls, tabular numeric values; mono for IDs                            |
| Shape      | 6px controls, 12px dialogs, fine borders and restrained shadows                             |

Artwork provides the strongest colour. Accent marks selection and actions,
not every panel. Hover and focus are distinct; selected tools also expose
`aria-pressed`. Motion only communicates state and respects reduced motion.

## Workspace and interactions

- The top bar holds file actions, undo/redo, command search and save state.
  Model tabs sit underneath it, above the panels they control.
- Structure and Artwork share the left panel. Search retains ancestors so
  a result still has a place in the model. The gallery uses actual decoded
  artwork, including TGA, with transparency preserved.
- Arrange owns selection, pan, zoom and base transform handles. Mesh opens an
  isolated artwork view; Record captures gestures into a selected binding cell. One canvas survives model switches and empty states. A fitted
  view follows resizing until the user pans or zooms.
- Properties follow the selected node's capabilities. Model-wide checks,
  welds, physics constants and metadata live in the Model tab.
- The bottom shelf holds params and bindings. Pose previews never author
  model data. A sweep previews the selected param and restores its old value.
- Panels resize by pointer or keyboard and remember their dimensions.
  Narrow screens use dismissible side panels; focus mode makes more room
  for artwork.

A numeric field has a draft, validates finite values and bounds, and commits
on Enter or blur. Escape restores its previous value. Rotations in Properties
use degrees; scalar binding key tooltips identify the protocol's radians.
Drag previews use scratch state, hold it until the committed revision reaches
the replica, and produce one undo entry. Escape, pointer cancellation, loss
of focus, and a model switch cancel the preview.

Unavailable actions are disabled. Errors remain visible until dismissed,
including when a native modal is open. Dirty model closure offers save,
discard, or keep editing; page unload also protects unsaved work. In-tab save
downloads a `.clm`; connected save reports the server's saved file.

`?` opens the shortcut guide. Ctrl/Command-K searches both actions and model
nodes. Tree navigation, resizing and canvas handles work with a keyboard.

## Components and protocol

The implementation order is the visual primitives, reusable authoring parts,
workspace composition, then browser workflows on the real runtime. These
boundaries keep future tools consistent:

| Owner                    | Responsibility                                                                                    |
| ------------------------ | ------------------------------------------------------------------------------------------------- |
| `catchlight-editor-core` | Picking, world bounds, isolated mesh drafts and history, UV mapping, recording delta calculations |
| `catchlight-editor-wasm` | Evaluated geometry, coordinate conversion, decoded artwork, owned draft/gesture handles           |
| `@catchlight/core`       | Session lifecycle, replica reads, typed protocol routing, scratch and storage                     |
| `@catchlight/react`      | Unstyled controls, authoring panels, gestures and workspace hooks                                 |
| `@catchlight/editor`     | Layout, theme, command palette and notifications                                                  |
| `apps/site`              | Backend choice and the original Mica starter model                                                |

No component maintains a second authored model. Queries read the replica;
edits go to the editor. Status reads provide actual undo/redo availability
and global physics values. Commands carrying attachments or reply payloads go through `sendWith`. Empty
session creation and JSON metadata use ordinary `send`.

| User workflow                        | React parts                                                   | Protocol families                                                                      |
| ------------------------------------ | ------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Open, create, save and close models  | `FileOpen`, `FileSave`, `SessionList`                         | Session lifecycle, import, save, status                                                |
| Organise artwork                     | `NodeTree`, `useWorkspaceActions`                             | Add, duplicate, delete, move, reorder, node patches                                    |
| Adjust a part or group               | `PropertiesPanel`, `SelectionOverlay`                         | Node info/set, local scratch transforms                                                |
| Bring in and replace artwork         | `AssetsPanel`, `TextureThumbnail`, `importArtwork`            | Texture list/add, grid mesh generation                                                 |
| Edit topology on artwork             | `MeshCanvas`, `MeshInspector`, `MeshTools`, `EditingProvider` | Isolated Rust draft, one guarded mesh set with deform refitting                        |
| Record a parameter keypoint          | `RecordingInspector`, `RecordingKeys`, `SelectionOverlay`     | Normalized pose capture, local scratch, atomic binding/axis/cell edits   |
| Define and pose controls             | `ParamsPanel`, param parts                                    | Param add/set/delete and continuous local pose                         |
| Author bindings                      | `BindingsPanel`, `BindingGrid`, `DeformPanel`                 | Binding-local axes, exact cells, interpolation and composed copy/reset/unset/invert |
| Clip and connect artwork             | `MaskPanel`, `SlotsPanel`, `WeldsPanel`                       | Masks, slots, whole weld set/delete                                                   |
| Tune motion                          | `SpinePanel`, `PhysicsPanel`, `PhysicsSettings`               | Joint and chain settings, pendulum outputs, global constants                           |
| Inspect model health and annotations | `ModelHealth`, `ExtensionsPanel`                              | Check, extensions, JSON metadata set/delete                                            |
| Export the visible preview           | `usePreviewExport`                                            | Viewport readback followed by a PNG download                                           |

The public protocol still supports advanced scripting operations beyond these
visible workflows: manifest interchange, structure import under an existing
parent, session forks, revision history, geometry reads and binary metadata.
These remain available through the session API. The UI supports base topology,
per-vertex recording, and scalar transform/appearance recording.
There are no animation-authoring commands, so the shelf has a pose sweep
rather than an inert timeline. Selection is one node or subtree at a time.

## Arrange, Mesh and Record

The three workspaces separate changes with different consequences. The logo's
blue marks topology; `--cl-record` adds a coral cue for keypoint capture. Labels,
selected controls and a persistent destination bar carry the same meaning
without relying on colour.

Mesh displays the original full-resolution artwork with editable vertices and
pinned edges. The model canvas remains mounted for the graphics device, and
the isolated view owns a separate camera. Move, add, connect, delete, generate,
and local undo/redo operate on a Rust draft. Apply sends one revision-guarded
mesh replacement, recomputes UVs, refits shape bindings and reports emptied
slots. Cancel discards the draft. Navigation, model switches and save pass
through Apply / Discard / Keep editing when a draft has changes. A concurrent
model edit preserves the draft and disables Apply until it is reloaded.
A custom, non-axis-aligned texture mapping is refused instead of silently
flattening the artwork.

Record addresses one node, one parameter or existing parameter pair, and a
normalized pose. Both axes retain their original order. The key shelf combines
positions from the selected properties for navigation; every property's binding
owns its axes and authored cells. Hollow diamonds are derived and filled ones
have authored contributions. Scrubbing and selecting a position only pose the
replica. Recording can start at any value. Arming alone creates no key; the first
completed gesture inserts missing positions only on the bindings it edits.
Binding panels provide axis controls for explicit insertion, movement and removal.

Each gesture captures its destination, model revision and starting pose. The
first completed gesture creates the required binding, inserts its positions and
authors exact cells in one guarded edit. An empty binding also receives an explicit
rest identity when the destination differs from rest; subsequent gestures update it.
Scalar keys receive only the additive delta or multiplicative ratio from that
gesture, preserving contributions from other bindings. Vertex offsets add to
the selected deform key. The base model is never used as a fallback recording
destination. Escape or focus loss cancels a drag; changing the selection,
recording property group, parameter pose or undo state stops capture. Stop
keeps completed edits. Physics pauses in Mesh and Record and resumes in Arrange.

## Verification and the starter

`apps/site/e2e/workspaces.ts` verifies fixed-texture drafts, local history,
Apply/Cancel, multi-vertex movement, recording between keys, independent binding positions, paired axes,
stale revisions and responsive layouts. `apps/site/e2e/studio.ts` drives the
remaining visible controls against the real wasm editor.
It checks transform previews and cancellation, undo, numeric validation,
params and two-param keys, masks, slots, spine fitting, model settings,
modal errors, artwork import, mesh generation, preview export, resizing,
mobile panels, dirty-close protection and saved-file reopening. Its probe
reads state for assertions; it does not author the edits being tested.

`bun run --filter catchlight-site e2e` adds both backends, WebGPU, WebGL2,
the browser offering neither, agent edits over a private socket and multiple
GL canvases. It serves the production bundle. Renderer readback validates
pixels because headless WebGPU can render without compositing its canvas into
a page screenshot. Screenshots are written under `target/e2e/` for inspection.

Mica is original layered artwork with 13 parts and six controls: head tilt,
look, blink, wave, tail sway and breathing. To regenerate it, build wasm and
start the site with the probe available, then run:

```sh
bun run --filter catchlight-site sample http://localhost:5173/
```

The script rasterises its own SVG artwork in Chromium and authors the model
through the editor protocol. It writes `apps/site/public/sample.clm`, which
is a Git LFS object. No private reference model is required.
