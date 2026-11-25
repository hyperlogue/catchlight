# catchlight-extension-humanoid

The official `catchlight.humanoid` extension for Catchlight: the standard
humanoid controls (head, body, eyes, brows, mouth), each mapped onto one of a
model's params, so one source of tracking can pose any model that maps them.
`controls_from_arkit` turns ARKit face blend shapes into control values.

```sh
cargo add catchlight-extension-humanoid
```

```rust,no_run
use catchlight_core::{load_model, ModelFormat, Puppet};
use catchlight_extension_humanoid::{ControlValues, Humanoid};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let model = load_model(&std::fs::read("character.clm")?, ModelFormat::Clm)?;
let mut puppet = Puppet::new(&model);
let mut controls = ControlValues::new();
controls.set("head_yaw", 0.3);
controls.set("mouth_open", 0.8);
model.extension::<Humanoid>()?.pose(&controls, &mut puppet);
puppet.tick(&model, 1.0 / 60.0);
# Ok(())
# }
```

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
