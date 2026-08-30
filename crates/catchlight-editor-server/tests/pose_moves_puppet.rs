//! Regression: posing a param on the editor's flattened puppet must move it
//! exactly like the viewport does (set value by name, then tick).

use catchlight_core::GlobalTransforms;
use catchlight_editor_server::Editor;

#[test]
fn posing_a_param_changes_the_ticked_state() {
    // `welded_seam`'s one param (`pull`) drives a deform, so posing it has to
    // show up in the ticked state.
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clp"
    ))
    .expect("welded_seam.clp");
    let ed = Editor::new();
    let session = ed.open_bytes("welded_seam", &bytes).expect("open");

    let moved = ed
        .with_puppet(session, |puppet| {
            let names: Vec<String> = puppet.params().iter().map(|p| p.name.clone()).collect();
            assert!(!names.is_empty(), "the rig has params");

            let mut transforms = GlobalTransforms::new();
            puppet.tick(&mut transforms, glam::Mat4::IDENTITY, 0.0);
            let baseline = state_signature(puppet, &transforms);

            let mut any_moved = false;
            for name in &names {
                let (min, max) = {
                    let p = puppet.param_by_name(name).expect("param");
                    (p.min, p.max)
                };
                assert!(puppet.set_param_value_by_name(name, max), "set {name}");
                puppet.tick(&mut transforms, glam::Mat4::IDENTITY, 0.0);
                let posed = state_signature(puppet, &transforms);
                if posed != baseline {
                    any_moved = true;
                }
                puppet.set_param_value_by_name(name, min.lerp(max, 0.5));
            }
            any_moved
        })
        .expect("with_puppet");
    assert!(moved, "no param pose changed transforms or deforms");
}

fn state_signature(
    puppet: &catchlight_core::LegacyPuppet,
    transforms: &GlobalTransforms,
) -> Vec<[i64; 2]> {
    let mut sig = Vec::new();
    let order = puppet.tree().with_dfs_order(|o| o.to_vec());
    for id in order {
        let m = transforms.get(id);
        let t = m.transform_point3(glam::vec3(0.0, 0.0, 0.0));
        sig.push([(t.x * 1e3) as i64, (t.y * 1e3) as i64]);
        if let Some(node) = puppet.get(id) {
            if let catchlight_core::NodeKind::Part(p) = &node.kind {
                for v in p.deform_stack.combined() {
                    sig.push([(v.x * 1e3) as i64, (v.y * 1e3) as i64]);
                }
            }
        }
    }
    sig
}
