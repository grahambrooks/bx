//! Parsing for the `owner/repo[@ref][#binary]` spec format.
//!
//! Examples that parse:
//! - `grahambrooks/symgraph`
//! - `grahambrooks/symgraph@v2026.4.13`
//! - `grahambrooks/symgraph@latest`
//! - `grahambrooks/symgraph#cli`
//! - `grahambrooks/symgraph@v2026.4.13#cli`
//!
//! Semver-range refs (`^v1.0`, `~1.0.0`) are reserved for a later milestone;
//! the parser accepts them as `Ref::Tag` for now and we resolve them as exact
//! tag matches. The CLI surface stays stable when we add real range logic.

use crate::error::{BxError, Result};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub owner: String,
    pub repo: String,
    pub reference: Ref,
    pub binary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Latest,
    Tag(String),
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)?;
        match &self.reference {
            Ref::Latest => {}
            Ref::Tag(t) => write!(f, "@{t}")?,
        }
        if let Some(bin) = &self.binary {
            write!(f, "#{bin}")?;
        }
        Ok(())
    }
}

impl FromStr for Spec {
    type Err = BxError;

    fn from_str(s: &str) -> Result<Self> {
        let original = s.to_string();
        let invalid = |reason: &str| BxError::InvalidSpec {
            spec: original.clone(),
            reason: reason.to_string(),
        };

        // Trim `github.com/` prefix if a user pasted a URL fragment.
        let s = s
            .strip_prefix("https://github.com/")
            .or_else(|| s.strip_prefix("github.com/"))
            .unwrap_or(s)
            .trim_end_matches('/');

        if s.is_empty() {
            return Err(invalid("empty spec"));
        }

        // Split off the binary selector `#name` first; it can appear after a
        // ref or directly after the repo.
        let (head, binary) = match s.rsplit_once('#') {
            Some((h, b)) if !b.is_empty() => (h, Some(b.to_string())),
            Some(_) => return Err(invalid("empty binary name after '#'")),
            None => (s, None),
        };

        // Then split off the ref `@tag`.
        let (repo_part, reference) = match head.split_once('@') {
            Some((r, t)) if !t.is_empty() => (r, parse_ref(t)),
            Some(_) => return Err(invalid("empty ref after '@'")),
            None => (head, Ref::Latest),
        };

        // Finally split owner/repo.
        let (owner, repo) = repo_part
            .split_once('/')
            .ok_or_else(|| invalid("expected owner/repo"))?;

        if owner.is_empty() || repo.is_empty() {
            return Err(invalid("owner and repo must both be non-empty"));
        }

        if owner.contains('/') || repo.contains('/') {
            return Err(invalid("owner/repo may not contain extra slashes"));
        }

        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            reference,
            binary,
        })
    }
}

fn parse_ref(s: &str) -> Ref {
    if s.eq_ignore_ascii_case("latest") {
        Ref::Latest
    } else {
        Ref::Tag(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Spec {
        s.parse().expect("should parse")
    }

    #[test]
    fn bare_owner_repo() {
        let spec = parse("grahambrooks/symgraph");
        assert_eq!(spec.owner, "grahambrooks");
        assert_eq!(spec.repo, "symgraph");
        assert_eq!(spec.reference, Ref::Latest);
        assert_eq!(spec.binary, None);
    }

    #[test]
    fn with_tag() {
        let spec = parse("grahambrooks/symgraph@v2026.4.13");
        assert_eq!(spec.reference, Ref::Tag("v2026.4.13".into()));
    }

    #[test]
    fn latest_keyword_normalises() {
        let spec = parse("grahambrooks/symgraph@latest");
        assert_eq!(spec.reference, Ref::Latest);
        let spec = parse("grahambrooks/symgraph@LATEST");
        assert_eq!(spec.reference, Ref::Latest);
    }

    #[test]
    fn with_binary() {
        let spec = parse("grahambrooks/symgraph#cli");
        assert_eq!(spec.binary.as_deref(), Some("cli"));
        assert_eq!(spec.reference, Ref::Latest);
    }

    #[test]
    fn with_tag_and_binary() {
        let spec = parse("grahambrooks/symgraph@v1.0.0#cli");
        assert_eq!(spec.reference, Ref::Tag("v1.0.0".into()));
        assert_eq!(spec.binary.as_deref(), Some("cli"));
    }

    #[test]
    fn accepts_url_paste() {
        let spec = parse("https://github.com/grahambrooks/symgraph");
        assert_eq!(spec.owner, "grahambrooks");
        assert_eq!(spec.repo, "symgraph");
        let spec = parse("github.com/grahambrooks/symgraph@v1.0.0");
        assert_eq!(spec.reference, Ref::Tag("v1.0.0".into()));
    }

    #[test]
    fn trailing_slash_tolerated() {
        let spec = parse("grahambrooks/symgraph/");
        assert_eq!(spec.repo, "symgraph");
    }

    #[test]
    fn display_round_trip() {
        for s in [
            "grahambrooks/symgraph",
            "grahambrooks/symgraph@v1.0.0",
            "grahambrooks/symgraph#cli",
            "grahambrooks/symgraph@v1.0.0#cli",
        ] {
            let spec: Spec = s.parse().unwrap();
            assert_eq!(spec.to_string(), s, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn rejects_missing_repo() {
        assert!("grahambrooks".parse::<Spec>().is_err());
        assert!("grahambrooks/".parse::<Spec>().is_err());
        assert!("/symgraph".parse::<Spec>().is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!("".parse::<Spec>().is_err());
    }

    #[test]
    fn rejects_empty_ref() {
        assert!("grahambrooks/symgraph@".parse::<Spec>().is_err());
    }

    #[test]
    fn rejects_empty_binary() {
        assert!("grahambrooks/symgraph#".parse::<Spec>().is_err());
    }

    #[test]
    fn rejects_extra_path_segments() {
        assert!("grahambrooks/symgraph/extra".parse::<Spec>().is_err());
    }
}
