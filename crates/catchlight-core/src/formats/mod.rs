//! catchlight's on-disk model format.
//!
//! Prefer a `#[serde(default)]` field (never `deny_unknown_fields`), then a
//! new section, over a [`clm::FORMAT_VERSION`] bump: the reader refuses every
//! version but its own, so a bump must ship, in the same release, a reader or
//! a `catchlight-cli` migration for the previous version's files.

pub mod clm;
pub mod container;
