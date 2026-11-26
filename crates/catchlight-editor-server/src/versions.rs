//! A session's saved versions: snapshots of its model's `.clm` bytes,
//! numbered per model and kept beside it as its review is. Saving one moves
//! no revision and writes no model file.
//!
//! A store holds `versions/<n>.json` per version, never rewritten, and the
//! bytes cut into content-defined chunks under `versions/chunks/<sha256>`:
//! consecutive versions mostly share their textures, and a cut that depends
//! only on nearby bytes finds the same chunks around an edit. A version is
//! its chunks concatenated, checked against the hash it was saved with.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

use catchlight_core::Model;
use catchlight_editor_protocol::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::kept_store::KeptStore;
use crate::lifecycle::sha256;
use crate::{Captured, Editor, EditorError, Payload};

const RECORD_FORMAT: u32 = 1;
const MAX_LABEL_BYTES: usize = 80;
const MAX_SUMMARY_BYTES: usize = 280;
const MAX_KEY_BYTES: usize = 128;

/// How many versions a model keeps, and how many distinct chunk bytes they
/// may hold between them; past either, saving is refused.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VersionLimits {
    pub versions: u64,
    pub bytes: u64,
}

impl Default for VersionLimits {
    fn default() -> Self {
        Self {
            versions: 1000,
            bytes: 2 << 30,
        }
    }
}

fn record_name(version: u64) -> String {
    format!("versions/{version}.json")
}

fn chunk_name(hash: &str) -> String {
    format!("versions/chunks/{hash}")
}

/// Content-defined chunking with a gear hash, as `FastCDC` does without its
/// normalisation: a cut lands where the hash of the last 64 bytes has its
/// top 16 bits clear, so about every 64 KiB past the minimum.
mod chunking {
    const MIN: usize = 16 * 1024;
    pub(super) const MAX: usize = 256 * 1024;

    const GEAR: [u64; 256] = {
        // splitmix64, so the table is fixed without a 2 KiB literal.
        let mut table = [0u64; 256];
        let mut state: u64 = 0x6a09_e667_f3bc_c908;
        let mut i = 0;
        while i < 256 {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            table[i] = z ^ (z >> 31);
            i += 1;
        }
        table
    };

    fn cut(data: &[u8]) -> usize {
        if data.len() <= MIN {
            return data.len();
        }
        let end = data.len().min(MAX);
        let mut hash = 0u64;
        for (i, &byte) in data.iter().enumerate().take(end).skip(MIN) {
            hash = (hash << 1).wrapping_add(GEAR[byte as usize]);
            if hash >> 48 == 0 {
                return i + 1;
            }
        }
        end
    }

    pub(super) fn split(mut data: &[u8]) -> Vec<&[u8]> {
        let mut chunks = Vec::new();
        while !data.is_empty() {
            let (chunk, rest) = data.split_at(cut(data));
            chunks.push(chunk);
            data = rest;
        }
        chunks
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct Keyed {
    key: String,
    /// Of the label and summary, to tell a retry from a reuse of its key.
    digest: String,
}

/// One version as a store keeps it.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Record {
    format: u32,
    #[serde(flatten)]
    info: VersionInfo,
    /// `(sha256, length)` in order.
    chunks: Vec<(String, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keyed: Option<Keyed>,
}

/// A session's versions and what its store is known to hold of them.
#[derive(Debug, Default)]
pub(crate) struct Versions {
    records: Vec<Record>,
    /// Chunks not yet in the store under the session's key: every chunk is
    /// here, in `stored`, or both.
    held: HashMap<String, Arc<[u8]>>,
    stored: HashSet<String>,
    /// How many of `records`, oldest first, the store holds.
    written: usize,
    /// Every distinct chunk's length, for the byte limit.
    sizes: HashMap<String, u32>,
    /// Set when the kept versions could not be read. Nothing is written
    /// while it stands, so files a person may still want are never replaced.
    unreadable: Option<String>,
}

fn bad(why: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn line(what: &str, text: Option<String>, max: usize) -> Result<Option<String>, EditorError> {
    let Some(text) = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if text.len() > max || text.contains(['\n', '\r']) {
        return Err(EditorError::BadRequest(format!(
            "a version's {what} is one line of at most {max} bytes"
        )));
    }
    Ok(Some(text))
}

fn digest(label: Option<&str>, summary: Option<&str>) -> String {
    let mut hash = Sha256::new();
    for part in [label, summary] {
        match part {
            Some(text) => {
                hash.update([1]);
                hash.update((text.len() as u64).to_le_bytes());
                hash.update(text.as_bytes());
            }
            None => hash.update([0]),
        }
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether two versions came from the same writer, by id alone, as review
/// tells comments apart.
fn same_author(a: &ReviewAuthor, b: &ReviewAuthor) -> bool {
    match (a, b) {
        (ReviewAuthor::Human { id: a, .. }, ReviewAuthor::Human { id: b, .. }) => a == b,
        (ReviewAuthor::Agent { id: a, .. }, ReviewAuthor::Agent { id: b, .. }) => a == b,
        _ => false,
    }
}

/// A version save as its author sent it.
pub(crate) struct Draft {
    pub label: Option<String>,
    pub summary: Option<String>,
    pub author: ReviewAuthor,
    pub key: Option<String>,
}

/// What a record read from a store must hold before its chunk names reach a
/// path or its length reaches an allocation: names that are sha256 digests,
/// lengths a save could have cut, and a length that is their sum.
fn check_record(record: &Record) -> Result<(), String> {
    let mut total = 0u64;
    for (hash, len) in &record.chunks {
        let digest = hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !digest {
            return Err(format!("chunk {hash:?} is not named by its sha256"));
        }
        if *len as usize > chunking::MAX {
            return Err(format!("chunk {hash} claims {len} bytes"));
        }
        total += u64::from(*len);
    }
    if total != record.info.byte_length {
        return Err(format!(
            "its chunks hold {total} bytes, not {}",
            record.info.byte_length
        ));
    }
    Ok(())
}

impl Versions {
    /// The versions `key` was left with. Versions that cannot be read never
    /// block opening their model.
    pub(crate) fn open(store: &dyn KeptStore, key: &str) -> Self {
        if !store.keeps() {
            return Self::default();
        }
        let mut versions = Self::default();
        for n in 1.. {
            let name = record_name(n);
            let read = store.read(key, &name).and_then(|found| {
                found
                    .map(|bytes| {
                        let record: Record = serde_json::from_slice(&bytes)
                            .map_err(|e| bad(format!("{}: {e}", store.place(key, &name))))?;
                        if record.format != RECORD_FORMAT || record.info.version != n {
                            return Err(bad(format!(
                                "{}: not version {n} in format {RECORD_FORMAT}",
                                store.place(key, &name)
                            )));
                        }
                        check_record(&record)
                            .map_err(|why| bad(format!("{}: {why}", store.place(key, &name))))?;
                        Ok(record)
                    })
                    .transpose()
            });
            match read {
                Ok(Some(record)) => {
                    for (hash, len) in &record.chunks {
                        versions.stored.insert(hash.clone());
                        versions.sizes.insert(hash.clone(), *len);
                    }
                    versions.records.push(record);
                }
                Ok(None) => break,
                Err(e) => {
                    return Self {
                        unreadable: Some(e.to_string()),
                        ..Self::default()
                    };
                }
            }
        }
        versions.written = versions.records.len();
        versions
    }

    pub(crate) fn kept(&self, store: &dyn KeptStore, key: Option<&str>) -> Kept {
        if !store.keeps() {
            Kept::Session
        } else if key.is_none() {
            Kept::Unsaved
        } else if self.unreadable.is_some() {
            Kept::Unreadable
        } else {
            Kept::Stored
        }
    }

    pub(crate) fn list(&self, store: &dyn KeptStore, key: Option<&str>) -> ResponseBody {
        let kept = self.kept(store, key);
        ResponseBody::Versions {
            versions: self.records.iter().map(|r| r.info.clone()).collect(),
            kept,
            kept_problem: (kept == Kept::Unreadable)
                .then(|| self.unreadable.clone())
                .flatten(),
        }
    }

    pub(crate) fn has(&self, version: u64) -> bool {
        self.record(version).is_ok()
    }

    fn record(&self, version: u64) -> Result<&Record, EditorError> {
        version
            .checked_sub(1)
            .and_then(|i| self.records.get(i as usize))
            .ok_or(EditorError::NoVersion(version))
    }

    /// The version saved, and whether this saved it rather than repeat a
    /// keyed save. A store that refuses leaves the versions as they were.
    #[expect(
        clippy::too_many_arguments,
        reason = "the inputs are independent, with no type that groups them"
    )]
    pub(crate) fn save(
        &mut self,
        store: &dyn KeptStore,
        key: Option<&str>,
        limits: VersionLimits,
        draft: Draft,
        bytes: &[u8],
        rev: u64,
        time: u64,
    ) -> Result<(VersionInfo, bool), EditorError> {
        let label = line("label", draft.label, MAX_LABEL_BYTES)?;
        let summary = line("summary", draft.summary, MAX_SUMMARY_BYTES)?;
        let author = crate::review::author_of(draft.author)?;
        let retry = match draft.key {
            Some(k) if k.is_empty() || k.len() > MAX_KEY_BYTES => {
                return Err(EditorError::BadRequest(format!(
                    "a retry key is 1 to {MAX_KEY_BYTES} bytes"
                )));
            }
            k => k,
        };
        let digest = digest(label.as_deref(), summary.as_deref());
        if let Some(retry) = &retry {
            let found = self.records.iter().find(|r| {
                r.keyed.as_ref().is_some_and(|k| &k.key == retry)
                    && same_author(&r.info.author, &author)
            });
            if let Some(found) = found {
                if found.keyed.as_ref().is_some_and(|k| k.digest == digest) {
                    return Ok((found.info.clone(), false));
                }
                return Err(EditorError::KeyConflict(retry.clone()));
            }
        }
        let count = self.records.len() as u64 + 1;
        if count > limits.versions {
            return Err(EditorError::Limit {
                resource: "versions",
                limit: limits.versions,
                requested: count,
            });
        }
        let pieces = chunking::split(bytes);
        let mut chunks = Vec::with_capacity(pieces.len());
        let mut fresh: HashMap<String, &[u8]> = HashMap::new();
        for piece in &pieces {
            let hash = sha256(piece);
            if !self.sizes.contains_key(&hash) {
                fresh.insert(hash.clone(), piece);
            }
            chunks.push((hash, piece.len() as u32));
        }
        let held: u64 = self.sizes.values().map(|&len| u64::from(len)).sum();
        let total = held + fresh.values().map(|p| p.len() as u64).sum::<u64>();
        if total > limits.bytes {
            return Err(EditorError::Limit {
                resource: "version_bytes",
                limit: limits.bytes,
                requested: total,
            });
        }
        let version = count;
        let info = VersionInfo {
            version,
            label,
            summary,
            author,
            time,
            rev,
            byte_length: bytes.len() as u64,
            sha256: sha256(bytes),
        };
        for (hash, piece) in fresh {
            self.sizes.insert(hash.clone(), piece.len() as u32);
            self.held.insert(hash, Arc::from(piece));
        }
        self.records.push(Record {
            format: RECORD_FORMAT,
            info: info.clone(),
            chunks,
            keyed: retry.map(|key| Keyed { key, digest }),
        });
        if let Err(e) = self.persist(store, key) {
            // Its chunks stay held: harmless, and a later save may share them.
            self.records.pop();
            return Err(e.into());
        }
        Ok((info, true))
    }

    /// Writes what the store does not hold yet under `key`: each chunk
    /// before the first record that names it, and each record once.
    pub(crate) fn persist(&mut self, store: &dyn KeptStore, key: Option<&str>) -> io::Result<()> {
        let Some(key) = key else { return Ok(()) };
        if !store.keeps() || self.unreadable.is_some() {
            return Ok(());
        }
        while self.written < self.records.len() {
            let record = &self.records[self.written];
            for (hash, _) in &record.chunks {
                if self.stored.contains(hash) {
                    continue;
                }
                let bytes = self.held.get(hash).ok_or_else(|| {
                    bad(format!(
                        "a piece of version {} is lost",
                        record.info.version
                    ))
                })?;
                store.write(key, &chunk_name(hash), bytes)?;
                self.stored.insert(hash.clone());
            }
            let name = record_name(record.info.version);
            let document = serde_json::to_vec(record).map_err(bad)?;
            // Two sessions on one model must not both number a version n.
            if let Some(there) = store.read(key, &name)?
                && there != document
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} was saved meanwhile by another session on this model; reopen it",
                        store.place(key, &name)
                    ),
                ));
            }
            store.write(key, &name, &document)?;
            self.written += 1;
        }
        let stored = &self.stored;
        self.held.retain(|hash, _| !stored.contains(hash));
        Ok(())
    }

    /// The session's model now lives at `to`: read what only the store
    /// under `from` holds, then write every version under the new key,
    /// replacing whatever versions were kept for it.
    pub(crate) fn rekey(
        &mut self,
        store: &dyn KeptStore,
        from: Option<&str>,
        to: &str,
    ) -> io::Result<()> {
        if !store.keeps() {
            return Ok(());
        }
        if let Some(from) = from.filter(|_| self.unreadable.is_none()) {
            for record in &self.records {
                for (hash, _) in &record.chunks {
                    if self.held.contains_key(hash) {
                        continue;
                    }
                    let bytes = store.read(from, &chunk_name(hash))?.ok_or_else(|| {
                        bad(format!(
                            "{} is missing",
                            store.place(from, &chunk_name(hash))
                        ))
                    })?;
                    self.held.insert(hash.clone(), Arc::from(bytes));
                }
            }
        }
        self.stored.clear();
        self.written = 0;
        self.unreadable = None;
        let mut stale = self.records.len() as u64 + 1;
        while store.read(to, &record_name(stale))?.is_some() {
            store.delete(to, &record_name(stale))?;
            stale += 1;
        }
        for n in 1..=self.records.len() as u64 {
            store.delete(to, &record_name(n))?;
        }
        self.persist(store, Some(to))
    }

    /// A version's `.clm`, exactly as it was saved.
    pub(crate) fn bytes(
        &self,
        store: &dyn KeptStore,
        key: Option<&str>,
        version: u64,
    ) -> Result<(VersionInfo, Vec<u8>), EditorError> {
        let record = self.record(version)?;
        let mut bytes = Vec::new();
        for (hash, _) in &record.chunks {
            if let Some(held) = self.held.get(hash) {
                bytes.extend_from_slice(held);
                continue;
            }
            let name = chunk_name(hash);
            let read = match key {
                Some(key) => store.read(key, &name)?,
                None => None,
            };
            let piece = read.ok_or_else(|| {
                bad(format!(
                    "version {version} is missing a piece: {}",
                    key.map_or_else(|| name.clone(), |key| store.place(key, &name))
                ))
            })?;
            bytes.extend_from_slice(&piece);
        }
        if sha256(&bytes) != record.info.sha256 {
            return Err(bad(format!("version {version} is damaged: its bytes changed")).into());
        }
        Ok((record.info.clone(), bytes))
    }
}

impl Editor {
    fn versions_of(
        &self,
        session: SessionId,
        version: u64,
    ) -> Result<Captured<(VersionInfo, Vec<u8>, String)>, EditorError> {
        self.with_session_captured(session, |s| {
            let (info, bytes) = s
                .versions
                .bytes(&*self.kept_store, s.file.as_deref(), version)?;
            Ok((info, bytes, s.title.clone()))
        })
    }

    pub(crate) fn version(
        &self,
        command: Command,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        match command {
            Command::VersionSave {
                session,
                label,
                summary,
                author,
                key,
            } => {
                let time = (self.clock)();
                let captured = self.with_session_captured(session, |s| {
                    s.writable()?;
                    let bytes = s.model.to_clm_bytes()?;
                    let rev = s.rev;
                    let draft = Draft {
                        label,
                        summary,
                        author,
                        key,
                    };
                    let file = s.file.clone();
                    s.versions.save(
                        &*self.kept_store,
                        file.as_deref(),
                        self.version_limits,
                        draft,
                        &bytes,
                        rev,
                        time,
                    )
                })?;
                let (version, saved) = captured.value;
                if saved {
                    self.notify(Event::VersionsChanged { session });
                }
                Ok(Captured {
                    value: ResponseBody::Version { version },
                    rev: captured.rev,
                })
            }
            Command::VersionList { session } => self.with_session_captured(session, |s| {
                Ok(s.versions.list(&*self.kept_store, s.file.as_deref()))
            }),
            Command::VersionGet { session, version } => {
                let captured = self.versions_of(session, version)?;
                let (version, bytes, _) = captured.value;
                *payload = Some(Payload {
                    content_type: "application/octet-stream",
                    bytes,
                });
                Ok(Captured {
                    value: ResponseBody::Version { version },
                    rev: captured.rev,
                })
            }
            Command::VersionRestore {
                session,
                version,
                if_rev,
            } => {
                let (info, bytes, _) = self.versions_of(session, version)?.value;
                let model = Model::from_clm_bytes(&bytes)?;
                self.edit_session_captured(session, |s| {
                    s.check_revision(if_rev)?;
                    s.model.replace_from(&model);
                    Ok(ResponseBody::Version { version: info })
                })
            }
            Command::VersionOpen { session, version } => {
                let viewing = VersionViewing { session, version };
                if let Some(open) = self.viewing_session(viewing) {
                    return Ok(Captured::at(ResponseBody::Session { session: open }, 0));
                }
                let (_, bytes, title) = self.versions_of(session, version)?.value;
                let model = Model::from_clm_bytes(&bytes)?;
                let title = catchlight_core::id::Name::truncated(format!("{title} v{version}"));
                self.publish_viewing(model, title.as_str().to_owned(), viewing)
            }
            other => Err(EditorError::BadRequest(format!(
                "{} is not a version command",
                other.tag()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {

    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;
    use crate::{Attachments, FileStorage, NoKeptStore, SidecarKeptStore, UserKeptStore};

    struct Fixture {
        editor: Editor,
        session: SessionId,
        events: Arc<Mutex<Vec<Event>>>,
        next: u64,
    }

    fn agent(name: &str) -> ReviewAuthor {
        ReviewAuthor::Agent {
            id: format!("{name}:1"),
            name: name.into(),
        }
    }

    impl Fixture {
        fn on(editor: Editor, command: Command) -> Self {
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            editor
                .subscribe(Box::new(move |event| {
                    sink.lock().unwrap().push(event.clone());
                }))
                .detach();
            let mut f = Self {
                editor,
                session: SessionId(0),
                events,
                next: 0,
            };
            f.session = match f.ok(command) {
                ResponseBody::Session { session } => session,
                other => panic!("{other:?}"),
            };
            f
        }

        fn new(editor: Editor) -> Self {
            Self::on(
                editor,
                Command::SessionNew {
                    name: None,
                    source: None,
                },
            )
        }

        fn send(&mut self, command: Command) -> Reply {
            self.next += 1;
            self.editor.handle(Request {
                id: self.next,
                command,
            })
        }

        fn ok(&mut self, command: Command) -> ResponseBody {
            match self.send(command) {
                Reply::Ok { body, .. } => body,
                other => panic!("{other:?}"),
            }
        }

        fn refused(&mut self, command: Command) -> (ErrorCode, Option<LimitInfo>) {
            match self.send(command) {
                Reply::Err { code, limit, .. } => (code, limit),
                other => panic!("{other:?}"),
            }
        }

        fn status(&mut self) -> StatusInfo {
            let session = self.session;
            match self.ok(Command::Status { session }) {
                ResponseBody::Status { status } => status,
                other => panic!("{other:?}"),
            }
        }

        fn save(&mut self, label: Option<&str>, key: Option<&str>) -> Reply {
            let session = self.session;
            self.send(Command::VersionSave {
                session,
                label: label.map(str::to_owned),
                summary: None,
                author: agent("claude"),
                key: key.map(str::to_owned),
            })
        }

        fn saved(&mut self, label: Option<&str>) -> VersionInfo {
            match self.save(label, None) {
                Reply::Ok {
                    body: ResponseBody::Version { version },
                    ..
                } => version,
                other => panic!("{other:?}"),
            }
        }

        fn list(&mut self) -> (Vec<VersionInfo>, Kept) {
            let session = self.session;
            match self.ok(Command::VersionList { session }) {
                ResponseBody::Versions { versions, kept, .. } => (versions, kept),
                other => panic!("{other:?}"),
            }
        }

        fn bytes(&mut self, version: u64) -> Vec<u8> {
            let session = self.session;
            let (reply, payload) = self.editor.handle_with(
                Request {
                    id: 99,
                    command: Command::VersionGet { session, version },
                },
                Attachments::none(),
            );
            assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
            payload.unwrap().bytes
        }

        fn model(&mut self) -> Vec<u8> {
            let session = self.session;
            self.editor
                .with_model(session, |m| m.to_clm_bytes().unwrap())
                .unwrap()
        }

        fn group(&mut self, name: &str) -> NodeId {
            let session = self.session;
            let root = match self.ok(Command::NodeTree { session }) {
                ResponseBody::Tree { root } => root.id,
                other => panic!("{other:?}"),
            };
            match self.ok(Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Group,
                name: Some(name.into()),
                node: None,
            }) {
                ResponseBody::Node { node, .. } => node,
                other => panic!("{other:?}"),
            }
        }

        /// A part drawing `png`, so the model is big enough to chunk.
        fn textured(&mut self, png: Vec<u8>) {
            let session = self.session;
            let root = match self.ok(Command::NodeTree { session }) {
                ResponseBody::Tree { root } => root.id,
                other => panic!("{other:?}"),
            };
            let node = match self.ok(Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Part,
                name: Some("art".into()),
                node: None,
            }) {
                ResponseBody::Node { node, .. } => node,
                other => panic!("{other:?}"),
            };
            let attachments: Attachments = std::iter::once(("texture".to_owned(), png)).collect();
            let (reply, _) = self.editor.handle_with(
                Request {
                    id: 98,
                    command: Command::TextureAdd {
                        session,
                        node,
                        encoding: TextureEncoding::Png,
                        texture: None,
                    },
                },
                attachments,
            );
            assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
        }

        fn version_events(&self) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, Event::VersionsChanged { .. }))
                .count()
        }
    }

    /// Noise, so the PNG does not compress below the chunk size.
    fn noise_png(seed: u32) -> Vec<u8> {
        let mut state = seed | 1;
        let image = image::RgbaImage::from_fn(256, 256, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            image::Rgba(state.to_le_bytes())
        });
        let mut png = Vec::new();
        image
            .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn temp(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cl-{label}-{}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(
                        path.strip_prefix(dir)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn a_version_is_numbered_kept_as_it_was_and_moves_nothing() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let session = f.session;
        f.group("a");
        let before = f.status();
        let first = f.saved(Some(" baseline "));
        assert_eq!(first.version, 1);
        assert_eq!(first.label.as_deref(), Some("baseline"));
        assert_eq!(first.rev, before.rev);
        assert_eq!(first.author, agent("claude"));
        let after = f.status();
        assert_eq!(
            (after.rev, after.dirty, after.undo_steps),
            (before.rev, before.dirty, before.undo_steps),
            "saving a version is no edit"
        );
        let at_first = f.model();
        assert_eq!(first.byte_length, at_first.len() as u64);
        assert_eq!(first.sha256, sha256(&at_first));

        f.group("b");
        let second = f.saved(None);
        assert_eq!((second.version, second.label.clone()), (2, None));
        assert_eq!(f.bytes(1), at_first, "an edit leaves a version as it was");
        assert_ne!(f.bytes(2), at_first);
        let (listed, kept) = f.list();
        assert_eq!(listed, [first, second]);
        assert_eq!(kept, Kept::Session);
        assert_eq!(f.version_events(), 2);

        assert_eq!(
            f.refused(Command::VersionGet {
                session,
                version: 3
            })
            .0,
            ErrorCode::NoVersion
        );
        let session = f.session;
        for bad in ["two\nlines", &"x".repeat(81)] {
            assert_eq!(
                f.refused(Command::VersionSave {
                    session,
                    label: Some(bad.into()),
                    summary: None,
                    author: agent("claude"),
                    key: None,
                })
                .0,
                ErrorCode::BadRequest
            );
        }
        assert_eq!(f.list().0.len(), 2, "nothing refused is numbered");
    }

    #[test]
    fn restoring_is_one_edit_that_undo_takes_back() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let session = f.session;
        f.group("a");
        let v1 = f.saved(None);
        let at_v1 = f.model();
        f.group("b");
        f.group("c");
        let edited = f.model();
        let before = f.status();

        let restored = match f.send(Command::VersionRestore {
            session,
            version: 1,
            if_rev: Some(before.rev),
        }) {
            Reply::Ok {
                body: ResponseBody::Version { version },
                rev,
                ..
            } => {
                assert_eq!(rev, Some(before.rev + 1));
                version
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(restored, v1);
        assert_eq!(f.model(), at_v1);
        assert_eq!(f.status().undo_steps, before.undo_steps + 1);
        assert_eq!(f.list().0.len(), 1, "a restore saves no version");

        let rev = f.status().rev;
        f.ok(Command::Undo {
            session,
            if_rev: rev,
        });
        assert_eq!(f.model(), edited);
        let rev = f.status().rev;
        f.ok(Command::Redo {
            session,
            if_rev: rev,
        });
        assert_eq!(f.model(), at_v1);

        let rev = f.status().rev;
        assert_eq!(
            f.refused(Command::VersionRestore {
                session,
                version: 1,
                if_rev: Some(rev - 1),
            })
            .0,
            ErrorCode::RevisionConflict
        );
        f.ok(Command::VersionRestore {
            session,
            version: 1,
            if_rev: None,
        });
        assert_eq!(
            f.status().rev,
            rev,
            "restoring what is there changes nothing"
        );
    }

    #[test]
    fn a_version_opened_for_viewing_refuses_every_edit() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let live = f.session;
        f.group("a");
        f.saved(Some("one"));
        f.group("b");
        let opened = match f.ok(Command::VersionOpen {
            session: live,
            version: 1,
        }) {
            ResponseBody::Session { session } => session,
            other => panic!("{other:?}"),
        };
        assert_ne!(opened, live);
        match f.ok(Command::VersionOpen {
            session: live,
            version: 1,
        }) {
            ResponseBody::Session { session } => assert_eq!(session, opened, "opened once"),
            other => panic!("{other:?}"),
        }
        let sessions = match f.ok(Command::SessionList) {
            ResponseBody::Sessions { sessions, .. } => sessions,
            other => panic!("{other:?}"),
        };
        let view = sessions.iter().find(|s| s.session == opened).unwrap();
        assert_eq!(
            view.viewing,
            Some(VersionViewing {
                session: live,
                version: 1
            })
        );
        assert!(view.title.ends_with(" v1"), "{}", view.title);
        assert_eq!(
            sessions.iter().find(|s| s.session == live).unwrap().viewing,
            None
        );

        let v1 = f.bytes(1);
        let viewed = f
            .editor
            .with_model(opened, |m| m.to_clm_bytes().unwrap())
            .unwrap();
        assert_eq!(viewed, v1);
        assert!(matches!(
            f.ok(Command::NodeTree { session: opened }),
            ResponseBody::Tree { .. }
        ));

        let root = match f.ok(Command::NodeTree { session: opened }) {
            ResponseBody::Tree { root } => root.id,
            other => panic!("{other:?}"),
        };
        let refusals = [
            Command::NodeAdd {
                session: opened,
                parent: root,
                kind: NodeKindArg::Group,
                name: None,
                node: None,
            },
            Command::Undo {
                session: opened,
                if_rev: 0,
            },
            Command::Save {
                session: opened,
                path: Some("x.clm".into()),
            },
            Command::VersionSave {
                session: opened,
                label: None,
                summary: None,
                author: agent("claude"),
                key: None,
            },
            Command::ReviewThreadAdd {
                session: opened,
                target: ReviewTarget::Model,
                author: agent("claude"),
                body: "hi".into(),
                pose: Vec::new(),
                images: Vec::new(),
                key: None,
                version: None,
            },
        ];
        for command in refusals {
            let tag = command.tag();
            match f.send(command) {
                Reply::Err { code, message, .. } => {
                    assert_eq!(code, ErrorCode::ReadOnly, "{tag}");
                    assert!(message.contains("version 1"), "{message}");
                }
                other => panic!("{tag}: {other:?}"),
            }
        }
        assert!(
            f.editor
                .handle(Request {
                    id: 1,
                    command: Command::PresenceSet {
                        session: opened,
                        presence: Presence::default(),
                    },
                })
                .is_ok_reply(),
            "looking around is no edit"
        );

        f.ok(Command::SessionClose { session: live });
        assert_eq!(
            f.refused(Command::NodeTree { session: opened }).0,
            ErrorCode::NoSession,
            "a view closes with its session"
        );
    }

    /// A version open that read its bytes before the session closed must
    /// not leave a view the close never saw.
    #[test]
    fn a_view_published_after_its_session_closed_is_refused() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let live = f.session;
        f.saved(None);
        let bytes = f.bytes(1);
        f.ok(Command::SessionClose { session: live });

        let viewing = VersionViewing {
            session: live,
            version: 1,
        };
        let published = f.editor.publish_viewing(
            Model::from_clm_bytes(&bytes).unwrap(),
            "late v1".into(),
            viewing,
        );
        assert!(
            matches!(published, Err(EditorError::NoSession(s)) if s == live),
            "{published:?}"
        );
        match f.ok(Command::SessionList) {
            ResponseBody::Sessions { sessions, .. } => {
                assert!(sessions.is_empty(), "{sessions:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    trait IsOk {
        fn is_ok_reply(&self) -> bool;
    }

    impl IsOk for Reply {
        fn is_ok_reply(&self) -> bool {
            matches!(self, Self::Ok { .. })
        }
    }

    #[test]
    fn a_retry_key_saves_once_and_a_reused_one_is_refused() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let session = f.session;
        let first = match f.save(Some("fix"), Some("k1")) {
            Reply::Ok {
                body: ResponseBody::Version { version },
                ..
            } => version,
            other => panic!("{other:?}"),
        };
        f.group("a");
        match f.save(Some("fix"), Some("k1")) {
            Reply::Ok {
                body: ResponseBody::Version { version },
                ..
            } => assert_eq!(version, first),
            other => panic!("{other:?}"),
        }
        assert_eq!(f.version_events(), 1, "a retry announces nothing");
        match f.save(Some("other"), Some("k1")) {
            Reply::Err { code, .. } => assert_eq!(code, ErrorCode::KeyConflict),
            other => panic!("{other:?}"),
        }
        let theirs = f.ok(Command::VersionSave {
            session,
            label: Some("fix".into()),
            summary: None,
            author: agent("codex"),
            key: Some("k1".into()),
        });
        assert!(
            matches!(theirs, ResponseBody::Version { version } if version.version == 2),
            "another author's key is its own"
        );
    }

    #[test]
    fn limits_refuse_a_version_past_the_count_or_the_bytes() {
        let mut f = Fixture::new(
            Editor::with_storage(Arc::new(crate::NoStorage)).with_version_limits(2, 1 << 30),
        );
        f.saved(None);
        f.saved(None);
        let session = f.session;
        let save = Command::VersionSave {
            session,
            label: None,
            summary: None,
            author: agent("claude"),
            key: None,
        };
        let (code, limit) = f.refused(save);
        assert_eq!(code, ErrorCode::LimitExceeded);
        let limit = limit.unwrap();
        assert_eq!(
            (limit.resource.as_str(), limit.limit, limit.requested),
            ("versions", 2, 3)
        );

        let mut g = Fixture::new(
            Editor::with_storage(Arc::new(crate::NoStorage)).with_version_limits(10, 64),
        );
        let session = g.session;
        let (code, limit) = g.refused(Command::VersionSave {
            session,
            label: None,
            summary: None,
            author: agent("claude"),
            key: None,
        });
        assert_eq!(code, ErrorCode::LimitExceeded);
        assert_eq!(limit.unwrap().resource, "version_bytes");
        assert!(g.list().0.is_empty());
    }

    #[test]
    fn a_reply_points_at_a_version_the_session_has() {
        let mut f = Fixture::new(Editor::with_storage(Arc::new(crate::NoStorage)));
        let session = f.session;
        let open = |version| Command::ReviewThreadAdd {
            session,
            target: ReviewTarget::Model,
            author: agent("claude"),
            body: "fixed".into(),
            pose: Vec::new(),
            images: Vec::new(),
            key: None,
            version,
        };
        assert_eq!(f.refused(open(Some(1))).0, ErrorCode::NoVersion);
        f.saved(None);
        match f.ok(open(Some(1))) {
            ResponseBody::ReviewThread { thread } => {
                assert_eq!(thread.comments[0].version, Some(1));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unsaved_models_versions_are_written_out_when_it_is_first_saved() {
        let dir = temp("versions-unsaved");
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&dir)))
                .with_kept_store(Arc::new(SidecarKeptStore::new(&dir)))
        };
        let mut f = Fixture::new(editor());
        f.saved(Some("draft"));
        assert_eq!(f.list().1, Kept::Unsaved);
        let draft = f.bytes(1);
        let events = f.version_events();
        let session = f.session;
        f.ok(Command::Save {
            session,
            path: Some("m.clm".into()),
        });
        assert_eq!(f.version_events(), events + 1, "how long they last changed");
        assert_eq!(f.list().1, Kept::Stored);
        let kept = dir.join("m.clm.kept");
        let files = files_under(&kept);
        assert!(files.contains(&"versions/1.json".to_owned()), "{files:?}");
        assert!(files.iter().any(|f| f.starts_with("versions/chunks/")));

        // Saved under another name, the versions go with it.
        f.ok(Command::Save {
            session,
            path: Some("n.clm".into()),
        });
        assert_eq!(files_under(&dir.join("n.clm.kept")), files);

        let mut g = Fixture::on(
            editor(),
            Command::SessionOpen {
                path: "n.clm".into(),
            },
        );
        assert_eq!(g.list(), f.list());
        assert_eq!(g.bytes(1), draft);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Both stores: what one editor saved, another reads back byte for byte,
    /// and the number after it is never handed out again.
    #[test]
    fn versions_outlive_the_editor_in_either_store() {
        for beside in [false, true] {
            let dir = temp("versions-kept");
            let models = dir.join("models");
            std::fs::create_dir_all(&models).unwrap();
            let kept_dir = if beside {
                models.clone()
            } else {
                dir.join("kept")
            };
            let store: Arc<dyn KeptStore> = if beside {
                Arc::new(SidecarKeptStore::new(&models))
            } else {
                Arc::new(UserKeptStore::new(&models, &kept_dir))
            };
            let editor = || {
                Editor::with_storage(Arc::new(FileStorage::new(&models)))
                    .with_kept_store(store.clone())
            };
            let mut f = Fixture::new(editor());
            let session = f.session;
            f.textured(noise_png(7));
            f.ok(Command::Save {
                session,
                path: Some("m.clm".into()),
            });
            let v1 = f.saved(Some("baseline"));
            f.group("tweak");
            let v2 = f.saved(None);
            let (b1, b2) = (f.bytes(1), f.bytes(2));
            assert!(b1.len() > 200_000, "{} bytes", b1.len());

            // The texture is kept once for both versions.
            let chunks: u64 = files_under(&kept_dir)
                .iter()
                .filter(|f| f.contains("versions/chunks/"))
                .map(|f| std::fs::metadata(kept_dir.join(f)).unwrap().len())
                .sum();
            assert!(
                chunks < b1.len() as u64 + b2.len() as u64 / 2,
                "{chunks} bytes kept for {} + {}",
                b1.len(),
                b2.len()
            );

            let mut g = Fixture::on(
                editor(),
                Command::SessionOpen {
                    path: "m.clm".into(),
                },
            );
            assert_eq!(g.list(), (vec![v1, v2], Kept::Stored));
            assert_eq!(g.bytes(1), b1);
            assert_eq!(g.bytes(2), b2);
            assert_eq!(g.saved(None).version, 3);
            let mut h = Fixture::on(
                editor(),
                Command::SessionOpen {
                    path: "m.clm".into(),
                },
            );
            assert_eq!(h.list().0.len(), 3);

            // A second session on the model must not number a version 4 too.
            g.saved(None);
            assert!(!h.save(None, None).is_ok_reply());
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn versions_that_cannot_be_read_are_left_alone() {
        let dir = temp("versions-unreadable");
        let store = Arc::new(SidecarKeptStore::new(&dir));
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&dir))).with_kept_store(store.clone())
        };
        let mut f = Fixture::new(editor());
        let session = f.session;
        f.ok(Command::Save {
            session,
            path: Some("m.clm".into()),
        });
        f.saved(None);
        let record = dir.join("m.clm.kept/versions/1.json");
        std::fs::write(&record, b"torn").unwrap();
        let mut g = Fixture::on(
            editor(),
            Command::SessionOpen {
                path: "m.clm".into(),
            },
        );
        let session = g.session;
        match g.ok(Command::VersionList { session }) {
            ResponseBody::Versions {
                versions,
                kept,
                kept_problem,
            } => {
                assert!(versions.is_empty());
                assert_eq!(kept, Kept::Unreadable);
                assert!(kept_problem.unwrap().contains("1.json"));
            }
            other => panic!("{other:?}"),
        }
        g.saved(None);
        assert_eq!(std::fs::read(&record).unwrap(), b"torn");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_record_that_names_chunks_it_cannot_have_is_unreadable() {
        let dir = temp("versions-forged");
        let store = Arc::new(SidecarKeptStore::new(&dir));
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&dir))).with_kept_store(store.clone())
        };
        let mut f = Fixture::new(editor());
        let session = f.session;
        f.ok(Command::Save {
            session,
            path: Some("m.clm".into()),
        });
        f.saved(None);
        let record = dir.join("m.clm.kept/versions/1.json");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        let hash = saved["chunks"][0][0].as_str().unwrap().to_owned();
        let len = saved["chunks"][0][1].as_u64().unwrap();
        let forgeries = [
            ("chunks", serde_json::json!([["../../../escape", len]])),
            ("chunks", serde_json::json!([[hash.to_uppercase(), len]])),
            ("chunks", serde_json::json!([[hash, 1u64 << 31]])),
            ("byte_length", serde_json::json!(1e18 as u64)),
        ];
        for (field, value) in forgeries {
            let mut forged = saved.clone();
            forged[field] = value;
            std::fs::write(&record, serde_json::to_vec(&forged).unwrap()).unwrap();
            let mut g = Fixture::on(
                editor(),
                Command::SessionOpen {
                    path: "m.clm".into(),
                },
            );
            let (versions, kept) = g.list();
            assert!(versions.is_empty(), "{forged}");
            assert_eq!(kept, Kept::Unreadable, "{forged}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_editor_that_keeps_nothing_says_versions_last_for_the_session() {
        let mut f = Fixture::new(
            Editor::with_storage(Arc::new(crate::NoStorage)).with_kept_store(Arc::new(NoKeptStore)),
        );
        f.saved(None);
        assert_eq!(f.list().1, Kept::Session);
    }

    #[test]
    fn the_user_store_prunes_versions_with_the_rest() {
        use std::time::Duration;
        let dir = temp("versions-gc");
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let store = UserKeptStore::new(&models, dir.join("kept"));
        let mut f = Fixture::new(
            Editor::with_storage(Arc::new(FileStorage::new(&models)))
                .with_kept_store(Arc::new(store.clone())),
        );
        let session = f.session;
        f.ok(Command::Save {
            session,
            path: Some("m.clm".into()),
        });
        f.saved(None);
        assert_eq!(store.unused(Duration::ZERO).unwrap(), []);
        std::fs::remove_file(models.join("m.clm")).unwrap();
        let unused = store.unused(Duration::ZERO).unwrap();
        assert_eq!(unused.len(), 1);
        let on_disk: u64 = files_under(&unused[0].dir)
            .iter()
            .map(|f| std::fs::metadata(unused[0].dir.join(f)).unwrap().len())
            .sum();
        assert_eq!(unused[0].bytes, on_disk, "nested chunks are counted");
        assert_eq!(store.prune(Duration::ZERO).unwrap(), unused);
        assert!(!unused[0].dir.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunks_cover_the_bytes_and_survive_an_insertion() {
        let mut state = 1u64;
        let mut data: Vec<u8> = (0..1_500_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let before = chunking::split(&data);
        assert_eq!(before.concat(), data);
        assert!(before.len() > 4, "{} chunks", before.len());
        assert!(before.iter().all(|c| c.len() <= 256 * 1024));
        let old: HashSet<String> = before.iter().map(|c| sha256(c)).collect();
        data.splice(700_000..700_000, *b"an edit in the middle");
        let after = chunking::split(&data);
        assert_eq!(after.concat(), data);
        let shared = after.iter().filter(|c| old.contains(&sha256(c))).count();
        assert!(
            shared + 2 >= after.len(),
            "{shared} of {} chunks shared",
            after.len()
        );
    }
}
