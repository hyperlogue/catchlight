//! Catppuccin Mocha visuals, vendored from <https://github.com/catppuccin/egui>
//! (MIT, copyright (c) 2022 Catppuccin): its crates.io release tracks egui <= 0.33
//! while we're on 0.34, and the theme reduces to this palette-to-Visuals mapping.

use eframe::egui::{self, epaint, style, Color32, Stroke};

// The Mocha palette entries the mapping below consumes.
const ROSEWATER: Color32 = Color32::from_rgb(245, 224, 220);
const PEACH: Color32 = Color32::from_rgb(250, 179, 135);
const MAROON: Color32 = Color32::from_rgb(235, 160, 172);
const BLUE: Color32 = Color32::from_rgb(137, 180, 250);
const TEXT: Color32 = Color32::from_rgb(205, 214, 244);
const OVERLAY1: Color32 = Color32::from_rgb(127, 132, 156);
const SURFACE2: Color32 = Color32::from_rgb(88, 91, 112);
const SURFACE1: Color32 = Color32::from_rgb(69, 71, 90);
const SURFACE0: Color32 = Color32::from_rgb(49, 50, 68);
const BASE: Color32 = Color32::from_rgb(30, 30, 46);
const MANTLE: Color32 = Color32::from_rgb(24, 24, 37);
const CRUST: Color32 = Color32::from_rgb(17, 17, 27);

pub(crate) fn install(ctx: &egui::Context) {
    // Mocha is dark-only; pin the preference so an OS light theme doesn't
    // swap the editor back to stock egui light. set_visuals writes to the
    // *current* theme's style, so the pin must come first.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals(mocha(egui::Visuals::dark()));
}

fn widget_visuals(old: style::WidgetVisuals, bg_fill: Color32) -> style::WidgetVisuals {
    style::WidgetVisuals {
        bg_fill,
        weak_bg_fill: bg_fill,
        bg_stroke: Stroke {
            color: OVERLAY1,
            ..old.bg_stroke
        },
        fg_stroke: Stroke {
            color: TEXT,
            ..old.fg_stroke
        },
        ..old
    }
}

fn mocha(old: egui::Visuals) -> egui::Visuals {
    let shadow_color = Color32::from_black_alpha(96);
    egui::Visuals {
        hyperlink_color: ROSEWATER,
        faint_bg_color: SURFACE0,
        extreme_bg_color: CRUST,
        code_bg_color: MANTLE,
        warn_fg_color: PEACH,
        error_fg_color: MAROON,
        window_fill: BASE,
        panel_fill: BASE,
        window_stroke: Stroke {
            color: OVERLAY1,
            ..old.window_stroke
        },
        widgets: style::Widgets {
            noninteractive: widget_visuals(old.widgets.noninteractive, BASE),
            inactive: widget_visuals(old.widgets.inactive, SURFACE0),
            hovered: widget_visuals(old.widgets.hovered, SURFACE2),
            active: widget_visuals(old.widgets.active, SURFACE1),
            open: widget_visuals(old.widgets.open, SURFACE0),
        },
        selection: style::Selection {
            bg_fill: BLUE.linear_multiply(0.2),
            stroke: Stroke {
                color: TEXT,
                ..old.selection.stroke
            },
        },
        window_shadow: epaint::Shadow {
            color: shadow_color,
            ..old.window_shadow
        },
        popup_shadow: epaint::Shadow {
            color: shadow_color,
            ..old.popup_shadow
        },
        dark_mode: true,
        ..old
    }
}
