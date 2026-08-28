//! Viewport camera: an orthographic window over the puppet's Y-up world.
//! `height` is the full world height visible; screen space is egui's Y-down,
//! so every mapping here flips Y once.

use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EditorCamera {
    pub center: glam::Vec2,
    pub height: f32,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            center: glam::Vec2::ZERO,
            height: 2000.0,
        }
    }
}

impl EditorCamera {
    /// Pixels per world unit inside `rect`.
    pub(crate) fn scale(&self, rect: egui::Rect) -> f32 {
        rect.height() / self.height
    }

    pub(crate) fn world_to_screen(&self, rect: egui::Rect, world: glam::Vec2) -> egui::Pos2 {
        let s = self.scale(rect);
        let d = world - self.center;
        rect.center() + egui::vec2(d.x * s, -d.y * s)
    }

    pub(crate) fn screen_to_world(&self, rect: egui::Rect, pos: egui::Pos2) -> glam::Vec2 {
        let s = self.scale(rect);
        let d = pos - rect.center();
        self.center + glam::vec2(d.x / s, -d.y / s)
    }

    /// Zoom by `factor` (>1 zooms in), keeping the world point under `pos`
    /// fixed on screen.
    pub(crate) fn zoom_around(&mut self, rect: egui::Rect, pos: egui::Pos2, factor: f32) {
        let anchor = self.screen_to_world(rect, pos);
        self.height = (self.height / factor).clamp(1.0, 1_000_000.0);
        let moved = self.screen_to_world(rect, pos);
        self.center += anchor - moved;
    }

    /// Pan by a screen-space delta (drag).
    pub(crate) fn pan(&mut self, rect: egui::Rect, delta: egui::Vec2) {
        let s = self.scale(rect);
        self.center -= glam::vec2(delta.x / s, -delta.y / s);
    }

    /// Frame a world-space AABB with some margin.
    pub(crate) fn focus(&mut self, rect: egui::Rect, min: glam::Vec2, max: glam::Vec2) {
        let size = max - min;
        self.center = (min + max) * 0.5;
        let aspect = (rect.width() / rect.height()).max(0.01);
        let need_h = (size.y.max(size.x / aspect)).max(1.0);
        self.height = need_h * 1.2;
    }
}
