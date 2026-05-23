//! `bx` — Run a binary from a GitHub release.
//!
//! Usage:
//!   bx <spec> [-- <args...>]
//!   bx prune [--keep N] [--all] [--dry-run]
//!
//! Examples:
//!   bx grahambrooks/symgraph --help
//!   bx grahambrooks/symgraph@v2026.4.13 serve
//!   bx --refresh grahambrooks/symgraph serve
//!   bx prune --keep 3

use bx::prune::{self, PruneOpts};
use bx::spec::Spec;
use bx::BxError;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "bx",
    version,
    about = "Run a binary from a GitHub release. npx, for compiled tools.",
    long_about = None,
)]
struct Cli {
    /// Subcommand (`prune`). Omit to invoke the run-a-spec path.
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

/// The default "run a binary from a release" invocation. These args are
/// flattened into the top-level `Cli` so `bx <spec>` keeps working without a
/// `run` subcommand keyword.
#[derive(Args, Debug)]
struct RunArgs {
    /// Force a re-download even if the binary is cached.
    #[arg(long, global = true)]
    refresh: bool,

    /// The spec to run: owner/repo[@ref][#binary]. Required unless a
    /// subcommand is given.
    spec: Option<String>,

    /// Arguments passed through to the binary. Use `--` to disambiguate
    /// flags from bx's own flags:
    ///   bx grahambrooks/symgraph -- --help
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Garbage-collect cached binaries.
    Prune(PruneCmd),
    /// Fetch (and verify) every tool listed in a .bx.toml manifest.
    Ensure(EnsureCmd),
    /// Pin a spec into a .bx.toml manifest with its archive checksum.
    Add(AddCmd),
}

#[derive(Args, Debug)]
struct PruneCmd {
    /// Keep this many most-recent tags per repository.
    #[arg(long, default_value_t = 1, value_name = "N")]
    keep: usize,

    /// Remove every cached binary (overrides --keep).
    #[arg(long)]
    all: bool,

    /// Show what would be removed without touching the filesystem.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Debug)]
struct EnsureCmd {
    /// Path to .bx.toml. Defaults to walking up from cwd.
    #[arg(long, value_name = "PATH")]
    manifest: Option<PathBuf>,

    /// Backfill checksums for the current platform if missing.
    #[arg(long)]
    record: bool,
}

#[derive(Args, Debug)]
struct AddCmd {
    /// Spec to pin: owner/repo[@ref][#binary]. `@latest` is resolved to a
    /// concrete tag before writing so the manifest stays reproducible.
    spec: String,

    /// Path to the manifest file. Defaults to ./.bx.toml in cwd.
    #[arg(long, value_name = "PATH")]
    manifest: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Prune(p)) => match prune::run(PruneOpts {
            keep: p.keep,
            all: p.all,
            dry_run: p.dry_run,
        }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                report_error(&e);
                ExitCode::from(1)
            }
        },
        Some(Command::Ensure(e)) => match bx::ensure(e.manifest.as_deref(), e.record).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                report_error(&err);
                ExitCode::from(1)
            }
        },
        Some(Command::Add(a)) => match bx::add(&a.spec, a.manifest.as_deref()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                report_error(&err);
                ExitCode::from(1)
            }
        },
        None => run_spec(cli.run).await,
    }
}

async fn run_spec(args: RunArgs) -> ExitCode {
    let Some(spec_str) = args.spec else {
        eprintln!("bx: missing spec (try `bx --help` or `bx <owner>/<repo>`)");
        return ExitCode::from(2);
    };

    let spec: Spec = match spec_str.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bx: {e}");
            return ExitCode::from(2);
        }
    };

    match bx::run(&spec, &args.args, args.refresh).await {
        Ok(code) => {
            // Clamp the exit code to a u8. POSIX exit codes are 0..=255.
            let clamped = code.clamp(0, 255) as u8;
            ExitCode::from(clamped)
        }
        Err(e) => {
            report_error(&e);
            ExitCode::from(1)
        }
    }
}

fn report_error(e: &BxError) {
    eprintln!("bx: {e}");
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("BX_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}
