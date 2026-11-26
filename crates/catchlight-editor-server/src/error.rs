//! The editor's error and the wire code and limit each one answers with.

use catchlight_core::formats::clm;
use catchlight_core::{InstallError, LoadLimitError, ModelError, Required};
use catchlight_editor_core::{HistoryError, ManifestError, MeshError};
use catchlight_editor_protocol::*;

#[cfg(not(target_arch = "wasm32"))]
use crate::PreviewError;
use crate::official;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("operation {index}: {source}")]
    Operation { index: u32, source: Box<Self> },
    #[error("{resource} requires {requested}, exceeding the limit {limit}")]
    Limit {
        resource: &'static str,
        limit: u64,
        requested: u64,
    },
    #[error("The model changed during this edit. Reload the draft or start the gesture again.")]
    RevisionConflict,
    #[error("no session {}", .0.0)]
    NoSession(SessionId),
    #[error("session {} was closed: an edit failed partway through it; open its file again", .0.0)]
    SessionPoisoned(SessionId),
    #[error("no node {0}")]
    NoNode(NodeId),
    #[error("no param {0}")]
    NoParam(ParamId),
    #[error("no texture {0}")]
    NoTexture(TexId),
    #[error("{0}")]
    BadTarget(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("no weld pairs those two parts")]
    UnknownWeld,
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("revision {0} is not retained")]
    RevisionUnavailable(u64),
    #[error("the session publication revision is exhausted")]
    RevisionExhausted,
    #[error("session has no file; pass a path to save")]
    NoSavePath,
    #[error("node {0} is not a part")]
    NotAPart(NodeId),
    #[error("part {0} draws no texture")]
    NoAlbedo(NodeId),
    #[error("mesh: {0}")]
    Mesh(#[from] MeshError),
    #[error("edit: {0}")]
    Edit(#[from] ModelError),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(String),
    #[cfg(not(target_arch = "wasm32"))]
    #[error("preview: {0}")]
    Preview(#[from] PreviewError),
    #[error("command is native-only (path IO / headless preview)")]
    NativeOnly,
    #[error("install: {0}")]
    Install(#[from] InstallError),
    #[error("no extension {0:?}")]
    NoExtension(String),
    #[error("no review thread {}", .0.0)]
    NoThread(ThreadId),
    #[error("no comment {0} on that thread")]
    NoComment(u64),
    #[error("no review image {0}")]
    NoImage(u64),
    #[error("the fetched comments changed before they were acknowledged; fetch again")]
    BatchStale,
    #[error("thread {} is claimed by {} ({})", .thread.0, .holder.name, .holder.id)]
    Claimed {
        thread: ThreadId,
        holder: ReviewIdentity,
    },
    #[error("retry key {0:?} was already used for something else")]
    KeyConflict(String),
    #[error("no version {0}")]
    NoVersion(u64),
    #[error("this session shows version {} of session {} and takes no edits; restore the version in that session, or fork this one to edit it", .0.version, .0.session.0)]
    ReadOnly(VersionViewing),
    #[error("{0}")]
    Humanoid(catchlight_extension_humanoid::authoring::AuthoringError),
    #[error("{0}")]
    Expressions(catchlight_extension_expressions::authoring::AuthoringError),
    #[error("extension {key:?} cannot be read by this editor: {why}")]
    ExtensionUnread { key: &'static str, why: String },
}

impl EditorError {
    pub(crate) fn limit_info(&self) -> Option<LimitInfo> {
        match self {
            Self::Operation { source, .. } => source.limit_info(),
            Self::Limit {
                resource,
                limit,
                requested,
            } => Some(LimitInfo {
                resource: (*resource).into(),
                limit: *limit,
                requested: *requested,
            }),
            Self::Mesh(MeshError::Limit(limit))
            | Self::Manifest(ManifestError::LoadLimit(limit)) => Some(load_limit_info(limit)),
            Self::Edit(e) | Self::Manifest(ManifestError::Model(e)) => {
                model_load_limit(e).map(load_limit_info)
            }
            Self::Install(InstallError::Model(e)) => model_load_limit(e).map(load_limit_info),
            _ => None,
        }
    }

    pub fn code(&self) -> ErrorCode {
        if self.limit_info().is_some() {
            return ErrorCode::LimitExceeded;
        }
        match self {
            Self::Operation { source, .. } => source.code(),
            Self::Limit { .. } => ErrorCode::LimitExceeded,
            Self::RevisionConflict => ErrorCode::RevisionConflict,
            Self::NoSession(_) => ErrorCode::NoSession,
            Self::SessionPoisoned(_) => ErrorCode::SessionPoisoned,
            Self::NoNode(_) => ErrorCode::NoNode,
            Self::NoParam(_) => ErrorCode::NoParam,
            Self::NoTexture(_) => ErrorCode::NoTexture,
            Self::BadTarget(_) => ErrorCode::BadTarget,
            Self::BadRequest(_) => ErrorCode::BadRequest,
            Self::UnknownWeld => ErrorCode::UnknownWeld,
            // A node of the wrong kind is the same answer a bad binding
            // target gets: the command parsed and does not fit what it names.
            Self::NotAPart(_) => ErrorCode::BadTarget,
            Self::NoAlbedo(_) => ErrorCode::NoTexture,
            // Only the empty mask is worth branching on — a client answers it
            // by offering a lower threshold. The rest is an ordinary refusal.
            Self::Mesh(MeshError::NothingToMesh) => ErrorCode::NothingToMesh,
            Self::Mesh(_) => ErrorCode::Edit,
            Self::NothingToUndo => ErrorCode::NothingToUndo,
            Self::NothingToRedo => ErrorCode::NothingToRedo,
            Self::RevisionUnavailable(_) => ErrorCode::RevisionUnavailable,
            Self::RevisionExhausted => ErrorCode::RevisionExhausted,
            Self::NoSavePath => ErrorCode::NoSavePath,
            Self::Manifest(_) => ErrorCode::Manifest,
            Self::Io(_) => ErrorCode::Io,
            Self::Image(_) => ErrorCode::Image,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Preview(_) => ErrorCode::Preview,
            Self::NativeOnly => ErrorCode::NativeOnly,
            Self::NoExtension(_) => ErrorCode::NoExtension,
            Self::NoThread(_) => ErrorCode::NoThread,
            Self::NoComment(_) => ErrorCode::NoComment,
            Self::NoImage(_) => ErrorCode::NoImage,
            Self::BatchStale => ErrorCode::BatchStale,
            Self::Claimed { .. } => ErrorCode::ThreadClaimed,
            Self::KeyConflict(_) => ErrorCode::KeyConflict,
            Self::NoVersion(_) => ErrorCode::NoVersion,
            Self::ReadOnly(_) => ErrorCode::ReadOnly,
            Self::Humanoid(e) => official::humanoid_code(e),
            Self::Expressions(e) => official::expressions_code(e),
            Self::ExtensionUnread { .. } => ErrorCode::ExtensionUnread,
            // Install's refusals answer with the codes the equivalent edit gets.
            Self::Install(e) => match e {
                InstallError::Collision { .. } => ErrorCode::DuplicateId,
                InstallError::Missing { id, .. } => match id {
                    Required::Node(_) | Required::Drawable(_) | Required::Part(_) => {
                        ErrorCode::NoNode
                    }
                    Required::Param(_) => ErrorCode::NoParam,
                    Required::Slot(..) => ErrorCode::UnknownSlot,
                },
                InstallError::WeldUnknownSlot { .. } => ErrorCode::WeldUnknownSlot,
                InstallError::WeldSlotPairedTwice { .. } => ErrorCode::WeldSlotPairedTwice,
                InstallError::WeldEndNotAPart { .. } => ErrorCode::BadWeldEnd,
                InstallError::DuplicateWeld { .. } => ErrorCode::DuplicateWeld,
                InstallError::Model(e) => model_error_code(e),
                // `InstallError` is non-exhaustive.
                _ => ErrorCode::Edit,
            },
            Self::Edit(e) => model_error_code(e),
        }
    }
}

fn model_load_limit(e: &ModelError) -> Option<&LoadLimitError> {
    match e {
        ModelError::LoadLimit(limit) | ModelError::Clm(clm::ClmError::LoadLimit(limit)) => {
            Some(limit)
        }
        _ => None,
    }
}

fn load_limit_info(limit: &LoadLimitError) -> LimitInfo {
    LimitInfo {
        resource: limit.resource.name().into(),
        limit: limit.limit,
        requested: limit.got,
    }
}

fn model_error_code(e: &ModelError) -> ErrorCode {
    match e {
        ModelError::UnknownNode => ErrorCode::NoNode,
        ModelError::UnknownParam => ErrorCode::NoParam,
        ModelError::UnknownTexture => ErrorCode::NoTexture,
        ModelError::UnknownSlot => ErrorCode::UnknownSlot,
        ModelError::DuplicateId(_) => ErrorCode::DuplicateId,
        ModelError::DuplicateSlot(_) => ErrorCode::DuplicateSlot,
        ModelError::WeldUnknownSlot(_) => ErrorCode::WeldUnknownSlot,
        ModelError::WeldSlotPairedTwice(_) => ErrorCode::WeldSlotPairedTwice,
        ModelError::WeldSelfPaired | ModelError::WeldEndNotAPart => ErrorCode::BadWeldEnd,
        ModelError::DuplicateWeld => ErrorCode::DuplicateWeld,
        ModelError::WeldWeightOutOfRange => ErrorCode::WeldWeightOutOfRange,
        ModelError::UnknownWeld => ErrorCode::UnknownWeld,
        ModelError::Fragment => ErrorCode::Fragment,
        ModelError::UnknownExtension(_) => ErrorCode::NoExtension,
        ModelError::ReservedExtension(_) => ErrorCode::ReservedExtension,
        ModelError::NotSpine | ModelError::NotMotionResponse => ErrorCode::BadTarget,
        ModelError::SpineTargetArity { .. } | ModelError::SpineJoints(_) => ErrorCode::BadTarget,
        // The size cap has no code of its own: a client that hit it
        // has nothing to branch on, only a value to shrink.
        _ => ErrorCode::Edit,
    }
}

impl From<HistoryError> for EditorError {
    fn from(error: HistoryError) -> Self {
        match error {
            HistoryError::NothingToUndo => Self::NothingToUndo,
            HistoryError::NothingToRedo => Self::NothingToRedo,
            HistoryError::RevisionUnavailable(revision) => Self::RevisionUnavailable(revision),
            HistoryError::InvalidPublicationRevision { .. } => Self::RevisionConflict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_limit_inside_an_install_is_answered_as_a_limit() {
        let limit = LoadLimitError {
            resource: catchlight_core::LoadResource::Nodes,
            limit: 4,
            got: 5,
        };
        let error =
            EditorError::Install(InstallError::Model(Box::new(ModelError::LoadLimit(limit))));
        assert_eq!(error.code(), ErrorCode::LimitExceeded);
        let info = error.limit_info().unwrap();
        assert_eq!(info.resource, limit.resource.name());
        assert_eq!((info.limit, info.requested), (4, 5));
    }
}
