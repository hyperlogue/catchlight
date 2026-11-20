# catchlight-cli

Command-line tools for `.clm` model files: inspect, patch, replace textures,
extract or merge addons, compare models, and render images. File editing
preserves encoded texture bytes without decoding them.

This tool operates on model files directly; use `catchlight-editor-cli` to
control a running editor session.

```sh
cargo install catchlight-cli
catchlight-cli --help
```

[API documentation](https://docs.rs/catchlight-cli) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
