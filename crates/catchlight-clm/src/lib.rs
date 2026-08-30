//! `catchlight-clm` — file-level operations on a `.clm` model file.
//!
//! Every operation here works on the file's **structure section**: decode the
//! container, edit the CBOR document, write it back. Texture bytes are moved
//! around as opaque blobs and are never decoded, so patching one field of a
//! model carrying a hundred megabytes of PNG costs a read and a write and
//! nothing else. That is the whole point of Ids being author-facing strings
//! stored in the file: a subtree can be cut out, a property set, or two files
//! compared without a renderer, a puppet, or an image decoder anywhere in the
//! process.
//!
//! # Why this is its own binary and not the editor CLI
//!
//! `catchlight-editor-cli` was the other candidate — it already has the
//! argument parsing and it is where agent tooling lives. It is the wrong home
//! for three reasons, and the third is the one that decides it:
//!
//! 1. It is a **client of a running server**. Its every command opens the
//!    editor's Unix socket and talks to a live session; nothing in it
//!    touches a file. These commands are the opposite — they never need a
//!    server, and a session's copy of a model is exactly the thing they must
//!    not silently disagree with.
//! 2. It is **Unix-only by construction** (`std::os::unix::net::UnixStream`,
//!    unconditionally). The runtime is tier-1 on Windows and `.clm` is the
//!    format everywhere, so file-level operations cannot live behind a
//!    `cfg(unix)` wall. This crate compiles anywhere `catchlight-core` does.
//! 3. `AGENTS.md`'s own decision draws the line: *"the editor-server Unix
//!    socket is permanent Linux-only dev/agent tooling, not a product
//!    surface"*. Putting a cross-platform, product-adjacent utility inside the
//!    client of that socket blurs the exact boundary that decision exists to
//!    keep sharp. It is also the seat AGENTS.md reserves for the one CLI the
//!    removed inspection examples
//!    are meant to come back as: they, too, are read-only questions about a
//!    `.clm` on disk.
//!
//! `xtask` was the third option and is ruled out by what it is: repo build
//! automation, `publish = false`, invoked as `cargo xtask`. These commands run
//! against a user's model, not against this checkout.
//!
//! # Invariants
//!
//! - **No image is ever decoded.** [`crate::file::read`] hands back
//!   [`ClmTexture`](catchlight_core::formats::clm::ClmTexture) values holding
//!   the file's verbatim source bytes; `patch`, `extract`, `merge`,
//!   `requirements` and `diff` only ever move or compare those bytes, and
//!   `replace-texture` writes new ones in without looking inside them. The
//!   proof is a test whose texture payload is not a decodable image at all
//!   (`patching_a_file_whose_textures_are_not_images`): anything that decoded
//!   would fail there.
//! - **Nothing changed means the same bytes.** Decoding and re-encoding a
//!   document is byte-identical (`ciborium` writes fields in declaration
//!   order, the container lays sections out in order), so setting a field to
//!   the value it already has rewrites the file unchanged. Callers rest on
//!   that: it is how `diff` on a round-tripped file is empty, and it is what
//!   makes "did this tool touch anything?" answerable with `cmp`.
//! - **A write is atomic and never leaves an unopenable file.** Output goes
//!   to a temporary file beside the destination and is renamed over it, and
//!   `patch` rebuilds a [`Model`](catchlight_core::Model) from the edited
//!   document *before* writing — so a patch that would break a load-time
//!   invariant is refused with that reader's own error rather than saved.
//!   Rebuilding a Model decodes no textures; it is the cheap half of a load.
//! - **A file is read as one shape or the other, never guessed.** A complete
//!   model has exactly one node with no parent; an addon fragment has none.
//!   [`file::load`] decides from the document itself and then uses the
//!   matching reader, which is the file-level form of the rule
//!   `crates/catchlight-core/src/model/file.rs` states. `merge` is the one
//!   command that does not choose: its addon argument is always read with the
//!   fragment reader, so handing it a complete model says so.
//! - **An Id is taken verbatim.** The charset (`[A-Za-z0-9_./-]`, no leading
//!   `.` or `/`) allows a leading `-`, which `catchlight_core::id` explicitly
//!   leaves to the CLI to handle: every command takes `--` to end its options,
//!   and `--help` says so.

pub mod diff;
pub mod file;
pub mod fragment;
pub mod patch;
pub mod texture;

use std::path::PathBuf;

use catchlight_core::formats::clm::ClmError;
use catchlight_core::id::IdError;
use catchlight_core::{InstallError, ModelError};

/// The exit status a failed command leaves. `diff` uses 1 for "the two files
/// differ", so every error is 2 and no command ever exits 1 for anything else.
pub const EXIT_ERROR: i32 = 2;
/// `diff`'s exit status when the two files are not identical.
pub const EXIT_DIFFERS: i32 = 1;

/// Why a file-level operation could not be carried out. Every variant names
/// the file, the Id or the field that stopped it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
    /// The document is a complete model on the wire (one node with no parent)
    /// but the complete-model reader refused it.
    #[error("{} is a complete model that does not load: {source}", .path.display())]
    NotAModel {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    /// The document is a fragment on the wire (no node without a parent) but
    /// the fragment reader refused it.
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
    /// The edited document no longer loads. The patch is not written.
    #[error("that patch would make {} unloadable: {source}", .path.display())]
    PatchBreaksFile {
        path: PathBuf,
        #[source]
        source: ModelError,
    },
    /// The file did not load before the patch either, so nothing can be said
    /// about the patch itself.
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
}

impl Error {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
