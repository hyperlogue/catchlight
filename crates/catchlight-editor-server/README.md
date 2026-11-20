# catchlight-editor-server

The Catchlight editor backend: model sessions, authoring commands, history,
and native headless previews. Embed `Editor` in an application or run the
server for editor clients.

The session core also supports in-browser wasm execution. The Unix-socket
server is Linux development tooling; `catchlight-editor-cli` connects to it.

```sh
cargo add catchlight-editor-server
```

[API documentation](https://docs.rs/catchlight-editor-server) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
