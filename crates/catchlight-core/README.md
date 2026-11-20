# catchlight-core

Model loading and animation evaluation for Catchlight, a 2.5D character
animation runtime based on mesh deformation. Load `.clm` files, apply a `Pose`,
and advance a `Puppet` against its `Model`. This crate has no GPU backend.

For a convenient runtime entry point with optional rendering integrations, use
[`catchlight`](https://docs.rs/catchlight).

```sh
cargo add catchlight-core
```

[API documentation](https://docs.rs/catchlight-core) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
