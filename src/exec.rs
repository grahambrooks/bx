//! Process execution.
//!
//! Stdio is inherited from the bx process, which is essential for MCP stdio
//! transport — the MCP client talks to bx's stdin/stdout, which is really
//! the child's stdin/stdout.
//!
//! We use `Command::status()` rather than `exec(3)` (via `CommandExt::exec`)
//! deliberately: we want a chance to do cleanup or logging if/when we add
//! features that need it. The performance cost over `exec` is negligible
//! compared to the MCP server's own startup.

use crate::error::Result;
use std::path::Path;
use std::process::Command;

pub fn run(binary: &Path, args: &[String]) -> Result<i32> {
    let status = Command::new(binary).args(args).status()?;

    if let Some(code) = status.code() {
        Ok(code)
    } else {
        // Killed by signal on Unix. Conventionally we return 128 + signo,
        // but we don't have the signal number from std. 130 (SIGINT) is the
        // most common case for a killed MCP server.
        Ok(130)
    }
}
