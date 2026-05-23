//! `bx` — fetch and run binaries from GitHub releases.
//!
//! See [`run`] for the top-level entry point used by the CLI, and the
//! individual modules for the building blocks.

pub mod asset;
pub mod cache;
pub mod error;
pub mod exec;
pub mod fetch;
pub mod github;
pub mod platform;
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
        fetch::ensure(&resolved, &platform, &cache_dir, spec).await?
    } else {
        find_binary(&cache_dir, spec)?
    };

    tracing::debug!(?binary_path, "executing");
    exec::run(&binary_path, args)
}

fn binary_in_cache(cache_dir: &Path, spec: &spec::Spec) -> bool {
    find_binary(cache_dir, spec).is_ok()
}

fn find_binary(cache_dir: &Path, spec: &spec::Spec) -> Result<PathBuf> {
    let preferred = spec.binary.as_deref().unwrap_or(&spec.repo);
    cache::find_binary(cache_dir, preferred)
}
