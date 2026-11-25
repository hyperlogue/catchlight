# catchlight-extension-animations

The official `catchlight.animations` extension for Catchlight: a model's
named animation clips, stored beside the model as typed extension data, and
an `AnimationPlayer` that plays them on a `Puppet`.

```sh
cargo add catchlight-extension-animations
```

```rust,no_run
use catchlight_core::{load_model, ModelFormat, Puppet};
use catchlight_extension_animations::{AnimationPlayer, Animations};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let model = load_model(&std::fs::read("character.clm")?, ModelFormat::Clm)?;
let mut puppet = Puppet::new(&model);
let mut player = AnimationPlayer::new(model.extension::<Animations>()?.clips());
player.play("Idle");
player.advance(&mut puppet, 1.0 / 60.0);
puppet.tick(&model, 1.0 / 60.0);
# Ok(())
# }
```

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
