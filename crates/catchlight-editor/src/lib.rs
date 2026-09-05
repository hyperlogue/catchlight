//! `catchlight-editor` — the desktop puppet editor GUI (egui).
//!
//! It embeds the editor server in-process and exposes its Unix socket, so a
//! CLI / agent can co-drive the same sessions. Model bytes reach it through
//! the OS file dialogs and the server's own storage keys.
//!
//! **Desktop only.** The browser editor is `catchlight-editor-wasm` plus the
//! TypeScript packages: it holds a replica of the model and talks the same
//! protocol, rather than compiling this egui app a second time.

mod app;
mod camera;
mod gizmo;
mod inspector;
mod io;
mod mesh_edit;
mod params_panel;
mod picking;
mod snapshot;
mod theme;
mod tree_panel;
mod viewport;

pub use app::App;
