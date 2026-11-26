# catchlight-editor-discovery

Where a local client finds a running Catchlight editor: the default Unix
socket path and the connection file that records the editor's HTTP address
and bearer token.

```sh
cargo add catchlight-editor-discovery
```

```rust
if let Some(path) = catchlight_editor_discovery::default_connection_path() {
    println!("a running editor writes its address to {}", path.display());
}
```

[API documentation](https://docs.rs/catchlight-editor-discovery) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
