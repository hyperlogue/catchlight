# catchlight-wgpu

The wgpu rendering backend for Catchlight. Render an evaluated puppet to a
window or an offscreen image with `WgpuRenderer` or `RenderContext`.
`RenderCache` holds the model GPU resources shared by its puppets.

See the API documentation for a complete headless rendering example. This
backend is also available as `catchlight::wgpu` with the `wgpu` feature.

```sh
cargo add catchlight-wgpu
```

[API documentation](https://docs.rs/catchlight-wgpu) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
