//! The remembered session: the one later commands target without `--session`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionId};

use crate::transport;

fn current_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .or_else(|| std::env::var_os("LOCALAPPDATA").filter(|_| cfg!(windows)))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("catchlight-editor").join("current")
}

/// The remembered session and the editor that numbered it. Every editor
/// numbers its sessions from 1, so the id alone could name another editor's.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Current {
    /// [`transport::Endpoint::describe`]: never the token.
    endpoint: String,
    instance: String,
    session: u64,
}

/// A file this cannot read as a [`Current`] selects nothing.
fn read_current() -> Result<Option<Current>> {
    let path = current_path();
    match std::fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn write_current(
    transport: &mut transport::Transport,
    endpoint: &transport::Endpoint,
    session: SessionId,
) -> Result<()> {
    let current = Current {
        endpoint: endpoint.describe(),
        instance: instance_of(transport)?,
        session: session.0,
    };
    let path = current_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec(&current)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// The remembered session, if the editor `transport` reached numbered it.
pub fn remembered(
    transport: &mut transport::Transport,
    endpoint: &transport::Endpoint,
) -> Result<Option<SessionId>> {
    let Some(current) = read_current()?.filter(|c| c.endpoint == endpoint.describe()) else {
        return Ok(None);
    };
    let instance = instance_of(transport)?;
    Ok((current.instance == instance).then_some(SessionId(current.session)))
}

fn instance_of(transport: &mut transport::Transport) -> Result<String> {
    let request = Request {
        id: 1,
        command: Command::SessionList,
    };
    match transport.call(request, serde_json::Map::new())? {
        Reply::Ok {
            body: ResponseBody::Sessions { instance, .. },
            ..
        } => Ok(instance),
        other => bail!("session_list answered {other:?}"),
    }
}
