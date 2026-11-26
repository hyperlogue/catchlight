//! `node`: the node tree.

use catchlight_editor_protocol::{
    BlendMode, Command, EditOp, NodeId, NodeKindArg, NodePatch, NodeReorderArgs, NodeReparentArgs,
    TexId,
};
use clap::Subcommand;

use crate::args::{parse_kind, parse_vec2, parse_vec3, wire_enum};
use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum NodeCmd {
    /// Print the node tree (Ids and names).
    Tree,
    /// Print one node in full, with the field names `node set` takes.
    Info { node: NodeId },
    /// Add a child node.
    Add {
        #[arg(long)]
        parent: NodeId,
        /// group | part | composite | meshgroup
        #[arg(long, value_parser = parse_kind)]
        kind: NodeKindArg,
        #[arg(long)]
        name: Option<String>,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<NodeId>,
    },
    /// Change fields on a node.
    Set {
        node: NodeId,
        /// The label a person reads; nothing is addressed by it.
        #[arg(long)]
        name: Option<String>,
        /// `x,y,z`
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec3)]
        translate: Option<[f32; 3]>,
        /// `x,y,z` (radians)
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec3)]
        rotate: Option<[f32; 3]>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec2)]
        scale: Option<[f32; 2]>,
        #[arg(long, allow_hyphen_values = true)]
        z_order: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        opacity: Option<f32>,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        texture: Option<TexId>,
        /// Draw no texture; the last part drawing a texture takes it with it.
        #[arg(long = "clear-texture", conflicts_with = "texture")]
        clear_texture: bool,
        #[arg(long = "lock-to-root")]
        lock_to_root: Option<bool>,
        /// normal | multiply | `color_dodge` | …
        #[arg(long = "blend-mode", value_parser = wire_enum::<BlendMode>)]
        blend_mode: Option<BlendMode>,
        /// `r,g,b`
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec3)]
        tint: Option<[f32; 3]>,
        /// `r,g,b`
        #[arg(long = "screen-tint", allow_hyphen_values = true, value_parser = parse_vec3)]
        screen_tint: Option<[f32; 3]>,
        #[arg(long = "mask-threshold")]
        mask_threshold: Option<f32>,
        #[arg(long = "propagate-meshgroup")]
        propagate_meshgroup: Option<bool>,
        #[arg(long = "mg-translate-children")]
        mg_translate_children: Option<bool>,
    },
    /// Move a node under a new parent (its Id does not change).
    Reparent {
        node: NodeId,
        #[arg(long)]
        to: NodeId,
    },
    /// Move a node within its parent's children (index clamps to the end).
    Reorder {
        node: NodeId,
        #[arg(long)]
        index: u32,
    },
    /// Reparent + position in one step: become `--parent`'s child at `--index`.
    Move {
        node: NodeId,
        #[arg(long)]
        parent: NodeId,
        #[arg(long)]
        index: u32,
    },
    /// Deep-copy a node's subtree as its next sibling, with fresh Ids.
    Duplicate { node: NodeId },
    /// Delete a node and its subtree.
    Delete { node: NodeId },
}

pub(crate) fn route(action: &NodeCmd) -> Route<'_> {
    match action {
        NodeCmd::Tree => Route::on_session(|session| Command::NodeTree { session }),
        NodeCmd::Info { node } => Route::on_session(move |session| Command::NodeInfo {
            session,
            node: node.clone(),
        }),
        NodeCmd::Add {
            parent,
            kind,
            name,
            id,
        } => Route::on_session(move |session| Command::NodeAdd {
            session,
            parent: parent.clone(),
            kind: *kind,
            name: name.clone(),
            node: id.clone(),
        }),
        NodeCmd::Set {
            node,
            name,
            translate,
            rotate,
            scale,
            z_order,
            opacity,
            enabled,
            texture,
            clear_texture,
            lock_to_root,
            blend_mode,
            tint,
            screen_tint,
            mask_threshold,
            propagate_meshgroup,
            mg_translate_children,
        } => Route::on_session(move |session| Command::NodeSet {
            session,
            node: node.clone(),
            patch: NodePatch {
                name: name.clone(),
                translate: *translate,
                rotate: *rotate,
                scale: *scale,
                z_order: *z_order,
                opacity: *opacity,
                enabled: *enabled,
                texture: match (clear_texture, texture) {
                    (true, _) => Some(None),
                    (false, Some(tex)) => Some(Some(tex.clone())),
                    (false, None) => None,
                },
                lock_to_root: *lock_to_root,
                blend_mode: *blend_mode,
                tint: *tint,
                screen_tint: *screen_tint,
                mask_threshold: *mask_threshold,
                propagate_meshgroup: *propagate_meshgroup,
                mg_translate_children: *mg_translate_children,
            },
        }),
        NodeCmd::Reparent { node, to } => Route::on_session(move |session| Command::NodeReparent {
            session,
            edit: NodeReparentArgs {
                node: node.clone(),
                to: to.clone(),
            },
        }),
        NodeCmd::Reorder { node, index } => {
            Route::on_session(move |session| Command::NodeReorder {
                session,
                edit: NodeReorderArgs {
                    node: node.clone(),
                    index: *index,
                },
            })
        }
        NodeCmd::Move {
            node,
            parent,
            index,
        } => Route::composed(move |remote| {
            remote.batch(vec![
                EditOp::NodeReparent {
                    edit: NodeReparentArgs {
                        node: node.clone(),
                        to: parent.clone(),
                    },
                },
                EditOp::NodeReorder {
                    edit: NodeReorderArgs {
                        node: node.clone(),
                        index: *index,
                    },
                },
            ])
        }),
        NodeCmd::Duplicate { node } => Route::on_session(move |session| Command::NodeDuplicate {
            session,
            node: node.clone(),
        }),
        NodeCmd::Delete { node } => Route::on_session(move |session| Command::NodeDelete {
            session,
            node: node.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use catchlight_editor_protocol::{Reply, Request, SessionId};
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;
    use crate::{execute, transport};

    fn id(value: &str) -> NodeId {
        NodeId::new(value).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn move_is_one_guarded_batch_after_one_captured_read() {
        use std::io::{BufRead, BufReader, Write};
        let cli = Cli::try_parse_from([
            "cli",
            "--session",
            "7",
            "node",
            "move",
            "panel",
            "--parent",
            "head",
            "--index",
            "2",
        ])
        .unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut client = transport::Transport::Socket(BufReader::new(client));
        let handler = std::thread::spawn(move || {
            let mut reader = BufReader::new(server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            assert!(matches!(
                request.command,
                Command::Status {
                    session: SessionId(7)
                }
            ));
            writeln!(reader.get_mut(), "{}", serde_json::json!({"reply":"ok", "id":request.id, "rev":12, "body":{"result":"empty"}})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            let Command::EditApply { if_rev, edits, .. } = request.command else {
                panic!()
            };
            assert_eq!(if_rev, 12);
            assert_eq!(edits.len(), 2);
            assert!(
                matches!(&edits[0], EditOp::NodeReparent { edit: NodeReparentArgs { node, to } } if node == &id("panel") && to == &id("head"))
            );
            assert!(matches!(
                &edits[1],
                EditOp::NodeReorder {
                    edit: NodeReorderArgs { index: 2, .. },
                    ..
                }
            ));
            writeln!(reader.get_mut(), "{}", serde_json::json!({"reply":"ok", "id":request.id, "rev":13, "body":{"result":"edit_results", "changed":true,"results":[]}})).unwrap();
        });
        assert!(matches!(
            execute(&cli, &mut client).unwrap(),
            Some(Reply::Ok { rev: Some(13), .. })
        ));
        handler.join().unwrap();
    }
}
