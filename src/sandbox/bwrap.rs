//! Linux bubblewrap argv generation.
//!
//! Adapted from MXC's `bwrap_common::bwrap_command` (MIT). Pure argv building
//! — no spawning — so it compiles and is unit-tested on every host.
//!
//! Differences from upstream: bx runs the *real binary* directly (`-- <prog>
//! <args…>`) rather than `sh -c <script>`, and it does **not** `--clearenv`.
//! An MCP server inherits bx's environment (PATH, HOME, and whatever the MCP
//! client passed) the same way it would unsandboxed; clearing it would break
//! servers that read config from the environment. Per-host network filtering
//! and the cooperative HTTP proxy are out of scope for this cut.
//!
//! bubblewrap inherits stdio from the `bwrap` process, which bx spawns with
//! inherited stdio — so MCP's raw-pipe stdin/stdout passthrough survives.

use super::{Network, Policy};

/// Build the full `bwrap` argument list for running `binary args…` under
/// `policy`, chdir'd into `cwd`. The returned vector does **not** include the
/// `bwrap` program name; callers do `Command::new("bwrap").args(build_args(…))`.
pub fn build_args(policy: &Policy, cwd: &str, binary: &str, args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    // Full network block only when no outbound is allowed.
    if policy.network == Network::Block {
        out.push("--unshare-net".into());
    }

    // Base filesystem: read-only view of the whole tree, then standard
    // virtual filesystems, then policy mounts last so they always win — even
    // when a policy path lives under /tmp (which we tmpfs below).
    out.extend(["--ro-bind".into(), "/".into(), "/".into()]);
    out.extend(["--dev".into(), "/dev".into()]);
    out.extend(["--proc".into(), "/proc".into()]);
    out.extend(["--tmpfs".into(), "/tmp".into()]);

    for path in &policy.readwrite_paths {
        out.extend(["--bind".into(), path.clone(), path.clone()]);
    }
    for path in &policy.readonly_paths {
        out.extend(["--ro-bind".into(), path.clone(), path.clone()]);
    }
    // Denied paths: mask with an empty tmpfs so contents are invisible.
    for path in &policy.denied_paths {
        out.extend(["--tmpfs".into(), path.clone()]);
    }

    if !cwd.is_empty() {
        out.extend(["--chdir".into(), cwd.to_string()]);
    }

    // Run the real binary directly. Environment is inherited (no --clearenv).
    out.push("--".into());
    out.push(binary.to_string());
    out.extend(args.iter().cloned());

    out
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
    fn namespaces_unshared() {
        let a = build_args(&policy(), "/w", "tool", &[]);
        for flag in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
        ] {
            assert!(a.contains(&flag.to_string()), "missing {flag}");
        }
    }

    #[test]
    fn network_block_unshares_net() {
        let a = build_args(&policy(), "/w", "tool", &[]);
        assert!(a.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn network_allow_keeps_net() {
        let mut p = policy();
        p.network = Network::Allow;
        let a = build_args(&p, "/w", "tool", &[]);
        assert!(!a.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn binary_and_args_are_last_after_separator() {
        let a = build_args(
            &policy(),
            "/w",
            "tool",
            &["serve".into(), "--port=1".into()],
        );
        let sep = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[sep + 1], "tool");
        assert_eq!(a[sep + 2], "serve");
        assert_eq!(a[sep + 3], "--port=1");
    }

    #[test]
    fn no_clearenv_emitted() {
        let a = build_args(&policy(), "/w", "tool", &[]);
        assert!(!a.contains(&"--clearenv".to_string()));
        assert!(!a.contains(&"--setenv".to_string()));
    }

    #[test]
    fn policy_mounts_come_after_standard_tmpfs() {
        let mut p = policy();
        p.readwrite_paths = vec!["/tmp/workspace".into()];
        let a = build_args(&p, "/w", "tool", &[]);
        let tmpfs_tmp = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/tmp")
            .unwrap();
        let ws = a
            .windows(2)
            .position(|w| w[0] == "--bind" && w[1] == "/tmp/workspace")
            .unwrap();
        assert!(
            ws > tmpfs_tmp,
            "policy mount must not be shadowed by /tmp tmpfs"
        );
    }

    #[test]
    fn chdir_set_and_omitted() {
        let a = build_args(&policy(), "/work", "tool", &[]);
        let i = a.iter().position(|x| x == "--chdir").unwrap();
        assert_eq!(a[i + 1], "/work");
        let b = build_args(&policy(), "", "tool", &[]);
        assert!(!b.contains(&"--chdir".to_string()));
    }

    #[test]
    fn denied_path_tmpfs_masked() {
        let mut p = policy();
        p.denied_paths = vec!["/work/.env".into()];
        let a = build_args(&p, "/work", "tool", &[]);
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == "/work/.env"));
    }
}
