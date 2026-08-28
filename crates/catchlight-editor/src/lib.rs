//! `catchlight-editor` — the puppet editor GUI, one codebase for desktop and web.
//!
//! Native: embeds the editor server in-process and exposes its Unix socket so a
//! CLI / agent can co-drive the same sessions. Web: the same egui app compiled
//! to wasm (eframe `WebRunner`), with the session engine running in the page.
//! Document bytes cross the boundary only through [`io`].

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
#[cfg(target_arch = "wasm32")]
mod web;

pub use app::App;
