//! catchlight's on-disk model format.
//!
//! [`container`] frames opaque sections and owns the version word; [`clm`]
//! gives them meaning (`Structure` CBOR + verbatim `Textures`) and holds the
//! value types a [`Model`](crate::Model) is made of. [`legacy`] is the arena
//! document `cargo xtask`'s fixture generators still author, and the shape
//! `catchlight-import-inochi2d` still produces.

pub mod clm;
pub mod container;
pub mod legacy;
