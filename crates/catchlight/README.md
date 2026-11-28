# Catchlight

**2.5D character animation through mesh deformation.** Catchlight simulates
three-dimensional movement by deforming two-dimensional parts.

[Browser demo and editor](https://hyperlogue.github.io/catchlight/) ·
[API documentation](https://docs.rs/catchlight) ·
[Source](https://github.com/hyperlogue/catchlight)

```toml
[dependencies]
catchlight = { version = "0.4", features = ["wgpu"] }
```

```rust,no_run
use catchlight::{load_model, ModelFormat, Pose, Puppet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("character.clm")?;
    let model = load_model(&bytes, ModelFormat::Clm)?;
    let mut puppet = Puppet::new(&model);

    puppet.apply_pose(&Pose::new());
    puppet.tick(&model, 1.0 / 60.0);
    Ok(())
}
```

The default feature evaluates models without a GPU; the `wgpu` feature adds
`catchlight::wgpu` for rendering.

Licensed under MIT OR Apache-2.0.
