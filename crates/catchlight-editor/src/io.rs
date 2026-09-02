//! Where asynchronous IO results land.
//!
//! The GUI is synchronous: it calls `Editor::handle` on the frame it draws.
//! Anything that finishes later pushes an [`IoEvent`] here and the app drains
//! the queue once per frame, so no module below has to be async.
//!
//! Nothing produces one today — the desktop file dialogs block, and the
//! browser flows that did (picker, blob download, OPFS autosave) left with the
//! wasm build of this crate. The queue stays because it is the seam a
//! long-running desktop task would use, and the app already drains it.

use std::sync::{Arc, Mutex};

use eframe::egui;

pub enum IoEvent {
    Opened {
        title: String,
        bytes: Vec<u8>,
    },
    /// The staged demo model — only adopted when nothing else is open.
    DemoLoaded {
        title: String,
        bytes: Vec<u8>,
    },
    PickedTexture {
        bytes: Vec<u8>,
        is_tga: bool,
    },
    /// A previous session's autosave exists — offer to restore it.
    AutosaveFound {
        bytes: Vec<u8>,
    },
    Status(String),
    Error(String),
}

/// Completed async IO lands here; the app drains it once per frame. Pushing
/// requests a repaint so results are picked up without polling.
pub struct IoQueue {
    events: Mutex<Vec<IoEvent>>,
    ctx: egui::Context,
}

impl IoQueue {
    pub fn new(ctx: egui::Context) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            ctx,
        })
    }

    pub fn push(&self, event: IoEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
        self.ctx.request_repaint();
    }

    pub fn drain(&self) -> Vec<IoEvent> {
        match self.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(_) => Vec::new(),
        }
    }
}
