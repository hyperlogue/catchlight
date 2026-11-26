//! The catchlight editor server.

#[cfg(not(target_arch = "wasm32"))]
mod atomic;
#[cfg(not(target_arch = "wasm32"))]
pub mod connection;
mod edit;
mod editor;
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod http;
mod kept_store;
mod lifecycle;
mod limits;
mod official;
mod ops;
#[cfg(not(target_arch = "wasm32"))]
mod preview;
mod query;
mod review;
mod session;
mod storage;
#[cfg(unix)]
mod transport;
mod versions;

#[cfg(unix)]
pub use catchlight_editor_discovery::default_socket_path;
pub use editor::{Attachments, Editor, Observer, Payload, Subscription};
pub use error::EditorError;
#[cfg(not(target_arch = "wasm32"))]
pub use http::{HttpOptions, HttpServer, UiFiles, bind_http, serve_http};
pub use kept_store::{KeptStore, NoKeptStore};
#[cfg(not(target_arch = "wasm32"))]
pub use kept_store::{SidecarKeptStore, UnusedKept, UserKeptStore, user_kept_dir};
#[cfg(not(target_arch = "wasm32"))]
pub use preview::PreviewError;
pub use query::{extension_value_info, replica_query, replica_reply};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::FileStorage;
pub use storage::{NoStorage, Storage, join_key, key_stem, parent_key};
#[cfg(unix)]
pub use transport::serve_unix;
