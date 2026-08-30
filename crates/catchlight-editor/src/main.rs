//! Native entry: embed the editor server in-process — the GUI calls
//! `Editor::handle` directly — and start the Unix socket on a background
//! thread, so a CLI / agent can attach to and co-drive the open puppet.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    use std::sync::Arc;
    use std::thread;

    use catchlight_editor::App;
    use catchlight_editor_server::{default_socket_path, serve_unix, Editor};

    let editor = Arc::new(Editor::new());

    // Optional CLI argument: a .clm to open at startup.
    let initial = std::env::args().nth(1).and_then(|path| {
        use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody};
        let title = catchlight_editor_server::file_stem(std::path::Path::new(&path));
        match editor.handle(Request {
            id: 0,
            command: Command::SessionOpen { path },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => Some((session, title)),
            Reply::Err { message, .. } => {
                eprintln!("open: {message}");
                None
            }
            _ => None,
        }
    });

    // Expose the socket so a CLI / agent can co-drive the same sessions.
    {
        let editor = editor.clone();
        thread::spawn(move || {
            let _ = serve_unix(editor, &default_socket_path());
        });
    }

    let native_options = eframe::NativeOptions {
        // The viewport shares this backend's wgpu device with catchlight.
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "catchlight-editor",
        native_options,
        Box::new(move |cc| {
            let app = match initial {
                Some((session, title)) => {
                    App::with_session(editor, cc.egui_ctx.clone(), session, title)
                }
                None => App::new(editor, cc.egui_ctx.clone()),
            };
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
