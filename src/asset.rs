//! Score-and-rank release assets to pick the right one for the current
//! platform.
//!
//! Approach:
//! 1. Reject obvious non-binaries (checksums, signatures, source tarballs).
//! 2. Reward matches on this platform's OS and arch keywords.
//! 3. Penalise matches on OTHER platforms' keywords.
//! 4. Reward preferred archive extensions for this OS.
//! 5. Tie-break by file size (larger wins; usually the full bundle).
//!
//! The scorer is intentionally simple — we can replace it with a manifest
//! lookup later without touching callers.

use crate::error::{BxError, Result};
use crate::github::Asset;
use crate::platform::{Os, Platform};

/// Pick the best-matching asset for the given platform.
pub fn select<'a>(platform: &Platform, tag: &str, assets: &'a [Asset]) -> Result<&'a Asset> {
    let mut scored: Vec<(i32, &Asset)> = assets
        .iter()
        .filter(|a| !is_noise(&a.name))
        .map(|a| (score(platform, a), a))
        .filter(|(s, _)| *s > 0)
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.size.cmp(&a.1.size)));

    if let Some((_, asset)) = scored.first() {
        return Ok(*asset);
    }

    Err(BxError::NoMatchingAsset {
        platform: platform.to_string(),
        tag: tag.to_string(),
        assets: assets.iter().map(|a| a.name.clone()).collect(),
    })
}

fn is_noise(name: &str) -> bool {
    let lower = name.to_lowercase();
    let noise_suffixes = [
        ".sha256",
        ".sha512",
        ".md5",
        ".sig",
        ".asc",
        ".pem",
        ".sbom",
        ".sbom.json",
        ".intoto.jsonl",
        ".pub",
    ];
    if noise_suffixes.iter().any(|s| lower.ends_with(s)) {
        return true;
    }
    // Source archives from automatic GitHub release tarballs.
    if lower == "source code" || lower.starts_with("source-code") {
        return true;
    }
    // MCPB bundles aren't directly executable — they're for Claude Desktop.
    if lower.ends_with(".mcpb") {
        return true;
    }
    false
}

fn score(platform: &Platform, asset: &Asset) -> i32 {
    let lower = asset.name.to_lowercase();
    let mut score = 0;

    // Reward matches on our keywords.
    for kw in platform.os_keywords() {
        if lower.contains(kw) {
            score += 10;
            break; // any one OS match is enough — avoid double-counting macos+darwin
        }
    }
    for kw in platform.arch_keywords() {
        if lower.contains(kw) {
            score += 10;
            break;
        }
    }

    // Penalise matches on other platforms. These are strong negative signals.
    for kw in platform.other_os_keywords() {
        if lower.contains(kw) {
            score -= 50;
        }
    }
    for kw in platform.other_arch_keywords() {
        if lower.contains(kw) {
            score -= 50;
        }
    }

    // Reward this OS's preferred extension.
    let prefers_zip = platform.os == Os::Windows;
    let preferred_ext = if prefers_zip {
        lower.ends_with(".zip")
    } else {
        lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
    };
    if preferred_ext {
        score += 3;
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".tar.bz2") {
        score += 1;
    }

    // Slight preference for archives over bare binaries — they usually carry
    // additional files we may want (LICENSE, completions, etc.) and they're
    // platform-conventional.
    if is_archive(&lower) {
        score += 1;
    }

    score
}

fn is_archive(name_lower: &str) -> bool {
    name_lower.ends_with(".tar.gz")
        || name_lower.ends_with(".tgz")
        || name_lower.ends_with(".zip")
        || name_lower.ends_with(".tar.xz")
        || name_lower.ends_with(".tar.bz2")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Asset;
    use crate::platform::{Arch, Os};

    fn asset(name: &str, size: u64) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://example.com/{name}"),
            size,
            content_type: None,
        }
    }

    fn linux_x64() -> Platform {
        Platform {
            os: Os::Linux,
            arch: Arch::X64,
        }
    }

    fn darwin_arm64() -> Platform {
        Platform {
            os: Os::Darwin,
            arch: Arch::Arm64,
        }
    }

    fn windows_x64() -> Platform {
        Platform {
            os: Os::Windows,
            arch: Arch::X64,
        }
    }

    #[test]
    fn picks_linux_x64_from_symgraph_style_release() {
        let assets = vec![
            asset("symgraph-2026.4.13-darwin-arm64.tar.gz", 5_000_000),
            asset("symgraph-2026.4.13-darwin-x64.tar.gz", 5_100_000),
            asset("symgraph-2026.4.13-linux-x64.tar.gz", 5_200_000),
            asset("symgraph-2026.4.13-windows-x64.zip", 5_300_000),
            asset("checksums.txt", 256),
        ];
        let picked = select(&linux_x64(), "v2026.4.13", &assets).unwrap();
        assert_eq!(picked.name, "symgraph-2026.4.13-linux-x64.tar.gz");
    }

    #[test]
    fn picks_darwin_arm64_correctly() {
        let assets = vec![
            asset("tool-v1-darwin-arm64.tar.gz", 100),
            asset("tool-v1-darwin-x86_64.tar.gz", 100),
            asset("tool-v1-linux-aarch64.tar.gz", 100),
        ];
        let picked = select(&darwin_arm64(), "v1", &assets).unwrap();
        assert_eq!(picked.name, "tool-v1-darwin-arm64.tar.gz");
    }

    #[test]
    fn picks_windows_zip() {
        let assets = vec![
            asset("tool-v1-windows-x64.zip", 100),
            asset("tool-v1-windows-x64.tar.gz", 100),
            asset("tool-v1-linux-x64.tar.gz", 100),
        ];
        let picked = select(&windows_x64(), "v1", &assets).unwrap();
        assert_eq!(picked.name, "tool-v1-windows-x64.zip");
    }

    #[test]
    fn handles_apple_keyword() {
        let assets = vec![
            asset("tool-aarch64-apple-darwin.tar.gz", 100),
            asset("tool-x86_64-unknown-linux-gnu.tar.gz", 100),
        ];
        let picked = select(&darwin_arm64(), "v1", &assets).unwrap();
        assert_eq!(picked.name, "tool-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn ignores_checksums_and_sigs() {
        let assets = vec![
            asset("tool-linux-x64.tar.gz", 100),
            asset("tool-linux-x64.tar.gz.sha256", 64),
            asset("tool-linux-x64.tar.gz.sig", 64),
        ];
        let picked = select(&linux_x64(), "v1", &assets).unwrap();
        assert_eq!(picked.name, "tool-linux-x64.tar.gz");
    }

    #[test]
    fn ignores_mcpb_bundles() {
        let assets = vec![
            asset("symgraph-2026.4.13-linux-x64.tar.gz", 5_000_000),
            asset("symgraph-2026.4.13-linux-x64.mcpb", 5_000_000),
        ];
        let picked = select(&linux_x64(), "v2026.4.13", &assets).unwrap();
        assert!(picked.name.ends_with(".tar.gz"));
    }

    #[test]
    fn errors_when_no_match() {
        let assets = vec![asset("tool-windows-x64.zip", 100)];
        let err = select(&linux_x64(), "v1", &assets).unwrap_err();
        assert!(matches!(err, BxError::NoMatchingAsset { .. }));
    }

    #[test]
    fn tie_breaks_by_size() {
        // Two assets with identical scoring keywords; bigger should win.
        let assets = vec![
            asset("tool-small-linux-x64.tar.gz", 1_000),
            asset("tool-full-linux-x64.tar.gz", 5_000_000),
        ];
        let picked = select(&linux_x64(), "v1", &assets).unwrap();
        assert_eq!(picked.name, "tool-full-linux-x64.tar.gz");
    }
}
