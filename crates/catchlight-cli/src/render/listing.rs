use catchlight_wgpu::{DrawableInfo, RenderList};
use std::fmt::Write as _;

/// Every drawable in z order, roots first and then each composite's children.
pub(super) fn listing(render_list: &RenderList) -> String {
    let mut out = String::new();
    // Writing into a String cannot fail, so the results are discarded.
    let _ = writeln!(
        out,
        "Render list: {} root drawables, {} composites with children",
        render_list.root_drawables.len(),
        render_list.composite_children.len()
    );
    let _ = writeln!(out, "\n=== ROOT DRAWABLES (sorted by z-order) ===");
    for (idx, drawable) in render_list.root_drawables.iter().enumerate() {
        match drawable {
            DrawableInfo::Part { .. } => part_line(&mut out, idx, drawable),
            DrawableInfo::Composite {
                node_id,
                z_order,
                blend_mode,
                opacity,
                ..
            } => {
                let _ = writeln!(
                    out,
                    "[{idx}] Composite node_id={node_id} z_order={z_order:.2} \
                     blend={blend_mode:?} opacity={opacity:.2}"
                );
            }
        }
    }
    for (composite_node_id, children) in &render_list.composite_children {
        let _ = writeln!(out, "\n=== COMPOSITE {composite_node_id} CHILDREN ===");
        for (idx, child) in children.iter().enumerate() {
            part_line(&mut out, idx, child);
        }
    }
    // The trailing newline is the caller's to add.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn part_line(out: &mut String, idx: usize, drawable: &DrawableInfo) {
    let DrawableInfo::Part {
        mesh_id,
        texture_id,
        transform,
        z_order,
        blend_mode,
        mask_sources,
        ..
    } = drawable
    else {
        return;
    };
    let pos = transform.project_point3(glam::Vec3::ZERO);
    let masks = if mask_sources.is_empty() {
        String::new()
    } else {
        format!(" masks={}", mask_sources.len())
    };
    let _ = writeln!(
        out,
        "[{idx}] Part entity={mesh_id} z_order={z_order:.2} texture={texture_id} \
         blend={blend_mode:?}{masks} pos=({:.1}, {:.1})",
        pos.x, pos.y
    );
}
