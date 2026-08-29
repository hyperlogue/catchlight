//! CPU picking against the deformed puppet: every Part's post-deform vertices
//! already live on the CPU, so a point test is a tree walk + point-in-triangle
//! — no renderer involvement.

use catchlight_core::{BlendMode, GlobalTransforms, MeshIndices, NodeKind, PartData, Puppet};
use glam::Vec2;

/// Core node ids of the Parts whose deformed mesh contains `world`,
/// front-most first (higher cumulative z order renders in front; ties go to the
/// later-drawn node).
pub(crate) fn pick_all(puppet: &Puppet, transforms: &GlobalTransforms, world: Vec2) -> Vec<u32> {
    let mut hits: Vec<(f32, usize, u32)> = Vec::new();
    let mut visit = 0usize;
    // (id, parent cumulative z); children pushed in reverse for draw order.
    let mut stack = vec![(puppet.root(), 0.0f32)];
    while let Some((id, parent_z)) = stack.pop() {
        let Some(node) = puppet.get(id) else { continue };
        if !node.enabled {
            continue;
        }
        let z = parent_z + node.z_order;
        visit += 1;
        match &node.kind {
            NodeKind::Composite(c) if opacity_culled(c.opacity, c.blend_mode) => continue,
            NodeKind::Part(p) => {
                let missing = p.albedo_texture.0 as usize >= puppet.textures().len();
                if !opacity_culled(p.opacity, p.blend_mode)
                    && !missing
                    && part_contains(p, &transforms.get(id), world)
                {
                    hits.push((z, visit, id.0));
                }
            }
            _ => {}
        }
        let children = puppet.tree().get_children(id);
        for c in children.into_iter().rev() {
            stack.push((c, z));
        }
    }
    // Descending z = front first; equal z: later visit draws on top, so it wins.
    // total_cmp, not partial_cmp: a NaN z_order (a degenerate ZOrder binding, a
    // NaN-poisoned physics driver) makes the partial_cmp form non-transitive,
    // which sort_by can detect and panic on.
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    hits.into_iter().map(|(_, _, id)| id).collect()
}

fn opacity_culled(opacity: f32, blend: BlendMode) -> bool {
    opacity == 0.0 && blend != BlendMode::Darken
}

/// GPU local position: `v - origin + deform`, then the node matrix.
pub(crate) fn part_world_vertex(p: &PartData, m: &glam::Mat4, i: usize) -> Vec2 {
    let v = p.mesh.vertices[i] - p.mesh.origin
        + p.deform_stack
            .combined()
            .get(i)
            .copied()
            .unwrap_or(Vec2::ZERO);
    m.transform_point3(glam::vec3(v.x, v.y, 0.0)).truncate()
}

fn part_contains(p: &PartData, m: &glam::Mat4, world: Vec2) -> bool {
    let n = p.mesh.vertices.len();
    let tri_hit = |a: usize, b: usize, c: usize| -> bool {
        if a >= n || b >= n || c >= n {
            return false;
        }
        point_in_triangle(
            world,
            part_world_vertex(p, m, a),
            part_world_vertex(p, m, b),
            part_world_vertex(p, m, c),
        )
    };
    match &p.mesh.indices {
        MeshIndices::U16(ix) => ix
            .as_chunks::<3>()
            .0
            .iter()
            .any(|t| tri_hit(t[0] as usize, t[1] as usize, t[2] as usize)),
        MeshIndices::U32(ix) => ix
            .as_chunks::<3>()
            .0
            .iter()
            .any(|t| tri_hit(t[0] as usize, t[1] as usize, t[2] as usize)),
    }
}

fn point_in_triangle(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    // A zero-area triangle makes all three cross products 0 for every p, so
    // without this guard a collapsed triangle "contains" the whole plane and
    // swallows every click.
    if cross(b - a, c - a).abs() <= f32::EPSILON {
        return false;
    }
    let d1 = cross(p - a, b - a);
    let d2 = cross(p - b, c - b);
    let d3 = cross(p - c, a - c);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// World-space AABB over the deformed Parts of the given subtrees.
pub(crate) fn world_bounds(
    puppet: &Puppet,
    transforms: &GlobalTransforms,
    roots: &[u32],
) -> Option<(Vec2, Vec2)> {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut any = false;
    for &root in roots {
        let mut stack = vec![catchlight_core::NodeIdx(root)];
        while let Some(id) = stack.pop() {
            if let Some(node) = puppet.get(id) {
                if let NodeKind::Part(p) = &node.kind {
                    let m = transforms.get(id);
                    for i in 0..p.mesh.vertices.len() {
                        let w = part_world_vertex(p, &m, i);
                        min = min.min(w);
                        max = max.max(w);
                        any = true;
                    }
                }
            }
            stack.extend(puppet.tree().get_children(id));
        }
    }
    any.then_some((min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::{
        CompositeData, DeformStack, Mesh, MeshIndices, Node, PartData, Puppet, PuppetTexture,
        TextureId,
    };

    fn white_tex() -> PuppetTexture {
        PuppetTexture {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255].into(),
        }
    }

    fn triangle_part(origin: Vec2) -> Node {
        let mesh = Mesh::new(
            vec![
                Vec2::new(-1.0, -1.0) + origin,
                Vec2::new(1.0, -1.0) + origin,
                Vec2::new(0.0, 1.0) + origin,
            ],
            vec![Vec2::ZERO; 3],
            MeshIndices::U16(vec![0, 1, 2]),
            origin,
        );
        Node {
            kind: NodeKind::Part(Box::new(PartData {
                deform_stack: DeformStack::new(3),
                mesh,
                albedo_texture: TextureId(0),
                opacity: 1.0,
                ..PartData::default()
            })),
            ..Default::default()
        }
    }

    #[test]
    fn opacity_zero_composite_does_not_hit_children() {
        let mut puppet = Puppet::new();
        puppet.set_textures(vec![white_tex()]);
        let c = CompositeData {
            opacity: 0.0,
            ..Default::default()
        };
        let comp = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Composite(Box::new(c)),
                ..Default::default()
            },
            None,
        );
        puppet.insert_child(comp, triangle_part(Vec2::ZERO), None);
        let mut tx = GlobalTransforms::new();
        puppet.compute_transforms(&mut tx);
        assert!(
            pick_all(&puppet, &tx, Vec2::ZERO).is_empty(),
            "culled composite children must not steal clicks"
        );
    }

    #[test]
    fn pick_uses_origin_shifted_vertices() {
        let mut puppet = Puppet::new();
        puppet.set_textures(vec![white_tex()]);
        let origin = Vec2::new(10.0, 0.0);
        puppet.insert_child(puppet.root(), triangle_part(origin), None);
        let mut tx = GlobalTransforms::new();
        puppet.compute_transforms(&mut tx);
        assert_eq!(
            pick_all(&puppet, &tx, Vec2::ZERO).len(),
            1,
            "GPU draws v - origin; a click at local 0 must hit"
        );
        assert!(
            pick_all(&puppet, &tx, origin).is_empty(),
            "unshifted verts would hit here and miss the pixels"
        );
    }

    #[test]
    fn pick_prefers_higher_z_and_later_equal_z_nodes() {
        let mut puppet = Puppet::new();
        puppet.set_textures(vec![white_tex()]);
        let root = puppet.root();
        let behind = puppet.insert_child(
            root,
            Node {
                z_order: -1.0,
                ..triangle_part(Vec2::ZERO)
            },
            None,
        );
        let first_front = puppet.insert_child(
            root,
            Node {
                z_order: 2.0,
                ..triangle_part(Vec2::ZERO)
            },
            None,
        );
        let last_front = puppet.insert_child(
            root,
            Node {
                z_order: 2.0,
                ..triangle_part(Vec2::ZERO)
            },
            None,
        );
        let mut tx = GlobalTransforms::new();
        puppet.compute_transforms(&mut tx);

        assert_eq!(
            pick_all(&puppet, &tx, Vec2::ZERO),
            vec![last_front.0, first_front.0, behind.0]
        );
    }
}
