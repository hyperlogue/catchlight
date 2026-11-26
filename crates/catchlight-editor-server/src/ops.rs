//! Model building shared by standalone commands and batch edits.

use std::collections::{HashMap, HashSet};

use catchlight_core::components::BlendMode as CoreBlendMode;
use catchlight_core::formats::clm::{
    self as clm, ClmIndices, ClmMesh, TextureEncoding as CoreTextureEncoding,
};
use catchlight_core::id::Name;
use catchlight_core::{
    BindingKey, BindingTarget as CoreBindingTarget, Mat4, Model, ModelComposite, ModelMeshGroup,
    ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelSpine, ModelWeld, ReadOptions, Vec3,
};
use catchlight_editor_core::{
    AlphaMask, ContourKnobs, GridKnobs, MeshError, StrandError, UvMap, contour_automesh,
    fit_strand, grid_automesh,
};
use catchlight_editor_protocol::*;

use crate::EditorError;
use crate::session::Session;

pub(crate) fn binding_key(
    params: BindingParams,
    node: NodeId,
    target: impl Into<BindingTarget>,
) -> Result<BindingKey, EditorError> {
    let target = CoreBindingTarget::from(target.into());
    Ok(match params.param_y {
        Some(y) => BindingKey::pair(params.param, y, node, target),
        None => BindingKey::new(params.param, node, target),
    })
}

/// Checked before a command mints a param or node around the joints.
pub(crate) fn check_spine_joints(joints: &[[f32; 2]]) -> Result<(), EditorError> {
    ModelSpine::check_joints(joints).map_err(|e| EditorError::BadTarget(e.to_string()))
}

fn clm_transform_matrix(t: &clm::ClmTransform) -> Mat4 {
    catchlight_core::Transform::from(t).to_matrix()
}

/// A response chain can sit between the mesh and its spine.
pub(crate) fn fitted_spine<'a>(model: &'a Model, node: &'a ModelNode) -> Option<&'a NodeId> {
    node.mesh()?;
    let mut parent = node.parent()?;
    loop {
        let n = model.node(parent)?;
        match n.kind {
            ModelNodeKind::Spine(_) => return Some(parent),
            ModelNodeKind::MotionResponse(_) => parent = n.parent()?,
            _ => return None,
        }
    }
}

/// A root-locked node ends inheritance, so its matrix is already world space.
fn rest_to_frame(model: &Model, id: &NodeId, frame: Option<&NodeId>) -> Result<Mat4, EditorError> {
    let mut cursor = Some(id);
    let mut matrix = Mat4::IDENTITY;
    while cursor != frame {
        let Some(id) = cursor else {
            let Some(frame) = frame else { break };
            let world = rest_to_frame(model, frame, None)?;
            let inverse =
                catchlight_core::components::checked_affine_inverse(world).ok_or_else(|| {
                    EditorError::BadTarget(format!("cannot fit in the singular frame of {frame}"))
                })?;
            return Ok(inverse * matrix);
        };
        let node = model
            .node(id)
            .ok_or_else(|| EditorError::NoNode(id.clone()))?;
        matrix = clm_transform_matrix(&node.transform) * matrix;
        cursor = if node.lock_to_root {
            None
        } else {
            node.parent()
        };
    }
    Ok(matrix)
}

pub(crate) fn spine_fit(
    s: &mut Session,
    part: &NodeId,
    links: u32,
    axis: Option<[f32; 2]>,
    id: Option<NodeId>,
    name: Option<String>,
) -> Result<ResponseBody, EditorError> {
    if links == 0 {
        return Err(EditorError::BadTarget(
            "a spine needs at least one link".into(),
        ));
    }
    let node = s
        .model
        .node(part)
        .ok_or_else(|| EditorError::NoNode(part.clone()))?;
    let mesh = node
        .mesh()
        .ok_or_else(|| EditorError::BadTarget(format!("node {part} holds no mesh to fit")))?;
    let fit = fit_strand(mesh, links, axis).map_err(|e| match e {
        StrandError::Limit(limit) => EditorError::Limit {
            resource: limit.resource.name(),
            limit: limit.limit,
            requested: limit.got,
        },
        e => EditorError::BadTarget(format!("cannot fit a spine to {part}: {e}")),
    })?;
    let parent = node.parent().cloned().ok_or_else(|| {
        EditorError::BadTarget(format!(
            "node {part} is the model's root, so a spine has nowhere to go above it"
        ))
    })?;
    let part_name = node.name.to_string();
    let existing = fitted_spine(&s.model, node).cloned();
    let part_local = rest_to_frame(&s.model, part, Some(existing.as_ref().unwrap_or(&parent)))?;
    let root_offset = part_local.transform_point3(Vec3::new(
        fit.root[0] - mesh.origin[0],
        fit.root[1] - mesh.origin[1],
        0.,
    ));
    let count = links as usize;
    let mut travelled = 0.0f32;
    let joints: Vec<[f32; 2]> = (0..count)
        .map(|i| {
            travelled += fit.lengths[i];
            // The linear part only: an offset from the root is a vector, and
            // the root's own place is what `root_offset` carries.
            let v = part_local.transform_vector3(Vec3::new(
                fit.axis[0] * travelled,
                fit.axis[1] * travelled,
                0.0,
            ));
            [v.x, v.y]
        })
        .collect();
    check_spine_joints(&joints)?;

    let held_targets = match &existing {
        Some(spine) => match s.model.node(spine).map(|n| &n.kind) {
            Some(ModelNodeKind::Spine(sp)) => sp.targets().to_vec(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };

    // Range is half turns, keyed only at its ends: a spine reads a param's
    // value, not its position along a range, and carries no cells to key.
    let mut params = Vec::with_capacity(count);
    for i in 0..count {
        match held_targets.get(i).and_then(Option::as_ref) {
            Some(held) if s.model.param(held).is_some() => params.push(held.clone()),
            _ => {
                let param = s.add_param(
                    None,
                    ModelParam {
                        name: Name::truncated(format!("{part_name} bend {}", i + 1)),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                    },
                )?;
                params.push(param);
            }
        }
    }

    let node = match existing {
        Some(spine) => {
            s.model.set_spine_joints(&spine, joints)?;
            spine
        }
        None => {
            let node = ModelNode::new(
                name.unwrap_or_else(|| format!("{part_name} spine")),
                ModelNodeKind::Spine(ModelSpine::new(joints)),
            );
            let spine = s.add_node(&parent, id, node)?;
            s.model.reparent(part, &spine)?;
            spine
        }
    };
    // Moving the pivot by d in spine space translates the spine by its
    // linear transform times d in parent space. Countertranslate each
    // inheriting child in spine space to preserve the artwork in every branch.
    s.model.update_node(&node, |n| {
        let shift = clm_transform_matrix(&n.transform).transform_vector3(root_offset);
        n.transform.translation = (Vec3::from(n.transform.translation) + shift).to_array();
    })?;
    let children = s
        .model
        .node(&node)
        .ok_or_else(|| EditorError::NoNode(node.clone()))?
        .children()
        .to_vec();
    for child in children {
        s.model.update_node(&child, |n| {
            if !n.lock_to_root {
                n.transform.translation =
                    (Vec3::from(n.transform.translation) - root_offset).to_array();
            }
        })?;
    }
    s.model
        .set_spine_targets(&node, params.iter().cloned().map(Some).collect())?;
    Ok(ResponseBody::SpineFit { node, params })
}

pub(crate) fn build_mesh(
    verts: Vec<[f32; 2]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<[u32; 3]>,
    origin: [f32; 2],
) -> Result<ClmMesh, EditorError> {
    let vcount = verts.len();
    if (!uvs.is_empty() && uvs.len() != vcount)
        || indices
            .iter()
            .any(|tri| tri.iter().any(|&i| i as usize >= vcount))
    {
        return Err(EditorError::BadTarget("malformed mesh".into()));
    }
    let flat: Vec<u32> = indices.concat();
    let indices = if u16::try_from(flat.iter().max().copied().unwrap_or(0)).is_ok() {
        ClmIndices::U16(flat.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(flat)
    };
    Ok(ClmMesh {
        verts: verts.concat(),
        uvs: uvs.concat(),
        indices,
        origin,
    })
}

/// The UV mapping is fitted from the mesh being replaced when there is one:
/// cropped imported textures are not centred, and a wrong guess moves the art
/// rather than failing.
pub(crate) fn automesh(
    model: &Model,
    node: &NodeId,
    mode: AutoMesh,
) -> Result<ClmMesh, EditorError> {
    let Some(ModelNodeKind::Part(part)) = model.node(node).map(|n| &n.kind) else {
        return match model.node(node) {
            Some(_) => Err(EditorError::NotAPart(node.clone())),
            None => Err(EditorError::NoNode(node.clone())),
        };
    };
    let albedo = part
        .albedo()
        .ok_or_else(|| EditorError::NoAlbedo(node.clone()))?;
    let texture = model
        .texture(albedo)
        .ok_or_else(|| EditorError::NoTexture(albedo.clone()))?;
    let alpha = AlphaMask::decode_as(&texture.data, texture.encoding)
        .ok_or_else(|| EditorError::Image(format!("texture {albedo} does not decode")))?;

    let mesh = part.mesh();
    let uv_map = UvMap::fit(&mesh.verts, &mesh.uvs)
        .unwrap_or_else(|| UvMap::from_texture_size(alpha.width() as f32, alpha.height() as f32));
    let origin = mesh.origin;

    let working = match mode {
        AutoMesh::Contour {
            threshold,
            simplify,
            margin,
            spacing,
            rings,
            min_distance,
            mirror_x,
        } => {
            let d = ContourKnobs::default();
            let knobs = ContourKnobs {
                threshold: threshold.unwrap_or(d.threshold),
                simplify: simplify.unwrap_or(d.simplify),
                margin: margin.unwrap_or(d.margin),
                spacing: spacing.unwrap_or(d.spacing),
                rings: rings.unwrap_or(d.rings),
                min_distance: min_distance.unwrap_or(d.min_distance),
                // `or`, not `unwrap_or`: the editor's own answer here is
                // "no mirror line", so an absent knob stays absent.
                mirror_x: mirror_x.or(d.mirror_x),
            };
            contour_automesh(&alpha, &knobs, &uv_map, origin)?
        }
        AutoMesh::Grid {
            threshold,
            cols,
            rows,
            axes_x,
            axes_y,
            margin,
        } => {
            let d = GridKnobs::default();
            let knobs = GridKnobs {
                threshold: threshold.unwrap_or(d.threshold),
                cols: cols.unwrap_or(d.cols),
                rows: rows.unwrap_or(d.rows),
                axes_x: axes_x.unwrap_or(d.axes_x),
                axes_y: axes_y.unwrap_or(d.axes_y),
                // Absent stays absent: the default margin is one texel,
                // which no fraction of the box names.
                margin: margin.or(d.margin),
            };
            grid_automesh(&alpha, &knobs, &uv_map, origin)?
        }
    };
    // A trace that found no contour at all leaves nothing to triangulate, and
    // an empty mesh would silently blank the part.
    if working.vertex_count() < 3 {
        return Err(MeshError::NothingToMesh.into());
    }
    Ok(working.to_mesh(&uv_map, Some(&alpha))?)
}

pub(crate) fn emptied_reply(node: NodeId, emptied: Vec<SlotId>) -> ResponseBody {
    ResponseBody::Emptied {
        node,
        slots: emptied,
    }
}

pub(crate) fn build_weld(a: NodeId, b: NodeId, pairs: Vec<SlotPair>) -> ModelWeld {
    ModelWeld::new(
        a,
        b,
        pairs
            .into_iter()
            .map(|p| catchlight_core::SlotPair {
                a: p.a,
                b: p.b,
                weight: p.weight,
            })
            .collect(),
    )
}

pub(crate) fn pairs_the_same_parts(one: &ModelWeld, other: &ModelWeld) -> bool {
    (one.a() == other.a() && one.b() == other.b()) || (one.a() == other.b() && one.b() == other.a())
}

pub(crate) fn joins(weld: &ModelWeld, a: &NodeId, b: &NodeId) -> bool {
    (weld.a() == a && weld.b() == b) || (weld.a() == b && weld.b() == a)
}

pub(crate) fn default_name(kind: NodeKindArg) -> String {
    match kind {
        NodeKindArg::MotionResponse => "MotionResponse",
        NodeKindArg::Group => "Group",
        NodeKindArg::Part => "Part",
        NodeKindArg::Composite => "Composite",
        NodeKindArg::MeshGroup => "MeshGroup",
    }
    .to_string()
}

pub(crate) fn make_kind(kind: NodeKindArg) -> ModelNodeKind {
    match kind {
        NodeKindArg::MotionResponse => ModelNodeKind::MotionResponse(Default::default()),
        NodeKindArg::Group => ModelNodeKind::Group,
        NodeKindArg::Part => ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
        NodeKindArg::Composite => ModelNodeKind::Composite(ModelComposite::new()),
        NodeKindArg::MeshGroup => ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClmMesh::default())),
    }
}

pub(crate) fn apply_patch(
    model: &mut Model,
    id: &NodeId,
    patch: &NodePatch,
) -> Result<(), EditorError> {
    model.update_node(id, |n| {
        if let Some(name) = &patch.name {
            n.name = Name::truncated(name);
        }
        if let Some(t) = patch.translate {
            n.transform.translation = t;
        }
        if let Some(r) = patch.rotate {
            n.transform.rotation = r;
        }
        if let Some(sc) = patch.scale {
            n.transform.scale = sc;
        }
        if let Some(z) = patch.z_order {
            n.z_order = z;
        }
        if let Some(en) = patch.enabled {
            n.enabled = en;
        }
        if let Some(v) = patch.lock_to_root {
            n.lock_to_root = v;
        }
    })?;
    if patch.opacity.is_none()
        && patch.blend_mode.is_none()
        && patch.tint.is_none()
        && patch.screen_tint.is_none()
        && patch.mask_threshold.is_none()
        && patch.propagate_meshgroup.is_none()
        && patch.mg_translate_children.is_none()
    {
        return Ok(());
    }
    model.update_node_kind(id, |kind| {
        if let Some(op) = patch.opacity {
            set_opacity(kind, op);
        }
        if let Some(mode) = patch.blend_mode.map(CoreBlendMode::from) {
            match kind {
                ModelNodeKind::Part(p) => p.blend_mode = mode,
                ModelNodeKind::Composite(c) => c.blend_mode = mode,
                _ => {}
            }
        }
        if let Some(t) = patch.tint {
            match kind {
                ModelNodeKind::Part(p) => p.tint = t,
                ModelNodeKind::Composite(c) => c.tint = t,
                _ => {}
            }
        }
        if let Some(t) = patch.screen_tint {
            match kind {
                ModelNodeKind::Part(p) => p.screen_tint = t,
                ModelNodeKind::Composite(c) => c.screen_tint = t,
                _ => {}
            }
        }
        if let Some(th) = patch.mask_threshold {
            match kind {
                ModelNodeKind::Part(p) => p.mask_threshold = th,
                ModelNodeKind::Composite(c) => c.mask_threshold = th,
                _ => {}
            }
        }
        if let Some(v) = patch.propagate_meshgroup
            && let ModelNodeKind::Composite(c) = kind
        {
            c.propagate_meshgroup = v;
        }
        if let Some(v) = patch.mg_translate_children
            && let ModelNodeKind::MeshGroup(mg) = kind
        {
            mg.translate_children = v;
        }
    })?;
    Ok(())
}

fn set_opacity(kind: &mut ModelNodeKind, op: f32) {
    match kind {
        ModelNodeKind::Part(p) => p.opacity = op,
        ModelNodeKind::Composite(c) => c.opacity = op,
        _ => {}
    }
}

pub(crate) fn clm_file_from_json(
    structure: &[u8],
    textures: &[ImportTexture],
    images: Vec<(String, Vec<u8>)>,
) -> Result<clm::ClmFile, EditorError> {
    let json = std::str::from_utf8(structure).map_err(|e| {
        EditorError::BadRequest(format!("the structure attachment is not UTF-8: {e}"))
    })?;
    let doc: clm::ClmStructure = serde_json::from_str(json).map_err(|e| {
        EditorError::BadRequest(format!("the attachment is not a .clm structure: {e}"))
    })?;

    let mut attached: HashMap<String, Vec<u8>> = images.into_iter().collect();
    let mut table = Vec::with_capacity(textures.len());
    for want in textures {
        let data = attached.remove(want.texture.as_str()).ok_or_else(|| {
            EditorError::BadRequest(format!(
                "texture {:?} was declared but no attachment texture:{} arrived",
                want.texture.as_str(),
                want.texture
            ))
        })?;
        table.push(clm::ClmTexture {
            id: want.texture.clone(),
            encoding: want.encoding.into(),
            alpha: want.alpha.into(),
            data,
        });
    }
    // Leftover attachments named undeclared textures; dropping them silently
    // would import a model missing an image.
    if let Some(stray) = attached.keys().min() {
        return Err(EditorError::BadRequest(format!(
            "attachment texture:{stray} names no texture this import declared"
        )));
    }

    Ok(clm::ClmFile {
        doc,
        textures: table,
        // A byte extension's payload lives in a container section, and JSON
        // has none; a marker with no bytes is refused by key when
        // the file is read. Set such an extension after the import.
        extensions: Vec::new(),
        unknown_sections: Vec::new(),
    })
}

/// Re-parents every root (a complete model's root or a fragment's unparented
/// nodes) under `parent`; the wire's `parent` wins over what a fragment names.
pub(crate) fn fragment_under(
    mut file: clm::ClmFile,
    parent: &NodeId,
) -> Result<Model, EditorError> {
    let carried: HashSet<&NodeId> = file.doc.nodes.iter().map(|n| &n.id).collect();
    let roots: Vec<usize> = file
        .doc
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent.as_ref().is_none_or(|p| !carried.contains(p)))
        .map(|(at, _)| at)
        .collect();
    for at in roots {
        file.doc.nodes[at].parent = Some(parent.clone());
    }
    Ok(Model::from_clm_file_with(&file, ReadOptions::fragment())?)
}

pub(crate) fn encoding_from_path(path: &str) -> CoreTextureEncoding {
    if path.to_ascii_lowercase().ends_with(".tga") {
        CoreTextureEncoding::Tga
    } else {
        CoreTextureEncoding::Png
    }
}

pub(crate) fn image_dims(
    bytes: &[u8],
    encoding: CoreTextureEncoding,
) -> Result<(u32, u32), EditorError> {
    catchlight_editor_core::image_dims_as(bytes, encoding)
        .map_err(|e| EditorError::Image(e.to_string()))
}
