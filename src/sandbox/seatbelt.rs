//! macOS Seatbelt profile generation.
//!
//! Adapted from MXC's `seatbelt_common::profile_builder` (MIT). Pure string
//! generation — no syscalls — so it compiles and is unit-tested on every host.
//! The application of the profile (`sandbox_init` in `pre_exec`) lives in
//! `exec.rs` and is macOS-only.
//!
//! Differences from upstream: bx always runs headless, so the configurable
//! UI/clipboard/GUI rules are replaced by a hard-coded locked-down block
//! (deny WindowServer, deny pasteboard, deny HID IOKit). Per-host network
//! filtering is dropped because Seatbelt cannot enforce it.
//!
//! The profile is deny-by-default with explicit allows layered on, then
//! explicit denies last so `denied_paths` overrides any broader allow
//! (Seatbelt is last-match-wins within an operation).

use super::{Network, Policy};
use crate::error::{BxError, Result};
use std::fmt::Write as _;

/// Build a complete Seatbelt profile string from `policy`. When
/// `policy.profile_override` is set it is returned verbatim.
pub fn build_profile(policy: &Policy) -> Result<String> {
    if let Some(override_profile) = &policy.profile_override {
        return Ok(override_profile.clone());
    }

    let mut out = String::with_capacity(2048);
    out.push_str("(version 1)\n");
    out.push_str("(deny default)\n");
    out.push_str(BASELINE_ALLOW);
    out.push_str(SYSTEM_READ_ALLOW);
    out.push_str(TTY_ALLOW);

    write_filesystem_allow(&mut out, policy)?;
    write_network_rules(&mut out, policy);
    write_nested_pty_rules(&mut out, policy);
    write_extra_mach_lookups(&mut out, policy);
    out.push_str(HEADLESS_UI_DENY);

    // Denies last so they win on conflict.
    write_filesystem_deny(&mut out, policy)?;

    Ok(out)
}

const BASELINE_ALLOW: &str = "\
;; --- baseline (required for any process to start) ---
(allow process-fork)
(allow process-exec)
(allow signal (target self))
(allow sysctl-read)
(allow file-read-metadata)
(allow mach-lookup
    (global-name \"com.apple.system.notification_center\")
    (global-name \"com.apple.system.logger\")
    (global-name \"com.apple.distributed_notifications@Uv3\")
    (global-name \"com.apple.CoreServices.coreservicesd\")
    (global-name \"com.apple.FSEvents\"))
";

const SYSTEM_READ_ALLOW: &str = "\
;; --- read-only access to system locations ---
(allow file-read-data (literal \"/\"))
(allow file-read*
    (subpath \"/bin\")
    (subpath \"/sbin\")
    (subpath \"/usr/bin\")
    (subpath \"/usr/sbin\")
    (subpath \"/usr/lib\")
    (subpath \"/usr/libexec\")
    (subpath \"/usr/share\")
    (subpath \"/System\")
    (subpath \"/Library\")
    (subpath \"/private/var/db/timezone\")
    (subpath \"/private/var/db/dyld\")
    (subpath \"/private/var/select\")
    (subpath \"/private/etc\"))
;; Standard bit-bucket / entropy devices — read+write because shell
;; redirections (`>/dev/null`, `</dev/urandom`) need both directions.
(allow file-read* file-write*
    (literal \"/dev/null\")
    (literal \"/dev/zero\")
    (literal \"/dev/random\")
    (literal \"/dev/urandom\"))
";

const TTY_ALLOW: &str = "\
;; --- controlling-terminal access ---
(allow file-read* file-write* file-ioctl
    (literal \"/dev/tty\")
    (regex #\"^/dev/ttys[0-9]+$\"))
(allow file-read* (subpath \"/dev/fd\"))
";

/// Headless lock-down: no WindowServer, no pasteboard, no HID injection.
/// These are denies layered over the `(deny default)` baseline for explicit
/// intent (and to override any inherited allow on future macOS versions).
const HEADLESS_UI_DENY: &str = "\
;; --- headless: deny WindowServer / LaunchServices UI ---
(deny mach-lookup
    (global-name \"com.apple.windowserver.active\")
    (global-name \"com.apple.windowserver.session\")
    (global-name \"com.apple.coreservices.launchservicesd\"))
;; --- headless: deny clipboard / pasteboard ---
(deny mach-lookup (global-name \"com.apple.pasteboard.1\"))
;; --- headless: deny HID iokit (input injection) ---
(deny iokit-open (iokit-user-client-class \"IOHIDLibUserClient\"))
";

fn write_filesystem_allow(out: &mut String, policy: &Policy) -> Result<()> {
    if !policy.readonly_paths.is_empty() {
        out.push_str(";; --- readonly paths ---\n");
        out.push_str("(allow file-read*\n");
        for p in &policy.readonly_paths {
            let expanded = expand_tilde(p)?;
            let _ = writeln!(out, "    (subpath {})", quote_scheme(&expanded));
        }
        out.push_str(")\n");
    }

    if !policy.readwrite_paths.is_empty() {
        out.push_str(";; --- readwrite paths ---\n");
        out.push_str("(allow file-read* file-write*\n");
        for p in &policy.readwrite_paths {
            let expanded = expand_tilde(p)?;
            let _ = writeln!(out, "    (subpath {})", quote_scheme(&expanded));
        }
        out.push_str(")\n");
    }

    Ok(())
}

fn write_filesystem_deny(out: &mut String, policy: &Policy) -> Result<()> {
    if !policy.denied_paths.is_empty() {
        out.push_str(";; --- denied paths (override broader allow rules) ---\n");
        out.push_str("(deny file-read* file-write*\n");
        for p in &policy.denied_paths {
            let expanded = expand_tilde(p)?;
            let _ = writeln!(out, "    (subpath {})", quote_scheme(&expanded));
        }
        out.push_str(")\n");
    }
    Ok(())
}

fn write_network_rules(out: &mut String, policy: &Policy) {
    match policy.network {
        Network::Block => {
            out.push_str(";; --- network: default-deny (no allow-network rules emitted) ---\n");
        }
        Network::Allow => {
            out.push_str(";; --- network: outbound allowed (any host) ---\n");
            out.push_str("(allow network-outbound)\n");
            out.push_str("(allow network-bind (local ip))\n");
            out.push_str("(allow system-socket)\n");
        }
    }

    // `network-bind` alone is insufficient for `listen()` on macOS — the
    // kernel rejects it with EPERM without `network-inbound`.
    if policy.allow_local_network {
        out.push_str(";; --- network: allow inbound on local IPs ---\n");
        out.push_str("(allow network-inbound (local ip))\n");
    }
}

/// Allow the inner process to allocate its own pty via `posix_openpt()`.
fn write_nested_pty_rules(out: &mut String, policy: &Policy) {
    if !policy.nested_pty {
        return;
    }
    out.push_str(";; --- nestedPty: allow inner process to allocate its own pty ---\n");
    out.push_str("(allow pseudo-tty)\n");
    out.push_str("(allow file-read* file-write* file-ioctl\n");
    out.push_str("    (literal \"/dev/ptmx\"))\n");
}

fn write_extra_mach_lookups(out: &mut String, policy: &Policy) {
    if policy.extra_mach_lookups.is_empty() {
        return;
    }
    out.push_str(";; --- extra mach-lookups (caller-provided) ---\n");
    out.push_str("(allow mach-lookup\n");
    for name in &policy.extra_mach_lookups {
        let _ = writeln!(out, "    (global-name {})", quote_scheme(name));
    }
    out.push_str(")\n");
}

/// Expand a leading `~` / `~/` to `$HOME`. Errors if `HOME` is unset and the
/// path needs expansion.
fn expand_tilde(path: &str) -> Result<String> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME").map_err(|_| {
            BxError::Sandbox(format!(
                "HOME not set; cannot expand '{path}' in seatbelt profile"
            ))
        })?;
        if path == "~" {
            Ok(home)
        } else {
            Ok(format!("{home}/{}", &path[2..]))
        }
    } else {
        Ok(path.to_string())
    }
}

/// Quote a string as a TinyScheme literal, escaping `\` and `"` so a path can
/// never break out of the quoted string and inject Scheme.
fn quote_scheme(s: &str) -> String {
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for c in s.chars() {
        match c {
            '\\' => q.push_str("\\\\"),
            '"' => q.push_str("\\\""),
            other => q.push(other),
        }
    }
    q.push('"');
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            nested_pty: true,
            ..Policy::default()
        }
    }

    #[test]
    fn baseline_has_deny_default_and_required_allows() {
        let p = build_profile(&policy()).unwrap();
        assert!(p.starts_with("(version 1)"));
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow process-exec)"));
        assert!(p.contains("(subpath \"/usr/lib\")"));
        assert!(p.contains("(allow file-read-data (literal \"/\"))"));
    }

    #[test]
    fn readonly_paths_emit_read_only_allow() {
        let mut pol = policy();
        pol.readonly_paths = vec!["/opt/tools".into()];
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(allow file-read*\n    (subpath \"/opt/tools\")"));
        assert!(!p.contains("file-write* (subpath \"/opt/tools\")"));
    }

    #[test]
    fn readwrite_paths_emit_read_write_allow() {
        let mut pol = policy();
        pol.readwrite_paths = vec!["/tmp/out".into()];
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(allow file-read* file-write*\n    (subpath \"/tmp/out\")"));
    }

    #[test]
    fn denied_paths_come_after_allows() {
        let mut pol = policy();
        pol.readwrite_paths = vec!["/tmp".into()];
        pol.denied_paths = vec!["/tmp/secret".into()];
        let p = build_profile(&pol).unwrap();
        let allow_idx = p.find("(allow file-read* file-write*").unwrap();
        let deny_idx = p.find("(deny file-read* file-write*").unwrap();
        assert!(deny_idx > allow_idx, "deny must win on last-match");
        assert!(p.contains("(subpath \"/tmp/secret\")"));
    }

    #[test]
    fn network_block_emits_no_outbound_allow() {
        let p = build_profile(&policy()).unwrap();
        assert!(!p.contains("(allow network-outbound)"));
        assert!(p.contains("network: default-deny"));
    }

    #[test]
    fn network_allow_emits_outbound() {
        let mut pol = policy();
        pol.network = Network::Allow;
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(allow network-outbound)"));
    }

    #[test]
    fn allow_local_network_emits_inbound() {
        let mut pol = policy();
        pol.allow_local_network = true;
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(allow network-inbound (local ip))"));
    }

    #[test]
    fn headless_denies_windowserver_and_pasteboard() {
        let p = build_profile(&policy()).unwrap();
        assert!(p.contains("(deny mach-lookup"));
        assert!(p.contains("com.apple.windowserver.active"));
        assert!(p.contains("com.apple.pasteboard.1"));
        assert!(p.contains("IOHIDLibUserClient"));
    }

    #[test]
    fn nested_pty_toggle() {
        let p_on = build_profile(&policy()).unwrap();
        assert!(p_on.contains("(allow pseudo-tty)"));
        let mut off = policy();
        off.nested_pty = false;
        let p_off = build_profile(&off).unwrap();
        assert!(!p_off.contains("(allow pseudo-tty)"));
    }

    #[test]
    fn extra_mach_lookups_emitted_and_escaped() {
        let mut pol = policy();
        pol.extra_mach_lookups = vec!["com.example.svc".into(), "weird\"name".into()];
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(global-name \"com.example.svc\")"));
        assert!(p.contains("(global-name \"weird\\\"name\")"));
    }

    #[test]
    fn profile_override_wins() {
        let mut pol = policy();
        pol.readonly_paths = vec!["/ignored".into()];
        pol.profile_override = Some("(version 1)(allow default)".into());
        assert_eq!(build_profile(&pol).unwrap(), "(version 1)(allow default)");
    }

    #[test]
    fn paths_with_quotes_and_backslashes_escaped() {
        let mut pol = policy();
        pol.readonly_paths = vec!["/tmp/a\"b\\c".into()];
        let p = build_profile(&pol).unwrap();
        assert!(p.contains("(subpath \"/tmp/a\\\"b\\\\c\")"));
    }
}
