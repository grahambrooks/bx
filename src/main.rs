//! `bx` — Run a binary from a GitHub release.
//!
//! Usage:
//!   bx <spec> [-- <args...>]
//!
//! Examples:
//!   bx grahambrooks/symgraph --help
//!   bx grahambrooks/symgraph@v2026.4.13 serve
//!   bx --refresh grahambrooks/symgraph serve

use bx::spec::Spec;
use clap::Parser;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "bx",
    version,
    about = "Run a binary from a GitHub release. npx, for compiled tools.",
    long_about = None,
    trailing_var_arg = true,
)]
struct Cli {
    /// Force a re-download even if the binary is cached.
    #[arg(long, global = true)]
    refresh: bool,

    /// The spec to run: owner/repo[@ref][#binary].
    ///
    /// Examples:
    ///   grahambrooks/symgraph
    ///   grahambrooks/symgraph@v2026.4.13
    ///   grahambrooks/symgraph@latest#cli
    spec: String,

    /// Arguments passed through to the binary. Use `--` to disambiguate
    /// flags from bx's own flags:
    ///   bx grahambrooks/symgraph -- --help
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();

    let spec: Spec = match cli.spec.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bx: {e}");
            return ExitCode::from(2);
        }
    };

    match bx::run(&spec, &cli.args, cli.refresh).await {
        Ok(code) => {
            // Clamp the exit code to a u8. POSIX exit codes are 0..=255.
            let clamped = code.clamp(0, 255) as u8;
            ExitCode::from(clamped)
        }
        Err(e) => {
            eprintln!("bx: {e}");
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::from(1)
        }
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
