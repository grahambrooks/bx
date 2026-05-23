use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, BxError>;

#[derive(Debug, Error)]
pub enum BxError {
    #[error("invalid spec '{spec}': {reason}")]
    InvalidSpec { spec: String, reason: String },

    #[error("unsupported platform: os={os}, arch={arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("github api error: {0}")]
    GitHubApi(String),

    #[error("release '{tag}' for {owner}/{repo} not found")]
    ReleaseNotFound {
        owner: String,
        repo: String,
        tag: String,
    },

    #[error("no asset matched platform {platform} in release '{tag}'. assets tried: {assets:?}")]
    NoMatchingAsset {
        platform: String,
        tag: String,
        assets: Vec<String>,
    },

    #[error("binary '{name}' not found in {dir}")]
    BinaryNotFound { name: String, dir: PathBuf },

    #[error("could not determine cache directory")]
    NoCacheDir,

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("archive error: {0}")]
    Archive(String),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("checksum mismatch for {asset}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        expected: String,
        actual: String,
        asset: String,
    },
}
