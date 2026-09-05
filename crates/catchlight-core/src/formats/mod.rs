//! catchlight's on-disk model format.
//!
//! [`container`] frames opaque sections and owns the version word; [`clm`]
//! gives them meaning (`Structure` CBOR + verbatim `Textures`) and holds the
//! value types a [`Model`](crate::Model) is made of. There is one format and
//! one reader: anything else a model can come from — an inochi2d `.inx`, a
//! manifest — is somebody else's crate, and produces a `.clm` file.

pub mod clm;
pub mod container;
