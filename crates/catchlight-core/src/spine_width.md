
## Independent hair locks and projected width

The existing spine targets and ordinary `Deform` bindings support local width
before bending in the same tick. No additional runtime node or model fields
are required for the bounded first trial described here. This is an authoring
recipe, not automatic artwork decomposition or a guarantee about arbitrary
meshes.

### Ownership and placement

Give each independently moving lock one spine, with its own bend params and
particle chain. Short bangs and long hanging locks are sibling subtrees under
their common head/body placement. Derive link lengths and material response
from each lock's measured span; vary stiffness/damping where appropriate.
Contribution weight controls authority, not response timing. Keep the current
generated weight of 0.5; with one driver and zero posed bends this halves the
simulated bends, but not necessarily the resulting vertex travel.

Carver owns semantic cuts, pixel ownership, neutral alpha reconstruction,
underpaint, and assignment of reflections, shadows and mask sources. A painted
ridge is only evidence for a cut. Do not put competing spines on an unsplit
part, nest spines to layer their motion, or duplicate an antialiased sheet and
independently feather its copies. Preserve the original sheet and its existing
single spine when ownership, root coverage, or the mesh budget is insufficient.
Width can remain absent in that fallback.

Apply width bindings directly to every affected meshed part below its one
spine, including companions that must stay attached to that surface. Sample
one common rest-space field at each part's vertices, then convert its offsets
to that part's local coordinates. Copying offset arrays between different
meshes or coordinate frames is incorrect. Preserve UVs, textures, masks,
blend modes, composite placement and z order. Spine descent stops at mesh
groups, nested spines, and composites with `propagate_mesh_group = false`;
validate the actual subtree before converting it. A mesh group below a spine
is a different interpolation path, not equivalent to direct part bindings.

Put common placement on an ancestor once. When wrapping already positioned
parts, preserve their neutral transforms and any existing cancellation of the
new parent's static and bound translations. Do not copy the head/body motion
onto both the new spine and its children without that cancellation. A common
mesh group above the spine warps the width-adjusted, bent geometry afterwards.

### Same-frame contract

`Puppet::tick` poses anchors, steps drivers and resolves their contributions
and limits, folds ordinary bindings, applies spine bends, then runs mesh
groups and welds. A width binding reads the **resolved bend param**, including
the chain's weight and limits, which is also what the spine reads in that
tick. Do not read raw particles, multiply by weight again, or feed a previous
frame's bend back through an external pose loop. Bend values are half turns;
convert to radians with `pi * bend` where an orientation prior needs it.
Binding key positions instead use normalized param coordinates:
`(value - min) / (max - min)`.

For rest vertex `p`, binding offset `d`, and its rest-assigned spine transform
`T`, the result in spine space is `T(p + d)`, not `T(p) + d`. The assignment
to a link and its ramp remains the one baked from rest geometry. Common
ancestor transforms are accounted for in the coordinate conversions; they
do not become a second deform. Width fields must preserve that rest
assignment rather than sliding vertices along the lock.

This same-frame result does **not** change physics-anchor dependencies.
Directly posed ancestor transforms reach anchors immediately; another
driver's output and mesh-group `translate_children` shifts reach dependent
anchors one frame later. Avoid driving the same lock's attachment with its
own bend outputs. Combined body movement must be reviewed under this existing
rule, even though width and bending themselves use the same current values.

### Bake a bounded orientation prior

Measure a continuous rest centerline coordinate `s`, unit normal `n(s)`, and
each vertex's signed transverse distance `u` in a common lock frame. Choose
a root envelope `e(s)` in `[0, 1]`, identically zero through the attachment
patch and smoothly increasing below it. The crown and root support stay fixed.
Use a smooth frame and envelope across link boundaries, not one constant
width per link. Mesh interpolation remains piecewise linear; sample enough
contour and curvature points to avoid visible painted-edge kinks.

Planar bending does not identify out-of-plane twist. Author an explicit small
prior, for example at link `i`:

```text
phi_i(s, head_yaw, bend_i) = phi0(s) + a(s) * head_yaw + c_i(s) * pi * bend_i
r_i = clamp(cos(phi_i) / cos(phi0), 0.98, 1.02)
```

Keep `phi0` safely away from edge-on, bound `phi_i` to the supported facing
range, and choose coefficients from paint/orientation evidence. Zero input
must give `phi_i = phi0` and exactly zero offsets. A zero rest orientation
only narrows; widening relative to rest requires a supported nonzero rest
orientation. The chain already supplies response lag; no width oscillator is
needed. This ratio describes projected width, not physical volume or stretch.

A binding spans at most two params. Use one pair `(head_yaw, bend_i)` per
participating link and target part. Blend neighboring links spatially with
smooth, nonnegative weights `lambda_i(s)` whose sum is at most one:

```text
d_i(s, u) = n(s) * u * e(s) * lambda_i(s) * (r_i - 1)
d = sum_i d_i
```

The shared weights allocate **one** 2% budget across all these bindings;
independently adding several full-strength 2% fields would exceed it. This
construction blends local projection ratios. It is not exactly
`cos(phi0 + a * yaw + sum_i c_i * bend_i) / cos(phi0)`, which is a nonlinear
function of more than two inputs. That distinction is acceptable only when
the small authored approximation matches the intended look.

Bake a small fully authored grid using **Linear** interpolation, with keys at
the neutral inputs and the supported extrema (add intermediate keys only
when measured approximation error warrants them). Author the neutral cell as
literal zeros. Linear/bilinear interpolation is a convex combination of the
bounded cells, so this fixed normal field keeps its width bound between keys.
Cubic interpolation can overshoot; stepped or nearest interpolation jumps.
Check the total field if the part already has Deform bindings: their offsets
add, and existing head-turn fields may already change width. The 0.98–1.02
bound applies to this authored local width contribution, not to every final
screen-space measurement after other deforms or placement.
The param pair, target node and property identify one binding. If that exact
binding already exists, merge the new field into its authored cells while
preserving the existing deformation; adding the same key cannot create a
second independent source.

This is a one-shot bake over the final mesh, with no added simulation and no
required extra vertices. Reuse boundary/curvature samples within the character
budget. Each extra binding still costs grid storage and per-frame vertex
evaluation; omit zero-support fields and measure before increasing lock count,
mesh density or grid size. Re-bake the fields if simplification changes vertices.

### Validation and remaining limits

The regression tests in `tests/spine_width_bindings.rs` exercise ordinary
bindings through the public model/puppet interface: neutral identity,
same-tick width and bend under live chains at 30/60/120 Hz, bounded overlapping
fields, fixed roots, companion geometry, parent placement, independent lock
response, and model-file round-trip. Existing `spine_node` tests cover the
mesh group above the spine; `spine_chain_node` covers the anchor lag rule.
The CLI and editor-server `spine_width_roundtrip` tests pass the same synthetic
model through a CLI file edit and editor import/export without losing fields.
These are synthetic geometry checks, not a rendered hairstyle acceptance.

Before enabling the Carver trial, compare neutral pixels and geometry against
the source with all width/bend inputs zero and physics disabled. Separately
check the settled dynamic pose: gravity and solver residuals need not give
exactly zero bends. Exercise yaw/pitch/roll, combined head/body motion, fast
pulses and stopping at 30/60/120 Hz. Verify short/long timing, root coverage,
painted-edge tangents, continuous local widths, clipping, reflection/shadow
placement and no newly reversed paint-bearing triangles. Include editor
import/export and CLI file operations on the generated model, a second
hairstyle, and the unsplit fallback. Pixel reconstruction and missing coverage
remain Carver acceptance work; no width mechanism can supply missing paint.

The spine ramp is position-continuous but does not guarantee tangent
continuity at joints, nor prevent triangle inversions. Its documented interior
stretch remains. A smooth width envelope cannot repair those properties;
small per-link bends, supported root geometry and appropriate mesh samples
must pass the visual checks. Do not describe the solver as rate-independent.

Consider native support only if an actual trial needs a coupled orientation
function beyond two-input fields, a shared nonlinear width budget that cannot
be baked adequately, or guaranteed tangent continuity through joints. Those
would be distinct missing contracts, not a missing same-frame ordering step.
Any future orientation/width fields should be optional, with absence giving
exactly the existing geometry and behavior (width factor one), including old
files and manual spines. No such fields are introduced by this recipe.
