//! The worker's private state: one owner-only file per registration under
//! `<state>/listen/agents/`, written atomically under a short lock.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use catchlight_editor_protocol::ReviewIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::harness::Target;
use super::process::{AgentProcess, Liveness};
use crate::transport::Endpoint;

/// `<base>/catchlight`, the directory the editor keeps `reviews/` in (see
/// `catchlight_editor_server::user_review_dir`). `CATCHLIGHT_STATE_DIR`
/// replaces it whole.
pub fn state_dir(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(dir) = absolute("CATCHLIGHT_STATE_DIR") {
        return Some(dir);
    }
    let base = if cfg!(target_os = "macos") {
        absolute("HOME")?.join("Library/Application Support")
    } else {
        absolute("XDG_STATE_HOME").or_else(|| Some(absolute("HOME")?.join(".local/state")))?
    };
    Some(base.join("catchlight"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// Changes on every `listen`: the worker writes back only to the
    /// registration it read.
    pub nonce: String,
    pub editor: Endpoint,
    pub session: u64,
    pub title: String,
    pub file: Option<String>,
    pub agent: ReviewIdentity,
    pub target: Target,
    pub owner: Option<AgentProcess>,
    /// The command the notice tells the agent to run.
    pub fetch: String,
    /// The batch last notified, or found waiting when it registered.
    pub notified: Option<String>,
    pub failure: Option<String>,
    /// Seconds since the epoch.
    pub registered: u64,
}

impl Registration {
    pub fn name(&self) -> String {
        file_name(&self.editor, self.session, &self.agent.id)
    }

    /// Whether `batch` holds a comment the last notice did not cover.
    pub fn is_new(&self, batch: &str) -> bool {
        let known: Vec<&str> = self
            .notified
            .as_deref()
            .map(|b| b.split('.').collect())
            .unwrap_or_default();
        batch.split('.').any(|id| !known.contains(&id))
    }
}

pub fn file_name(editor: &Endpoint, session: u64, agent: &str) -> String {
    let editor = match editor {
        Endpoint::Socket(path) => format!("socket:{}", path.display()),
        Endpoint::Http { url, .. } => format!("http:{}", url.trim_end_matches('/')),
    };
    let key = serde_json::json!([editor, session, agent]).to_string();
    let hash = Sha256::digest(key.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.json")
}

pub struct Store {
    pub dir: PathBuf,
    /// This user's id, read off a directory it owns.
    pub uid: u32,
}

impl Store {
    pub fn open(state: &Path) -> Result<Self> {
        let dir = state.join("listen");
        private_dir(&dir)?;
        private_dir(&dir.join("agents"))?;
        let uid = std::fs::metadata(&dir)?.uid();
        Ok(Self { dir, uid })
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join("worker.log")
    }

    pub fn worker_lock(&self) -> PathBuf {
        self.dir.join("worker.lock")
    }

    /// The running worker's pid and how many polls it has made.
    pub fn worker_polls(&self) -> PathBuf {
        self.dir.join("worker.polls")
    }

    /// A private directory for the images of one registration's comments:
    /// a shared, guessable temp path would let another user plant files the
    /// agent then opens. The agent may be sandboxed away from the state
    /// directory, so this falls back to the runtime directory, then to a
    /// fresh owner-only directory in the system temp directory.
    pub fn images(
        &self,
        editor: &Endpoint,
        session: u64,
        agent: &str,
        runtime: Option<&Path>,
    ) -> Result<PathBuf> {
        let name = file_name(editor, session, agent);
        let leaf = format!("session-{session}-{}", &name[..12]);
        let mut failed = Vec::new();
        let preferred = [
            Some(self.dir.join("images").join(&leaf)),
            runtime.map(|r| {
                r.join("catchlight-editor")
                    .join("review-images")
                    .join(&leaf)
            }),
        ];
        for dir in preferred.into_iter().flatten() {
            match private_dir(&dir).and_then(|()| probe(&dir)) {
                Ok(()) => return Ok(dir),
                Err(err) => failed.push(format!("{}: {err:#}", dir.display())),
            }
        }
        for _ in 0..8 {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).map_err(|e| anyhow!("no system randomness: {e}"))?;
            let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let dir = std::env::temp_dir().join(format!("catchlight-review-{hex}"));
            // Not recursive: only a directory this call made is trusted.
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {
                    probe(&dir)?;
                    return Ok(dir);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    failed.push(format!("{}: {err}", dir.display()));
                    break;
                }
            }
        }
        bail!(
            "no private directory for review images: {}",
            failed.join("; ")
        )
    }

    pub fn stop_file(&self) -> PathBuf {
        self.dir.join("stop")
    }

    fn agents(&self) -> PathBuf {
        self.dir.join("agents")
    }

    /// Held across every read-modify-write of the registrations.
    pub fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.dir.join("state.lock"))?;
        file.lock()?;
        Ok(file)
    }

    /// Unreadable or foreign files are skipped, never followed.
    pub fn list(&self) -> Result<Vec<Registration>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.agents())? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|n| n.ends_with(".json")) else {
                continue;
            };
            if let Some(reg) = self.read(name)? {
                out.push(reg);
            }
        }
        out.sort_by_key(|r| r.registered);
        Ok(out)
    }

    pub fn read(&self, name: &str) -> Result<Option<Registration>> {
        let path = self.agents().join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        if !meta.is_file() || meta.uid() != self.uid || meta.mode() & 0o077 != 0 {
            return Ok(None);
        }
        let parsed = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Registration>(&bytes).ok())
            .filter(|reg| reg.name() == name);
        if parsed.is_none() {
            // A corrupt record cannot deliver; drop its credential copy.
            self.remove(name)?;
        }
        Ok(parsed)
    }

    pub fn write(&self, reg: &Registration) -> Result<()> {
        let path = self.agents().join(reg.name());
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("no system randomness: {e}"))?;
        let tmp = path.with_extension(format!(
            "{}.tmp",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        let written = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&serde_json::to_vec(reg)?)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.with_context(|| format!("writing {}", path.display()))
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        match std::fs::remove_file(self.agents().join(name)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        }
    }

    /// Re-reads `reg` and applies `change` only if it is still the
    /// registration the caller read: a fresh `listen` or an `unlisten` wins.
    pub fn update(&self, reg: &Registration, change: impl FnOnce(&mut Registration)) -> Result<()> {
        let _lock = self.lock()?;
        let Some(mut current) = self.read(&reg.name())? else {
            return Ok(());
        };
        if current.nonce != reg.nonce {
            return Ok(());
        }
        change(&mut current);
        self.write(&current)
    }

    /// Removes the registrations whose owning harness has exited, and
    /// returns them.
    pub fn sweep(
        &self,
        liveness: &mut dyn FnMut(&AgentProcess) -> Liveness,
    ) -> Result<Vec<Registration>> {
        let _lock = self.lock()?;
        let mut gone = Vec::new();
        for reg in self.list()? {
            if reg
                .owner
                .as_ref()
                .is_some_and(|o| liveness(o) == Liveness::Dead)
            {
                self.remove(&reg.name())?;
                gone.push(reg);
            }
        }
        Ok(gone)
    }
}

/// That files can be written there, from wherever this process runs.
fn probe(dir: &Path) -> Result<()> {
    let path = dir.join(format!(".probe-{}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing in {}", dir.display()))?;
    std::fs::remove_file(&path)?;
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        bail!("{}: not a directory", path.display());
    }
    // Only the owner may change the mode: this refuses a directory someone
    // else planted.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("{}: not a private directory of this user", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn registration(agent: &str, notified: Option<&str>) -> Registration {
        Registration {
            nonce: "n".into(),
            editor: Endpoint::Socket("/run/editor.sock".into()),
            session: 1,
            title: "lan".into(),
            file: None,
            agent: ReviewIdentity {
                id: agent.into(),
                name: "claude".into(),
            },
            target: Target::Codex {
                thread: "t".into(),
                executable: None,
                home: None,
            },
            owner: None,
            fetch: "catchlight-editor --session 1 review fetch".into(),
            notified: notified.map(Into::into),
            failure: None,
            registered: 0,
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cl-listen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_state_dir_sits_beside_the_reviews() {
        let var = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        if !cfg!(target_os = "macos") {
            assert_eq!(
                state_dir(&var(&[("HOME", "/h")])),
                Some(PathBuf::from("/h/.local/state/catchlight"))
            );
            assert_eq!(
                state_dir(&var(&[("HOME", "/h"), ("XDG_STATE_HOME", "/s")])),
                Some(PathBuf::from("/s/catchlight"))
            );
        }
        assert_eq!(
            state_dir(&var(&[("HOME", "/h"), ("CATCHLIGHT_STATE_DIR", "/x")])),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(state_dir(&var(&[("CATCHLIGHT_STATE_DIR", "rel")])), None);
    }

    #[test]
    fn a_notice_covers_a_batch_until_another_comment_arrives() {
        let fresh = registration("a", None);
        assert!(fresh.is_new("3"));
        let told = registration("a", Some("3.4"));
        assert!(!told.is_new("3.4"));
        assert!(
            !told.is_new("4"),
            "a thread taken by another agent is no news"
        );
        assert!(told.is_new("3.4.7"));
        assert!(told.is_new("7"));
    }

    #[test]
    fn listening_again_replaces_and_unlisten_removes() {
        let dir = temp("replace");
        let store = Store::open(&dir).unwrap();
        let mode = std::fs::metadata(store.dir.join("agents")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o700);

        store.write(&registration("claude:a", None)).unwrap();
        let mut again = registration("claude:a", Some("1"));
        again.nonce = "m".into();
        store.write(&again).unwrap();
        store.write(&registration("codex:b", None)).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2, "one registration per agent and session");
        let mine = listed.iter().find(|r| r.agent.id == "claude:a").unwrap();
        assert_eq!(mine.nonce, "m");
        let path = store.dir.join("agents").join(mine.name());
        assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);

        // The worker's write to the replaced registration is dropped.
        store
            .update(&registration("claude:a", None), |r| {
                r.failure = Some("x".into());
            })
            .unwrap();
        assert_eq!(store.read(&mine.name()).unwrap().unwrap().failure, None);
        store
            .update(&again, |r| r.failure = Some("x".into()))
            .unwrap();
        assert_eq!(
            store
                .read(&mine.name())
                .unwrap()
                .unwrap()
                .failure
                .as_deref(),
            Some("x")
        );

        store.remove(&mine.name()).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn images_go_somewhere_private_even_when_the_state_dir_is_shut() {
        let dir = temp("images");
        let store = Store::open(&dir).unwrap();
        let editor = Endpoint::Socket("/run/editor.sock".into());
        let private = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o777 == 0o700;

        let images = store.images(&editor, 1, "claude:a", None).unwrap();
        assert!(images.starts_with(store.dir.join("images")));
        assert!(private(&images));
        assert_ne!(images, store.images(&editor, 1, "codex:b", None).unwrap());

        // A file where the directory would go stands in for a sandbox.
        std::fs::remove_dir_all(store.dir.join("images")).unwrap();
        std::fs::write(store.dir.join("images"), "").unwrap();
        let runtime = dir.join("runtime");
        let images = store
            .images(&editor, 1, "claude:a", Some(&runtime))
            .unwrap();
        assert!(images.starts_with(runtime.join("catchlight-editor")));
        assert!(private(&images));

        std::fs::remove_dir_all(&runtime).unwrap();
        std::fs::write(&runtime, "").unwrap();
        let images = store
            .images(&editor, 1, "claude:a", Some(&runtime))
            .unwrap();
        assert!(
            images.starts_with(std::env::temp_dir()),
            "{}",
            images.display()
        );
        assert!(private(&images));
        std::fs::remove_dir_all(&images).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dead_owner_takes_its_registration_with_it() {
        let dir = temp("sweep");
        let store = Store::open(&dir).unwrap();
        let owner = |pid| AgentProcess {
            pid,
            platform: "linux".into(),
            boot_id: "b".into(),
            namespace: "n".into(),
            started_at: "1".into(),
        };
        let mut dead = registration("claude:dead", None);
        dead.owner = Some(owner(10));
        let mut alive = registration("claude:alive", None);
        alive.owner = Some(owner(20));
        let mut unknown = registration("claude:unknown", None);
        unknown.owner = Some(owner(30));
        let untracked = registration("claude:untracked", None);
        for reg in [&dead, &alive, &unknown, &untracked] {
            store.write(reg).unwrap();
        }
        let gone = store
            .sweep(&mut |o| match o.pid {
                10 => Liveness::Dead,
                20 => Liveness::Alive,
                _ => Liveness::Unknown,
            })
            .unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].agent.id, "claude:dead");
        let mut left: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|r| r.agent.id)
            .collect();
        left.sort();
        assert_eq!(left, ["claude:alive", "claude:unknown", "claude:untracked"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_or_loose_file_is_never_read() {
        let dir = temp("corrupt");
        let store = Store::open(&dir).unwrap();
        let reg = registration("claude:a", None);
        store.write(&reg).unwrap();
        let path = store.dir.join("agents").join(reg.name());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.list().unwrap().is_empty());
        std::fs::write(store.dir.join("agents").join("bad.json"), "{").unwrap();
        std::fs::set_permissions(
            store.dir.join("agents").join("bad.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(!store.dir.join("agents").join("bad.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
