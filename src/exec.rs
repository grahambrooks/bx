//! Process execution.
//!
//! Stdio is inherited from the bx process, which is essential for MCP stdio
//! transport — the MCP client talks to bx's stdin/stdout, which is really
//! the child's stdin/stdout. This holds for every code path here, sandboxed
//! or not: macOS applies the sandbox in-process via `pre_exec` on bx's own
//! `Command` (stdio untouched), and Linux execs `bwrap` with inherited stdio.
//! We never route the child through a PTY or capture its output.
//!
//! We use `Command::status()` rather than `exec(3)` (via `CommandExt::exec`)
//! deliberately: we want a chance to do cleanup or logging if/when we add
//! features that need it. The performance cost over `exec` is negligible
//! compared to the MCP server's own startup.

use crate::error::Result;
use crate::sandbox::Policy;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Run `binary args…`, optionally under `policy`.
///
/// When `policy` is `None` this is exactly the legacy unsandboxed path — a
/// contract: opting out of sandboxing must change nothing about how the
/// binary runs. When `Some`, the platform backend wraps the command (macOS
/// Seatbelt or Linux bubblewrap); on platforms without a backend we warn and
/// run unsandboxed unless `BX_SANDBOX_FALLBACK=error` is set.
pub fn run(binary: &Path, args: &[String], policy: Option<&Policy>) -> Result<i32> {
    match policy {
        None => exec_status(Command::new(binary).args(args)),
        Some(p) => run_sandboxed(binary, args, p),
    }
}

/// Map an `ExitStatus` to bx's returned code. Signal-killed children have no
/// code; we return 130 (the SIGINT convention) as the most common case.
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(130)
}

fn exec_status(cmd: &mut Command) -> Result<i32> {
    Ok(exit_code(cmd.status()?))
}

#[cfg(target_os = "macos")]
fn run_sandboxed(binary: &Path, args: &[String], policy: &Policy) -> Result<i32> {
    use crate::error::BxError;
    use crate::sandbox::seatbelt;
    use std::ffi::{c_char, CStr, CString};
    use std::os::unix::process::CommandExt;

    // libsandbox is shipped on every macOS; `sandbox_init` is deprecated in
    // the headers but still used by first-party apps (and Chromium) through
    // macOS 15+.
    #[link(name = "sandbox")]
    extern "C" {
        fn sandbox_init(
            profile: *const c_char,
            flags: u64,
            errorbuf: *mut *mut c_char,
        ) -> libc::c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    let profile = seatbelt::build_profile(policy)?;
    let profile_c = CString::new(profile)
        .map_err(|e| BxError::Sandbox(format!("seatbelt profile contains a NUL byte: {e}")))?;

    let mut cmd = Command::new(binary);
    cmd.args(args);

    // SAFETY: the closure runs between fork() and exec(). It performs no Rust
    // allocation — only a single FFI call with a pre-allocated CString, and
    // raw `libc::write`/`CStr` on the error path. This is the same fork+exec
    // pattern Chromium uses to apply Seatbelt.
    unsafe {
        cmd.pre_exec(move || {
            let mut errorbuf: *mut c_char = std::ptr::null_mut();
            let rc = sandbox_init(profile_c.as_ptr(), 0, &mut errorbuf);
            if rc != 0 {
                if !errorbuf.is_null() {
                    let msg = CStr::from_ptr(errorbuf).to_bytes();
                    let prefix = b"bx: sandbox_init failed: ";
                    libc::write(2, prefix.as_ptr().cast(), prefix.len());
                    libc::write(2, msg.as_ptr().cast(), msg.len());
                    libc::write(2, b"\n".as_ptr().cast(), 1);
                    sandbox_free_error(errorbuf);
                }
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            Ok(())
        });
    }

    exec_status(&mut cmd)
}

#[cfg(target_os = "linux")]
fn run_sandboxed(binary: &Path, args: &[String], policy: &Policy) -> Result<i32> {
    use crate::sandbox::bwrap;

    // bubblewrap is an external, unprivileged helper — there is no in-process
    // Linux equivalent. If it's missing we can't sandbox.
    if !bwrap_available() {
        return fallback(
            binary,
            args,
            "bubblewrap (bwrap) not found on PATH — install it to enable Linux sandboxing",
        );
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let binary_str = binary.to_string_lossy();
    let bwrap_args = bwrap::build_args(policy, &cwd, &binary_str, args);

    tracing::debug!("running under bubblewrap");
    exec_status(Command::new("bwrap").args(bwrap_args))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn run_sandboxed(binary: &Path, args: &[String], _policy: &Policy) -> Result<i32> {
    fallback(
        binary,
        args,
        "sandboxing is not implemented on this platform",
    )
}

/// Shared "no sandbox backend available" handling. Honors
/// `BX_SANDBOX_FALLBACK=error` (refuse to run) versus the default (warn and
/// run unsandboxed) per the CLAUDE.md contract.
#[cfg(not(target_os = "macos"))]
fn fallback(binary: &Path, args: &[String], reason: &str) -> Result<i32> {
    if std::env::var("BX_SANDBOX_FALLBACK").as_deref() == Ok("error") {
        return Err(crate::error::BxError::Sandbox(format!(
            "{reason}; refusing because BX_SANDBOX_FALLBACK=error"
        )));
    }
    tracing::warn!("{reason}; running unsandboxed");
    exec_status(Command::new(binary).args(args))
}

#[cfg(target_os = "linux")]
fn bwrap_available() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}
