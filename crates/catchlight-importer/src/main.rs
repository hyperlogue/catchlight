//! Standalone importer entry point; built only with the `cli` feature.
//! Conversion lives in the library; this target owns arguments and file IO.
//! Recognized container magic takes precedence over extensions. Extensions
//! only select a validator when no signature is recognized; they never bypass
//! that format's validation. ZIP inputs must still contain a LivLiv package.
use anyhow::{bail, Context, Result};
use catchlight_importer::livliv::{Options, Package};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    report: Option<PathBuf>,
    inspect: bool,
    allow_lossy: bool,
}

enum Format {
    Inx,
    LivlivZip,
    LivlivDirectory(PathBuf),
}

fn main() -> Result<()> {
    let Some(args) = parse(std::env::args_os().skip(1))? else {
        return Ok(());
    };
    match detect(&args.input)? {
        Format::Inx => inx(&args),
        format => livliv(&args, format),
    }
}

fn print_usage() {
    println!("usage: catchlight-importer <input> [-o model.clm] [options]");
    println!("  Detects INX/INP and LivLiv ZIP packages by magic bytes, then extension.");
    println!("  LivLiv directories and their model.json files are also accepted.");
    println!("  -o, --out <path>    Output model (default: input with a .clm extension)");
    println!("  --inspect          Validate a LivLiv package without exporting");
    println!("  --allow-lossy      Acknowledge reported LivLiv approximations");
    println!("  --report <path>    Write the LivLiv compatibility report");
    println!("  --                 Treat remaining arguments as paths");
    println!("Existing outputs are refused. Required watermarks still block export.");
}

fn parse(args: impl Iterator<Item = OsString>) -> Result<Option<Args>> {
    let mut args = args;
    let mut input = None;
    let mut output = None;
    let mut report = None;
    let mut inspect = false;
    let mut allow_lossy = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        match if positional { None } else { arg.to_str() } {
            Some("-h" | "--help") => {
                print_usage();
                return Ok(None);
            }
            Some("--") => positional = true,
            Some("-o" | "--out") => {
                output = Some(PathBuf::from(args.next().context("-o requires a path")?))
            }
            Some("--report") => {
                report = Some(PathBuf::from(
                    args.next().context("--report requires a path")?,
                ))
            }
            Some("--inspect") => inspect = true,
            Some("--allow-lossy") => allow_lossy = true,
            Some(flag) if flag.starts_with('-') => {
                bail!("unknown option: {flag}; use -- before a filename starting with '-'")
            }
            _ => {
                if input.is_some() {
                    bail!("unexpected argument: {}", arg.to_string_lossy());
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }
    Ok(Some(Args {
        input: input.context("usage: catchlight-importer <input> [-o model.clm] [options]")?,
        output,
        report,
        inspect,
        allow_lossy,
    }))
}

fn detect(input: &Path) -> Result<Format> {
    if input.is_dir() {
        if !input.join("model.json").is_file() {
            bail!(
                "{} is not a LivLiv package directory: missing model.json",
                input.display()
            );
        }
        return Ok(Format::LivlivDirectory(input.to_path_buf()));
    }
    let mut magic = Vec::with_capacity(8);
    std::fs::File::open(input)
        .with_context(|| format!("reading {}", input.display()))?
        .take(8)
        .read_to_end(&mut magic)?;
    if magic == b"TRNSRTS\0" {
        return Ok(Format::Inx);
    }
    if magic == b"NYANPASU" {
        bail!(
            "{} is already a Catchlight .clm model; no import is needed",
            input.display()
        );
    }
    if [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"]
        .iter()
        .any(|signature| magic.starts_with(*signature))
    {
        return Ok(Format::LivlivZip);
    }
    if input.file_name().is_some_and(|name| name == "model.json") {
        let parent = input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        return Ok(Format::LivlivDirectory(parent.to_path_buf()));
    }
    match input
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("inx" | "inp") => Ok(Format::Inx),
        Some("livliv" | "zip") => Ok(Format::LivlivZip),
        _ => bail!(
            "cannot detect the format of {}; expected INX/INP or a LivLiv package",
            input.display()
        ),
    }
}

fn inx(args: &Args) -> Result<()> {
    if args.inspect || args.allow_lossy || args.report.is_some() {
        bail!("--inspect, --allow-lossy and --report are only supported for LivLiv inputs");
    }
    let bytes =
        std::fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
    let model = catchlight_importer::import_inx_bytes(&bytes)
        .with_context(|| format!("importing {}", args.input.display()))?;
    write_model(args, &model)?;
    Ok(())
}

fn livliv(args: &Args, format: Format) -> Result<()> {
    let package = match format {
        Format::LivlivDirectory(root) => Package::from_directory(&root)?,
        Format::LivlivZip => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args.input)?
                .take(256 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            Package::from_zip(&bytes)?
        }
        Format::Inx => bail!("expected a LivLiv package"),
    }
    .validate()?;
    let report = package.report();
    if args.inspect {
        write_report(&report, args.report.as_deref())?;
        eprintln!(
            "validated {} ({} layers); conversion blockers: {}",
            package.name(),
            package.layer_count(),
            report.blockers.len()
        );
        return Ok(());
    }
    let converted = match package.convert(Options {
        allow_lossy: args.allow_lossy,
    }) {
        Ok(converted) => converted,
        Err(error) => {
            write_report(&report, args.report.as_deref())?;
            return Err(error.into());
        }
    };
    write_report(&converted.report, args.report.as_deref())?;
    write_model(args, &converted.model)
}

fn write_model(args: &Args, model: &catchlight_core::Model) -> Result<()> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.input.with_extension("clm"));
    let bytes = model.to_clm_bytes()?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("creating {}", output.display()))?;
    std::io::Write::write_all(&mut file, &bytes)?;
    eprintln!("wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}

fn write_report(report: &catchlight_importer::livliv::Report, path: Option<&Path>) -> Result<()> {
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
