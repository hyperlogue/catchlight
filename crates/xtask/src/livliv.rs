//! Workspace entry point for the LivLiv adapter; conversion lives in importer.
use anyhow::{bail, Context, Result};
use catchlight_importer::livliv::{Options, Package};
use std::io::Read;
use std::path::PathBuf;

pub fn run(args: &[String]) -> Result<()> {
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
    let input = input.context("usage: cargo xtask import-livliv <directory|package.livliv> [--inspect] [--allow-lossy] [-o out.clm] [--report report.json]")?;
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
    let json = serde_json::to_string_pretty(&report)?;
    eprintln!("{json}");
    if let Some(path) = report_path {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        std::io::Write::write_all(&mut file, format!("{json}\n").as_bytes())?;
    }
    if inspect {
        eprintln!(
            "validated {} ({} layers); conversion blockers: {}",
            package.name(),
            package.layer_count(),
            report.blockers.len()
        );
        return Ok(());
    }
    let converted = package.convert(options)?;
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
