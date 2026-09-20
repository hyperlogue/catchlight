# Catchlight studio

The approved editor design atlas, version 5
(`artifact_61435508aba949769aa9e2b1a8e5f5bb`), establishes the visual direction.
This document carries its decisions into the production editor and future UI
work. The character occupies the center; structure, properties and posing tools
stay close to it. The production editor also exposes model tabs, rigging and
file operations using the same visual system.

## Design principles

- **Hierarchy:** the selected object, editable values and next action lead.
  Use size, weight, contrast and spacing together. Working text stays readable;
  secondary information earns less emphasis through color, not tiny type.
- **Grouping:** put controls beside the content they affect. Modes belong at
  the top of the center pane; navigation and zoom tools sit on the canvas.
  Separate work areas with 1px dividers. Group related fields with space, then
  use a larger gap before the next property section.
- **Restraint:** artwork supplies the strongest color. Selection is a single
  continuous row fill with a brighter label. Keyboard focus has a separate
  ring. Give a task one visually dominant action, such as Apply mesh or Start
  recording; everyday file actions stay quiet.
- **Explicit ownership:** show whether an edit changes the base model, a mesh
  draft or a parameter keypoint. Recording names the node, parameter, pose and
  property group. Scrubbing previews a pose; it does not author a key.
- **Recovery:** keep drafts until an explicit commit or cancel. Leaving a mesh
  draft offers Apply, Discard or Keep editing. Stop recording keeps completed
  keys. Validation and errors say what the person can do next.
- **Adaptation:** respond to the space a pane has, including after resizing.
  Wrap controls before they collide. Narrow layouts use dismissible side
  panels, preserve the studio identity and keep the canvas usable.

## Visual system

`src/theme.css` is the token and component-style source of truth. Its rules live
under `@layer catchlight`, scoped to `.catchlight`; hosts can override tokens
or compose the unstyled React parts. Apply changes to existing rules so each
state has one definition.

### Identity and color

Use the existing `assets/logo-transparent.svg`: the original circle, highlight
and gradient on a transparent background. The header displays it at **20px**
next to the lowercase **catchlight** wordmark and small, spaced **STUDIO** suffix.
Keep the full title on mobile by arranging the header into two rows. The
original white-backed `assets/logo.svg` remains available for standalone use.
Graphite is the application surface; the identity study's pale neutral surface
is suitable for presentations outside the editor.

| Role | Default | Token / treatment |
| --- | --- | --- |
| Canvas | `#171d22` | `--cl-bg`; the wasm viewport clear color uses its linear RGB equivalent |
| Panels | `#1d242b` | `--cl-mantle` |
| Recessed surfaces | `#12171b` | `--cl-crust` |
| Editable controls | `#29343e` | `--cl-surface`; a small lift above the panel |
| Main text | `#e8eef2` | `--cl-text` |
| Supporting text | `#b9c5ce` / `#92a3b0` | `--cl-text-secondary` / `--cl-text-dim` |
| Intent | `#72bffb` | `--cl-accent`; original logo blue, with `#9bd3fd` hover/focus and `#68b7f7` stronger emphasis |
| Selected row | `#233b4e` | `--cl-accent-soft`, brighter label, no extra edge bar or inner outline |
| Work-area boundaries | `#364550` | `--cl-divider`, 1px |
| Record action | `#dc3345` | `--cl-record`, white text, circle to start and square to stop |
| Recording markers | `#ff6370` | `--cl-record-marker`, readable on selected key backgrounds |
| Recording surfaces | `#382b30` | `--cl-record-soft`, with pale `--cl-record-text` |

Use saturated red for the recording action and small indicators. Larger
recording areas use subdued tints. Keep labels and shapes meaningful without
color: authored keys are filled diamonds; derived keys are hollow. Selection,
hover, keyboard focus, recording and errors are separate states.

### Type, controls and spacing

- System sans, regular and semibold; 13px working text and values, 12px labels,
  11px metadata. Selected object titles are 17px. Use tabular numerals for
  editable values; reserve monospace for identifiers and code.
- Use the 4/8/12/16/24/32 spacing scale. Place coordinates close together and
  allow roughly 28px between property groups. Typical side panels start at
  232px and 288px and remain resizable.
- Desktop buttons are at least 32px high; numeric fields are 34px. Main touch
  controls grow to 40px. Controls have 6px corners, groups 8–10px, dialogs 14px.
- Icons are normally 16px and support labels. Import artwork has 16px horizontal
  padding. Fields keep useful widths; three-component vectors may use a row
  below their label instead of squeezing digits into narrow columns.
- Axis/channel names precede a number; units follow it. `NumberField` uses
  `prefix` for X/Y/Z, RGB and grid row/column labels, and `unit` for °, ×, px,
  Hz or %. Keep unit text outside the editable value.
- Keyboard focus uses a 2px ring. A focused field has one shared outline around
  the value and its prefix/unit. Motion only communicates state and respects
  reduced-motion preferences.

## Workspace and interactions

- The top bar holds file actions, undo/redo, command search and save state.
  Model tabs sit underneath it, above the panels they control. Arrange, Mesh
  and Record stay in the center-pane toolbar. Selection and pan sit at the
  lower left of the canvas; zoom and fit sit at the lower right.
- Structure and Artwork share the left panel. Search retains ancestors so
  a result still has a place in the model. The gallery uses actual decoded
  artwork, including TGA, with transparency preserved.
- Arrange owns selection, pan, zoom and base transform handles. Mesh opens an
  isolated artwork view; Record captures gestures into a selected binding cell.
  One canvas survives model switches and empty states. A fitted view leaves
  breathing room around the artwork and follows resizing until the user pans
  or zooms.
- Properties follow the selected node's capabilities. Model-wide checks,
  welds, physics constants and metadata live in the Model tab.
- The bottom shelf holds params and bindings. Pose previews never author
  model data. A sweep previews the selected param and restores its old value.
- Panels resize by pointer or keyboard and remember their dimensions.
  Below 1000px the structure panel becomes a drawer; below 600px both side
  panels do. The toolbar toggles, close buttons, scrim and Escape dismiss them;
  closing returns focus to the toggle. Focus mode makes more room for artwork.

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
blue marks topology; `--cl-record` adds a red cue for keypoint capture. Labels,
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

`bun run --filter catchlight-demo e2e` adds both backends, WebGPU, WebGL2,
the browser offering neither, agent edits over a private socket and multiple
GL canvases. It serves the production bundle. Renderer readback validates
pixels because headless WebGPU can render without compositing its canvas into
a page screenshot. Screenshots are written under `target/e2e/` for inspection.

Mica is original layered artwork with 13 parts and six controls: head tilt,
look, blink, wave, tail sway and breathing. To regenerate it, build wasm and
start the site with the probe available, then run:

```sh
bun run --filter catchlight-demo sample http://localhost:5173/
```

The script rasterises its own SVG artwork in Chromium and authors the model
through the editor protocol. It writes `apps/site/public/sample.clm`, which
is a Git LFS object. No private reference model is required.

## Reviewing UI changes

Review the built editor with its real wasm replica and renderer. Compare a
selected part in Arrange, a mesh draft, and armed recording with the approved
hierarchy and palette. Also inspect the affected specialized panels, empty
states and dialogs; prototype-only controls are not substitutes for these.

Check desktop, tablet, phone and intermediate widths, including narrow resized
panes. Look for clipped values, overlapping controls, inaccessible panel
contents and competing selection/focus treatments. Exercise numeric commit
and Escape, undo, mesh Apply/Discard, recording destination changes, drawer
focus and dialog recovery as relevant. Use renderer readback when a headless
GPU canvas does not appear in screenshots. The browser harness and its runtime
requirements are documented beside `apps/site/e2e/run.ts` and `drive.ts`.
