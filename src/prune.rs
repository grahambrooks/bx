//! `bx prune` — garbage-collect old cache entries.
//!
//! Default policy: for each `<owner>/<repo>/`, keep the N most-recently-modified
//! tag directories and remove the rest. `--all` removes every tag dir. `--dry-run`
//! prints the plan without touching the filesystem.
//!
//! Newness is judged by directory mtime, which is updated by `fetch::ensure`
//! on extraction and by `lib::run`'s cache hit (the binary path is touched at
//! exec time via the kernel's atime/mtime semantics on most platforms). The
//! coarse-grained choice is intentional — recording an explicit "last used"
//! timestamp would mean either an extra file per cache dir or a sidecar
//! database; both add complexity for marginal gain at this scale.

use crate::cache;
use crate::error::Result;
use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy)]
pub struct PruneOpts {
    pub keep: usize,
    pub all: bool,
    pub dry_run: bool,
}

/// Entry point used by the CLI. Resolves the real cache root, then delegates
/// to `prune_at` (the testable inner function).
pub fn run(opts: PruneOpts) -> Result<()> {
    let root = cache::root()?;
    if !root.exists() {
        println!("bx prune: cache directory does not exist; nothing to do");
        return Ok(());
    }
    prune_at(&root, opts)
}

pub fn prune_at(root: &Path, opts: PruneOpts) -> Result<()> {
    let removals = plan(root, &opts)?;

    if removals.is_empty() {
        println!("bx prune: nothing to remove");
        return Ok(());
    }

    let verb = if opts.dry_run { "would remove" } else { "removed" };
    let mut total: u64 = 0;
    for removal in &removals {
        let display_path = removal.path.strip_prefix(root).unwrap_or(&removal.path);
        println!(
            "  {verb}: {} ({})",
            display_path.display(),
            format_size(removal.size)
        );
        if !opts.dry_run {
            fs::remove_dir_all(&removal.path)?;
        }
        total += removal.size;
    }
    println!(
        "bx prune: {} {} entries, {} freed",
        verb,
        removals.len(),
        format_size(total)
    );
    Ok(())
}

#[derive(Debug)]
struct Removal {
    path: PathBuf,
    size: u64,
}

fn plan(root: &Path, opts: &PruneOpts) -> Result<Vec<Removal>> {
    let mut removals = Vec::new();

    for owner in read_subdirs(root)? {
        for repo in read_subdirs(&owner)? {
            let mut tags: Vec<(PathBuf, SystemTime)> = read_subdirs(&repo)?
                .into_iter()
                .filter_map(|p| {
                    let mt = fs::metadata(&p).ok()?.modified().ok()?;
                    Some((p, mt))
                })
                .collect();

            // Newest first.
            tags.sort_by_key(|(_, mt)| Reverse(*mt));

            let skip = if opts.all { 0 } else { opts.keep.min(tags.len()) };
            for (path, _) in tags.into_iter().skip(skip) {
                let size = dir_size(&path);
                removals.push(Removal { path, size });
            }
        }
    }

    Ok(removals)
}

fn read_subdirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.path());
        }
    }
    Ok(out)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// Create `<root>/<rel>/dummy` for each entry, sleeping briefly between so
    /// each tag dir gets a distinct mtime. Modern FS (APFS/ext4) has at least
    /// ms resolution; the sleep guards against runners with coarser clocks.
    fn populate(root: &Path, entries: &[&str]) {
        for rel in entries {
            let dir = root.join(rel);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dummy"), b"x").unwrap();
            thread::sleep(Duration::from_millis(15));
        }
    }

    fn opts(keep: usize, all: bool, dry_run: bool) -> PruneOpts {
        PruneOpts { keep, all, dry_run }
    }

    #[test]
    fn empty_cache_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        prune_at(tmp.path(), opts(1, false, false)).unwrap();
    }

    #[test]
    fn keeps_newest_tag_per_repo() {
        let tmp = tempfile::tempdir().unwrap();
        populate(tmp.path(), &["o/r/v1.0", "o/r/v1.1", "o/r/v1.2"]);
        prune_at(tmp.path(), opts(1, false, false)).unwrap();
        assert!(!tmp.path().join("o/r/v1.0").exists());
        assert!(!tmp.path().join("o/r/v1.1").exists());
        assert!(tmp.path().join("o/r/v1.2").exists());
    }

    #[test]
    fn keep_n_preserves_n_newest() {
        let tmp = tempfile::tempdir().unwrap();
        populate(tmp.path(), &["o/r/v1", "o/r/v2", "o/r/v3", "o/r/v4"]);
        prune_at(tmp.path(), opts(2, false, false)).unwrap();
        assert!(!tmp.path().join("o/r/v1").exists());
        assert!(!tmp.path().join("o/r/v2").exists());
        assert!(tmp.path().join("o/r/v3").exists());
        assert!(tmp.path().join("o/r/v4").exists());
    }

    #[test]
    fn repos_pruned_independently() {
        let tmp = tempfile::tempdir().unwrap();
        populate(
            tmp.path(),
            &["o1/r1/v1", "o1/r1/v2", "o2/r2/v1", "o2/r2/v2"],
        );
        prune_at(tmp.path(), opts(1, false, false)).unwrap();
        assert!(!tmp.path().join("o1/r1/v1").exists());
        assert!(tmp.path().join("o1/r1/v2").exists());
        assert!(!tmp.path().join("o2/r2/v1").exists());
        assert!(tmp.path().join("o2/r2/v2").exists());
    }

    #[test]
    fn all_removes_everything() {
        let tmp = tempfile::tempdir().unwrap();
        populate(tmp.path(), &["o/r/v1", "o/r/v2"]);
        prune_at(tmp.path(), opts(99, true, false)).unwrap();
        assert!(!tmp.path().join("o/r/v1").exists());
        assert!(!tmp.path().join("o/r/v2").exists());
    }

    #[test]
    fn dry_run_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        populate(tmp.path(), &["o/r/v1", "o/r/v2"]);
        prune_at(tmp.path(), opts(1, false, true)).unwrap();
        assert!(tmp.path().join("o/r/v1").exists());
        assert!(tmp.path().join("o/r/v2").exists());
    }

    #[test]
    fn format_size_thresholds() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }
}
