//! `texture`: the session's textures.

use catchlight_editor_protocol::{Command, NodeId, TexId, TextureEncoding};
use clap::Subcommand;
use serde_json::json;

use crate::args::absolute;
use crate::cli::resolve_session;
use crate::route::{Call, Route};

#[derive(Subcommand)]
pub(crate) enum TextureCmd {
    /// Add a PNG/TGA texture and make a part draw it, in one edit. A texture
    /// the part was the last to draw goes with it.
    Add {
        node: NodeId,
        path: String,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<TexId>,
    },
    /// List the session's textures.
    List,
}

pub(crate) fn route(action: &TextureCmd) -> Route<'_> {
    match action {
        TextureCmd::Add { node, path, id } => Route::plain(move |cli| {
            let command = Command::TextureAdd {
                session: resolve_session(cli)?,
                node: node.clone(),
                encoding: encoding_of(path),
                texture: id.clone(),
            };
            Ok(Call::from(command).sibling("files", json!({ "texture": absolute(path)? })))
        }),
        TextureCmd::List => Route::on_session(|session| Command::TextureList { session }),
    }
}

fn encoding_of(path: &str) -> TextureEncoding {
    if path.to_ascii_lowercase().ends_with(".tga") {
        TextureEncoding::Tga
    } else {
        TextureEncoding::Png
    }
}
