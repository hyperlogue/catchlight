# Catchlight

Catchlight is a software stack for 2.5D character animation: a character is a
tree of nodes whose textured 2D meshes are deformed and stacked in depth to
suggest 3D movement.

## Language

### The model

**Model**:
A complete character — its node tree, params and their bindings, textures,
welds, physics settings, animations and extensions — as authored, plus what
is derived from them without any pose. The one form everything else comes
from: the file stores it, the editor changes it, puppets animate it, render
caches are built from it.
_Avoid_: rig, document

**Model file**:
A model as stored on disk.
_Avoid_: document

**Id**:
The identity of a node, param or texture: a string, chosen by the author or
generated, that never changes on its own and is stored with the model. A
generated node Id carries the kind and the parent it was created under —
`head/part-3f9a2c1e`, say; that prefix is a reading aid, not a path, and
stays as-is when the node moves. Only the author renames an Id, and renaming
breaks any addon that referenced the old one.
_Avoid_: ref, index, handle, name

**Name**:
The label a human sees on a node or param. Free to change and free to
repeat; nothing refers to anything by name.
_Avoid_: id

**Node**:
One element of a model's tree. Every node has an Id, a name, a transform, a
z order and a kind.

**Kind**:
What a node is — group, part, composite, mesh group or simple physics. Fixed
when the node is created.

**Group**:
A node with no geometry of its own; it exists to position its children.
_Avoid_: empty

**Part**:
A node that draws a mesh with a texture.
_Avoid_: layer, sprite

**Composite**:
A node that renders its subtree as one image before blending it into the
scene, so opacity, blend mode and masks apply to the subtree as a whole.

**Mesh group**:
A node whose mesh deforms the geometry beneath it: every descendant vertex
inside its mesh follows the triangle it currently sits in, after the vertex's
own deforms, so a child's deforms compose with the group's. A mesh group is
never drawn.
_Avoid_: deformer, warp, dynamic mesh group (every mesh group is)

**Simple physics**:
A driver node holding a pendulum hung at its own position; the pendulum's
swing is written into params.

**Meshed node**:
A node that carries a mesh — a part or a mesh group.
_Avoid_: drawable (that's what the renderer draws)

**Drawable**:
A node the renderer draws: a part or a composite.

**Opacity**:
How solid a drawable is. Authored on the drawable and not inherited: a
part's opacity is its own, and a composite's applies to its subtree as one
image.
_Avoid_: alpha (that's a pixel channel)

**Lock to root**:
A node setting: the node's transform is relative to the model's root,
ignoring every ancestor's transform.

**Translate children**:
A mesh group setting: descendants without a mesh (groups, simple physics
nodes) are moved as whole nodes by the group's deform instead of being left
in place.

**Propagate mesh group**:
A composite setting: whether a mesh group above the composite deforms the
nodes inside it.

**Transform**:
A node's placement relative to its parent — translation, rotation and scale.
A deform never changes a transform.

**Z order**:
A number every node carries; higher draws in front. A node's z order adds to
its parent's, and the total orders the drawables.
_Avoid_: zsort

**Mesh**:
A meshed node's 2D geometry: vertices, triangles and an origin, plus texture
coordinates on a part.

**Texture**:
An image a part draws with; several parts may share one. Stored exactly as
the author supplied it, never re-encoded.

**Mask**:
A drawable's rule for being clipped by another node's shape: the source node,
and whether what the source covers is kept or cut away.
_Avoid_: mask binding

**Slot**:
A named handle on a part, filled by one of the part's vertices or left
unfilled, so that welds can refer to vertices by name. Whoever owns the part
fills its slots; re-authoring the mesh empties every slot, and the author must
fill them again. A slot is unique within its part, never across a model.
_Avoid_: seam, vertex ref

**Weld**:
A pairing of two parts, slot by slot: each pair names a slot on each side and
a weight. Each pair of vertices is pulled toward a shared point after all
other deformation, so the join stays closed. One weld per pair of parts.

### Params and posing

**Param**:
A named scalar the author exposes for posing: a range, a default value, and
the key positions along it. Bindings read a param's current value.
_Avoid_: parameter, axis

**Key position**:
A position along a param, normalized 0..1 across its range, at which
bindings may hold authored cells.
_Avoid_: axis point

**Binding**:
A param's control over one property of one node — or two params' joint
control over it. Its grid has a cell at every key position of its param, or
at every pair of key positions of its two params, and the params' current
values interpolate between the cells.
_Avoid_: mask binding (that's a mask), child binding (that's a pin)

**Cell**:
One point of a binding's grid, holding one value — authored or derived.

**Keypoint**:
A cell whose value the author set. Cells that are not keypoints are derived
from the keypoints around them.

**Deform**:
A per-vertex offset applied to a node's mesh. A node's deform is the sum of
the deforms from every source acting on it: bindings, mesh groups, welds.
_Avoid_: warp

**Pose**:
An assignment of values to a model's params. A *posed* puppet has had a pose
applied.

**Base value**:
The value a node property has in the model, before any pose is applied.
_Avoid_: base (alone — say base value or base model)

**Rest**:
The state of a puppet whose params are all at their defaults and whose
drivers have all settled.

**Key pose**:
A pose holding one param at one of its key positions — or two params that
some binding spans together at a pair of theirs — with every other param at
its default and no driver running. Together a model's key poses visit every
authored cell.
_Avoid_: keyframe (that's animation), sweep

**Authored**:
Set directly by a person and stored as they set it.

**Derived**:
Computed from authored values by a rule; it can always be recomputed and is
never edited directly.

**Driver**:
A writer of param values other than the pose. Simple physics is a driver that
happens to be a node; a driver need not be one.

**Contribution**:
One driver's weighted claim on a param, blended by weight with the posed
value and with other drivers' claims, in no particular order.

### Animating

**Puppet**:
A model being animated: its pose, its drivers' state, and the evaluated
frame. The model itself is never changed by animating it, and several
puppets can animate one model.
_Avoid_: instance, actor

**Evaluated frame**:
The transforms and deforms a puppet's last tick produced — what a render
cache is refreshed from.

**Tick**:
Evaluating a puppet's next frame: drivers step, the pose is applied through
bindings, and transforms and deforms are resolved.

**Pin**:
A mesh group's hold on one descendant vertex: the triangle it currently lies
in, found afresh every tick from where the vertex is. Never authored.
_Avoid_: attachment (that's bytes beside a command), binding, child binding

**Scratch**:
An edit in progress, shown live on a puppet and never in the model: a
deform, a transform or a param value under a drag. It lasts until a command
authors it or the drag ends.
_Avoid_: preview (that's a rendered image), draft, optimistic update

**Render cache**:
The renderer's own derived copy of a model — prepared from the model,
refreshed from a puppet every frame, rebuilt when the model changes. Never
authoritative.

**Camera**:
The view a frame is drawn through: a centre and a height in world units,
the width following the target. World space is Y-up and the camera never
flips it.
_Avoid_: framing, view

**Render list**:
What a posed puppet hands the renderer: its drawables in the order they are
drawn, with the masking and compositing each one needs.
_Avoid_: draw list, scene

**Animation**:
A named, timed sequence of param values with a length in frames: an optional
lead-in played once, then the body between it and an optional lead-out,
repeated.

**Lane**:
One animation's track over a single param.

**Keyframe**:
A value the author set on a lane at one frame of an animation.
_Avoid_: keypoint (that's an authored binding cell)

### Authoring

**Import**:
Producing a session's model from what a client supplies — an inochi2d
export, a manifest, a structure with its images, or a model file — rather
than opening one from the store. Into a pristine session it becomes the
session's model; under a parent it is installed as an addon.

**Manifest**:
A hand-written description of a model assembled from loose textures, with
meshes generated from the textures rather than authored.

**Base model**:
The model an addon is authored against and installed into.
_Avoid_: base (alone — say base value or base model), host

**Addon**:
A fragment — a subtree with its bindings, welds and textures — authored
against a base model and installed into it. It names what it needs from the
base by Id, carries no params and no extensions, and never changes the
base's params. Two addons that provide the same Id are alternatives: only
one can be installed at a time.

**Fragment**:
The shape of an addon: a model whose every root names a parent it does not
carry. A complete model has one root with no parent, and a model is one
shape or the other, never guessed.
_Avoid_: partial model, subtree

**Requirement**:
An Id an addon or a manifest names but does not carry — a base model's node
or param, a manifest's texture — which must be present wherever it is
installed or imported. Found by scanning, never declared.
_Avoid_: dependency

**Install**:
Merging an addon into a model as one edit, after checking that every
requirement is present and no Id the addon provides is already taken.

**Extract**:
Cutting a subtree out of a model as an addon: the subtree, the bindings on
it, the welds touching it and the textures it draws. Install's inverse, up
to order.

**Extension**:
A vendor's annotation on a whole model, filed under a key the vendor owns
and carried without ever being read: its value is JSON or opaque bytes. The
key is vendor first, then a dot — `molan.caster` — and `catchlight.` is the
format's own. An addon carries none.
_Avoid_: metadata, custom data, plugin

### Editing

**Editor**:
The one thing that edits: it holds sessions, takes commands and keeps each
session's history. It runs in the tab, in a local process or in a service,
and every client speaks to it the same way.
_Avoid_: server (that's one place it runs), backend

**Session**:
One open model in the editor, with its history and its revision.

**Revision**:
The count of edits a session has taken. Every reply and every feed names the
revision the session is at, and a replica only ever moves to a higher one.
_Avoid_: version, generation

**Pristine**:
A session that has never been edited: a bare root and nothing else. The one
state an import may replace whole.
_Avoid_: empty, blank

**Store**:
Where the editor's own files live. A path on the wire names a file in the
store, never one a client holds.
_Avoid_: filesystem, storage

**Open**:
Reading a model file from the store into a new session, which saves back to
it.
_Avoid_: load (that's the runtime reading a file), import

**Replica**:
A tab's own copy of one session's model, with a puppet posing it: fed by the
editor and never edited locally, so it answers reads and draws frames
without a round trip.
_Avoid_: mirror, cache, local model

**Structure**:
A model with the bytes of its textures and byte extensions taken out. It is
what a feed carries and what the first section of a model file holds.
_Avoid_: skeleton, metadata

**Feed**:
Bringing a replica to a revision: the structure at that revision, plus any
bytes it names that the replica lacks. Feeds of one session never overlap
and never go backwards.
_Avoid_: sync, push

**Command**:
One request to the editor. An edit changes the session's model and moves
the revision; a presence command publishes view state and changes nothing;
a scratch command is served by the local puppet; a replica query is
answered from a replica; a server query needs the editor itself.
_Avoid_: request (that's the envelope), action, document command

**Attachment**:
Bytes that arrive beside a command, under a name the command declares — a
texture, a model file, a manifest. Nothing is ever staged: bytes enter only
inside the command that uses them.
_Avoid_: upload, blob, staged file

**Payload**:
Bytes that leave beside a reply — a preview's image, an extension's bytes.
_Avoid_: attachment (that's the way in), body

**Presence**:
What one client shows the others about itself — its selection, for now.
Published, never saved, never undone.

**Preview**:
A rendered image of a session's model at a given pose, through a camera.
_Avoid_: scratch (that's the live edit)

**Isolate**:
Drawing a chosen few parts and nothing else, over transparency, so a part's
art comes back whole rather than as what its masks leave of it.
_Avoid_: solo, extract (that's cutting an addon out)
