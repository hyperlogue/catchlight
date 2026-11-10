//! Standalone importer entry point; built only with the `cli` feature.
//! Conversion lives in the library; this target owns arguments and file IO.
use anyhow::{anyhow, bail, Context, Result};
use catchlight_importer::livliv::{Options, Package};
use std::io::Read;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_usage();
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("inx") => inx(&args[1..]),
        Some("livliv") => livliv(&args[1..]),
        _ => {
            print_usage();
            bail!("choose an import format: inx or livliv");
        }
    }
}

fn print_usage() {
    println!("usage: catchlight-importer <format> <input> [options]");
    println!("  inx <model.inx|model.inp> [-o model.clm]");
    println!("  livliv <directory|package.livliv> [--inspect] [--allow-lossy]");
    println!("      [-o model.clm] [--report report.json]");
    println!("Default output: input path with a .clm extension. Existing outputs are refused.");
    println!(
        "--inspect validates LivLiv packages and reports conversion blockers without exporting."
    );
    println!("--allow-lossy acknowledges reported approximations; required watermarks still block export.");
}

fn inx(args: &[String]) -> Result<()> {
    use catchlight_importer::import_inx_bytes;

    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                output = Some(PathBuf::from(
                    args.get(i + 1).ok_or_else(|| anyhow!("-o needs a path"))?,
                ));
                i += 2;
            }
            other if other.starts_with('-') => bail!("unknown option: {other}"),
            other => {
                if input.is_some() {
                    bail!("unexpected argument: {other}");
                }
                input = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let input = input
        .ok_or_else(|| anyhow!("usage: catchlight-importer inx <model.inx> [-o <out.clm>]"))?;
    let output = output.unwrap_or_else(|| input.with_extension("clm"));

    let bytes = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    // `.inx` and `.inp` share one container; the extension is only a label.
    let imported =
        import_inx_bytes(&bytes).with_context(|| format!("importing {}", input.display()))?;
    let encoded = imported.to_clm_bytes().context("writing .clm")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("creating {}", output.display()))?;
    std::io::Write::write_all(&mut file, &encoded)
        .with_context(|| format!("writing {}", output.display()))?;

    eprintln!(
        "imported {} -> {} ({} nodes, {} textures, {:.2} MB)",
        input.display(),
        output.display(),
        imported.node_count(),
        imported.texture_ids().len(),
        encoded.len() as f64 / 1e6,
    );
    Ok(())
}

fn livliv(args: &[String]) -> Result<()> {
    let mut input = None;
    let mut output = None;
    let mut report_path = None;
    let mut options = Options::default();
    let mut inspect = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-lossy" => options.allow_lossy = true,
            "--inspect" => inspect = true,
            "-o" | "--out" => {
                output = Some(PathBuf::from(args.next().context("-o requires a path")?))
            }
            "--report" => {
                report_path = Some(PathBuf::from(
                    args.next().context("--report requires a path")?,
                ))
            }
            _ if arg.starts_with('-') => bail!("unknown option: {arg}"),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument: {arg}"),
        }
    }
    let input = input.context("usage: catchlight-importer livliv <directory|package.livliv> [--inspect] [--allow-lossy] [-o out.clm] [--report report.json]")?;
    let package = if input.is_dir() {
        Package::from_directory(&input)?
    } else {
        let mut bytes = Vec::new();
        std::fs::File::open(&input)?
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        Package::from_zip(&bytes)?
    }
    .validate()?;
    let report = package.report();
    if inspect {
        write_report(&report, report_path.as_deref())?;
        eprintln!(
            "validated {} ({} layers); conversion blockers: {}",
            package.name(),
            package.layer_count(),
            report.blockers.len()
        );
        return Ok(());
    }
    let converted = match package.convert(options) {
        Ok(converted) => converted,
        Err(error) => {
            write_report(&report, report_path.as_deref())?;
            return Err(error.into());
        }
    };
    // Successful reports include measured baking error and table counts.
    write_report(&converted.report, report_path.as_deref())?;
    let output = output.unwrap_or_else(|| input.with_extension("clm"));
    // A failed conversion never creates an output; successful writes also
    // refuse to replace an existing file or the source package.
    let bytes = converted.model.to_clm_bytes()?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    eprintln!("wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}

fn write_report(
    report: &catchlight_importer::livliv::Report,
    path: Option<&std::path::Path>,
) -> Result<()> {
    let json = serde_json::to_string_pretty(report)?;
    eprintln!("{json}");
    if let Some(path) = path {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        std::io::Write::write_all(&mut file, format!("{json}\n").as_bytes())?;
    }
    Ok(())
}
