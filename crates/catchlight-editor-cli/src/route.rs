//! How each command is carried out, as its group decides.

use anyhow::Result;
use catchlight_editor_protocol::{Command, SessionId};
use serde_json::{Map, Value};

use crate::cli::{Cli, resolve_session};
use crate::compose::CapturedClient;
use crate::transport::{Endpoint, Transport};

pub(crate) type Local<'a> = Box<dyn FnOnce(&Cli) -> Result<i32> + 'a>;
pub(crate) type Typed<'a> = Box<dyn FnOnce(&Cli, &mut Transport, &Reached<'_>) -> Result<i32> + 'a>;
pub(crate) type Compose<'a> = Box<dyn FnOnce(&mut CapturedClient<'_>) -> Result<Call> + 'a>;
pub(crate) type Build<'a> = Box<dyn FnOnce(&Cli) -> Result<Call> + 'a>;

/// Each command's one way through: a command variant with no route does not
/// compile.
pub(crate) enum Route<'a> {
    /// Without reaching an editor; the exit code.
    Local(Local<'a>),
    /// Remembers a session of the editor reached as the default.
    Remember(SessionId),
    /// By its group's own code, which prints what it receives; the exit code.
    Typed(Typed<'a>),
    /// Reads that capture a revision, then one command guarded by it.
    Composed(Compose<'a>),
    /// One command whose reply prints as it is.
    Plain(Build<'a>),
}

impl<'a> Route<'a> {
    pub(crate) fn local(run: impl FnOnce(&Cli) -> Result<i32> + 'a) -> Self {
        Self::Local(Box::new(run))
    }

    pub(crate) fn typed(
        run: impl FnOnce(&Cli, &mut Transport, &Reached<'_>) -> Result<i32> + 'a,
    ) -> Self {
        Self::Typed(Box::new(run))
    }

    pub(crate) fn composed<C: Into<Call>>(
        compose: impl FnOnce(&mut CapturedClient<'_>) -> Result<C> + 'a,
    ) -> Self {
        Self::Composed(Box::new(|remote| compose(remote).map(Into::into)))
    }

    pub(crate) fn plain<C: Into<Call>>(build: impl FnOnce(&Cli) -> Result<C> + 'a) -> Self {
        Self::Plain(Box::new(|cli| build(cli).map(Into::into)))
    }

    /// One command on the target session.
    pub(crate) fn on_session(build: impl FnOnce(SessionId) -> Command + 'a) -> Self {
        Self::plain(|cli| Ok(build(resolve_session(cli)?)))
    }
}

/// How this command reached the editor, for `listen`.
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only the unix review listener uses it")
)]
pub(crate) struct Reached<'a> {
    pub program: &'a str,
    pub endpoint: &'a Endpoint,
}

/// A command and the fields sent beside it.
pub(crate) struct Call {
    pub command: Command,
    /// `files` and `out` paths, absolute: the server shares this filesystem
    /// but not this working directory.
    pub siblings: Map<String, Value>,
    /// The session the reply names becomes the default.
    pub remember: bool,
}

impl From<Command> for Call {
    fn from(command: Command) -> Self {
        Self {
            command,
            siblings: Map::new(),
            remember: false,
        }
    }
}

impl Call {
    pub(crate) fn sibling(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.siblings.insert(key.into(), value.into());
        self
    }

    pub(crate) const fn remembered(mut self) -> Self {
        self.remember = true;
        self
    }
}
