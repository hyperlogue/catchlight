fn main() -> anyhow::Result<()> {
    catchlight_editor_cli::run(
        "catchlight-editor-cli",
        catchlight_editor_cli::Discovery::SocketFirst,
        std::env::args_os(),
    )
}
