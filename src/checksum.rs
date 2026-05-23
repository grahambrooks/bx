//! SHA-256 helpers for archive checksum verification.
//!
//! We hash the *archive* (what we downloaded), not the extracted binary,
//! because:
//! - The archive is what travels the wire — that's where tampering happens.
//! - Extraction is non-deterministic (umask, attribute preservation, the
//!   chmod-+x repair in `fetch.rs::ensure_target_executable`) so a binary
//!   hash would drift across machines.
//! - It matches the ecosystem (sigstore bundles, brew, the `sha256sums`
//!   files most release pipelines already publish), making future
//!   cross-verification cheap.

use crate::error::Result;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const BUF_SIZE: usize = 64 * 1024;

pub fn sha256_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; BUF_SIZE];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Case-insensitive comparison. SHA-256 hex is conventionally lowercase but
/// users hand-editing `.bx.toml` will sometimes paste uppercase from other
/// tools — we don't want a cosmetic difference to fail verification.
pub fn equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hashes_known_value() {
        // sha256("hello") per any reference table.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp.as_file(), "hello").unwrap();
        assert_eq!(
            sha256_hex(tmp.path()).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn hashes_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            sha256_hex(tmp.path()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn equal_is_case_insensitive() {
        assert!(equal("abc", "ABC"));
        assert!(equal("DeAdBeEf", "deadbeef"));
        assert!(!equal("abc", "abd"));
    }
}
