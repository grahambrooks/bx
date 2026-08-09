//! Process-wide TLS crypto provider selection.
//!
//! reqwest is built with `rustls-no-provider`, which wires up rustls and the
//! platform certificate verifier but deliberately leaves the crypto provider
//! unchosen — the alternative, reqwest's `rustls` feature, hard-wires
//! `aws-lc-rs` and its NASM build requirement on Windows (see Cargo.toml).
//!
//! The cost of that choice is that *someone* has to install a provider before
//! the first `reqwest::Client` is built. reqwest does not return an error if
//! nobody does — `ClientBuilder::build` **panics** with "No rustls crypto
//! provider is configured", so bx exits 101 with a backtrace rather than a
//! diagnosable error. That someone is [`install_crypto_provider`], called
//! from each of the two client construction sites (`github::build_client`,
//! `fetch::download`).
//!
//! It is deliberately *not* called from `main`: the pinned-ref fast path in
//! `lib::run` execs a cached binary without touching the network, and that
//! path stays free of setup work it will never use.

use std::sync::Once;

/// Install `ring` as the process-default rustls crypto provider.
///
/// Idempotent and safe to call from anywhere, including concurrently. Must be
/// called before building a `reqwest::Client`.
pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // `install_default` errors only if a provider is already installed,
        // which is just as good an outcome as installing one ourselves.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_leaves_a_provider() {
        install_crypto_provider();
        install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
