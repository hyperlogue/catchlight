//! `session`: open, fork, list, pick and close sessions.

use catchlight_editor_protocol::{Command, SessionId, SessionSource};
use clap::Subcommand;
use serde_json::json;

use crate::args::absolute;
use crate::route::{Call, Route};

#[derive(Subcommand)]
pub(crate) enum SessionCmd {
    /// Start a new empty model.
    New {
        #[arg(long)]
        name: Option<String>,
        /// Initialize from a complete local CLM in one request.
        #[arg(long)]
        clm: Option<String>,
    },
    /// Fork the captured model into a clean session with independent history.
    Fork {
        #[arg(long)]
        name: Option<String>,
    },
    /// Open an existing `.clm`.
    Open { path: String },
    /// List open sessions.
    List,
    /// Remember a session as the default for later commands.
    Use { id: u64 },
    /// Close a session.
    Close,
}

pub(crate) fn route(action: &SessionCmd) -> Route<'_> {
    match action {
        SessionCmd::New { name, clm } => Route::plain(move |_| {
            let call = Call::from(Command::SessionNew {
                name: name.clone(),
                source: clm.as_ref().map(|_| SessionSource::Clm {}),
            });
            Ok(match clm {
                Some(path) => call.sibling("files", json!({"model": absolute(path)?})),
                None => call,
            }
            .remembered())
        }),
        SessionCmd::Fork { name } => Route::composed(move |remote| {
            Ok(Call::from(Command::SessionFork {
                session: remote.session,
                if_rev: remote.guard()?,
                name: name.clone(),
            })
            .remembered())
        }),
        SessionCmd::Open { path } => Route::plain(move |_| {
            Ok(Call::from(Command::SessionOpen { path: path.clone() }).remembered())
        }),
        SessionCmd::List => Route::plain(|_| Ok(Command::SessionList)),
        SessionCmd::Use { id } => Route::Remember(SessionId(*id)),
        SessionCmd::Close => Route::on_session(|session| Command::SessionClose { session }),
    }
}
