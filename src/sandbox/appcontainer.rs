//! Windows AppContainer plan generation.
//!
//! Adapted in spirit from MXC's Windows backend (MIT), which runs untrusted
//! agent code in an AppContainer — optionally a Less-Privileged AppContainer
//! (LPAC). Like [`super::seatbelt`] and [`super::bwrap`] this module is
//! **pure**: it turns a [`Policy`] into a [`WindowsPlan`] with no syscalls, so
//! it compiles and is unit-tested on every host. The Win32 application of the
//! plan (derive the profile SID, add/revert the ACE grants, `CreateProcessW`
//! with `SECURITY_CAPABILITIES`) is windows-only and lives in `exec.rs`.
//!
//! Why a *plan* and not a profile string: AppContainer's filesystem model is
//! ACL-based, not a launch-time allow-list like the other two backends. The
//! kernel checks each access against the file's DACL using the token's package
//! SID + capability SIDs. So the plan enumerates three things the Win32 layer
//! needs: (a) which network capabilities to put in `SECURITY_CAPABILITIES`,
//! (b) whether to opt out of `ALL APPLICATION PACKAGES` (LPAC), and (c) the
//! per-path ACE grants exec.rs must add before launch and revert after exit so
//! the contained child can read+exec the binary out of the read-only cache and
//! touch its cwd. Reverting on exit is why the grants travel in the plan rather
//! than being baked into a string.

use super::{Network, Policy};

/// Filesystem access an ACE grant confers on the AppContainer package SID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read + execute — binaries and read-only trees.
    Read,
    /// Read + write + execute — the cwd subtree under looser profiles.
    ReadWrite,
}

/// A single path the Win32 layer must grant the package SID before launch and
/// revert afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AceGrant {
    pub path: String,
    pub access: Access,
}

/// Well-known AppContainer capabilities bx may request. Network is the only
/// axis [`Policy`] exposes, mirroring the all-or-nothing model the other
/// backends enforce; each maps to a fixed capability SID (`S-1-15-3-<rid>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// `internetClient` — outbound connections as a client (RID 1).
    InternetClient,
    /// `internetClientServer` — also accept inbound from the internet (RID 2).
    InternetClientServer,
    /// `privateNetworkClientServer` — local/LAN access incl. loopback (RID 3).
    PrivateNetworkClientServer,
}

impl Capability {
    /// The capability's well-known relative identifier under `S-1-15-3-*`.
    pub fn rid(self) -> u32 {
        match self {
            Capability::InternetClient => 1,
            Capability::InternetClientServer => 2,
            Capability::PrivateNetworkClientServer => 3,
        }
    }
}

/// A fully-resolved AppContainer launch plan, consumed by `exec.rs` on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsPlan {
    /// Stable per-binary AppContainer profile name (<= 64 chars, derived from
    /// the binary path so distinct tools get distinct package SIDs).
    pub profile_name: String,
    /// Opt out of `ALL APPLICATION PACKAGES` (Less-Privileged AppContainer).
    pub lpac: bool,
    /// Capability SIDs to place in `SECURITY_CAPABILITIES`. Empty ⇒ no network.
    pub capabilities: Vec<Capability>,
    /// Per-path ACEs to add before launch and revert after exit.
    pub grants: Vec<AceGrant>,
    /// Paths to explicitly DENY the package SID (wins over a broader grant).
    pub denies: Vec<String>,
}

/// Build the AppContainer plan for running `binary` under `policy`.
pub fn build_plan(policy: &Policy, binary: &str) -> WindowsPlan {
    let mut capabilities = Vec::new();
    if policy.network == Network::Allow {
        capabilities.push(Capability::InternetClient);
        capabilities.push(Capability::PrivateNetworkClientServer);
    }
    if policy.allow_local_network {
        if !capabilities.contains(&Capability::PrivateNetworkClientServer) {
            capabilities.push(Capability::PrivateNetworkClientServer);
        }
        capabilities.push(Capability::InternetClientServer);
    }

    let mut grants: Vec<AceGrant> = Vec::new();
    for p in &policy.readonly_paths {
        grants.push(AceGrant {
            path: p.clone(),
            access: Access::Read,
        });
    }
    for p in &policy.readwrite_paths {
        grants.push(AceGrant {
            path: p.clone(),
            access: Access::ReadWrite,
        });
    }
    // The binary must be readable+executable even if no readonly path happens
    // to cover it (e.g. a hand-built Policy). Cheap insurance against a child
    // that cannot even start; skip when an existing grant already contains it.
    if !grants.iter().any(|g| binary.starts_with(&g.path)) {
        grants.push(AceGrant {
            path: binary.to_string(),
            access: Access::Read,
        });
    }

    WindowsPlan {
        profile_name: profile_name_for(binary),
        lpac: policy.lpac,
        capabilities,
        grants,
        denies: policy.denied_paths.clone(),
    }
}

/// Deterministic AppContainer profile name derived from the binary path.
/// AppContainer names are capped at 64 chars and may only contain a restricted
/// character set; a fixed `bx_` prefix plus a SHA-256 hash slice (hex) is well
/// within both and keeps distinct tools in distinct package SIDs.
fn profile_name_for(binary: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(binary.as_bytes());
    format!("bx_{}", hex::encode(&digest[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Policy {
        Policy {
            nested_pty: true,
            ..Policy::default()
        }
    }

    #[test]
    fn network_block_requests_no_capabilities() {
        let plan = build_plan(&base(), r"C:\cache\tool.exe");
        assert!(plan.capabilities.is_empty());
    }

    #[test]
    fn network_allow_requests_internet_client() {
        let mut p = base();
        p.network = Network::Allow;
        let plan = build_plan(&p, r"C:\cache\tool.exe");
        assert!(plan.capabilities.contains(&Capability::InternetClient));
    }

    #[test]
    fn allow_local_network_requests_inbound_and_private() {
        let mut p = base();
        p.allow_local_network = true;
        let plan = build_plan(&p, r"C:\cache\tool.exe");
        assert!(plan
            .capabilities
            .contains(&Capability::PrivateNetworkClientServer));
        assert!(plan
            .capabilities
            .contains(&Capability::InternetClientServer));
    }

    #[test]
    fn readonly_and_readwrite_paths_become_grants() {
        let mut p = base();
        p.readonly_paths = vec![r"C:\cache".into()];
        p.readwrite_paths = vec![r"C:\work".into()];
        let plan = build_plan(&p, r"C:\cache\tool.exe");
        assert!(plan.grants.contains(&AceGrant {
            path: r"C:\cache".into(),
            access: Access::Read,
        }));
        assert!(plan.grants.contains(&AceGrant {
            path: r"C:\work".into(),
            access: Access::ReadWrite,
        }));
    }

    #[test]
    fn binary_outside_any_grant_gets_its_own_read_grant() {
        // No readonly path covers the binary, so it must be granted directly.
        let plan = build_plan(&base(), r"C:\elsewhere\tool.exe");
        assert!(plan.grants.contains(&AceGrant {
            path: r"C:\elsewhere\tool.exe".into(),
            access: Access::Read,
        }));
    }

    #[test]
    fn binary_under_existing_grant_is_not_duplicated() {
        let mut p = base();
        p.readonly_paths = vec![r"C:\cache".into()];
        let plan = build_plan(&p, r"C:\cache\tool.exe");
        // Only the cache grant — no extra grant for the binary path itself.
        assert_eq!(plan.grants.len(), 1);
    }

    #[test]
    fn lpac_flows_from_policy() {
        let mut p = base();
        p.lpac = true;
        assert!(build_plan(&p, r"C:\cache\tool.exe").lpac);
        p.lpac = false;
        assert!(!build_plan(&p, r"C:\cache\tool.exe").lpac);
    }

    #[test]
    fn denied_paths_carried_into_plan() {
        let mut p = base();
        p.denied_paths = vec![r"C:\work\.env".into()];
        let plan = build_plan(&p, r"C:\cache\tool.exe");
        assert_eq!(plan.denies, vec![r"C:\work\.env".to_string()]);
    }

    #[test]
    fn profile_name_is_deterministic_bounded_and_distinct() {
        let a = build_plan(&base(), r"C:\cache\a.exe").profile_name;
        let a2 = build_plan(&base(), r"C:\cache\a.exe").profile_name;
        let b = build_plan(&base(), r"C:\cache\b.exe").profile_name;
        assert_eq!(a, a2, "same binary ⇒ same profile name");
        assert_ne!(a, b, "different binaries ⇒ different profile names");
        assert!(a.starts_with("bx_"));
        assert!(a.len() <= 64);
    }

    #[test]
    fn capability_rids_match_well_known_values() {
        assert_eq!(Capability::InternetClient.rid(), 1);
        assert_eq!(Capability::InternetClientServer.rid(), 2);
        assert_eq!(Capability::PrivateNetworkClientServer.rid(), 3);
    }
}
