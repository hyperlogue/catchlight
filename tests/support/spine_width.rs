//! One synthetic width-binding model shared by runtime and file-transfer tests.

#![allow(dead_code, clippy::unwrap_used)]

use catchlight_core::formats::clm::{ClmIndices, ClmMesh, ClmPhysics};
use catchlight_core::{
    BindingKey, BindingTarget, LinkFeel, MaskMode, Mat2, Model, ModelChain, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelSpine, Name, NodeId, ParamId, ScalarTarget,
    SeededHex, Vec2,
};

pub const ROWS: usize = 9;
pub const LIMIT: f32 = 0.06;

pub struct Lock {
    pub spine: NodeId,
    pub parts: [NodeId; 2],
    pub bends: [ParamId; 2],
}

pub struct Fixture {
    pub model: Model,
    pub yaw: ParamId,
    pub body: ParamId,
    pub locks: Vec<Lock>,
}

fn param(model: &mut Model, hex: &mut SeededHex, name: &str, range: f32) -> ParamId {
    model
        .add_param(
            ModelParam::new(Name::truncated(name), -range, range, 0.0),
            hex,
        )
        .unwrap()
}

pub fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

pub fn envelope(t: f32) -> f32 {
    smooth(((t - 0.125) / 0.375).clamp(0.0, 1.0))
}

// A bounded bilinear field gives an analytic oracle between authored keys.
// A production author samples a supported cosine prior instead. The runtime
// knows only the baked offsets in either case.
pub fn width_delta(yaw: f32, bend: f32) -> f32 {
    0.008 * yaw + 0.010 * bend / LIMIT + 0.002 * yaw * bend / LIMIT
}

impl Fixture {
    pub fn new() -> Self {
        let mut model = Model::new();
        let mut hex = SeededHex::new(84);
        model.set_physics(ClmPhysics {
            pixels_per_meter: 1.0,
            gravity: 981.0,
            ..ClmPhysics::default()
        });
        let yaw = param(&mut model, &mut hex, "yaw", 1.0);
        let body = param(&mut model, &mut hex, "body", 1.0);
        let root = model.root().unwrap().clone();
        let body_node = model
            .add_node(
                &root,
                ModelNode::new("body", ModelNodeKind::Group),
                &mut hex,
            )
            .unwrap();
        let head = model
            .add_node(
                &body_node,
                ModelNode::new("head", ModelNodeKind::Group),
                &mut hex,
            )
            .unwrap();
        for (driver, node, target, amount) in [
            (&yaw, &head, ScalarTarget::Tx, 15.0),
            (&yaw, &head, ScalarTarget::Rz, 0.12),
            (&body, &body_node, ScalarTarget::Ty, 8.0),
            (&body, &body_node, ScalarTarget::Rz, -0.08),
        ] {
            let key = BindingKey::new(driver.clone(), node.clone(), BindingTarget::Scalar(target));
            model
                .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
                .unwrap();
            for (i, value) in [-amount, 0.0, amount].into_iter().enumerate() {
                model.set_binding_key(&key, [i as u32, 0], value).unwrap();
            }
        }
        let mut locks = Vec::new();
        for (length, response, root_x) in [(40.0, 5.0, -12.0), (140.0, 1.5, 12.0)] {
            let bends = [
                param(&mut model, &mut hex, "upper bend", LIMIT),
                param(&mut model, &mut hex, "lower bend", LIMIT),
            ];
            let mut chain = ModelChain::new(2);
            chain.weight = 0.5;
            chain.set_links(vec![
                LinkFeel {
                    stiffness: response,
                    damping: 0.8,
                    limit: Some(LIMIT),
                    ..LinkFeel::default()
                };
                2
            ]);
            let mut spine_data = ModelSpine::new(vec![[0.0, -length / 2.0], [0.0, -length]]);
            spine_data.set_chain(Some(chain));
            let spine = model
                .add_node(
                    &head,
                    ModelNode::new("lock", ModelNodeKind::Spine(spine_data)),
                    &mut hex,
                )
                .unwrap();
            model
                .update_node(&spine, |n| n.transform.translation[0] = root_x)
                .unwrap();
            model
                .set_spine_targets(&spine, bends.iter().cloned().map(Some).collect())
                .unwrap();
            let parts = [0.0f32, 0.35].map(|angle| {
                // A companion with a different local frame and mesh origin
                // samples the same lock-space field, rather than copying it.
                let to_local = Mat2::from_angle(-angle);
                let origin = Vec2::new(3.0, 7.0);
                let mut verts = Vec::new();
                let mut uvs = Vec::new();
                let mut indices = Vec::new();
                for row in 0..ROWS {
                    let t = row as f32 / (ROWS - 1) as f32;
                    for x in [-5.0, 5.0] {
                        verts.extend_from_slice(
                            &(to_local * Vec2::new(x, -t * length) + origin).to_array(),
                        );
                        uvs.extend_from_slice(&[(x + 5.0) / 10.0, t]);
                    }
                    if row + 1 < ROWS {
                        let v = (2 * row) as u16;
                        indices.extend_from_slice(&[v, v + 1, v + 3, v, v + 3, v + 2]);
                    }
                }
                let mesh = ClmMesh {
                    verts,
                    uvs,
                    indices: ClmIndices::U16(indices),
                    origin: origin.to_array(),
                };
                let part = model
                    .add_node(
                        &spine,
                        ModelNode::new("paint", ModelNodeKind::Part(ModelPart::new(mesh))),
                        &mut hex,
                    )
                    .unwrap();
                model
                    .update_node(&part, |n| n.transform.rotation[2] = angle)
                    .unwrap();
                for (link, bend) in bends.iter().enumerate() {
                    let key = BindingKey::pair(
                        yaw.clone(),
                        bend.clone(),
                        part.clone(),
                        BindingTarget::Deform,
                    );
                    model
                        .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]; 2])
                        .unwrap();
                    for (iy, b) in [-LIMIT, 0.0, LIMIT].into_iter().enumerate() {
                        for (ix, h) in [-1.0, 0.0, 1.0].into_iter().enumerate() {
                            let mut offsets = Vec::new();
                            for row in 0..ROWS {
                                let t = row as f32 / (ROWS - 1) as f32;
                                let share = if link == 0 {
                                    1.0 - smooth(t)
                                } else {
                                    smooth(t)
                                };
                                for x in [-5.0, 5.0] {
                                    let d = to_local
                                        * Vec2::new(
                                            x * envelope(t) * share * width_delta(h, b),
                                            0.0,
                                        );
                                    offsets.extend_from_slice(&d.to_array());
                                }
                            }
                            model
                                .set_deform_vertices(&key, [ix as u32, iy as u32], offsets)
                                .unwrap();
                        }
                    }
                }
                part
            });
            model
                .mask_add(&parts[1], &parts[0], MaskMode::Mask)
                .unwrap();
            locks.push(Lock {
                spine,
                parts,
                bends,
            });
        }
        Self {
            model,
            yaw,
            body,
            locks,
        }
    }
}
