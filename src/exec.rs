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

/// Windows: launch the binary in an AppContainer. The plan (capabilities,
/// LPAC, per-path ACE grants) is generated purely by `sandbox::appcontainer`;
/// this function applies it — derives the package SID, grants the ACEs (and
/// reverts them when the child exits via [`win::GrantGuard`]), then
/// `CreateProcessW` with a `SECURITY_CAPABILITIES` proc-thread attribute and
/// inherited std handles. Stdio passthrough is preserved exactly as on the
/// other backends: handles are inherited raw (`STARTF_USESTDHANDLES` +
/// `bInheritHandles`), never routed through a console/PTY.
///
/// Unlike the no-backend platforms, a *failure to apply* the sandbox here is a
/// hard error (fail closed): the user opted into containment, so we never
/// silently downgrade to unsandboxed on Windows.
#[cfg(target_os = "windows")]
fn run_sandboxed(binary: &Path, args: &[String], policy: &Policy) -> Result<i32> {
    use crate::sandbox::appcontainer;
    let binary_str = binary.to_string_lossy().into_owned();
    let plan = appcontainer::build_plan(policy, &binary_str);
    tracing::debug!(
        profile = %plan.profile_name,
        lpac = plan.lpac,
        "launching AppContainer"
    );
    win::launch(&plan, &binary_str, args)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn run_sandboxed(binary: &Path, args: &[String], _policy: &Policy) -> Result<i32> {
    fallback(
        binary,
        args,
        "sandboxing is not implemented on this platform",
    )
}

/// Shared "no sandbox backend available" handling. Honors
/// `BX_SANDBOX_FALLBACK=error` (refuse to run) versus the default (warn and
/// run unsandboxed) per the CLAUDE.md contract. Compiled only where a fallback
/// is reachable: not macOS (always Seatbelt) and not Windows (always
/// AppContainer, which fails closed rather than falling back).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

/// Windows AppContainer application layer.
///
/// This is bx's *own* code — it applies the plan produced by
/// `sandbox::appcontainer`; it does not vendor MXC's exec path (which routes
/// the child through a console and would break MCP's raw-stdio contract). Every
/// Win32 call is checked and the only mutation of host state — the per-path ACE
/// grants — is reverted by [`GrantGuard`] on scope exit, success or failure.
///
/// Numeric Win32 constants are defined locally rather than imported so the FFI
/// surface (which can only be compiled on the Windows CI runner) depends on as
/// few exact `windows-sys` symbol paths as possible; the values are stable ABI.
#[cfg(target_os = "windows")]
mod win {
    use crate::error::{BxError, Result};
    use crate::sandbox::appcontainer::{Access, WindowsPlan};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        BuildTrusteeWithSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
        DENY_ACCESS, EXPLICIT_ACCESS_W, SET_ACCESS, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
    };
    use windows_sys::Win32::Security::{
        AllocateAndInitializeSid, FreeSid, GetSecurityDescriptorControl, ACL, PSID,
        SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
        EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, STARTUPINFOEXW,
    };

    // --- stable ABI constants (see module note) ---
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    /// CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE — apply to the whole subtree.
    const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
    const SE_GROUP_ENABLED: u32 = 0x4;
    /// `SE_FILE_OBJECT` for the Get/SetNamedSecurityInfo object type.
    const SE_FILE_OBJECT: i32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    /// `PROTECTED_DACL_SECURITY_INFORMATION` — keep the DACL from inheriting.
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    /// `UNPROTECTED_DACL_SECURITY_INFORMATION` — let the DACL inherit.
    const UNPROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x2000_0000;
    /// `SE_DACL_PROTECTED` bit of `SECURITY_DESCRIPTOR_CONTROL`.
    const SE_DACL_PROTECTED: u16 = 0x1000;
    /// Identifier authority 15 (`SECURITY_APP_PACKAGE_AUTHORITY`).
    const APP_PACKAGE_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 15];
    /// First sub-authority of a capability SID (`S-1-15-3-*`).
    const SECURITY_CAPABILITY_BASE_RID: u32 = 3;
    const TRUSTEE_IS_SID: i32 = 0;
    const TRUSTEE_IS_GROUP: i32 = 2;
    const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
    const INFINITE: u32 = 0xFFFF_FFFF;
    /// `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`.
    const ATTR_SECURITY_CAPABILITIES: usize = 0x0002_0009;
    /// `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY`.
    const ATTR_ALL_APP_PACKAGES_POLICY: usize = 0x0002_000F;
    /// `PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT` (the LPAC opt-out).
    const ALL_APP_PACKAGES_OPT_OUT: u32 = 0x1;

    fn last_error(ctx: &str) -> BxError {
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        BxError::Sandbox(format!("{ctx} failed (GetLastError={code})"))
    }

    /// UTF-16, NUL-terminated — the encoding every wide Win32 API expects.
    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Quote `argv` into a single Windows command line per the MSVCRT rules
    /// `CommandLineToArgvW` reverses. Keeps argument boundaries intact for the
    /// child exactly as `std::process::Command` would.
    fn command_line(binary: &str, args: &[String]) -> Vec<u16> {
        let mut s = String::new();
        append_quoted(&mut s, binary);
        for a in args {
            s.push(' ');
            append_quoted(&mut s, a);
        }
        wide(&s)
    }

    fn append_quoted(out: &mut String, arg: &str) {
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{0b}', '"']) {
            out.push_str(arg);
            return;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    for _ in 0..(backslashes * 2 + 1) {
                        out.push('\\');
                    }
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    for _ in 0..backslashes {
                        out.push('\\');
                    }
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        for _ in 0..(backslashes * 2) {
            out.push('\\');
        }
        out.push('"');
    }

    /// Resolve the AppContainer package SID for `name`, registering the profile
    /// if it does not yet exist. The profile is intentionally left registered
    /// (a cheap per-binary cache); we never delete it, which also avoids racing
    /// a concurrent run of the same binary.
    fn container_sid(name: &str) -> Result<PSID> {
        let wname = wide(name);
        let mut sid: PSID = std::ptr::null_mut();
        // S_OK or "already exists" both mean we can proceed; for the latter we
        // still need the SID, so derive it deterministically from the name.
        let hr = unsafe {
            CreateAppContainerProfile(
                wname.as_ptr(),
                wname.as_ptr(),
                wname.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if hr == 0 && !sid.is_null() {
            return Ok(sid);
        }
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid) };
        if hr != 0 || sid.is_null() {
            return Err(BxError::Sandbox(format!(
                "could not resolve AppContainer SID for {name} (hr={hr:#x})"
            )));
        }
        Ok(sid)
    }

    /// Build a well-known capability SID `S-1-15-3-<rid>`. Owned by the caller;
    /// free with [`FreeSid`].
    fn capability_sid(rid: u32) -> Result<PSID> {
        let authority = SID_IDENTIFIER_AUTHORITY {
            Value: APP_PACKAGE_AUTHORITY,
        };
        let mut sid: PSID = std::ptr::null_mut();
        let ok = unsafe {
            AllocateAndInitializeSid(
                &authority,
                2,
                SECURITY_CAPABILITY_BASE_RID,
                rid,
                0,
                0,
                0,
                0,
                0,
                0,
                &mut sid,
            )
        };
        if ok == 0 || sid.is_null() {
            return Err(last_error("AllocateAndInitializeSid"));
        }
        Ok(sid)
    }

    /// Which `*_DACL_SECURITY_INFORMATION` protection flag reproduces `sd`'s
    /// current inheritance disposition.
    ///
    /// `SetNamedSecurityInfoW` given only `DACL_SECURITY_INFORMATION` does not
    /// carry the `SE_DACL_PROTECTED` bit across, so a protected DACL comes back
    /// unprotected: its explicit ACEs are dropped in favour of ones inherited
    /// from the parent. Effective rights often look identical, which is why
    /// this went unnoticed — but on a directory whose parent grants more than
    /// its own ACEs did, it silently widens access, and it outlives the run.
    /// Passing the flag explicitly on both the grant and the revert pins the
    /// disposition to whatever it was before bx touched the path.
    fn dacl_protection_flag(sd: *mut c_void) -> u32 {
        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
        if ok == 0 {
            // Unreadable control bits: say nothing rather than assert the
            // wrong disposition, leaving the legacy behaviour for this path.
            return 0;
        }
        if control & SE_DACL_PROTECTED != 0 {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        }
    }

    /// Reverts the DACL of one path to the descriptor captured before the grant
    /// was applied. Restoring runs on `Drop` so a panic or early return still
    /// undoes the host-state mutation.
    struct GrantGuard {
        path: Vec<u16>,
        original_dacl: *mut ACL,
        security_descriptor: *mut c_void,
        new_dacl: *mut ACL,
        /// `DACL_SECURITY_INFORMATION` plus the captured protection flag.
        security_info: u32,
    }

    impl Drop for GrantGuard {
        fn drop(&mut self) {
            unsafe {
                SetNamedSecurityInfoW(
                    self.path.as_mut_ptr(),
                    SE_FILE_OBJECT,
                    self.security_info,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    self.original_dacl,
                    std::ptr::null_mut(),
                );
                if !self.security_descriptor.is_null() {
                    LocalFree(self.security_descriptor as _);
                }
                if !self.new_dacl.is_null() {
                    LocalFree(self.new_dacl as _);
                }
            }
        }
    }

    /// Grant `container_sid` access to `path` (read+exec, or +write), capturing
    /// the prior DACL so it can be reverted. The grant is inheritable so a whole
    /// directory subtree is covered by a single ACE on the root.
    fn grant_path(path: &str, sid: PSID, access: Access, deny: bool) -> Result<GrantGuard> {
        let mut wpath = wide(path);

        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != 0 {
            return Err(BxError::Sandbox(format!(
                "GetNamedSecurityInfoW({path}) failed (error={rc})"
            )));
        }

        // Capture the inheritance disposition before mutating anything; both
        // the grant below and the revert in `Drop` have to reassert it.
        let security_info = DACL_SECURITY_INFORMATION | dacl_protection_flag(sd);

        let mut trustee: TRUSTEE_W = unsafe { std::mem::zeroed() };
        unsafe { BuildTrusteeWithSidW(&mut trustee, sid) };
        trustee.TrusteeForm = TRUSTEE_IS_SID;
        trustee.TrusteeType = TRUSTEE_IS_GROUP;

        let permissions = match access {
            Access::Read => GENERIC_READ | GENERIC_EXECUTE,
            Access::ReadWrite => GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE,
        };
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: permissions,
            grfAccessMode: if deny { DENY_ACCESS } else { SET_ACCESS },
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: trustee,
        };

        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl) };
        if rc != 0 {
            unsafe {
                if !sd.is_null() {
                    LocalFree(sd as _);
                }
            }
            return Err(BxError::Sandbox(format!(
                "SetEntriesInAclW({path}) failed (error={rc})"
            )));
        }

        let rc = unsafe {
            SetNamedSecurityInfoW(
                wpath.as_mut_ptr(),
                SE_FILE_OBJECT,
                security_info,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            unsafe {
                if !sd.is_null() {
                    LocalFree(sd as _);
                }
                if !new_dacl.is_null() {
                    LocalFree(new_dacl as _);
                }
            }
            return Err(BxError::Sandbox(format!(
                "SetNamedSecurityInfoW({path}) failed (error={rc})"
            )));
        }

        Ok(GrantGuard {
            path: wpath,
            original_dacl: old_dacl,
            security_descriptor: sd,
            new_dacl,
            security_info,
        })
    }

    /// Apply the plan and run the child to completion, returning its exit code.
    pub fn launch(plan: &WindowsPlan, binary: &str, args: &[String]) -> Result<i32> {
        let app_sid = container_sid(&plan.profile_name)?;

        // Capability SIDs for SECURITY_CAPABILITIES (network, if any).
        let mut cap_sids: Vec<PSID> = Vec::new();
        for cap in &plan.capabilities {
            cap_sids.push(capability_sid(cap.rid())?);
        }
        let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = cap_sids
            .iter()
            .map(|&sid| SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: SE_GROUP_ENABLED,
            })
            .collect();

        // Grant (and arm reverts for) every policy path plus explicit denies.
        // Guards drop at end of scope — after the child has exited.
        let mut guards: Vec<GrantGuard> = Vec::new();
        let mut grant_err = None;
        for g in &plan.grants {
            match grant_path(&g.path, app_sid, g.access, false) {
                Ok(guard) => guards.push(guard),
                Err(e) => {
                    grant_err = Some(e);
                    break;
                }
            }
        }
        if grant_err.is_none() {
            for d in &plan.denies {
                match grant_path(d, app_sid, Access::ReadWrite, true) {
                    Ok(guard) => guards.push(guard),
                    Err(e) => {
                        grant_err = Some(e);
                        break;
                    }
                }
            }
        }
        let result = if let Some(e) = grant_err {
            Err(e)
        } else {
            spawn(plan, &mut cap_attrs, app_sid, binary, args)
        };

        // Cleanup: reverts happen as `guards` drops; free the SIDs we own.
        drop(guards);
        for sid in cap_sids {
            unsafe { FreeSid(sid) };
        }
        // The AppContainer SID from CreateAppContainerProfile is freed with
        // FreeSid as well.
        unsafe { FreeSid(app_sid) };
        result
    }

    fn spawn(
        plan: &WindowsPlan,
        cap_attrs: &mut [SID_AND_ATTRIBUTES],
        app_sid: PSID,
        binary: &str,
        args: &[String],
    ) -> Result<i32> {
        let mut sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: app_sid,
            Capabilities: if cap_attrs.is_empty() {
                std::ptr::null_mut()
            } else {
                cap_attrs.as_mut_ptr()
            },
            CapabilityCount: cap_attrs.len() as u32,
            Reserved: 0,
        };

        let attr_count: u32 = if plan.lpac { 2 } else { 1 };

        // Size, then allocate, the proc-thread attribute list.
        let mut size: usize = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attr_count, 0, &mut size);
        }
        let mut attr_buf = vec![0u8; size];
        // `LPPROC_THREAD_ATTRIBUTE_LIST` is just `*mut c_void`; use it directly
        // so we don't depend on the alias being a named `windows-sys` export.
        let attr_list = attr_buf.as_mut_ptr() as *mut c_void;
        if unsafe { InitializeProcThreadAttributeList(attr_list, attr_count, 0, &mut size) } == 0 {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }

        let mut lpac_value: u32 = ALL_APP_PACKAGES_OPT_OUT;
        let result = (|| {
            if unsafe {
                UpdateProcThreadAttribute(
                    attr_list,
                    0,
                    ATTR_SECURITY_CAPABILITIES,
                    &mut sec_caps as *mut _ as *const c_void,
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_error(
                    "UpdateProcThreadAttribute(SECURITY_CAPABILITIES)",
                ));
            }
            if plan.lpac
                && unsafe {
                    UpdateProcThreadAttribute(
                        attr_list,
                        0,
                        ATTR_ALL_APP_PACKAGES_POLICY,
                        &mut lpac_value as *mut _ as *const c_void,
                        std::mem::size_of::<u32>(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                } == 0
            {
                return Err(last_error("UpdateProcThreadAttribute(LPAC)"));
            }

            // Inherit bx's std handles raw — the MCP stdio passthrough contract.
            let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            si.StartupInfo.hStdOutput = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            si.StartupInfo.hStdError = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
            si.lpAttributeList = attr_list;

            let mut cmdline = command_line(binary, args);
            let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    cmdline.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1, // bInheritHandles = TRUE
                    EXTENDED_STARTUPINFO_PRESENT,
                    std::ptr::null(),
                    std::ptr::null(),
                    &si.StartupInfo,
                    &mut pi,
                )
            };
            if ok == 0 {
                return Err(last_error("CreateProcessW"));
            }

            unsafe {
                WaitForSingleObject(pi.hProcess, INFINITE);
                let mut code: u32 = 0;
                GetExitCodeProcess(pi.hProcess, &mut code);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
                Ok(code as i32)
            }
        })();

        unsafe { DeleteProcThreadAttributeList(attr_list) };
        result
    }
}
