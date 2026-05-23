//! Download a release asset and extract it into the cache.

use crate::asset;
use crate::cache;
use crate::error::{BxError, Result};
use crate::github::{Asset, Resolved};
use crate::platform::Platform;
use crate::spec::Spec;
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const USER_AGENT: &str = concat!("bx/", env!("CARGO_PKG_VERSION"));

/// Make sure the cache directory contains the binary we want, downloading
/// and extracting if not. Returns the path to the resolved executable.
pub async fn ensure(
    resolved: &Resolved,
    platform: &Platform,
    cache_dir: &Path,
    spec: &Spec,
) -> Result<PathBuf> {
    let chosen = asset::select(platform, &resolved.tag, &resolved.release.assets)?;
    tracing::info!(asset = %chosen.name, "downloading");

    std::fs::create_dir_all(cache_dir)?;
    let tmp_dir = tempfile::tempdir_in(cache_dir.parent().unwrap_or(cache_dir))?;
    let downloaded = download(chosen, tmp_dir.path()).await?;
    extract_or_place(&downloaded, &chosen.name, cache_dir)?;

    let preferred_name = spec.binary.as_deref().unwrap_or(&spec.repo);
    cache::find_binary(cache_dir, preferred_name)
}

async fn download(asset: &Asset, into: &Path) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(BxError::from)?;

    let response = client.get(&asset.browser_download_url).send().await?;
    if !response.status().is_success() {
        return Err(BxError::GitHubApi(format!(
            "download failed: {} {}",
            response.status(),
            asset.browser_download_url
        )));
    }

    let out_path = into.join(&asset.name);
    let mut file = tokio::fs::File::create(&out_path).await?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(out_path)
}

fn extract_or_place(downloaded: &Path, name: &str, cache_dir: &Path) -> Result<()> {
    let lower = name.to_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_tar_gz(downloaded, cache_dir)
    } else if lower.ends_with(".zip") {
        extract_zip(downloaded, cache_dir)
    } else {
        // Bare binary — move it into place and chmod +x on Unix.
        let target = cache_dir.join(strip_archive_suffix(name));
        std::fs::copy(downloaded, &target)?;
        make_executable(&target)?;
        Ok(())
    }
}

fn extract_tar_gz(archive: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(into)
        .map_err(|e| BxError::Archive(format!("tar.gz extract failed: {e}")))?;
    Ok(())
}

fn extract_zip(archive: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    zip.extract(into)?;
    // ZipArchive::extract preserves Unix permissions only when the central
    // directory has them. Add +x where it's a regular file but not marked
    // executable, on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![into.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o111 == 0 {
                        let mut perms = meta.permissions();
                        perms.set_mode(mode | 0o755);
                        let _ = std::fs::set_permissions(&path, perms);
                    }
                }
            }
        }
    }
    Ok(())
}

fn strip_archive_suffix(name: &str) -> &str {
    for suffix in [".tar.gz", ".tgz", ".zip", ".tar.xz", ".tar.bz2"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(windows)]
fn make_executable(_path: &Path) -> Result<()> {
    // On Windows, executability is determined by extension, not perms.
    Ok(())
}
