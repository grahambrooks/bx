//! Platform detection — OS and architecture, plus the substring vocabularies
//! the asset matcher uses to score release filenames.

use crate::error::{BxError, Result};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Darwin,
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    pub fn current() -> Result<Self> {
        let os = match std::env::consts::OS {
            "macos" => Os::Darwin,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            other => {
                return Err(BxError::UnsupportedPlatform {
                    os: other.to_string(),
                    arch: std::env::consts::ARCH.to_string(),
                })
            }
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X64,
            "aarch64" => Arch::Arm64,
            other => {
                return Err(BxError::UnsupportedPlatform {
                    os: std::env::consts::OS.to_string(),
                    arch: other.to_string(),
                })
            }
        };
        Ok(Self { os, arch })
    }

    /// Substrings that, in lowercased asset names, indicate this OS.
    /// Order doesn't matter; any match contributes to the score.
    pub fn os_keywords(&self) -> &'static [&'static str] {
        match self.os {
            Os::Darwin => &["darwin", "macos", "apple", "osx", "mac"],
            Os::Linux => &["linux", "unknown-linux"],
            Os::Windows => &["windows", "win64", "win32", "win"],
        }
    }

    /// Substrings that indicate this architecture.
    pub fn arch_keywords(&self) -> &'static [&'static str] {
        match self.arch {
            Arch::X64 => &["x86_64", "x86-64", "x64", "amd64"],
            Arch::Arm64 => &["aarch64", "arm64"],
        }
    }

    /// Substrings of OTHER OSes — used to penalise wrong-OS assets so
    /// `linux-x64` and `darwin-x64` don't both score equally when we want one.
    pub fn other_os_keywords(&self) -> Vec<&'static str> {
        let mine = self.os;
        let mut out = Vec::new();
        for &(os, kws) in &[
            (Os::Darwin, &["darwin", "macos", "apple", "osx"][..]),
            (Os::Linux, &["linux"][..]),
            (Os::Windows, &["windows", "win64", "win32"][..]),
        ] {
            if os != mine {
                out.extend_from_slice(kws);
            }
        }
        out
    }

    pub fn other_arch_keywords(&self) -> Vec<&'static str> {
        let mine = self.arch;
        let mut out = Vec::new();
        for &(arch, kws) in &[
            (Arch::X64, &["x86_64", "x86-64", "x64", "amd64"][..]),
            (Arch::Arm64, &["aarch64", "arm64"][..]),
        ] {
            if arch != mine {
                out.extend_from_slice(kws);
            }
        }
        out
    }

    pub fn exe_extension(&self) -> &'static str {
        match self.os {
            Os::Windows => ".exe",
            _ => "",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let os = match self.os {
            Os::Darwin => "darwin",
            Os::Linux => "linux",
            Os::Windows => "windows",
        };
        let arch = match self.arch {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        };
        write!(f, "{os}-{arch}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_resolves_on_supported_targets() {
        // This will pass on any platform we actually build for in CI.
        let p = Platform::current().expect("supported");
        assert!(!p.os_keywords().is_empty());
        assert!(!p.arch_keywords().is_empty());
    }

    #[test]
    fn other_keywords_exclude_self() {
        let p = Platform {
            os: Os::Linux,
            arch: Arch::X64,
        };
        assert!(!p.other_os_keywords().contains(&"linux"));
        assert!(!p.other_arch_keywords().contains(&"x86_64"));
        assert!(p.other_os_keywords().contains(&"darwin"));
        assert!(p.other_arch_keywords().contains(&"arm64"));
    }
}
