//! Download a release asset and extract it into the cache.

use crate::asset;
use crate::cache;
use crate::checksum;
use crate::error::{BxError, Result};
use crate::github::{Asset, Resolved};
use crate::platform::Platform;
use crate::spec::Spec;
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const USER_AGENT: &str = concat!("bx/", env!("CARGO_PKG_VERSION"));

/// Result of a successful fetch + extract. Carries the computed archive
/// SHA-256 so callers (e.g. `bx add` / `bx ensure --record`) can persist
/// it to a `.bx.toml` manifest without re-hashing.
pub struct Fetched {
    pub binary: PathBuf,
    pub archive_sha256: String,
}

/// Make sure the cache directory contains the binary we want, downloading
/// and extracting if not. If `expected_sha256` is supplied, the archive is
/// verified against it after download and a mismatch aborts the install.
pub async fn ensure(
    resolved: &Resolved,
    platform: &Platform,
    cache_dir: &Path,
    spec: &Spec,
    expected_sha256: Option<&str>,
) -> Result<Fetched> {
    let chosen = asset::select(platform, &resolved.tag, &resolved.release.assets)?;
    tracing::info!(asset = %chosen.name, "downloading");

    std::fs::create_dir_all(cache_dir)?;
    let tmp_dir = tempfile::tempdir_in(cache_dir.parent().unwrap_or(cache_dir))?;
    let downloaded = download(chosen, tmp_dir.path()).await?;

    let archive_sha256 = checksum::sha256_hex(&downloaded)?;
    if let Some(expected) = expected_sha256 {
        if !checksum::equal(expected, &archive_sha256) {
            return Err(BxError::ChecksumMismatch {
                expected: expected.to_string(),
                actual: archive_sha256,
                asset: chosen.name.clone(),
            });
        }
        tracing::debug!(asset = %chosen.name, "checksum verified");
    }

    extract_or_place(&downloaded, &chosen.name, cache_dir)?;

    let preferred_name = spec.binary.as_deref().unwrap_or(&spec.repo);
    // Some publishers ship tarballs whose binary entry has mode 0o644 —
    // tar's unpack faithfully preserves that and find_binary then rejects
    // the file. Repair the exec bit here so `bx` Just Works on those.
    ensure_target_executable(cache_dir, preferred_name)?;
    let binary = cache::find_binary(cache_dir, preferred_name)?;
    Ok(Fetched {
        binary,
        archive_sha256,
    })
}

#[cfg(unix)]
fn ensure_target_executable(dir: &Path, name: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
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
            if file_name == name {
                let mut perms = std::fs::metadata(&path)?.permissions();
                let mode = perms.mode();
                if mode & 0o111 == 0 {
                    perms.set_mode(mode | 0o755);
                    std::fs::set_permissions(&path, perms)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_target_executable(_dir: &Path, _name: &str) -> Result<()> {
    Ok(())
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
