//! catchlight's on-disk model format.
//!
//! [`container`] frames opaque sections and owns the version word; [`clm`]
//! gives them meaning (`Structure` CBOR + verbatim `Textures`) and holds the
//! value types a [`Model`](crate::Model) is made of. There is one format and
//! one reader: anything else a model can come from — an inochi2d `.inx`, a
//! manifest — is somebody else's crate, and produces a `.clm` file.
//!
//! # How the format changes
//!
//! A `.clm` outlives the build that wrote it, so each kind of change has one
//! way to be made, in order of preference:
//!
//! 1. **A new field.** CBOR maps are keyed by name: a field added with
//!    `#[serde(default)]` is read by old and new builds alike. No version
//!    bump. (And never `deny_unknown_fields`, which would end this.)
//! 2. **A new section.** The container numbers its sections, and a reader
//!    carries one it does not recognise through a load and a save untouched
//!    ([`clm::ClmUnknownSection`]). A section that an older build can ignore
//!    without misreading the rest of the file needs no version bump either;
//!    `Extensions` arrived this way.
//! 3. **A [`clm::FORMAT_VERSION`] bump**, for a change an older reader would
//!    *misread* rather than miss. The reader refuses every version but its
//!    own, so a bump strands every file already written — which is why **a
//!    change that bumps the version must ship, in the same release, a way
//!    to bring the previous version's files forward**: either a reader for
//!    the previous version beside the new one, or a `catchlight-cli` migrate
//!    subcommand that rewrites an old file as a new one. A bump with neither
//!    is not a finished change.
//!
//! The reverse direction is not promised: a build is not expected to open a
//! file written by a newer version, only to refuse it by number
//! ([`clm::ClmError::UnsupportedVersion`]) instead of guessing.

pub mod clm;
pub mod container;
