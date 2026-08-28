use anyhow::{anyhow, Result};
use visual_tests::{generate_configs, list_configs, update_all, SharedHarness};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = args
        .next()
        .ok_or_else(|| anyhow!("usage: visual-tests <update|list> [--filter SUBSTR]"))?;

    let mut filter: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--filter" => {
                filter = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--filter needs a value"))?,
                );
            }
            other => return Err(anyhow!("unknown arg '{other}'")),
        }
    }

    let configs = generate_configs();
    let configs: Vec<_> = if let Some(f) = filter {
        configs
            .into_iter()
            .filter(|c| c.name.contains(&f))
            .collect()
    } else {
        configs
    };

    match cmd.as_str() {
        "update" => {
            let harness = SharedHarness::new()?;
            println!("[visual-tests] update: {} configs", configs.len());
            update_all(&harness, &configs)?;
            println!("[visual-tests] update complete");
            Ok(())
        }
        "list" => {
            list_configs(&configs);
            Ok(())
        }
        other => Err(anyhow!(
            "unknown subcommand '{other}'; expected 'update' or 'list'"
        )),
    }
}
