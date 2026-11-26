//! `param`: scalar input parameters.

use catchlight_editor_protocol::{Command, ParamId};
use clap::Subcommand;

use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum ParamCmd {
    /// Create a scalar input parameter. Key positions belong to bindings.
    Add {
        name: String,
        #[arg(long, allow_hyphen_values = true)]
        min: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        max: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        default: Option<f32>,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<ParamId>,
    },
    /// List inputs, ranges, defaults and binding counts.
    List,
    /// Change param metadata; normalized key positions survive a range change.
    Set {
        param: ParamId,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        min: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        max: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        default: Option<f32>,
    },
    /// Delete a param (and its bindings).
    Delete { param: ParamId },
}

pub(crate) fn route(action: &ParamCmd) -> Route<'_> {
    Route::on_session(move |session| match action {
        ParamCmd::Add {
            name,
            min,
            max,
            default,
            id,
        } => Command::ParamAdd {
            session,
            name: name.clone(),
            min: min.unwrap_or(0.0),
            max: max.unwrap_or(1.0),
            default: default.unwrap_or(0.0),
            param: id.clone(),
        },
        ParamCmd::List => Command::ParamList { session },
        ParamCmd::Set {
            param,
            name,
            min,
            max,
            default,
        } => Command::ParamSet {
            session,
            param: param.clone(),
            name: name.clone(),
            min: *min,
            max: *max,
            default: *default,
        },
        ParamCmd::Delete { param } => Command::ParamDelete {
            session,
            param: param.clone(),
        },
    })
}
