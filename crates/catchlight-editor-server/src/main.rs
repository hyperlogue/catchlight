//! Headless standalone server: create an [`Editor`] and listen on the canonical
//! socket. The GUI will instead embed the library and call `serve_unix` itself.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use std::sync::Arc;

    use catchlight_editor_server::{default_socket_path, serve_unix, Editor};

    let editor = Arc::new(Editor::new());
    let path = default_socket_path();
    eprintln!("catchlight-editor-server: listening on {}", path.display());
    serve_unix(editor, &path)
}

#[cfg(not(unix))]
fn main() {
    eprintln!("catchlight-editor-server: the socket server is unix-only");
}
