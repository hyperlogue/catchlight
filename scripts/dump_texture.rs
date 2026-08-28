// This file has no build path (there is no `scripts/Cargo.toml`). Treat it as
// a snippet to paste into an example, not a runnable tool.

use catchlight_core::formats::InxModel;
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = InxModel::parse(BufReader::new(File::open("example_models/reference/reference.inx")?))?;

    // From the render output, we know:
    // - 'Mouth Inner' uses texture=11
    // - 'Lip' uses texture=10

    for (i, tex) in model.textures.iter().enumerate() {
        if i == 10 || i == 11 {
            let img = image::load_from_memory(&tex.data)?;
            img.save(format!("texture_{}.png", i))?;
            println!("Saved texture_{}.png ({}x{})", i, img.width(), img.height());
        }
    }

    Ok(())
}
