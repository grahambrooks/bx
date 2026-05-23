//! `bx` — fetch and run binaries from GitHub releases.
//!
//! See [`run`] for the top-level entry point used by the CLI, and the
//! individual modules for the building blocks.

pub mod asset;
pub mod cache;
pub mod checksum;
pub mod error;
pub mod exec;
pub mod fetch;
pub mod github;
pub mod manifest;
pub mod platform;
pub mod prune;
pub mod spec;

pub use error::{BxError, Result};

use std::path::{Path, PathBuf};

/// Resolve a spec, ensure the binary is in the cache, and exec it with the
/// given args (stdio inherited from the calling process).
///
/// Pinned refs (`@v1.0.0`) take a fast path: if the binary is already in the
/// cache, we exec it without any network call at all. This makes MCP servers
/// fired from a config snappy after the first run. Unpinned (`@latest`) refs
/// always resolve through the GitHub API because that's what "latest" means.
///
/// If a `.bx.toml` manifest is found in the cwd ancestry and contains an
/// entry whose `spec` field matches this invocation exactly, any recorded
/// per-platform checksum is enforced on download. Cache hits skip
/// verification — the lockfile-style contract is "verify once on install,
/// trust on use."
pub async fn run(spec: &spec::Spec, args: &[String], refresh: bool) -> Result<i32> {
    let platform = platform::Platform::current()?;
    tracing::debug!(?platform, "detected platform");

    // Fast path for pinned refs.
    if !refresh {
        if let spec::Ref::Tag(tag) = &spec.reference {
            let cache_dir = cache::binary_dir(spec, tag)?;
            if let Ok(binary) = find_binary(&cache_dir, spec) {
                tracing::debug!(?binary, "cache hit (pinned ref)");
                return exec::run(&binary, args);
            }
        }
    }

    let resolved = github::resolve(spec).await?;
    tracing::debug!(tag = %resolved.tag, "resolved release");

    let cache_dir = cache::binary_dir(spec, &resolved.tag)?;
    let binary_path = if refresh || !binary_in_cache(&cache_dir, spec) {
        let expected = expected_checksum_for(spec, &platform);
        let fetched =
            fetch::ensure(&resolved, &platform, &cache_dir, spec, expected.as_deref()).await?;
        fetched.binary
    } else {
        find_binary(&cache_dir, spec)?
    };

    tracing::debug!(?binary_path, "executing");
    exec::run(&binary_path, args)
}

/// Run `bx ensure` against a manifest path (or walk-up from cwd if `None`).
/// Fetches every tool listed in the manifest, verifying recorded checksums
/// and (if `record` is true) recording the computed checksum for any
/// platform that was previously unrecorded.
pub async fn ensure(manifest_path: Option<&Path>, record: bool) -> Result<()> {
    let platform = platform::Platform::current()?;
    let platform_slug = platform.to_string();

    let path = resolve_manifest_path(manifest_path)?;
    let mut m = manifest::Manifest::load(&path)?;

    if m.tools.is_empty() {
        println!("bx ensure: manifest has no tools — nothing to do");
        return Ok(());
    }

    let mut changed = false;
    for tool_spec in m.tools.iter().map(|t| t.spec.clone()).collect::<Vec<_>>() {
        let spec: spec::Spec = tool_spec.parse()?;
        let resolved = github::resolve(&spec).await?;
        let cache_dir = cache::binary_dir(&spec, &resolved.tag)?;

        let expected = m
            .tool(&tool_spec)
            .and_then(|t| t.checksums.get(&platform_slug).cloned());

        if binary_in_cache(&cache_dir, &spec) && expected.is_some() {
            // Cache already has it AND we have a recorded checksum that was
            // enforced when it was first installed — skip re-fetch.
            println!("  ok: {tool_spec} (cached)");
            continue;
        }

        let fetched =
            fetch::ensure(&resolved, &platform, &cache_dir, &spec, expected.as_deref()).await?;

        if record && expected.is_none() {
            if m.record_checksum(&tool_spec, &platform_slug, &fetched.archive_sha256) {
                changed = true;
                println!(
                    "  recorded: {tool_spec} [{platform_slug}] = {}",
                    fetched.archive_sha256
                );
            }
        } else {
            println!("  ok: {tool_spec}");
        }
    }

    if changed {
        m.save(&path)?;
        println!("bx ensure: manifest updated ({})", path.display());
    }
    Ok(())
}

/// Run `bx add <spec>`. Resolves the spec to a concrete tag (rewriting
/// `@latest` if necessary so the manifest stays pin-shaped), fetches it,
/// and records the archive checksum for the current platform in the
/// manifest at `path` (or `./.bx.toml` if none given).
pub async fn add(spec_str: &str, path: Option<&Path>) -> Result<()> {
    let platform = platform::Platform::current()?;
    let platform_slug = platform.to_string();

    let mut spec: spec::Spec = spec_str.parse()?;
    let resolved = github::resolve(&spec).await?;

    // Pin the spec to the concrete tag so the manifest is reproducible —
    // `bx add owner/repo` (latest) writes `owner/repo@v2026.5.23`.
    spec.reference = spec::Ref::Tag(resolved.tag.clone());
    let pinned_spec_str = spec.to_string();

    let cache_dir = cache::binary_dir(&spec, &resolved.tag)?;
    let fetched = fetch::ensure(&resolved, &platform, &cache_dir, &spec, None).await?;

    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(manifest::FILE_NAME));
    let mut m = if path.is_file() {
        manifest::Manifest::load(&path)?
    } else {
        manifest::Manifest::default()
    };

    m.record_checksum(&pinned_spec_str, &platform_slug, &fetched.archive_sha256);
    m.save(&path)?;
    println!(
        "bx add: {pinned_spec_str} [{platform_slug}] = {}\n         -> {}",
        fetched.archive_sha256,
        path.display()
    );
    Ok(())
}

fn resolve_manifest_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    manifest::find(&cwd).ok_or_else(|| {
        BxError::Manifest(format!(
            "no {} found in {} or any parent directory",
            manifest::FILE_NAME,
            cwd.display()
        ))
    })
}

/// If the cwd ancestry contains a `.bx.toml` and it lists this spec exactly,
/// return the checksum recorded for the current platform (if any).
fn expected_checksum_for(spec: &spec::Spec, platform: &platform::Platform) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let path = manifest::find(&cwd)?;
    let m = manifest::Manifest::load(&path).ok()?;
    let spec_str = spec.to_string();
    m.tool(&spec_str)?
        .checksums
        .get(&platform.to_string())
        .cloned()
}

fn binary_in_cache(cache_dir: &Path, spec: &spec::Spec) -> bool {
    find_binary(cache_dir, spec).is_ok()
}

fn find_binary(cache_dir: &Path, spec: &spec::Spec) -> Result<PathBuf> {
    let preferred = spec.binary.as_deref().unwrap_or(&spec.repo);
    cache::find_binary(cache_dir, preferred)
}
