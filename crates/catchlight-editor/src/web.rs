//! Browser entry: mount the app on a canvas via eframe's `WebRunner`. The
//! session engine (`Editor`) lives in the page; a staged `reference.clm` next to the
//! page is fetched as the initial document when present.

use std::sync::Arc;

use catchlight_editor_server::Editor;
use wasm_bindgen::prelude::*;

use crate::app::App;
use crate::io;

#[wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let editor = Arc::new(Editor::new());
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                let app = App::new(editor, cc.egui_ctx.clone());
                io::autosave_probe(app.io_queue());
                io::fetch_demo(app.io_queue(), "reference.clm");
                Ok(Box::new(app) as Box<dyn eframe::App>)
            }),
        )
        .await
}
