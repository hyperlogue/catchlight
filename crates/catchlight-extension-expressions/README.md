# catchlight-extension-expressions

The official `catchlight.expressions` extension for Catchlight: a model's
named expressions, such as the standard emotions and visemes. Each sets
humanoid controls or the model's own params, and plays blended by weight.

```sh
cargo add catchlight-extension-expressions
```

```rust,no_run
use catchlight_core::{load_model, ModelFormat, Puppet};
use catchlight_extension_expressions::{ExpressionWeights, Expressions};
use catchlight_extension_humanoid::{ControlValues, Humanoid};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let model = load_model(&std::fs::read("character.clm")?, ModelFormat::Clm)?;
let mut puppet = Puppet::new(&model);
let expressions = model.extension::<Expressions>()?;
let weights: ExpressionWeights = [("happy", 0.7), ("aa", 1.0)].into_iter().collect();
let mut controls = ControlValues::new();
expressions.blend_controls(&weights, &mut controls);
model.extension::<Humanoid>()?.pose(&controls, &mut puppet);
expressions.blend_params(&weights, &mut puppet);
puppet.tick(&model, 1.0 / 60.0);
# Ok(())
# }
```

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
