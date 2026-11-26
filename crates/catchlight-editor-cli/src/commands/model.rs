//! The top-level commands on a session's model as a whole.

use anyhow::{anyhow, bail};
use catchlight_editor_protocol::{Camera, Command, ParamPose};
use serde_json::Map;

use crate::args::{absolute, read_json};
use crate::cli::resolve_session;
use crate::route::{Call, Route};

/// `request`: any typed command, sent as the JSON gives it.
pub(crate) fn request<'a>(
    file: &'a str,
    attachments: &'a [String],
    out: Option<&'a str>,
) -> Route<'a> {
    Route::plain(move |_| {
        let command: Command = serde_json::from_str(&read_json(file)?)?;
        let mut files = Map::new();
        for attachment in attachments {
            let (name, path) = attachment
                .split_once('=')
                .ok_or_else(|| anyhow!("attachment must be NAME=PATH"))?;
            if name.is_empty() || files.insert(name.into(), absolute(path)?.into()).is_some() {
                bail!("attachment names must be nonempty and unique");
            }
        }
        let mut call = Call::from(command);
        if !files.is_empty() {
            call = call.sibling("files", files);
        }
        Ok(match out {
            Some(path) => call.sibling("out", absolute(path)?),
            None => call,
        })
    })
}

pub(crate) fn edit(file: &str, validate: bool) -> Route<'_> {
    Route::composed(move |remote| {
        let edits = serde_json::from_str(&read_json(file)?)?;
        let session = remote.session;
        let if_rev = remote.guard()?;
        Ok(if validate {
            Command::EditValidate {
                session,
                if_rev,
                edits,
            }
        } else {
            Command::EditApply {
                session,
                if_rev,
                edits,
            }
        })
    })
}

pub(crate) fn history() -> Route<'static> {
    Route::plain(|cli| {
        Ok(Command::EditHistoryGet {
            session: resolve_session(cli)?,
            if_rev: cli.if_rev,
        })
    })
}

pub(crate) fn goto(revision: u64) -> Route<'static> {
    Route::composed(move |remote| {
        Ok(Command::EditGoto {
            session: remote.session,
            if_rev: remote.guard()?,
            revision,
        })
    })
}

pub(crate) fn export(out: &str) -> Route<'_> {
    Route::composed(move |remote| {
        let command = Command::ModelExport {
            session: remote.session,
            if_rev: remote.guard()?,
        };
        Ok(Call::from(command).sibling("out", absolute(out)?))
    })
}

pub(crate) fn undo() -> Route<'static> {
    Route::composed(|remote| {
        Ok(Command::Undo {
            session: remote.session,
            if_rev: remote.guard()?,
        })
    })
}

pub(crate) fn redo() -> Route<'static> {
    Route::composed(|remote| {
        Ok(Command::Redo {
            session: remote.session,
            if_rev: remote.guard()?,
        })
    })
}

pub(crate) fn preview<'a>(
    out: &'a str,
    params: &'a [ParamPose],
    size: Option<[u32; 2]>,
    camera_height: Option<f32>,
    camera_center: Option<[f32; 2]>,
) -> Route<'a> {
    Route::plain(move |cli| {
        let command = Command::Preview {
            session: resolve_session(cli)?,
            pose: params.to_vec(),
            size,
            camera: camera_height.map(|height| Camera {
                center: camera_center.unwrap_or([0.0, 0.0]),
                height,
            }),
        };
        Ok(Call::from(command).sibling("out", absolute(out)?))
    })
}
