//! [`NavTransportError`] — the public failure surface of this crate.
//!
//! Every error variant is loud per CLAUDE.md rule 12. There is no
//! "silent fallback" path — a missing keychain item is an error, a
//! malformed embedded PEM is an error, a TLS handshake failure is an
//! error. None of these resolve to a default.

use thiserror::Error;

/// Public error type for `aberp-nav-transport`.
///
/// Variants are grouped: trust-store construction, credentials loading,
/// HTTP client construction. Each variant carries enough context for an
/// audit-ledger entry without leaking secret material — the credential
/// errors deliberately do NOT include the secret value in `Display`.
#[derive(Debug, Error)]
pub enum NavTransportError {
    /// The vendored PEM that ships with the binary failed to parse.
    /// This is a build-time invariant — if it fires at runtime, the
    /// binary itself is malformed. ADR-0020 §1 names the pinned
    /// issuing root as part of the build provenance.
    #[error("embedded NAV trust anchor PEM failed to parse: {0}")]
    EmbeddedPemMalformed(String),

    /// rustls rejected a parsed certificate as a trust anchor (e.g.,
    /// the DER decoded but is not a valid CA certificate). Same
    /// severity as `EmbeddedPemMalformed` — this is the binary-is-
    /// malformed path, surfaced separately because the failure reason
    /// differs. The wrapped `String` carries rustls's diagnostic.
    #[error("rustls rejected embedded NAV trust anchor: {0}")]
    EmbeddedCertificateRejected(String),

    /// `reqwest::ClientBuilder::build()` failed. The most common
    /// proximate cause is a TLS-backend configuration mismatch; the
    /// `#[from]` lets callers inspect the inner error.
    #[error("failed to build reqwest::Client: {0}")]
    ClientBuild(#[source] reqwest::Error),

    /// A required keychain item is missing. The variant names the item
    /// (login / password / sign-key / change-key) so the operator sees
    /// which one to populate — but never includes the value itself.
    /// Per ADR-0009 §4 + ADR-0020 §3, all four items are required;
    /// partial loading is refused (CLAUDE.md rule 12).
    #[error("NAV credential `{item}` not found in OS keychain for tenant `{tenant_id}`")]
    KeychainItemMissing {
        tenant_id: String,
        item: &'static str,
    },

    /// The keychain backend itself failed (locked keychain, permission
    /// denied, unsupported platform). Distinct from `KeychainItemMissing`
    /// — that one is a populated-keychain-but-missing-entry case, this
    /// one is a keychain-itself-failed case. The `#[source]` preserves
    /// the underlying `keyring::Error` for triage.
    #[error("keychain backend failure for item `{item}`: {source}")]
    KeychainBackend {
        item: &'static str,
        #[source]
        source: keyring::Error,
    },
}
