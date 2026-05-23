//! Minimal GitHub Releases client.
//!
//! Only fetches the two endpoints we need:
//! - `GET /repos/{owner}/{repo}/releases/latest`
//! - `GET /repos/{owner}/{repo}/releases/tags/{tag}`
//!
//! We don't use octocrab because we want the binary small and the surface
//! area limited. If we later need authenticated requests or higher rate
//! limits, the `GITHUB_TOKEN` env var is honoured here.

use crate::error::{BxError, Result};
use crate::spec::{Ref, Spec};
use serde::Deserialize;

const USER_AGENT: &str = concat!("bx/", env!("CARGO_PKG_VERSION"));
const DEFAULT_API_BASE: &str = "https://api.github.com";

fn api_base() -> String {
    std::env::var("BX_GITHUB_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub name: Option<String>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    pub content_type: Option<String>,
}

/// What we hand back to callers — a release we've resolved to, plus the tag
/// string we used (which we need for cache paths).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub tag: String,
    pub release: Release,
}

pub async fn resolve(spec: &Spec) -> Result<Resolved> {
    let client = build_client()?;
    let base = api_base();
    let url = match &spec.reference {
        Ref::Latest => format!("{base}/repos/{}/{}/releases/latest", spec.owner, spec.repo),
        Ref::Tag(tag) => format!(
            "{base}/repos/{}/{}/releases/tags/{}",
            spec.owner, spec.repo, tag
        ),
    };

    tracing::debug!(%url, "fetching release");
    let response = client.get(&url).send().await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        let tag = match &spec.reference {
            Ref::Latest => "latest".to_string(),
            Ref::Tag(t) => t.clone(),
        };
        return Err(BxError::ReleaseNotFound {
            owner: spec.owner.clone(),
            repo: spec.repo.clone(),
            tag,
        });
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(BxError::GitHubApi(format!(
            "status {status}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }

    let release: Release = response.json().await?;
    let tag = release.tag_name.clone();
    Ok(Resolved { tag, release })
}

fn build_client() -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        reqwest::header::HeaderValue::from_static("2022-11-28"),
    );

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            let value = format!("Bearer {token}");
            if let Ok(header) = reqwest::header::HeaderValue::from_str(&value) {
                headers.insert(reqwest::header::AUTHORIZATION, header);
            }
        }
    }

    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .map_err(BxError::from)
}
