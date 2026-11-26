//! `version`: numbered snapshots of a session's model, kept beside it.

use std::fmt::Write as _;

use anyhow::Context as _;
use sha2::{Digest, Sha256};

use super::*;

#[derive(clap::Args)]
pub struct VersionArgs {
    /// The name the person sees on versions you save; as for `review`.
    #[arg(long, global = true, env = "CATCHLIGHT_AGENT")]
    name: Option<String>,
    #[command(subcommand)]
    action: VersionCmd,
}

#[derive(Subcommand)]
enum VersionCmd {
    /// Keep the model as it is now as the next numbered version. Moves no
    /// revision and saves no file.
    Save {
        /// A short name, shown in the version list.
        #[arg(short, long)]
        label: Option<String>,
        /// One line on what changed.
        #[arg(short = 'm', long = "summary")]
        summary: Option<String>,
        /// Save as the person rather than as the agent.
        #[arg(long)]
        human: bool,
        /// Repeating the command with the same key saves nothing new and
        /// prints the version it saved; the same key with another label or
        /// summary is refused.
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
    },
    /// Print the session's versions, newest first.
    List,
    /// Replace the model with a version's, as one undoable edit.
    Restore { version: u64 },
    /// Write a version's `.clm`, byte for byte as it was saved.
    Export {
        version: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Open a version as a read-only session, to inspect or preview it with
    /// `--session <id>`; it closes with this session.
    Open { version: u64 },
}

/// Who saves: the agent `review` would write as, or the person.
pub fn author(name: Option<&str>, human: bool) -> ReviewAuthor {
    let who = review::agent(name);
    if human {
        ReviewAuthor::Human {
            id: None,
            name: None,
        }
    } else {
        ReviewAuthor::Agent {
            id: who.id,
            name: who.name,
        }
    }
}

pub fn run(cli: &Cli, args: &VersionArgs, stream: &mut transport::Transport) -> Result<i32> {
    let session = resolve_session(cli)?;
    let mut siblings = serde_json::Map::new();
    let command = match &args.action {
        VersionCmd::Save {
            label,
            summary,
            human,
            key,
        } => Command::VersionSave {
            session,
            label: label.clone(),
            summary: summary.clone(),
            author: author(args.name.as_deref(), *human),
            key: key.clone(),
        },
        VersionCmd::List => Command::VersionList { session },
        VersionCmd::Restore { version } => Command::VersionRestore {
            session,
            version: *version,
            if_rev: cli.if_rev,
        },
        VersionCmd::Export { version, out } => {
            let out = out
                .to_str()
                .ok_or_else(|| anyhow!("{}: not a UTF-8 path", out.display()))?;
            siblings.insert("out".into(), absolute(out)?.into());
            Command::VersionGet {
                session,
                version: *version,
            }
        }
        VersionCmd::Open { version } => Command::VersionOpen {
            session,
            version: *version,
        },
    };
    let reply = stream.call(Request { id: 1, command }, siblings)?;
    if let (
        VersionCmd::Export { out, .. },
        Reply::Ok {
            body: ResponseBody::Version { version },
            ..
        },
    ) = (&args.action, &reply)
    {
        verify(out, version)?;
    }
    if cli.json {
        print_reply(cli, &reply);
    } else {
        match &reply {
            Reply::Ok { body, rev, .. } => print!("{}", describe(&args.action, body, *rev)),
            other => print_reply(cli, other),
        }
    }
    Ok(i32::from(matches!(reply, Reply::Err { .. })))
}

/// The file holds what the version is: a truncated or foreign file must not
/// pass for it.
fn verify(path: &std::path::Path, version: &VersionInfo) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading back {}", path.display()))?;
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if bytes.len() as u64 != version.byte_length || hash != version.sha256 {
        bail!(
            "{}: not the {} bytes of version {}",
            path.display(),
            version.byte_length,
            version.version
        );
    }
    Ok(())
}

fn describe(action: &VersionCmd, body: &ResponseBody, rev: Option<u64>) -> String {
    let mut out = String::new();
    match (action, body) {
        (VersionCmd::Save { .. }, ResponseBody::Version { version }) => {
            let _ = writeln!(out, "saved {}", line(version));
        }
        (VersionCmd::Restore { .. }, ResponseBody::Version { version }) => {
            let _ = writeln!(
                out,
                "restored version {} as rev {}; `undo` takes it back",
                version.version,
                rev.unwrap_or(version.rev)
            );
        }
        (VersionCmd::Export { out: path, .. }, ResponseBody::Version { version }) => {
            let _ = writeln!(
                out,
                "wrote version {} to {} ({} bytes, sha256 {})",
                version.version,
                path.display(),
                version.byte_length,
                version.sha256
            );
        }
        (VersionCmd::Open { version }, ResponseBody::Session { session }) => {
            let _ = writeln!(
                out,
                "session {} shows version {version}, read-only; pass --session {} to read or preview it",
                session.0, session.0
            );
        }
        (
            _,
            ResponseBody::Versions {
                versions,
                kept,
                kept_problem,
            },
        ) => {
            match kept {
                Kept::Stored => {}
                Kept::Unsaved => {
                    let _ = writeln!(
                        out,
                        "(the model has no file yet: these versions are kept once it is saved)"
                    );
                }
                Kept::Session => {
                    let _ = writeln!(
                        out,
                        "(this editor keeps no versions: they last until the session closes)"
                    );
                }
                Kept::Unreadable => {
                    let _ = writeln!(
                        out,
                        "(the versions kept with this model could not be read ({}); they are left as they are, and versions saved now last until the session closes)",
                        kept_problem.as_deref().unwrap_or("no reason given")
                    );
                }
            }
            if versions.is_empty() {
                let _ = writeln!(out, "(no versions)");
            }
            for version in versions.iter().rev() {
                let _ = writeln!(out, "{}", line(version));
                if let Some(summary) = &version.summary {
                    let _ = writeln!(out, "  {summary}");
                }
            }
        }
        (_, body) => {
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(body).unwrap_or_default()
            );
        }
    }
    out
}

fn line(version: &VersionInfo) -> String {
    let mut out = format!("version {}", version.version);
    if let Some(label) = &version.label {
        let _ = write!(out, " {label:?}");
    }
    let who = match &version.author {
        ReviewAuthor::Human { name: None, .. } => "human".to_owned(),
        ReviewAuthor::Human {
            name: Some(name), ..
        } => format!("human {name}"),
        ReviewAuthor::Agent { name, .. } => format!("agent {name}"),
    };
    let _ = write!(out, "  by {who}  at rev {}", version.rev);
    if version.time > 0 {
        let _ = write!(out, "  {}", utc(version.time));
    }
    out
}

/// `YYYY-MM-DD HH:MMZ` from milliseconds since the Unix epoch.
fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(n: u64, label: Option<&str>) -> VersionInfo {
        VersionInfo {
            version: n,
            label: label.map(str::to_owned),
            summary: Some("tidied the hair".into()),
            author: ReviewAuthor::Agent {
                id: "a".into(),
                name: "claude".into(),
            },
            time: 1_764_072_000_000,
            rev: 7,
            byte_length: 3,
            sha256: String::new(),
        }
    }

    #[test]
    fn a_version_reads_as_one_line_with_its_summary_below() {
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(1_764_072_000_000), "2025-11-25 12:00Z");
        let listed = describe(
            &VersionCmd::List,
            &ResponseBody::Versions {
                versions: vec![version(1, Some("baseline")), version(2, None)],
                kept: Kept::Unsaved,
                kept_problem: None,
            },
            None,
        );
        assert_eq!(
            listed,
            "(the model has no file yet: these versions are kept once it is saved)\n\
             version 2  by agent claude  at rev 7  2025-11-25 12:00Z\n  tidied the hair\n\
             version 1 \"baseline\"  by agent claude  at rev 7  2025-11-25 12:00Z\n  tidied the hair\n"
        );
    }

    #[test]
    fn an_export_is_checked_against_the_version() {
        let dir = std::env::temp_dir().join(format!("cl-version-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v.clm");
        std::fs::write(&path, b"abc").unwrap();
        let mut v = version(1, None);
        v.sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        assert!(verify(&path, &v).is_ok());
        std::fs::write(&path, b"abd").unwrap();
        assert!(verify(&path, &v).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
