//! `rename`: change a node's, param's or texture's Id.

use catchlight_editor_protocol::{Command, NodeId, ParamId, Rename, TexId};
use clap::Subcommand;

use crate::route::Route;

#[derive(Subcommand)]
pub enum RenameCmd {
    /// Rename a node's Id, rewriting every reference in the model.
    Node { from: NodeId, to: NodeId },
    /// Rename a param's Id.
    Param { from: ParamId, to: ParamId },
    /// Rename a texture's Id.
    Texture { from: TexId, to: TexId },
}

pub fn route(action: &RenameCmd) -> Route<'_> {
    let rename = match action {
        RenameCmd::Node { from, to } => Rename::Node {
            from: from.clone(),
            to: to.clone(),
        },
        RenameCmd::Param { from, to } => Rename::Param {
            from: from.clone(),
            to: to.clone(),
        },
        RenameCmd::Texture { from, to } => Rename::Texture {
            from: from.clone(),
            to: to.clone(),
        },
    };
    Route::on_session(|session| Command::RenameId { session, rename })
}
