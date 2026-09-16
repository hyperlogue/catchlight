//! Finished-session publication and captured model transfer.
//!
//! Source validation and model construction finish privately before a session
//! joins the registry. Empty creation, source creation, store opening and fork
//! share one revision-zero initializer. Export captures under the source lock
//! and serializes outside it; installation mutates only under the destination
//! publication lock. Neither operation couples the CLI to editor state.

use std::collections::HashSet;

use catchlight_core::LoadLimits;
use sha2::{Digest, Sha256};

use super::*;

pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Editor {
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
        check_attachment_budget(attachments)?;
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
        self.publish_session(model, Some(title), Some(path), None)
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
        if model.is_fragment() {
            return Err(ModelError::Fragment.into());
        }
        if let Some(name) = &name {
            validate_title(name)?;
        }
        let session = self.alloc_id();
        let title = name.unwrap_or_else(|| format!("untitled-{}", session.0));
        self.insert_session(session, Session::new(session, model, title, file));
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
        check_attachment_budget(attachments)?;
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
    if let Some(required) = required {
        if !attachments.0.contains_key(required) {
            return Err(EditorError::BadRequest(format!(
                "missing attachment {required}"
            )));
        }
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

fn check_attachment_budget(attachments: &Attachments) -> Result<(), EditorError> {
    let requested = attachments.0.values().fold(0u64, |total, bytes| {
        total.saturating_add(bytes.len() as u64)
    });
    let limit = LoadLimits::default().encoded_bytes;
    if requested > limit {
        return Err(EditorError::Limit {
            resource: "encoded_bytes",
            limit,
            requested,
        });
    }
    Ok(())
}
