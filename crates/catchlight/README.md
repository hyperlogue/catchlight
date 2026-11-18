# Catchlight

**2.5D character animation through mesh deformation.** Catchlight simulates
three-dimensional movement by deforming two-dimensional parts. Embed the runtime
in your application, render with wgpu, or animate characters in Bevy.

[Try the browser demo and editor](https://hyperlogue.github.io/catchlight/) ·
[API documentation](https://docs.rs/catchlight) ·
[Source](https://github.com/hyperlogue/catchlight)

## Get started

```toml
[dependencies]
catchlight = "0.1.0"
```

Load a `.clm` model and evaluate a frame:

```rust,no_run
use catchlight::{load_model, ModelFormat, Pose, Puppet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("character.clm")?;
    let model = load_model(&bytes, ModelFormat::Clm)?;
    let mut puppet = Puppet::new(&model);

    let pose = Pose::new();
    puppet.apply_pose(&pose);
    puppet.tick(&model, 1.0 / 60.0);
    Ok(())
}
```

Keep the model and puppet between frames. Update the pose and tick the puppet
each frame, then pass it to your renderer. Evaluation alone does not draw pixels.
The runtime loads `.clm` files; external-format conversion belongs to the
separate importer tooling.

## Choose a renderer

| Feature | API | Purpose |
| --- | --- | --- |
| Default | `catchlight::*` | Core model, pose, and animation APIs; no GPU or engine dependency |
| `wgpu` | `catchlight::wgpu` | GPU rendering, including headless rendering |
| `bevy` | `catchlight::bevy` | Bevy assets, components, and plugin; also enables `wgpu` |

```toml
[dependencies]
catchlight = { version = "0.1.0", features = ["wgpu"] }
```

Use `catchlight::wgpu::{RenderContext, RenderCache}` for headless rendering;
see the [renderer walkthrough](https://docs.rs/catchlight-wgpu).
For Bevy, select `features = ["bevy"]` and use
`catchlight::bevy::CatchlightPlugin`; see the
[Bevy integration](https://docs.rs/catchlight-bevy).

## Part of the Catchlight stack

This crate re-exports the existing libraries without wrapping their types:
[catchlight-core](https://docs.rs/catchlight-core),
[catchlight-wgpu](https://docs.rs/catchlight-wgpu), and
[catchlight-bevy](https://docs.rs/catchlight-bevy).

Authoring and automation use the separate `catchlight-editor-core`,
`catchlight-editor-protocol`, `catchlight-editor-server`, and
`catchlight-editor-cli` crates. `catchlight-cli` operates on model files.
See the [project overview](https://github.com/hyperlogue/catchlight#readme)
for the web editor, Python client, and tooling.

Licensed under MIT OR Apache-2.0.
