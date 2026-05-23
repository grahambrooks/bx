//! `.bx.toml` manifest — pinned tool versions + per-platform checksums.
//!
//! Schema:
//!
//! ```toml
//! [[tool]]
//! spec = "grahambrooks/symgraph@v2026.4.13"
//!
//! [tool.checksums]
//! darwin-arm64 = "abc..."
//! linux-x64    = "def..."
//! ```
//!
//! `spec` is matched against invocations with an exact string compare — a
//! manifest entry pinned at `@v1.0` will only be consulted when the user
//! invokes that exact form. Aliasing (`bx <name>` resolving via manifest)
//! is M3 territory.
//!
//! `checksums` keys are the platform slugs produced by `platform.rs::Display`
//! (e.g. `darwin-arm64`, `linux-x64`). Per-platform because the archive
//! contents differ across targets.
//!
//! `find` walks the cwd's ancestors looking for `.bx.toml`, matching the
//! ergonomic of cargo/npm/git locating their project files.

use crate::error::{BxError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = ".bx.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Manifest {
    #[serde(default, rename = "tool", skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tool {
    pub spec: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checksums: BTreeMap<String, String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text)
            .map_err(|e| BxError::Manifest(format!("{}: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| BxError::Manifest(format!("serialize: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn tool(&self, spec: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.spec == spec)
    }

    pub fn tool_mut(&mut self, spec: &str) -> Option<&mut Tool> {
        self.tools.iter_mut().find(|t| t.spec == spec)
    }

    /// Insert or overwrite the checksum for the given spec + platform slug.
    /// Creates the tool entry if missing. Returns true if the manifest was
    /// changed (caller decides whether to save).
    pub fn record_checksum(&mut self, spec: &str, platform: &str, sha256: &str) -> bool {
        if let Some(tool) = self.tool_mut(spec) {
            let existing = tool.checksums.get(platform);
            if existing.map(|s| s.as_str()) == Some(sha256) {
                return false;
            }
            tool.checksums.insert(platform.to_string(), sha256.to_string());
            return true;
        }
        let mut checksums = BTreeMap::new();
        checksums.insert(platform.to_string(), sha256.to_string());
        self.tools.push(Tool {
            spec: spec.to_string(),
            checksums,
        });
        true
    }
}

/// Walk from `start` up through parent directories looking for `.bx.toml`.
pub fn find(start: &Path) -> Option<PathBuf> {
    let mut current = start;
    loop {
        let candidate = current.join(FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        let parent = current.parent()?;
        if parent == current {
            return None;
        }
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_checksums() {
        let mut m = Manifest::default();
        m.record_checksum("owner/repo@v1.0", "linux-x64", "abc");
        m.record_checksum("owner/repo@v1.0", "darwin-arm64", "def");

        let tmp = tempfile::NamedTempFile::new().unwrap();
        m.save(tmp.path()).unwrap();
        let loaded = Manifest::load(tmp.path()).unwrap();

        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].spec, "owner/repo@v1.0");
        assert_eq!(loaded.tools[0].checksums.get("linux-x64").unwrap(), "abc");
        assert_eq!(loaded.tools[0].checksums.get("darwin-arm64").unwrap(), "def");
    }

    #[test]
    fn parses_minimal_manifest() {
        let text = r#"
[[tool]]
spec = "owner/repo@v1.0"
"#;
        let m: Manifest = toml::from_str(text).unwrap();
        assert_eq!(m.tools.len(), 1);
        assert!(m.tools[0].checksums.is_empty());
    }

    #[test]
    fn record_checksum_inserts_then_updates() {
        let mut m = Manifest::default();
        assert!(m.record_checksum("o/r@v1", "linux-x64", "aaa"));
        // Same value → no change.
        assert!(!m.record_checksum("o/r@v1", "linux-x64", "aaa"));
        // Different value → change.
        assert!(m.record_checksum("o/r@v1", "linux-x64", "bbb"));
        assert_eq!(m.tool("o/r@v1").unwrap().checksums["linux-x64"], "bbb");
    }

    #[test]
    fn find_walks_up_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.path().join(".bx.toml"), "").unwrap();
        assert_eq!(find(&nested).unwrap(), tmp.path().join(".bx.toml"));
    }

    #[test]
    fn find_returns_none_when_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find(tmp.path()).is_none());
    }
}
