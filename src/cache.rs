//! Cache layout:
//!
//! ```text
//! $XDG_CACHE_HOME/bx/                (Linux: ~/.cache/bx)
//! $HOME/Library/Caches/bx/           (macOS)
//! %LOCALAPPDATA%\bx\cache\            (Windows)
//!   └─ <owner>/<repo>/<tag>/
//!       ├─ symgraph                    # the binary
//!       ├─ LICENSE
//!       └─ ...
//! ```
//!
//! One directory per resolved tag. We do not GC automatically — `bx prune`
//! will land later.

use crate::error::{BxError, Result};
use crate::spec::Spec;
use directories::ProjectDirs;
use std::path::{Path, PathBuf};

pub fn root() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "bx", "bx").ok_or(BxError::NoCacheDir)?;
    Ok(dirs.cache_dir().to_path_buf())
}

pub fn binary_dir(spec: &Spec, tag: &str) -> Result<PathBuf> {
    let safe_tag = sanitise(tag);
    Ok(root()?.join(&spec.owner).join(&spec.repo).join(safe_tag))
}

/// Look for an executable in a cache directory. Tries, in order:
/// 1. `<dir>/<name>` (and `<name>.exe` on Windows)
/// 2. `<dir>/bin/<name>`
/// 3. The first executable file whose name matches `<name>` anywhere in `<dir>`
///    (handles archives that expand to a versioned subdirectory).
pub fn find_binary(dir: &Path, name: &str) -> Result<PathBuf> {
    let candidates = [
        dir.join(name),
        #[cfg(windows)]
        dir.join(format!("{name}.exe")),
        dir.join("bin").join(name),
        #[cfg(windows)]
        dir.join("bin").join(format!("{name}.exe")),
    ];

    for candidate in &candidates {
        if is_executable(candidate) {
            return Ok(candidate.clone());
        }
    }

    // Fall back to walking the directory looking for a matching executable.
    if let Some(found) = walk_for(dir, name)? {
        return Ok(found);
    }

    Err(BxError::BinaryNotFound {
        name: name.to_string(),
        dir: dir.to_path_buf(),
    })
}

fn walk_for(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let stem = file_name.trim_end_matches(".exe");
            if stem == name && is_executable(&path) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()),
        Some(ref ext) if matches!(ext.as_str(), "exe" | "bat" | "cmd")
    )
}

fn sanitise(tag: &str) -> String {
    // Tags are usually safe (vX.Y.Z), but a defensive replace keeps the cache
    // path predictable if someone passes a SHA or branch with slashes later.
    tag.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_replaces_path_separators() {
        assert_eq!(sanitise("v1.0.0"), "v1.0.0");
        assert_eq!(sanitise("feature/branch"), "feature_branch");
    }
}
