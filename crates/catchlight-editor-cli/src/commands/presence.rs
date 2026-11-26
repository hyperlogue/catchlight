//! `presence`: shared pose and selection.

use catchlight_editor_protocol::{Command, NodeId, ParamPose, Presence};
use clap::Subcommand;

use crate::args::parse_param;
use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum PresenceCmd {
    /// Publish pose / selection. `--param <id>=<value>` (repeatable).
    Set {
        #[arg(long = "param", allow_hyphen_values = true, value_parser = parse_param)]
        params: Vec<ParamPose>,
        #[arg(long)]
        select: Option<NodeId>,
    },
    /// Read the current shared presence.
    Get,
}

pub(crate) fn route(action: &PresenceCmd) -> Route<'_> {
    match action {
        PresenceCmd::Set { params, select } => {
            Route::on_session(move |session| Command::PresenceSet {
                session,
                presence: Presence {
                    pose: params.clone(),
                    camera: None,
                    selection: select.clone(),
                },
            })
        }
        PresenceCmd::Get => Route::on_session(|session| Command::PresenceGet { session }),
    }
}
