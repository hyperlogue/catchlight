fn main() -> anyhow::Result<std::process::ExitCode> {
    catchlight_editor_cli::run(
        "catchlight-editor-cli",
        catchlight_editor_cli::Discovery::SocketFirst,
        std::env::args_os(),
    )
}
