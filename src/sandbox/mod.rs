//! Optional sandboxing for the binary `bx` execs.
//!
//! ## Contract
//!
//! Out of the box `bx` runs binaries with **no sandbox**. A policy is only
//! constructed when the user opts in — via `[tool.sandbox]` in `.bx.toml`,
//! the `--sandbox <profile>` flag, or `BX_SANDBOX_DEFAULT` in the env. When
//! no policy is selected, resolution (`lib::resolve_sandbox`) returns `None`
//! and `exec::run` takes its original unsandboxed path.
//!
//! ## Provenance
//!
//! The platform backends ([`seatbelt`] for macOS, [`bwrap`] for Linux) are
//! adapted from Microsoft's MXC project (<https://github.com/microsoft/mxc>,
//! MIT-licensed). We vendor the *profile/argv generators* — the security-
//! sensitive, frequently-updated string generation — but deliberately do
//! **not** use MXC's `ScriptRunner` execution path, which routes the child
//! through a PTY and captures its stdout/stderr. That model is incompatible
//! with bx's non-negotiable contract that stdio is inherited raw (MCP stdio
//! transport speaks newline-framed JSON-RPC over pipes; a PTY would corrupt
//! it). Instead bx applies the generated profile to its *own* `Command` —
//! `sandbox_init()` in `pre_exec` on macOS, or `bwrap … -- <argv>` with
//! inherited stdio on Linux — keeping `exec.rs`'s passthrough intact.
//!
//! The bx-native [`Policy`] struct here is a trimmed projection of MXC's
//! `ContainerPolicy`: only the fields the two generators read, plus a couple
//! of macOS escape hatches. bx always runs headless, so MXC's UI/clipboard/
//! GUI knobs are not exposed — the seatbelt profile hard-codes the locked-down
//! (no WindowServer, no pasteboard, no HID) variant.

pub mod appcontainer;
pub mod bwrap;
pub mod seatbelt;

use crate::error::{BxError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Outbound network disposition. Per-host filtering is intentionally absent:
/// macOS Seatbelt cannot filter by hostname at all, so we expose only the
/// all-or-nothing axis that both backends can actually enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Network {
    /// Deny all outbound (the secure default for untrusted binaries).
    #[default]
    Block,
    /// Allow outbound to any host.
    Allow,
}

/// A fully-resolved, absolute-path sandbox policy ready to be compiled into a
/// platform profile. This is what the [`seatbelt`] and [`bwrap`] generators
/// consume; it carries no profile-name abstraction (that lives in [`Profile`]).
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub readonly_paths: Vec<String>,
    pub readwrite_paths: Vec<String>,
    /// Paths masked off even if a broader allow rule would cover them. Wins
    /// on conflict (Seatbelt last-match-deny; bwrap tmpfs mask).
    pub denied_paths: Vec<String>,
    pub network: Network,
    /// Allow `bind()`/`listen()` on local IPs (servers accepting inbound).
    pub allow_local_network: bool,
    /// macOS only: let the child allocate its own ptys via `posix_openpt`.
    /// On by default because many tools spawn shells/REPLs that need it.
    pub nested_pty: bool,
    /// Windows only: launch as a Less-Privileged AppContainer (opt out of
    /// `ALL APPLICATION PACKAGES`). Tighter, but the child can only load DLLs
    /// that grant `ALL RESTRICTED APPLICATION PACKAGES`, so it is enabled for
    /// the `strict` profile only. Ignored on non-Windows backends.
    pub lpac: bool,
    /// macOS only: hand-authored Seatbelt profile that bypasses generation.
    pub profile_override: Option<String>,
    /// macOS only: extra Mach service global-names to allow `mach-lookup` for.
    pub extra_mach_lookups: Vec<String>,
}

/// The three built-in profiles, ordered loosest-last. These are the stable,
/// user-facing surface; the MXC schema underneath them is alpha and may churn,
/// so we never expose its fields directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Deny-all + read cache + read cwd, no network. Recommended for the
    /// "untrusted public binary" threat model.
    #[default]
    Strict,
    /// `strict` + write cwd + read `~/.config`. For tools that need to
    /// persist into the working tree.
    Project,
    /// Read `$HOME`, write cwd, network allowed. Adoption-friendly, weak.
    Permissive,
}

impl FromStr for Profile {
    type Err = BxError;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Profile::Strict),
            "project" => Ok(Profile::Project),
            "permissive" => Ok(Profile::Permissive),
            other => Err(BxError::Sandbox(format!(
                "unknown sandbox profile '{other}' (expected strict|project|permissive)"
            ))),
        }
    }
}

impl Profile {
    /// Expand this profile into a concrete [`Policy`] for the given working
    /// directory and cache root. `home` is the user's home dir (used by the
    /// `project`/`permissive` profiles); when `None`, the home-derived paths
    /// are simply omitted rather than erroring.
    pub fn into_policy(self, cwd: &Path, cache_root: &Path, home: Option<&Path>) -> Policy {
        let cwd = cwd.to_string_lossy().into_owned();
        let cache = cache_root.to_string_lossy().into_owned();
        let base = Policy {
            nested_pty: true,
            ..Policy::default()
        };
        match self {
            Profile::Strict => Policy {
                readonly_paths: vec![cache, cwd],
                network: Network::Block,
                lpac: true,
                ..base
            },
            Profile::Project => {
                let mut readonly = vec![cache];
                if let Some(h) = home {
                    readonly.push(h.join(".config").to_string_lossy().into_owned());
                }
                Policy {
                    readonly_paths: readonly,
                    readwrite_paths: vec![cwd],
                    network: Network::Block,
                    ..base
                }
            }
            Profile::Permissive => {
                let mut readonly = Vec::new();
                if let Some(h) = home {
                    readonly.push(h.to_string_lossy().into_owned());
                }
                Policy {
                    readonly_paths: readonly,
                    readwrite_paths: vec![cwd],
                    network: Network::Allow,
                    ..base
                }
            }
        }
    }
}

/// The `[tool.sandbox]` table in `.bx.toml` (and the shape the `--sandbox`
/// flag / `BX_SANDBOX_DEFAULT` desugar into). A bare profile name is the
/// common case; the path/network fields are additive escape hatches layered
/// on top of the profile.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    /// Profile name. Defaults to `strict` when the table is present but the
    /// key is omitted.
    #[serde(default = "default_profile_name")]
    pub profile: String,
    /// Extra read-only paths added to the profile's set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readonly_paths: Vec<String>,
    /// Extra read-write paths added to the profile's set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readwrite_paths: Vec<String>,
    /// Paths to mask off (wins over any allow).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_paths: Vec<String>,
    /// Override the profile's network disposition. `None` keeps the profile
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_network: Option<bool>,
}

fn default_profile_name() -> String {
    "strict".to_string()
}

impl Config {
    /// Build a `Config` from a bare profile name (the `--sandbox <name>` /
    /// `BX_SANDBOX_DEFAULT=<name>` forms).
    pub fn from_profile(name: &str) -> Self {
        Config {
            profile: name.to_string(),
            ..Config::default()
        }
    }

    /// Resolve this config into an absolute-path [`Policy`].
    pub fn into_policy(self, cwd: &Path, cache_root: &Path, home: Option<&Path>) -> Result<Policy> {
        let profile: Profile = self.profile.parse()?;
        let mut policy = profile.into_policy(cwd, cache_root, home);
        policy.readonly_paths.extend(self.readonly_paths);
        policy.readwrite_paths.extend(self.readwrite_paths);
        policy.denied_paths.extend(self.denied_paths);
        if let Some(allow) = self.allow_network {
            policy.network = if allow {
                Network::Allow
            } else {
                Network::Block
            };
        }
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paths() -> (PathBuf, PathBuf, PathBuf) {
        (
            PathBuf::from("/work/project"),
            PathBuf::from("/home/u/.cache/bx"),
            PathBuf::from("/home/u"),
        )
    }

    #[test]
    fn strict_reads_cache_and_cwd_no_network() {
        let (cwd, cache, home) = paths();
        let p = Profile::Strict.into_policy(&cwd, &cache, Some(&home));
        assert!(p.readonly_paths.contains(&"/work/project".to_string()));
        assert!(p.readonly_paths.contains(&"/home/u/.cache/bx".to_string()));
        assert!(p.readwrite_paths.is_empty());
        assert_eq!(p.network, Network::Block);
        // strict is the only profile that opts into LPAC (Windows backend).
        assert!(p.lpac);
    }

    #[test]
    fn project_adds_cwd_write_and_config_read() {
        let (cwd, cache, home) = paths();
        let p = Profile::Project.into_policy(&cwd, &cache, Some(&home));
        assert!(p
            .readwrite_paths
            .contains(&cwd.to_string_lossy().into_owned()));
        // Build the expected path with `join` so the separator matches the
        // platform (the code does `home.join(".config")`; Windows uses `\`).
        let expected_config = home.join(".config").to_string_lossy().into_owned();
        assert!(p.readonly_paths.contains(&expected_config));
        assert_eq!(p.network, Network::Block);
        assert!(!p.lpac);
    }

    #[test]
    fn permissive_allows_network_and_home_read() {
        let (cwd, cache, home) = paths();
        let p = Profile::Permissive.into_policy(&cwd, &cache, Some(&home));
        assert!(p.readonly_paths.contains(&"/home/u".to_string()));
        assert!(p.readwrite_paths.contains(&"/work/project".to_string()));
        assert_eq!(p.network, Network::Allow);
    }

    #[test]
    fn profile_parse_rejects_garbage() {
        assert!("nope".parse::<Profile>().is_err());
        assert_eq!("Strict".parse::<Profile>().unwrap(), Profile::Strict);
    }

    #[test]
    fn config_overrides_layer_on_profile() {
        let (cwd, cache, home) = paths();
        let cfg = Config {
            profile: "strict".into(),
            readwrite_paths: vec!["/tmp/out".into()],
            denied_paths: vec!["/work/project/.env".into()],
            allow_network: Some(true),
            ..Config::default()
        };
        let p = cfg.into_policy(&cwd, &cache, Some(&home)).unwrap();
        assert!(p.readwrite_paths.contains(&"/tmp/out".to_string()));
        assert!(p.denied_paths.contains(&"/work/project/.env".to_string()));
        assert_eq!(p.network, Network::Allow);
    }

    #[test]
    fn home_none_omits_home_paths_without_erroring() {
        let (cwd, cache, _) = paths();
        let p = Profile::Permissive.into_policy(&cwd, &cache, None);
        assert!(p.readonly_paths.is_empty());
    }
}
