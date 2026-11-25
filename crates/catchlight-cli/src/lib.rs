//! `catchlight-cli` — the command line over a `.clm` model file.
//!
//! - This crate never depends on `catchlight-editor-protocol`,
//!   `catchlight-editor-server`, or their clients; an `xtask` test enforces it.
//! - File operations never decode an image; texture bytes move verbatim.
//! - Re-encoding an unchanged structure is byte-identical.
//! - Writes are atomic, and `patch` rebuilds the Model before writing so a
//!   patch that breaks a load-time invariant is refused, not saved.

pub mod diff;
pub mod extension;
pub mod file;
pub mod fragment;
pub mod humanoid;
pub mod patch;
pub mod poses;
pub mod render;
pub mod storage;
pub mod texture;

use std::path::PathBuf;

use catchlight_core::formats::clm::ClmError;
use catchlight_core::id::IdError;
use catchlight_core::{InstallError, ModelError};

/// `diff` uses 1 for "the two files differ", so every error is 2.
pub const EXIT_ERROR: i32 = 2;
pub const EXIT_DIFFERS: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage: {0}")]
    Storage(String),
    #[error("poses budget {budget}: requested {requested}, maximum {limit}; reduce key positions or model complexity")]
    PosesLimit {
        budget: &'static str,
        limit: u64,
        requested: u64,
    },
    #[error("render: {0}")]
    Render(String),
    #[error("render request {request}: {source}")]
    RenderRequest {
        request: String,
        #[source]
        source: Box<Error>,
    },
    #[error("render budget {budget}: requested {requested}, maximum {limit}; {hint}")]
    RenderLimit {
        budget: &'static str,
        limit: u64,
        requested: u64,
        hint: &'static str,
    },
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{} is a Git LFS pointer, not the file it names; fetch it with `git lfs pull`",
        .path.display()
    )]
    GitLfsPointer { path: PathBuf },
    #[error("{} is not a .clm file: {source}", .path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: ClmError,
    },
    #[error("{} could not be encoded: {source}", .path.display())]
    Encode {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    #[error("{} is a complete model that does not load: {source}", .path.display())]
    NotAModel {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    #[error("{} is an addon fragment that does not load: {source}", .path.display())]
    NotAFragment {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    #[error("{} has no nodes at all, so it is neither a model nor an addon", .path.display())]
    Empty { path: PathBuf },
    #[error("{value:?} is not a valid id: {source}")]
    BadId {
        value: String,
        #[source]
        source: IdError,
    },
    #[error("{} carries no {kind} with the id {id:?}", .path.display())]
    NoSuchId {
        path: PathBuf,
        kind: &'static str,
        id: String,
    },
    #[error(
        "{} carries both a node and a param with the id {id:?}; say which with --node or --param",
        .path.display()
    )]
    AmbiguousId { path: PathBuf, id: String },
    #[error("{owner} has no field {field:?}; it has: {}", .known.join(", "))]
    NoSuchField {
        owner: String,
        field: String,
        known: Vec<&'static str>,
    },
    #[error("{field} takes {expected}, not {value:?}")]
    BadValue {
        field: String,
        expected: String,
        value: String,
    },
    #[error("that patch would make {} unloadable: {source}", .path.display())]
    PatchBreaksFile {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    #[error("{} did not load before the patch either: {source}", .path.display())]
    AlreadyBroken {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    #[error(
        "{} is not an image this format stores: its bytes carry no PNG or TGA signature and its \
         extension is not .png or .tga",
        .path.display()
    )]
    UnknownImageEncoding { path: PathBuf },
    #[error(
        "node {node:?} has no parent, so cutting it out yields a complete model rather than an \
         addon; extract its children instead"
    )]
    ExtractingARoot { node: String },
    #[error("{0}")]
    Install(#[from] InstallError),
    #[error(
        "{} is not a .clm; convert it first with `catchlight-importer --help`",
        .path.display()
    )]
    NotAClm { path: PathBuf },
    #[error("{stage}: {message}")]
    Gpu {
        stage: &'static str,
        message: String,
    },
    #[error("{} could not be written as a png: {source}", .path.display())]
    Png {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("{} could not be encoded as cbor: {source}", .path.display())]
    Cbor {
        path: PathBuf,
        #[source]
        source: ciborium::ser::Error<std::io::Error>,
    },
    #[error(
        "extension {key:?} is a byte value; say where to write it with --out rather than printing \
         bytes to a terminal"
    )]
    BytesNeedAFile { key: String },
    #[error(
        "extension key {key:?} starts with {prefix:?}, which is catchlight's own; a vendor key \
         names its vendor first"
    )]
    ReservedExtension { key: String, prefix: &'static str },
    #[error(transparent)]
    Register(#[from] catchlight_core::extension::RegisterError),
    #[error("extension {key:?} cannot be read by this build: {why}")]
    Unread {
        key: String,
        why: catchlight_core::extension::Unread,
    },
    #[error(
        "{} holds a plain value under {key:?} where this build expects the extension's \
         maintained data, so it cannot be edited here",
        .path.display()
    )]
    NotMaintained { path: PathBuf, key: String },
    #[error(
        "{} maps no control {control:?}; `catchlight-cli humanoid list` shows its mappings",
        .path.display()
    )]
    NotMapped { path: PathBuf, control: String },
    #[error(
        "{name:?} is not a standard humanoid control{}; `catchlight-cli humanoid controls` \
         lists them, and --nonstandard keeps a name outside the list",
        .nearest.map(|n| format!(" (did you mean {n:?}?)")).unwrap_or_default()
    )]
    NonstandardControl {
        name: String,
        nearest: Option<&'static str>,
    },
    #[error("extension {key:?} would carry {size} bytes, over the {max}-byte cap")]
    ExtensionTooLarge {
        key: String,
        size: usize,
        max: usize,
    },
    /// Every in-memory edit names an already-checked node, so this is a bug.
    #[error("editing the model in memory failed: {0}")]
    Model(#[from] ModelError),
}

impl Error {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// wgpu errors are `Box<dyn Error>`, neither `Send` nor a thiserror `source`.
    fn gpu(stage: &'static str, source: impl std::fmt::Display) -> Self {
        Self::Gpu {
            stage,
            message: source.to_string(),
        }
    }
}
