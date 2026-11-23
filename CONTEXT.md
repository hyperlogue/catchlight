# Catchlight

Catchlight is a software stack for 2.5D character animation: a character is a
tree of nodes whose textured 2D meshes are deformed and stacked in depth to
suggest 3D movement.

Two different things in this codebase are called the reference. **The
reference** is inochi2d and nijilive: catchlight reads their models and
matches their rendering semantics, so a comment saying a rule matches the
reference is naming behaviour catchlight had to reproduce, not a rule it
chose. [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) says where that
reaches. **The reference model** is the private rig at
`example_models/reference/`; no such rig ships here, so the tests that need
one are ignored.

## Language

### The model

**Model**:
A complete character — its node tree, params and their bindings, textures,
welds, animations and extensions — as authored, plus what
is derived from them without any pose. The one form everything else comes
from: the file stores it, the editor changes it, puppets animate it, render
caches are built from it.
_Avoid_: rig, document

**Model file**:
A model as stored on disk, in `.clm`, the only format catchlight loads.
Sections a reader does not recognise are carried through a save unchanged.
_Avoid_: document

**Generation**:
A model's edit count: every change to the model moves it, and a generation
is never reused, not even after an undo. What is derived from a model —
a bake, a render cache — is valid for one generation.
_Avoid_: revision (that's a session's publication number), version

**Id**:
The identity of a node, param, texture or slot: a string, chosen by the
author or generated, that never changes on its own and is stored with the
model. A generated node Id is its parent's Id, a slash, then the kind and a
random suffix — `head/part-3f9a2c1e`, say; that prefix is a reading aid, not
a path, and stays as-is when the node moves. Only the author renames an Id,
and renaming breaks any addon that referenced the old one.
_Avoid_: ref, index, name

**Name**:
The label a human sees on a node or param. Free to change and free to
repeat; no part of a model refers to a node or param by name. Animations are
the exception: an animation is played by its name.
_Avoid_: id

**Node**:
One element of a model's tree. Every node has an Id, a name, a transform, a
z order, a kind, and whether it is enabled; its place among its siblings is
part of the model too.

**Root**:
The one node of a complete model with no parent. It is always there and
cannot be removed or moved.

**Enabled**:
A node setting: a disabled node, and everything beneath it, is not drawn.
_Avoid_: visible, hidden

**Kind**:
What a node is — group, part, composite, mesh group, motion response or spine.
Fixed when the node is created.

**Group**:
A node with no geometry of its own; it exists to position its children.
_Avoid_: empty

**Part**:
A node that draws a mesh with a texture, its albedo. A part with no albedo
draws nothing.
_Avoid_: layer, sprite

**Composite**:
A node that renders its subtree as one image before blending it into the
scene, so opacity, tint, blend mode and masks apply to the subtree as a whole.

**Blend mode**:
How a drawable's pixels combine with what is already drawn beneath it —
normal, multiply, screen and the rest of the reference's set.

**Tint**:
A colour a drawable's pixels are multiplied by. A **screen tint** is a
colour screened over them instead. Like opacity, authored on the drawable
and not inherited.

**Mesh group**:
A node whose mesh deforms the geometry beneath it: every vertex of a part or
mesh group beneath it, reached without passing through a group, follows the
triangle it currently sits in, after the vertex's own deforms, so a child's
deforms compose with the group's. A group beneath it is moved whole, if
translate children is on. A mesh group is never drawn.
_Avoid_: deformer, warp, dynamic mesh group (every mesh group is)

**Motion response**:
A driver node whose scalar spring follows a weighted combination of
**inputs** — posed params, projected part slots and its immediate parent
response — and writes the lag (followed value minus target) to an optional
**output** param. A child reads the parent's unclipped followed value. The
lag returns to zero at a held pose. A param that is a response's output
belongs to the response: animations do not write it.

**Spine**:
A node holding a chain of joints drawn on the art, with a bend param for each
link; every part and mesh group beneath it, reached without passing through a
group, turns with the joints above it. A spine's walk stops at an inner
spine, so spines do not nest. Never drawn.
_Avoid_: bone, skeleton, deformer

**Link**:
One segment of a spine between two joints. A link with no bend param is
**rigid**: it never turns.
_Avoid_: segment, bone

**Joint**:
One hinge of a spine, at the far end of a link. The bend a link turns by is
read at the joint above it — the spine's own origin for the first link — so a
spine's joints and its links are the same count and offset by one.
_Avoid_: pivot, bone

**Guide part**:
A part a spine's joints may follow: each joint sits where the guide part's
deform carries its drawn position, so links can stretch as the guide part
moves.

**Bend**:
A link's turn away from the drawn shape, in half turns, counterclockwise in
the model's Y-up frame. Zero is the art as drawn.
_Avoid_: angle, rotation

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
A node setting: the node is placed relative to the frame the whole model is
drawn in, ignoring every ancestor's transform, the root's included.

**Translate children**:
A mesh group setting, on for a newly created mesh group. Its deform
translates each group it reaches as a whole, carrying the group's
descendants with it. Turning the setting off leaves group transforms in
place.

**Propagate mesh group**:
A composite setting, off by default: whether a mesh group or spine above the
composite deforms the nodes inside it.

**Transform**:
A node's placement relative to its parent — translation, rotation and scale.
Translate children is the only way a deform moves a transform, and only the
posed one; the authored transform is never changed by posing.

**Z order**:
A number every node carries; higher draws in front. A node's z order adds to
its parent's, and the total orders drawables among their siblings in the
draw: a composite is ordered as one unit, and its children only among
themselves. Ties keep sibling order.
_Avoid_: zsort

**Mesh**:
A meshed node's 2D geometry: vertices, triangles and an origin, plus texture
coordinates when it is drawn with a texture.

**Texture**:
An image a part draws with; several parts may share one. The model stores it
exactly as it was given and never re-encodes it. A texture no part uses any
longer is removed.

**Mask**:
A drawable's rule for being clipped by another drawable's shape: the source,
and whether what the source covers is kept (**mask**) or cut away (**dodge
mask**). A drawable may have several, applied in order, and never masks
itself.
_Avoid_: mask binding

**Slot**:
A handle on a part, identified by an Id unique within the part, filled by one
of the part's vertices or left unfilled, so welds and motion responses can
refer to vertices by Id. Whoever owns the part fills its slots; re-authoring
the mesh empties every slot, and the author must fill them again.
_Avoid_: seam (that's the join a weld closes), vertex ref

**Weld**:
A pairing of two different parts, slot by slot: each pair names a slot on
each side and a weight, the first part's share of the meeting point. Each
pair of vertices is pulled toward a shared point after all other
deformation, so the join stays closed. One weld per pair of parts, and a
slot in at most one of its pairs.
_Avoid_: glue

### Params and posing

**Param**:
A named scalar the author exposes for posing, with a range and a default
value. Bindings read a param's current value.
_Avoid_: parameter, axis

**Binding**:
A param's control over one property of one node — its target — or two
params' joint control over it. Each binding owns its key positions along each
driving param and its interpolation mode; their combinations form the grid
whose cells the current values interpolate between. Several bindings on one
target combine: positions, rotations, scales and z order add; opacity and
tint multiply.
_Avoid_: mask binding (that's a mask), child binding (that's a pin)

**Target**:
The property a binding drives: a node's deform, or one scalar — a component
of its translation, rotation or scale, its z order, opacity or a tint
channel.

**Key position**:
A position owned by one binding, normalized 0..1 across a driving param's
range, at which that binding may hold authored cells.
_Avoid_: axis point

**Cell**:
One point of a binding's grid, holding one value — authored or derived. A
deform binding's cell holds an offset for every vertex.

**Keypoint**:
A cell whose value the author set. Cells that are not keypoints are derived
from the keypoints around them.

**Corrective binding**:
A binding driven by a motion response's output. Its cells at zero lag hold
the target's identity, so the binding changes nothing while the response is
at rest.

**Interpolation mode**:
How values between keys are read: nearest, linear, stepped or cubic.
Bindings and lanes each carry one. **Stepped** holds a key's value from its
own position until the next key.

**Deform**:
A per-vertex offset applied to a node's mesh. A node's deform is the sum of
the deforms from every source acting on it: bindings, mesh groups, spines,
welds and scratch.
_Avoid_: warp

**Pose**:
An assignment of values to a model's params; a param the pose leaves out
reads its default. A *posed* puppet has had a pose applied.

**Base value**:
The value a node property has in the model, before any pose is applied.
_Avoid_: base (alone — say base value or base model)

**Rest**:
The state of a puppet whose params are all at their defaults and whose
drivers have all settled. Distinct from **rest geometry**, a mesh as drawn
before any deform.

**Key pose**:
A pose holding a binding's driving inputs at one of that binding's grid
positions, with every other param at its default and no driver running.
A sampler may visit the union of positions across bindings; that union does
not own or change any binding's grid.
_Avoid_: keyframe (that's animation), sweep

**Authored**:
Set directly by a person and stored as they set it.

**Derived**:
Computed from authored values by a rule; it can always be recomputed and is
never edited directly.

**Driver**:
A writer of param values that claims them rather than setting the pose. A
motion response is the only driver; an animation sets the pose and is not
one.

**Contribution**:
One driver's weighted claim on a param. Claims are blended by weight in no
particular order; the posed value fills whatever weight the claims leave
under one, so a claim of full weight overrides the pose.
_Avoid_: claim (as a separate concept — a claim is a contribution)

### Animating

**Puppet**:
A model being animated: its pose, its drivers' state, its animation playback,
its scratch, and the evaluated frame. The model itself is never changed by
animating it, and several puppets can animate one model.
_Avoid_: instance, actor

**Bake**:
What a model is compiled into for puppets to evaluate, made once per
generation and shared by every puppet of that model. A puppet whose model
has moved on rebakes at its next tick.

**Evaluated frame**:
The transforms, z orders, colours and deforms a puppet's last tick produced
— what a render cache is refreshed from.

**Tick**:
Evaluating a puppet's next frame: rebake if the model changed, step the
playing animation, which sets the pose, and step the drivers, which claim
params; then, if anything changed, apply the pose through bindings, apply
scratch, resolve transforms, and deform by spines, mesh groups and welds in
that order.

**Pin**:
A spine's or mesh group's hold on one vertex beneath it — the link that
turns it, or the triangle it is followed from — found from rest geometry
once per bake. A mesh group's pin is only a starting point: each pass
searches from it for the triangle the vertex currently sits in. Never
authored.
_Avoid_: attachment (that's bytes beside a command), child binding

**Scratch**:
An edit in progress, shown live on a puppet and never in the model: a
deform or a transform under a drag. A scratch transform lasts until it is
cleared; a scratch deform also ends when the puppet rebakes. Dragging a
param is not scratch: it changes the pose.
_Avoid_: preview (that's a rendered image), draft, optimistic update

**Render cache**:
The renderer's own derived copy of a model — prepared from the model,
shared by every puppet of it, refreshed from a puppet every frame, rebuilt
when the model's generation changes. Never authoritative.

**Camera**:
The view a frame is drawn through: a centre and a height in world units,
the width following the target. World space is Y-up and the camera never
flips it. Its centre and height together are its framing.
_Avoid_: view

**Render list**:
What the render cache collects from a posed puppet: its drawables in the
order they are drawn, with the masking and compositing each one needs.
_Avoid_: draw list, scene

**Animation**:
A named, timed sequence of param values with a length in frames: an optional
lead-in played once, then the **loop region** between it and an optional
lead-out, repeated.

**Lane**:
One animation's track over a single param, with its own interpolation mode.

**Keyframe**:
A value the author set on a lane at one frame of an animation. Before the
first keyframe and after the last, the lane holds the nearest one.
_Avoid_: keypoint (that's an authored binding cell)

### Authoring

**Import**:
Creating catchlight data from something else: a model in a foreign format
(inochi2d, LivLiv) converted to a model file; loose artwork added as new
parts to an open model; or a manifest or a structure with its images made
into a new session. A structure that is a fragment can instead be installed
into an existing session.

**Manifest**:
A JSON description of a model as loose textures, params and nodes, with
meshes generated from the textures rather than authored. Written by hand or
exported from a session; lossy, where a model file is not.

**Base model**:
The model an addon is authored against and installed into.
_Avoid_: base (alone — say base value or base model), host

**Addon**:
A fragment — a subtree with its bindings, welds, textures and animations —
authored against a base model and installed into it. It names what it needs
from the base by Id, carries no params and no extensions, and never changes
the base's params. Each binding and weld it carries touches a node it
provides. Two addons that provide the same Id are alternatives: only one can
be installed at a time.

**Fragment**:
The shape of an addon: a model whose every root names a parent it does not
carry. A complete model has one root with no parent, and a model is one
shape or the other, never guessed.
_Avoid_: partial model, subtree

**Requirement**:
Something an addon names but does not carry, which must be present in the
base it is installed into: a node, a drawable a mask names, a part a weld
names, a slot on such a part, or a param. Found by scanning, never declared.
Textures are never requirements: an addon carries its own.
_Avoid_: dependency

**Install**:
Merging an addon into a model as one edit, after checking that every
requirement is present and no Id the addon provides is already taken.
_Avoid_: merge (the CLI's verb for it)

**Uninstall**:
Undoing an install: removing every node it added with its whole current
subtree, and the bindings, welds, textures and animations it brought.

**Extract**:
Copying a subtree out of a model as an addon, leaving the model unchanged:
the subtree, the bindings on it, the welds touching it and the textures it
draws. Animations stay behind.

**Extension**:
A vendor's annotation on a whole model, filed under a key the vendor owns
and carried without ever being read: its value is JSON or opaque bytes. The
key is vendor first, then a dot — `acme.camera` — and `catchlight.` is the
format's own. An addon carries none.
_Avoid_: metadata, custom data, plugin

### Editing

**Studio**:
Catchlight's authoring application, where a person and an agent work on the
same model through shared editor sessions. It runs locally against a
connected editor, or in the browser alone as the demo.
_Avoid_: editor (the thing that edits, which the studio talks to)

**Editor**:
The one thing that edits: it holds sessions, takes commands and keeps each
session's history. It runs in the tab or as a local server, and every client
speaks to it the same way.

**Session**:
One open model in the editor, with its history, its revision, and the store
file it saves to, if any.

**History**:
A session's record of its edits, which can be undone, redone or jumped
through. Bounded: the oldest entries fall away.

**Revision**:
A session's increasing publication number, advanced by a changed edit or
history navigation. Session replies and feeds identify the revision they
capture; failed and no-op edits leave it unchanged. An edit may name the
revision it expects, and fails if the session has moved past it.
_Avoid_: version, generation (that's the model's edit count)

**Fork**:
A new session started from another session's model at a given revision,
with a history of its own.

**Store**:
Where the editor's own files live. A path on the wire names a file in the
store, never one a client holds.
_Avoid_: filesystem

**Open**:
Reading a model file from the store into a new session, which saves back to
it.
_Avoid_: load (that's the runtime reading a file), import

**Save**:
Writing a session's model back to its file in the store. Moves no revision.

**Export**:
Handing a session's model out as a payload — a model file, or a manifest
with its textures — without saving the session.

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
One request to the editor. An **edit** changes a model or which sessions
exist, and is the only kind that moves a revision; **presence** shares view
state; a **query** changes no session. Replica queries are pure functions of
the model and can be answered locally; server queries need the editor's
sessions, store or renderer.
_Avoid_: request (that's the envelope), action, document command

**Edit batch**:
Several edits sent as one command and applied as one history step, or only
checked.

**Attachment**:
Bytes that arrive beside a command, under a name the command declares — a
texture, a model file, a manifest. Nothing is ever staged: bytes enter only
inside the command that uses them.
_Avoid_: upload, blob, staged file

**Payload**:
Bytes that leave beside a reply — an exported model, a preview's image or
an extension's bytes.
_Avoid_: attachment (that's the way in), body

**Presence**:
A session's shared pose, camera and selection, as last published by any
client. Published, never saved, never undone.

**Preview**:
A rendered image of a session's model at a given pose, through a camera.
_Avoid_: scratch (that's the live edit)

**Keep**:
Rendering a chosen few parts and nothing else, over transparency. Masks
still clip the kept parts unless they are stripped too.
_Avoid_: solo, isolate (that's rendering a composite on its own), extract
(that's cutting an addon out)
