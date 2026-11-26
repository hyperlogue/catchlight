//! A session's review threads. Kept beside the model, never in it: no
//! revision, no history entry, nothing in the model file. A session with a
//! file writes each change through to the editor's
//! [`KeptStore`](crate::KeptStore).

mod commands;
mod document;
mod images;
mod state;
#[cfg(test)]
mod tests;

pub(crate) use document::ReviewKeeping;
pub(crate) use state::{ReviewState, author_of};
