//! `catchlight-editor-cli` — a thin client for the editor server.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

mod args;
mod cli;
mod commands;
mod compose;
mod current;
mod extension;
#[cfg(unix)]
mod listen;
mod official;
mod print;
mod review;
mod route;
mod transport;
mod version;

// `review`, `version`, `extension` and `official` take these through
// `use super::*`.
use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::*;
use clap::{ArgMatches, Subcommand};

use args::{MAX_COMMAND_BYTES, absolute, parse_floats};
use cli::{Cli, Cmd, resolve_session};
use commands::{
    binding, deform, mask, mesh, model, node, param, presence, rename, session, slot, texture, weld,
};
use print::{code_name, print_body, print_reply};
use route::{Reached, Route};

pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect()
}

/// Which running editor a command reaches when no flag names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    SocketFirst,
    ConnectionFirst,
}

/// `args` starts with the program name; `name` replaces it in help and errors.
pub fn run<I, T>(name: &'static str, discovery: Discovery, args: I) -> Result<ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::{CommandFactory, FromArgMatches};
    let matches = Cli::command()
        .name(name)
        .bin_name(name)
        .get_matches_from(args);
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let code = match route(&cli.cmd, &matches) {
        Route::Local(local) => local(&cli)?,
        Route::Remember(session) => {
            let (mut transport, endpoint) = connect(&cli, discovery)?;
            current::write_current(&mut transport, &endpoint, session)?;
            println!("using session {}", session.0);
            0
        }
        Route::Typed(typed) => {
            let (mut transport, endpoint) = connect(&cli, discovery)?;
            cli.session = targeted(cli.session, &mut transport, &endpoint)?;
            let reached = Reached {
                program: name,
                endpoint: &endpoint,
            };
            typed(&cli, &mut transport, &reached)?
        }
        Route::Composed(compose) => {
            let (mut transport, endpoint) = connect(&cli, discovery)?;
            cli.session = targeted(cli.session, &mut transport, &endpoint)?;
            let sent = composed(&cli, compose, &mut transport)?;
            answer(&cli, sent, &mut transport, &endpoint)?
        }
        Route::Plain(build) => {
            let (mut transport, endpoint) = connect(&cli, discovery)?;
            cli.session = targeted(cli.session, &mut transport, &endpoint)?;
            let sent = plain(&cli, build, &mut transport)?;
            answer(&cli, sent, &mut transport, &endpoint)?
        }
    };
    Ok(exit_code(code))
}

/// Every command's route.
fn route<'a>(cmd: &'a Cmd, matches: &'a ArgMatches) -> Route<'a> {
    let set_matches = |group: &str| {
        matches
            .subcommand_matches(group)
            .and_then(|m| m.subcommand_matches("set"))
    };
    match cmd {
        Cmd::Request {
            file,
            attachments,
            out,
        } => model::request(file, attachments, out.as_deref()),
        Cmd::Edit { file, validate } => model::edit(file, *validate),
        Cmd::History => model::history(),
        Cmd::Goto { revision } => model::goto(*revision),
        Cmd::Export { out } => model::export(out),
        Cmd::Session { action } => session::route(action),
        Cmd::Save { path } => Route::on_session(move |session| Command::Save {
            session,
            path: path.clone(),
        }),
        Cmd::ExportManifest { path } => Route::on_session(move |session| Command::ExportManifest {
            session,
            path: path.clone(),
        }),
        Cmd::Status => Route::on_session(|session| Command::Status { session }),
        Cmd::Check => Route::on_session(|session| Command::Check { session }),
        Cmd::Node { action } => node::route(action),
        Cmd::Rename { action } => rename::route(action),
        Cmd::Texture { action } => texture::route(action),
        Cmd::Param { action } => param::route(action),
        Cmd::Binding { action } => binding::route(action),
        Cmd::Mask { action } => mask::route(action),
        Cmd::Mesh { action } => mesh::route(action),
        Cmd::Slot { action } => slot::route(action),
        Cmd::Weld { action } => weld::route(action),
        Cmd::Deform { action } => deform::route(action),
        Cmd::Undo => model::undo(),
        Cmd::Redo => model::redo(),
        Cmd::Presence { action } => presence::route(action),
        Cmd::Review(args) => review::route(args),
        Cmd::Version(args) => Route::typed(move |cli, stream, _| version::run(cli, args, stream)),
        Cmd::Extension { action } => {
            Route::typed(move |cli, stream, _| extension::run(cli, action, stream))
        }
        Cmd::Humanoid { action } => {
            Route::typed(move |cli, stream, _| official::humanoid(cli, action, stream))
        }
        Cmd::Expression { action } => {
            let set = set_matches("expression");
            Route::typed(move |cli, stream, _| {
                official::expression(cli, ExpressionGroup::Expression, action, set, stream)
            })
        }
        Cmd::Viseme { action } => {
            let set = set_matches("viseme");
            Route::typed(move |cli, stream, _| official::viseme(cli, action, set, stream))
        }
        Cmd::Clip { action } => {
            Route::typed(move |cli, stream, _| official::clip(cli, action, stream))
        }
        Cmd::Preview {
            out,
            params,
            size,
            camera_height,
            camera_center,
        } => model::preview(out, params, *size, *camera_height, *camera_center),
    }
}

/// A reply, and whether the session it names becomes the default.
struct Sent {
    reply: Reply,
    remember: bool,
}

fn composed(
    cli: &Cli,
    compose: route::Compose,
    transport: &mut transport::Transport,
) -> Result<Sent> {
    let mut remote = compose::CapturedClient::new(cli, transport)?;
    let call = compose(&mut remote)?;
    Ok(Sent {
        reply: remote.send(call.command, call.siblings)?,
        remember: call.remember,
    })
}

fn plain(cli: &Cli, build: route::Build, transport: &mut transport::Transport) -> Result<Sent> {
    let call = build(cli)?;
    let request = Request {
        id: 1,
        command: call.command,
    };
    Ok(Sent {
        reply: transport.call(request, call.siblings)?,
        remember: call.remember,
    })
}

/// Prints the reply; the exit code.
fn answer(
    cli: &Cli,
    sent: Sent,
    transport: &mut transport::Transport,
    endpoint: &transport::Endpoint,
) -> Result<i32> {
    if sent.remember
        && let Reply::Ok {
            body: ResponseBody::Session { session } | ResponseBody::SessionFork { session, .. },
            ..
        } = &sent.reply
    {
        current::write_current(transport, endpoint, *session)?;
    }
    print_reply(cli, &sent.reply);
    Ok(i32::from(matches!(sent.reply, Reply::Err { .. })))
}

fn connect(cli: &Cli, discovery: Discovery) -> Result<(transport::Transport, transport::Endpoint)> {
    transport::resolve(&transport::Target {
        server: cli.server.clone(),
        token: cli.token.clone(),
        socket: cli.socket.clone(),
        connection_first: discovery == Discovery::ConnectionFirst,
    })
}

/// `--session`, else the session remembered for the editor reached.
fn targeted(
    session: Option<u64>,
    transport: &mut transport::Transport,
    endpoint: &transport::Endpoint,
) -> Result<Option<u64>> {
    match session {
        Some(session) => Ok(Some(session)),
        None => Ok(current::remembered(transport, endpoint)?.map(|session| session.0)),
    }
}

/// A status the platform cannot represent fails rather than passing as 0.
fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

/// The command a plain route sends.
#[cfg(test)]
fn build_command(cli: &Cli) -> Result<Command> {
    match route(&cli.cmd, &ArgMatches::default()) {
        Route::Plain(build) => Ok(build(cli)?.command),
        _ => bail!("not a plain command"),
    }
}

/// The reply to what a composed or plain route sends; `None` for the others.
#[cfg(test)]
fn execute(cli: &Cli, transport: &mut transport::Transport) -> Result<Option<Reply>> {
    Ok(match route(&cli.cmd, &ArgMatches::default()) {
        Route::Composed(compose) => Some(composed(cli, compose, transport)?.reply),
        Route::Plain(build) => Some(plain(cli, build, transport)?.reply),
        Route::Local(_) | Route::Remember(_) | Route::Typed(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_refused_command_returns_failure_to_the_caller() {
        use std::io::{BufRead, Write};
        let dir = std::env::temp_dir().join(format!("clirun{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("e.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let editor = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            let reply = Reply::Err {
                id: 1,
                code: ErrorCode::NoNode,
                message: "no node".into(),
                op_index: None,
                limit: None,
            };
            writeln!(&stream, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
        });
        let socket = path.to_str().unwrap();
        let args = [
            "cli",
            "--socket",
            socket,
            "--session",
            "1",
            "node",
            "delete",
            "x",
        ];
        let code = run("cli", Discovery::SocketFirst, args).unwrap();
        editor.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(code, ExitCode::FAILURE);
    }
}
