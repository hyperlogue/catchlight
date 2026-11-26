//! Session lifecycle: create, open, fork, list, close, save and export.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use catchlight_extension_animations::Animations;
use catchlight_extension_expressions::Expressions;
use catchlight_extension_humanoid::Humanoid;
use sha2::{Digest, Sha256};

use catchlight_core::formats::clm::TextureEncoding as CoreTextureEncoding;
use catchlight_core::id::Name;
use catchlight_core::{LoadBudget, Model, ModelError};
use catchlight_editor_core::{Manifest, ModelManifestExt as _, TextureData};
use catchlight_editor_protocol::*;

use crate::editor::lock;
use crate::ops::{clm_file_from_json, encoding_from_path, fragment_under};
use crate::session::{Captured, Session};
use crate::{
    Attachments, Editor, EditorError, Payload, join_key, key_stem, parent_key, review, versions,
};

pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Editor {
    pub(super) fn alloc_id(&self) -> SessionId {
        SessionId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// A view of a version is refused once the session it is of has left
    /// the registry: its close looks for views only after that, so it would
    /// never find this one.
    pub(super) fn insert_session(
        &self,
        id: SessionId,
        session: Session,
    ) -> Result<(), EditorError> {
        {
            let mut sessions = lock(&self.sessions);
            if let Some(viewing) = session.viewing
                && !sessions.contains_key(&viewing.session)
            {
                return Err(EditorError::NoSession(viewing.session));
            }
            sessions.insert(id, Arc::new(Mutex::new(session)));
        }
        self.notify_sessions();
        Ok(())
    }

    pub(super) fn create_session(
        &self,
        name: Option<String>,
        source: Option<SessionSource>,
        attachments: &mut Attachments,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        if let Some(name) = &name {
            validate_title(name)?;
        }
        check_source_attachments(source.as_ref(), attachments)?;
        let (model, derived_name) = match source {
            None => (Model::new(), None),
            Some(SessionSource::Clm {}) => {
                let bytes = required_attachment(attachments, "model")?;
                let model = Model::from_clm_bytes(&bytes)?;
                let name = model_title(&model);
                (model, name)
            }
            Some(SessionSource::Json { textures }) => {
                let structure = required_attachment(attachments, "structure")?;
                let images = attachments.take_family("texture");
                let file = clm_file_from_json(&structure, &textures, images)?;
                let model = Model::from_clm_file(&file)?;
                let name = model_title(&model);
                (model, name)
            }
            Some(SessionSource::Manifest {}) => {
                let bytes = required_attachment(attachments, "manifest")?;
                let text = std::str::from_utf8(&bytes).map_err(|error| {
                    EditorError::BadRequest(format!("manifest is not UTF-8: {error}"))
                })?;
                let mut budget = LoadBudget::default();
                let manifest = Manifest::from_json_with_budget(text, &mut budget)?;
                let images: HashMap<String, Arc<[u8]>> = attachments
                    .take_family("texture")
                    .into_iter()
                    .map(|(reference, bytes)| (reference, bytes.into()))
                    .collect();
                let references: HashSet<&str> = manifest
                    .textures
                    .iter()
                    .map(|texture| texture.path.as_str())
                    .collect();
                if let Some(unused) = images
                    .keys()
                    .filter(|key| !references.contains(key.as_str()))
                    .min()
                {
                    return Err(EditorError::BadRequest(format!(
                        "unused attachment texture:{unused}"
                    )));
                }
                let mut data = HashMap::new();
                for texture in &manifest.textures {
                    let bytes = images.get(&texture.path).ok_or_else(|| {
                        EditorError::BadRequest(format!(
                            "manifest needs attachment texture:{}",
                            texture.path
                        ))
                    })?;
                    if data
                        .insert(
                            texture.id.clone(),
                            TextureData {
                                encoding: encoding_from_path(&texture.path),
                                bytes: bytes.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err(EditorError::BadRequest(format!(
                            "manifest repeats texture id {:?}",
                            texture.id
                        )));
                    }
                }
                let model = Model::from_manifest_with_budget(&manifest, &data, &mut budget)?;
                let name = (!manifest.name.is_empty()).then_some(manifest.name);
                (model, name)
            }
        };
        self.publish_session(model, name.or(derived_name), None, None)
    }

    pub(super) fn open_session(&self, path: String) -> Result<Captured<ResponseBody>, EditorError> {
        let model = Model::from_clm_bytes(&self.storage.read(&path)?)?;
        let title = key_stem(&path);
        let review = review::ReviewKeeping::open(&*self.kept_store, &path, &model);
        let versions = versions::Versions::open(&*self.kept_store, &path);
        self.publish_session_with(model, Some(title), Some(path), None, |session| {
            (session.review, session.review_keeping) = review;
            session.versions = versions;
        })
    }

    /// The open session viewing exactly this version, if any.
    pub(super) fn viewing_session(&self, viewing: VersionViewing) -> Option<SessionId> {
        self.sessions_where(|s| s.viewing == Some(viewing))
            .into_iter()
            .next()
    }

    pub(super) fn views_of(&self, session: SessionId) -> Vec<SessionId> {
        self.sessions_where(|s| s.viewing.is_some_and(|v| v.session == session))
    }

    fn sessions_where(&self, wanted: impl Fn(&Session) -> bool) -> Vec<SessionId> {
        let handles: Vec<_> = lock(&self.sessions)
            .iter()
            .map(|(&id, session)| (id, session.clone()))
            .collect();
        let mut found: Vec<SessionId> = handles
            .into_iter()
            .filter(|(id, handle)| self.enter(*id, handle).is_ok_and(|s| wanted(&s)))
            .map(|(id, _)| id)
            .collect();
        found.sort_by_key(|id| id.0);
        found
    }

    pub(super) fn publish_viewing(
        &self,
        model: Model,
        title: String,
        viewing: VersionViewing,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        self.publish_session_with(model, Some(title), None, None, |session| {
            session.viewing = Some(viewing);
        })
    }

    pub(super) fn fork_session(
        &self,
        session: SessionId,
        if_rev: u64,
        name: Option<String>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        if let Some(name) = &name {
            validate_title(name)?;
        }
        let capture = self.with_session_captured(session, |source| {
            source.check_revision(Some(if_rev))?;
            Ok((source.model.clone(), source.title.clone()))
        })?;
        let (model, source_title) = capture.value;
        // Forks get a fresh model identity while sharing immutable payloads.
        let mut independent = Model::new();
        independent.replace_from(&model);
        self.publish_session(
            independent,
            name.or(Some(source_title)),
            None,
            Some(SessionOrigin {
                session,
                rev: if_rev,
            }),
        )
    }

    fn publish_session(
        &self,
        model: Model,
        name: Option<String>,
        file: Option<String>,
        source: Option<SessionOrigin>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        self.publish_session_with(model, name, file, source, |_| {})
    }

    fn publish_session_with(
        &self,
        model: Model,
        name: Option<String>,
        file: Option<String>,
        source: Option<SessionOrigin>,
        kept: impl FnOnce(&mut Session),
    ) -> Result<Captured<ResponseBody>, EditorError> {
        if model.is_fragment() {
            return Err(ModelError::Fragment.into());
        }
        if let Some(name) = &name {
            validate_title(name)?;
        }
        register_official_extensions()?;
        let session = self.alloc_id();
        let title = name.unwrap_or_else(|| format!("untitled-{}", session.0));
        let mut opened = Session::new(session, model, title, file);
        kept(&mut opened);
        self.insert_session(session, opened)?;
        let body = match source {
            Some(source) => ResponseBody::SessionFork { session, source },
            None => ResponseBody::Session { session },
        };
        Ok(Captured::at(body, 0))
    }

    pub(super) fn export_model(
        &self,
        session: SessionId,
        if_rev: u64,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let capture = self.with_session_captured(session, |source| {
            source.check_revision(Some(if_rev))?;
            Ok(source.model.clone())
        })?;
        let bytes = capture.value.to_clm_bytes()?;
        let body = ResponseBody::ModelExport {
            format: ModelFormat::Clm,
            byte_length: bytes.len() as u64,
            sha256: sha256(&bytes),
        };
        *payload = Some(Payload {
            content_type: "application/octet-stream",
            bytes,
        });
        Ok(Captured {
            value: body,
            rev: capture.rev,
        })
    }

    pub(super) fn import_structure(
        &self,
        session: SessionId,
        parent: NodeId,
        if_rev: u64,
        textures: Vec<ImportTexture>,
        attachments: &mut Attachments,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let structure = required_attachment(attachments, "structure")?;
        let images = attachments.take_family("texture");
        let file = clm_file_from_json(&structure, &textures, images)?;
        let incoming = fragment_under(file, &parent)?;
        self.edit_session_captured(session, |destination| {
            destination.check_revision(Some(if_rev))?;
            destination.model.install(&incoming)?;
            Ok(ResponseBody::Session { session })
        })
    }

    pub(super) fn list_sessions(&self) -> Result<Captured<ResponseBody>, EditorError> {
        let handles: Vec<_> = lock(&self.sessions)
            .iter()
            .map(|(&id, session)| (id, session.clone()))
            .collect();
        let mut sessions = Vec::with_capacity(handles.len());
        for (id, handle) in handles {
            let Ok(s) = self.enter(id, &handle) else {
                continue;
            };
            sessions.push(SessionInfo {
                session: id,
                title: s.title.clone(),
                file: s.file.clone(),
                dirty: s.dirty(),
                rev: s.rev,
                node_count: s.model.node_count() as u32,
                viewing: s.viewing,
            });
        }
        sessions.sort_by_key(|s| s.session.0);
        Ok(Captured::unscoped(ResponseBody::Sessions {
            sessions,
            instance: self.instance.clone(),
        }))
    }

    pub(super) fn close_session(
        &self,
        session: SessionId,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        // Never hold the registry lock while waiting for this handle.
        // Queued operations may already own an Arc; the closed marker
        // makes those handles fail after this publication wins the lock.
        let handle = self.session(session)?;
        {
            let mut state = self.enter(session, &handle)?;
            state.closed = true;
            lock(&self.sessions).remove(&session);
        }
        // A version open for viewing goes with the session it is of.
        for view in self.views_of(session) {
            // A view closed meanwhile is already gone.
            if let Ok(handle) = self.session(view)
                && let Ok(mut state) = self.enter(view, &handle)
            {
                state.closed = true;
                lock(&self.sessions).remove(&view);
            }
        }
        self.notify_sessions();
        Ok(Captured::unscoped(ResponseBody::Empty))
    }

    pub(super) fn save(
        &self,
        session: SessionId,
        path: Option<String>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let handle = self.session(session)?;
        let (key, rev, moved, review, versions) = {
            let mut s = self.enter(session, &handle)?;
            s.writable()?;
            let key = match path {
                Some(p) => p,
                None => s.file.clone().ok_or(EditorError::NoSavePath)?,
            };
            let bytes = s.model.to_clm_bytes()?;
            // Save publication includes its persistent write; failure
            // leaves session metadata untouched.
            self.storage.write(&key, &bytes)?;
            let from = s.file.replace(key.clone());
            s.saved_rev = s.rev;
            // The model is saved whatever becomes of its review: the
            // review is written second and its failure is reported
            // against a save that has fully taken effect.
            let moved = from.as_deref() != Some(key.as_str());
            let Session {
                review,
                review_keeping,
                ..
            } = &mut *s;
            let kept = if moved {
                review_keeping.rekey(&*self.kept_store, from.as_deref(), &key, review)
            } else {
                review_keeping.write(&*self.kept_store, Some(&key), review)
            };
            let versions = if moved {
                s.versions.rekey(&*self.kept_store, from.as_deref(), &key)
            } else {
                s.versions.persist(&*self.kept_store, Some(&key))
            };
            (key, s.rev, moved, kept, versions)
        };
        // A save moves no revision but flips `dirty`.
        self.notify_model_changed(session, rev);
        if moved {
            // What the review and versions say of how long they are
            // kept changed.
            self.notify(Event::ReviewChanged { session });
            self.notify(Event::VersionsChanged { session });
        }
        review.map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("saved the model to {key}, but not its review: {e}"),
            )
        })?;
        versions.map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("saved the model to {key}, but not its versions: {e}"),
            )
        })?;
        Ok(Captured::at(ResponseBody::Saved { path: key }, rev))
    }

    pub(super) fn export_manifest(
        &self,
        session: SessionId,
        path: String,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let handle = self.session(session)?;
        let (manifest, textures, rev) = {
            let s = self.enter(session, &handle)?;
            let manifest = s.model.to_manifest().to_json()?;
            let textures = s
                .model
                .texture_ids()
                .iter()
                .enumerate()
                .filter_map(|(i, tid)| {
                    let texture = s.model.texture(tid)?;
                    let ext = match texture.encoding {
                        CoreTextureEncoding::Tga => "tga",
                        CoreTextureEncoding::Png => "png",
                    };
                    Some((format!("tex{i}.{ext}"), texture.data.clone()))
                })
                .collect::<Vec<_>>();
            (manifest, textures, s.rev)
        };
        self.storage.write(&path, manifest.as_bytes())?;
        let base = parent_key(&path).to_string();
        for (name, data) in textures {
            self.storage.write(&join_key(&base, &name), &data)?;
        }
        Ok(Captured::at(ResponseBody::Saved { path }, rev))
    }
}

fn validate_title(title: &str) -> Result<(), EditorError> {
    Name::new(title)
        .map(|_| ())
        .map_err(|error| EditorError::BadRequest(format!("invalid session name: {error}")))
}

fn model_title(model: &Model) -> Option<String> {
    let title = &model.node(model.root()?)?.name;
    (!title.is_empty()).then(|| title.to_string())
}

fn required_attachment(attachments: &mut Attachments, name: &str) -> Result<Vec<u8>, EditorError> {
    attachments
        .take(name)
        .ok_or_else(|| EditorError::BadRequest(format!("missing attachment {name}")))
}

fn check_source_attachments(
    source: Option<&SessionSource>,
    attachments: &Attachments,
) -> Result<(), EditorError> {
    let (required, textures) = match source {
        None => (None, false),
        Some(SessionSource::Clm {}) => (Some("model"), false),
        Some(SessionSource::Json { .. }) => (Some("structure"), true),
        Some(SessionSource::Manifest {}) => (Some("manifest"), true),
    };
    if let Some(required) = required
        && !attachments.0.contains_key(required)
    {
        return Err(EditorError::BadRequest(format!(
            "missing attachment {required}"
        )));
    }
    for name in attachments.names() {
        if Some(name) != required && !(textures && name.starts_with("texture:")) {
            return Err(EditorError::BadRequest(format!(
                "this session source does not use attachment {name}"
            )));
        }
    }
    Ok(())
}

/// A session installs addons and checks its model, which merge and check the
/// official extensions through the process-wide registry; registering them
/// once serves every session.
fn register_official_extensions() -> Result<(), EditorError> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            catchlight_core::extension::register_official::<Animations>()
                .and_then(|()| catchlight_core::extension::register_official::<Humanoid>())
                .and_then(|()| catchlight_core::extension::register_official::<Expressions>())
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(EditorError::BadRequest)
}
