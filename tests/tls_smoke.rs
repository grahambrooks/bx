//! Real-network TLS smoke test.
//!
//! Every other test in this repo talks to an in-process `TcpListener` over
//! plain HTTP, so nothing exercises the TLS stack at all. That stack is not
//! self-configuring: reqwest is built with `rustls-no-provider`, and
//! `bx::tls::install_crypto_provider` has to install `ring` as the process
//! default before the first client is built (see Cargo.toml for why we avoid
//! reqwest's `rustls` feature and its aws-lc-rs/NASM requirement). Drop that
//! call — or the direct `rustls` dependency it needs — and every request
//! fails at `ClientBuilder::build`, with the whole offline suite still green.
//!
//! Worth running per-OS rather than once, because the two halves fail
//! differently. A missing provider breaks all three platforms identically,
//! but root certificates come from `rustls-platform-verifier`, which uses the
//! OS trust store on macOS/Windows and `rustls-native-certs` on Linux — a
//! genuinely different path per runner.
//!
//! `#[ignore]` by default so `cargo test` stays offline and hermetic. CI runs
//! it explicitly:
//!
//! ```sh
//! cargo test --test tls_smoke -- --ignored
//! ```
//!
//! CI also passes `GITHUB_TOKEN`. Unauthenticated api.github.com allows 60
//! requests/hour per IP and hosted runners share egress IPs, so without a
//! token this would flake on rate limits rather than on TLS. bx picks the
//! token up from the environment on its own (see `github::build_client`).

use assert_cmd::Command;

/// Resolve a tag that does not exist, against the real api.github.com.
///
/// The 404 is the point: reaching a *clean* "release not found" means the
/// handshake completed and the certificate chain verified. A TLS failure —
/// no crypto provider, an empty root store — surfaces as a `Network` error
/// with completely different text, so the two outcomes cannot be confused.
#[test]
#[ignore = "requires network; run with --ignored"]
fn https_to_github_api_completes_a_handshake() {
    let cache = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_CACHE_DIR", cache.path())
        // Do NOT set BX_GITHUB_API_BASE — the real host is the whole point.
        .arg("grahambrooks/bx@v0.0.0-tls-smoke-does-not-exist");

    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);

    assert!(
        stderr.contains("not found"),
        "expected a clean 404 proving TLS succeeded, got: {stderr}"
    );

    // Name the failure modes explicitly: a bare "not found" assertion would
    // also pass on some non-TLS errors, and these messages are what a
    // provider or trust-store regression actually looks like.
    for symptom in [
        "No rustls crypto provider is configured",
        "invalid peer certificate",
        "UnknownIssuer",
        "CaUsedAsEndEntity",
    ] {
        assert!(
            !stderr.contains(symptom),
            "TLS stack regression ({symptom}) in: {stderr}"
        );
    }
}

/// The download path builds a *second*, separately configured client in
/// `fetch::download`, against a different host (the release-asset CDN, via a
/// redirect). Installing the provider in only one of the two call sites would
/// leave this broken while the test above still passed.
#[test]
#[ignore = "requires network; run with --ignored"]
fn https_download_path_fetches_a_real_release_asset() {
    let cache = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_CACHE_DIR", cache.path())
        .arg("grahambrooks/symgraph")
        .arg("--")
        .arg("--version");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("symgraph"),
        "expected the downloaded binary to run, got: {stdout}"
    );
}
