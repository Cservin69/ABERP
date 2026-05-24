//! [`NavTransport`] — a constructed `reqwest::Client` whose entire TLS
//! trust state is the two pinned NAV anchors and nothing else.
//!
//! Per ADR-0020 §1 + ADR-0021 §A5:
//!
//!   1. **Trust store owned by us, not reqwest.** The `rustls::ClientConfig`
//!      is built in [`crate::trust::build_pinned_client_config`] with the
//!      two pinned anchors as its sole `RootCertStore` contents. We hand
//!      that pre-built config to reqwest via `use_preconfigured_tls`.
//!      Reqwest does NOT layer additional roots on top — it takes the
//!      config as-is. This is the load-bearing posture: with the older
//!      `add_root_certificate` API reqwest *adds* to its default trust
//!      store (which on the `rustls` feature transitively includes
//!      webpki-roots), so pinning that way would silently degrade to
//!      "pin set + Mozilla CA bundle". Caught by
//!      `tests/trust_negation.rs`.
//!   2. `https_only(true)` — refuse plaintext HTTP. NAV doesn't accept
//!      plaintext anyway; this is belt-and-suspenders so a misconfigured
//!      `NavEndpoint::base_url()` cannot silently downgrade.
//!   3. `min_tls_version(TLS_1_2)` — TLS 1.0/1.1 are refused at the
//!      client. NAV currently serves TLS 1.2/1.3 (verified at PR-7-A
//!      pin time via `openssl s_client`). The pre-built `ClientConfig`
//!      already disables TLS 1.0/1.1 via rustls's "safe defaults"; this
//!      is redundant belt-and-suspenders, but harmless.
//!
//! PR-7-A wires the construction. It does NOT issue submission calls
//! — those land in PR-7-B (`manageInvoice`) and PR-7-C
//! (`queryTransactionStatus`). The only network call PR-7-A makes is
//! the env-gated TLS-handshake conformance test in
//! `tests/tls_handshake.rs`.

use reqwest::tls::Version;
use reqwest::Client;

use crate::endpoint::NavEndpoint;
use crate::error::NavTransportError;
use crate::trust;

/// The constructed transport — a `reqwest::Client` bound to one
/// `NavEndpoint`. Cheap to clone (`Client` is `Arc`-shared internally).
#[derive(Debug, Clone)]
pub struct NavTransport {
    endpoint: NavEndpoint,
    client: Client,
}

impl NavTransport {
    /// Construct a transport for the named endpoint. Loud-fails if any
    /// of: the vendored PEMs fail to parse, rustls rejects them as
    /// trust anchors, or reqwest rejects the pre-built config.
    pub fn new(endpoint: NavEndpoint) -> Result<Self, NavTransportError> {
        let tls_config = trust::build_pinned_client_config()?;

        // `use_preconfigured_tls` takes an `impl Any`; reqwest 0.13
        // wraps the value in `Some(...)` internally and then downcasts
        // to `Option<rustls::ClientConfig>`. We therefore pass the bare
        // `ClientConfig` (NOT `Some(config)` — that would produce
        // `Option<Option<ClientConfig>>` after reqwest's wrap, and the
        // downcast would fail with "Unknown TLS backend"). The version
        // line on `rustls` in the workspace Cargo.toml MUST match
        // reqwest's transitive choice (Cargo.lock is authoritative);
        // a mismatch silently fails the downcast and reqwest falls
        // back to its defaults.
        let client = Client::builder()
            .use_preconfigured_tls(tls_config)
            .https_only(true)
            .min_tls_version(Version::TLS_1_2)
            .build()
            .map_err(NavTransportError::ClientBuild)?;

        Ok(Self { endpoint, client })
    }

    /// Which endpoint this transport targets. Useful for `tracing`
    /// spans and the test that asserts construction succeeded.
    pub fn endpoint(&self) -> NavEndpoint {
        self.endpoint
    }

    /// Borrow the underlying `reqwest::Client`. PR-7-B's submission
    /// code uses `.post(...)` against this client; PR-7-A only exposes
    /// it for the conformance test.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
