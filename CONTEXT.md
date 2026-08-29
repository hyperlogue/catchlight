# Catchlight

Catchlight is a software stack for 2.5D character animation: a character is a
tree of nodes whose textured 2D meshes are deformed and stacked in depth to
suggest 3D movement.

## Language

### The model

**Model**:
A complete character — its node tree, params and their bindings, textures,
welds, physics settings and animations — as authored, plus what is derived
from them without any pose. The one form everything else comes from: the file
stores it, the editor changes it, puppets animate it, render caches are built
from it.
_Avoid_: rig, document

**Model file**:
A model as stored on disk.
_Avoid_: document

**Id**:
The identity of a node, param or texture: a string, chosen by the author or
generated, that never changes on its own and is stored with the model. A
generated Id carries the kind and the parent it was created under —
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
inside its mesh follows the triangle it sits in. A mesh group is never drawn.
_Avoid_: deformer, warp

**Simple physics**:
A driver node holding a pendulum hung at its own position; the pendulum's
swing is written into params.

**Meshed node**:
A node that carries a mesh — a part or a mesh group.
_Avoid_: drawable (that's what the renderer draws)

**Drawable**:
A node the renderer draws: a part or a composite.

**Lock to root**:
A node setting: the node's transform is relative to the model's root,
ignoring every ancestor's transform.

**Dynamic mesh group**:
A mesh group setting: the group deforms each vertex from where the vertex
currently is, after the deforms already applied to it, rather than from its
authored position — so a child's own deforms compose with the group's.

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

**Seam**:
A named set of slots on a part, each filled by one of the part's vertices, so
that welds can refer to vertices by slot. Whoever owns the part fills its
seams; re-authoring the mesh empties every slot, and the author must fill
them again.
_Avoid_: vertex ref

**Weld**:
A pairing of two parts' seams, slot by slot. Each pair of vertices is pulled
toward a shared point after all other deformation, so the seam stays closed.

### Params and posing

**Param**:
A named scalar the author exposes for posing: a range, a default value, and
the key positions along it. Bindings read a param's current value.
_Avoid_: parameter, axis

**Key position**:
A value of a param at which bindings may hold authored cells.
_Avoid_: axis point

**Binding**:
A param's control over one property of one node — or two params' joint
control over it. Its grid has a cell at every key position of its param, or
at every pair of key positions of its two params, and the params' current
values interpolate between the cells.
_Avoid_: mask binding (that's a mask), child binding (that's an attachment)

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

**Attachment**:
A mesh group's hold on one descendant vertex: the triangle it lies in and its
position within that triangle. Never authored: baked once for a mesh group
that is not dynamic, found afresh every tick for a dynamic one.
_Avoid_: binding, child binding

**Scratch deform**:
A deform a puppet holds for an edit in progress — shown live, never part of
the model.
_Avoid_: preview deform

**Render cache**:
The renderer's own derived copy of a model — prepared from the model,
refreshed from a puppet every frame, rebuilt when the model changes. Never
authoritative.

**Animation**:
A named, timed sequence of param values with a length, an optional lead-in
played once, and a body that repeats.

**Lane**:
One animation's track over a single param.

**Keyframe**:
A value the author set on a lane at one frame of an animation.
_Avoid_: keypoint (that's an authored binding cell)

### Authoring

**Import**:
Producing a model from something that is not a model: an inochi2d export, or
a manifest.

**Manifest**:
A hand-written description of a model assembled from loose textures, with
meshes generated from the textures rather than authored.

**Base model**:
The model an addon is authored against and installed into.
_Avoid_: base (alone — say base value or base model), host

**Addon**:
A partial model — a subtree with its bindings, welds and textures — authored
against a base model and installed into it. It references what it needs from
the base model by Id and never changes the base model's params. Two addons
that provide the same Id are alternatives: only one can be installed at a
time.

**Install**:
Merging an addon into a model, after checking that every Id the addon needs
is present.

**Session**:
One open model in the editor, with its undo history.

**Preview**:
A rendered image of a session's model at a given pose.
_Avoid_: scratch (that's the live edit)
