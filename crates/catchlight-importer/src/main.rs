//! Standalone importer entry point; built only with the `cli` feature.
//! Conversion lives in the library; this target owns arguments and file IO.
//! Recognized container magic takes precedence over extensions. Extensions
//! only select a validator when no signature is recognized; they never bypass
//! that format's validation. ZIP inputs must still contain a `LivLiv` package.
use anyhow::{Context, Result, bail};
use catchlight_importer::livliv::{self, Options, Package};
use clap::Parser;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Every failure, as in `catchlight-cli`.
const EXIT_ERROR: u8 = 2;
/// A reader closed stdout before the report was written.
const EXIT_BROKEN_PIPE: u8 = 141;

const AFTER_HELP: &str = "\
Existing outputs are refused. Required watermarks still block export.

Exit status:
  0    the import or inspection succeeded
  2    it failed
  141  a reader closed the output pipe before the report was written";

#[derive(Parser)]
#[expect(clippy::doc_markdown, reason = "field docs are the plain --help text")]
#[command(
    name = "catchlight-importer",
    about = "Convert an INX/INP model or a LivLiv package into a .clm model",
    long_about = "Convert an INX/INP model or a LivLiv package into a .clm model.\n\nThe format is \
                  detected by magic bytes, then by extension. LivLiv directories and their \
                  model.json files are also accepted.",
    after_help = AFTER_HELP,
    after_long_help = AFTER_HELP
)]
struct Args {
    /// The INX/INP file, LivLiv ZIP, LivLiv directory or its model.json.
    input: PathBuf,
    /// Output model (default: the input with a .clm extension).
    #[arg(short = 'o', long = "out", conflicts_with = "inspect")]
    output: Option<PathBuf>,
    /// Validate a LivLiv package and print its compatibility report to stdout
    /// without exporting.
    #[arg(long)]
    inspect: bool,
    /// Accept the approximations the LivLiv report lists; every LivLiv
    /// conversion needs it.
    #[arg(long, conflicts_with = "inspect")]
    allow_lossy: bool,
    /// Also write the LivLiv compatibility report to this file.
    #[arg(long)]
    report: Option<PathBuf>,
}

/// A failed write to stdout, told apart so a closed pipe exits quietly.
#[derive(Debug, thiserror::Error)]
#[error("writing output: {0}")]
struct Stdout(#[source] std::io::Error);

enum Format {
    Inx,
    LivlivZip,
    LivlivDirectory(PathBuf),
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = detect(&args.input).and_then(|format| match format {
        Format::Inx => inx(&args),
        format => livliv(&args, format),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error)
            if error
                .downcast_ref::<Stdout>()
                .is_some_and(|Stdout(e)| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::from(EXIT_BROKEN_PIPE)
        }
        Err(error) => {
            eprintln!("catchlight-importer: {error:#}");
            ExitCode::from(EXIT_ERROR)
        }
    }
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
    let reading = || format!("reading {}", input.display());
    std::fs::File::open(input)
        .with_context(reading)?
        .take(8)
        .read_to_end(&mut magic)
        .with_context(reading)?;
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
            .unwrap_or_else(|| Path::new("."));
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
    let reading = || format!("reading {}", args.input.display());
    let file = std::fs::File::open(&args.input).with_context(reading)?;
    // The parser bounds every section against its load budget before reading it.
    let parsed = catchlight_importer::InxModel::parse(std::io::BufReader::new(file))
        .with_context(|| format!("importing {}", args.input.display()))?;
    let model = catchlight_importer::import_inx_model(&parsed)
        .with_context(|| format!("importing {}", args.input.display()))?;
    write_model(args, &model)?;
    Ok(())
}

fn livliv(args: &Args, format: Format) -> Result<()> {
    let reading = || format!("reading {}", args.input.display());
    let package = match format {
        Format::LivlivDirectory(root) => {
            Package::from_directory(&root).with_context(|| format!("reading {}", root.display()))?
        }
        Format::LivlivZip => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args.input)
                .with_context(reading)?
                .take(256 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .with_context(reading)?;
            Package::from_zip(&bytes).with_context(reading)?
        }
        Format::Inx => bail!("expected a LivLiv package"),
    }
    .validate()?;
    let report = package.report();
    if args.inspect {
        let json = report_json(&report)?;
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(json.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(Stdout)?;
        if let Some(path) = &args.report {
            write_new(path, json.as_bytes())?;
        }
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
            // The conversion error is the one to report; a failed report
            // write is only mentioned beside it.
            if let Err(report_error) = diagnose(&report, args.report.as_deref()) {
                eprintln!("catchlight-importer: {report_error:#}");
            }
            return Err(match error {
                livliv::Error::LossyNotAllowed => anyhow::anyhow!(
                    "{error}; rerun with --allow-lossy to accept the losses the report lists"
                ),
                error => error.into(),
            });
        }
    };
    diagnose(&converted.report, args.report.as_deref())?;
    write_model(args, &converted.model)
}

fn write_model(args: &Args, model: &catchlight_core::Model) -> Result<()> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.input.with_extension("clm"));
    let bytes = model.to_clm_bytes()?;
    write_new(&output, &bytes)?;
    eprintln!("wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}

fn report_json(report: &livliv::Report) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(report)?))
}

/// Prints `report` to stderr and writes it to `path` when one is given.
fn diagnose(report: &livliv::Report, path: Option<&Path>) -> Result<()> {
    let json = report_json(report)?;
    eprint!("{json}");
    if let Some(path) = path {
        write_new(path, json.as_bytes())?;
    }
    Ok(())
}

/// Refuses an existing `path`. The bytes land under a temporary name first,
/// so a failed write leaves nothing at `path` to block the retry.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let writing = || format!("writing {}", path.display());
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".catchlight-importer-");
    // tempfile's own default is owner-only; a new output takes the umask's.
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut file = builder.tempfile_in(dir).with_context(writing)?;
    file.write_all(bytes).with_context(writing)?;
    file.as_file().sync_all().with_context(writing)?;
    file.persist_noclobber(path)
        .map_err(|e| e.error)
        .with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(writing)?;
    Ok(())
}
