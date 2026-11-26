//! Friendly summaries of replies, and the raw JSON under `--json`.

use catchlight_editor_protocol::*;

use crate::cli::Cli;
use crate::{official, review};

pub fn print_reply(cli: &Cli, reply: &Reply) {
    if cli.json {
        if let Ok(s) = serde_json::to_string(reply) {
            println!("{s}");
        }
        return;
    }
    match reply {
        Reply::Err { code, message, .. } => eprintln!("error [{}]: {message}", code_name(*code)),
        Reply::Event(_) => {}
        Reply::Ok { body, .. } => print_body(body),
    }
}

pub fn code_name(code: ErrorCode) -> String {
    serde_json::to_string(&code)
        .map_or_else(|_| "error".into(), |s| s.trim_matches('"').to_string())
}

pub fn print_body(body: &ResponseBody) {
    match body {
        ResponseBody::MeshInfo { .. }
        | ResponseBody::ModelStructure { .. }
        | ResponseBody::GeometrySample { .. }
        | ResponseBody::EditHistory { .. }
        | ResponseBody::EditResults { .. }
        | ResponseBody::BindingCells { .. }
        | ResponseBody::SessionFork { .. }
        | ResponseBody::ModelExport { .. }
        | ResponseBody::Version { .. }
        | ResponseBody::Versions { .. }
        | ResponseBody::HumanoidMapped { .. }
        | ResponseBody::HumanoidUnmapped { .. }
        | ResponseBody::Humanoid { .. }
        | ResponseBody::Expression { .. }
        | ResponseBody::ExpressionDeleted { .. }
        | ResponseBody::Expressions { .. }
        | ResponseBody::VisemesAdded { .. }
        | ResponseBody::Clips { .. } => {
            println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
        }
        ResponseBody::ExtensionWarnings { warnings } => official::print_warnings(warnings),
        ResponseBody::Empty => println!("ok"),
        ResponseBody::Session { session } => println!("session {}", session.0),
        ResponseBody::Sessions { sessions, .. } => {
            if sessions.is_empty() {
                println!("(no open sessions)");
            }
            for s in sessions {
                let viewing = s.viewing.map_or(String::new(), |v| {
                    format!(
                        "  read-only: version {} of session {}",
                        v.version, v.session.0
                    )
                });
                println!(
                    "session {}  {}{}  nodes={} rev={}{viewing}",
                    s.session.0,
                    s.title,
                    if s.dirty { " *" } else { "" },
                    s.node_count,
                    s.rev
                );
            }
        }
        ResponseBody::Node { node, dropped } => {
            println!("node {node}");
            print_dropped(dropped);
        }
        ResponseBody::Param { param } => println!("param {param}"),
        ResponseBody::Slot { slot } => println!("slot {} on {}", slot.slot, slot.node),
        ResponseBody::Extensions { extensions } => {
            if extensions.is_empty() {
                println!("(no extensions)");
            }
            for e in extensions {
                println!("extension {}  {}", e.key, describe_extension(&e.value));
            }
        }
        ResponseBody::Extension { key, value } => {
            println!("extension {key}  {}", describe_extension(value));
        }
        ResponseBody::Params { params } => {
            if params.is_empty() {
                println!("(no params)");
            }
            for p in params {
                let driven = match &p.driven_by {
                    Some(node) => format!(" driven_by={node}"),
                    None => String::new(),
                };
                println!(
                    "param {}  {}  [{}, {}] default={} bindings={}{driven}",
                    p.id, p.name, p.min, p.max, p.default, p.bindings
                );
            }
        }
        ResponseBody::Bindings { bindings } => {
            if bindings.is_empty() {
                println!("(no bindings)");
            }
            for b in bindings {
                let params = match &b.param_y {
                    Some(y) => format!("{} x {}", b.param, y),
                    None => b.param.to_string(),
                };
                println!(
                    "binding {}  {}  {}  {}x{}",
                    b.target, params, b.interpolate, b.width, b.height
                );
                println!("  axes={:?}", b.key_positions);
                for (y, row) in b.keys.iter().enumerate() {
                    let cells: Vec<String> = row
                        .iter()
                        .enumerate()
                        .map(|(x, value)| match value {
                            Some(v) => format!("{v}"),
                            None if authored(b, x, y) => "set".into(),
                            None => "-".into(),
                        })
                        .collect();
                    println!("  y={y}: [{}]", cells.join(" "));
                }
            }
        }
        ResponseBody::Texture { texture, dropped } => {
            println!("texture {texture}");
            print_dropped(dropped);
        }
        ResponseBody::Textures { textures } => {
            for t in textures {
                println!("texture {}  {}x{}", t.id, t.width, t.height);
            }
        }
        ResponseBody::Tree { root } => print_tree(root, 0),
        ResponseBody::NodeInfo { node } => print_node_info(node),
        ResponseBody::Status { status } => println!(
            "{}{}  nodes={} params={} textures={} rev={}",
            status.title,
            if status.dirty { " *" } else { "" },
            status.node_count,
            status.param_count,
            status.texture_count,
            status.rev
        ),
        ResponseBody::Warnings { warnings } => {
            if warnings.is_empty() {
                println!("no problems");
            }
            for w in warnings {
                println!("warning: {w}");
            }
        }
        ResponseBody::Preview { preview } => {
            println!("preview {}x{}", preview.width, preview.height);
        }

        ResponseBody::Saved { path } => println!("saved -> {path}"),
        ResponseBody::Presence { presence } => match presence {
            None => println!("(no presence)"),
            Some(p) => {
                let pose: Vec<String> = p
                    .pose
                    .iter()
                    .map(|v| format!("{}={}", v.param, v.value))
                    .collect();
                println!(
                    "pose=[{}] selection={}",
                    pose.join(","),
                    p.selection
                        .as_ref()
                        .map_or_else(|| "-".into(), ToString::to_string)
                );
            }
        },
        ResponseBody::Slots { slots } => {
            if slots.is_empty() {
                println!("(no slots)");
            }
            for s in slots {
                match s.vertex {
                    Some(v) => println!("slot {}  v{v}", s.id),
                    None => println!("slot {}  -", s.id),
                }
            }
        }
        ResponseBody::Welds { welds } => {
            if welds.is_empty() {
                println!("(no welds)");
            }
            for w in welds {
                let pairs: Vec<String> = w
                    .pairs
                    .iter()
                    .map(|p| format!("{}:{}={}", p.a, p.b, p.weight))
                    .collect();
                println!("weld {} <-> {}  [{}]", w.a, w.b, pairs.join(" "));
            }
        }
        ResponseBody::Emptied { node, slots } => {
            if slots.is_empty() {
                println!("ok (no slots to refill)");
            } else {
                println!("ok; refill these slots on {node}:");
                for s in slots {
                    println!("  {s}");
                }
            }
        }
        ResponseBody::Review { .. }
        | ResponseBody::ReviewThread { .. }
        | ResponseBody::ReviewPending { .. }
        | ResponseBody::ReviewImage { .. }
        | ResponseBody::ReviewFetched { .. }
        | ResponseBody::ReviewAcked { .. }
        | ResponseBody::ReviewClaimed { .. } => {
            print!("{}", review::describe(body, true, None).unwrap_or_default());
        }
        ResponseBody::SpineFit { node, params } => {
            println!("spine {node}");
            for (link, param) in params.iter().enumerate() {
                println!("  link {} <- {param}", link + 1);
            }
        }
    }
}

/// The only authored signal for deform cells, which hold no scalar.
fn authored(binding: &BindingInfo, x: usize, y: usize) -> bool {
    binding
        .authored
        .get(y)
        .and_then(|row| row.get(x))
        .copied()
        .unwrap_or(false)
}

fn print_dropped(dropped: &[TexId]) {
    for t in dropped {
        println!("dropped texture {t}");
    }
}

/// Field names match `node set` flags so values can be written straight back.
fn print_node_info(node: &NodeInfo) {
    println!(
        "{} [{}] {} parent={}",
        node.id,
        node.kind,
        node.name,
        node.parent
            .as_ref()
            .map_or_else(|| "-".into(), ToString::to_string)
    );
    let [tx, ty, tz] = node.translate;
    let [rx, ry, rz] = node.rotate;
    let [sx, sy] = node.scale;
    println!("  translate {tx},{ty},{tz}");
    println!("  rotate {rx},{ry},{rz}");
    println!("  scale {sx},{sy}");
    println!("  z-order {}", node.z_order);
    println!("  enabled {}", node.enabled);
    println!("  lock-to-root {}", node.lock_to_root);
    if let Some(opacity) = node.opacity {
        println!("  opacity {opacity}");
    }
    if let Some(mode) = &node.blend_mode {
        println!("  blend-mode {mode}");
    }
    if let Some([r, g, b]) = node.tint {
        println!("  tint {r},{g},{b}");
    }
    if let Some([r, g, b]) = node.screen_tint {
        println!("  screen-tint {r},{g},{b}");
    }
    if let Some(threshold) = node.mask_threshold {
        println!("  mask-threshold {threshold}");
    }
    if let Some(texture) = &node.texture {
        println!("  texture {texture}");
    }
    if let (Some(verts), Some(tris)) = (node.vertex_count, node.triangle_count) {
        println!("  mesh {verts} verts, {tris} tris");
    }
    if let Some(v) = node.propagate_meshgroup {
        println!("  propagate-meshgroup {v}");
    }
    if let Some(v) = node.mg_translate_children {
        println!("  mg-translate-children {v}");
    }
}

fn print_tree(node: &TreeNode, depth: usize) {
    println!(
        "{}{} [{}] {} (z={})",
        "  ".repeat(depth),
        node.id,
        node.kind,
        node.name,
        node.z_order
    );
    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

fn describe_extension(value: &ExtensionValueInfo) -> String {
    match value {
        ExtensionValueInfo::Json { value } => {
            serde_json::to_string(value).unwrap_or_else(|_| "<unprintable json>".into())
        }
        ExtensionValueInfo::Bytes { size, hash } => format!("{size} bytes  {hash}"),
        ExtensionValueInfo::Maintained {
            version,
            refs,
            value,
            size,
            hash,
        } => {
            let payload = match (value, size, hash) {
                (Some(value), _, _) => {
                    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable json>".into())
                }
                (None, Some(size), Some(hash)) => format!("{size} bytes  {hash}"),
                _ => String::new(),
            };
            let refs = serde_json::to_string(refs).unwrap_or_default();
            format!("version {version}  refs {refs}  {payload}")
        }
    }
}
