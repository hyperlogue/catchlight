//! The server and the CLI each keep a copy of where an editor is found; under
//! the same environment they must name the same places.

#[cfg(unix)]
#[test]
fn the_cli_dials_the_socket_the_server_binds() {
    assert_eq!(
        catchlight_editor_cli::default_socket_path(),
        catchlight_editor_server::default_socket_path()
    );
}

#[test]
fn the_cli_reads_the_connection_file_the_server_writes() {
    assert_eq!(
        catchlight_editor_cli::default_connection_path(),
        catchlight_editor_server::connection::default_connection_path()
    );
}
