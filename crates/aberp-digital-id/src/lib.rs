//! ABERP digital-identity provider abstraction (S344 / PR-38, ADR-0070).
//!
//! # Why this crate exists
//!
//! The defense-aerospace pivot (`[[defense-aerospace-pivot]]`) requires that
//! every audit-emitting operation eventually carry the *operator's digital
//! identity* — a CAC certificate, an HU eID assertion, a Qatar MFA factor, a
//! signed token — so the tamper-evident audit ledger (ADR-0008) can attest
//! *who* authorised each fiscal/manufacturing action, not merely *what*
//! changed. This is a Part-11 / DFARS-grade requirement and a Day-1 item.
//!
//! Rather than wire a specific identity vendor into the audit emit sites,
//! this crate defines a single trait — [`DigitalIdProvider`] — and ships one
//! implementation: [`MockProvider`]. Real backends slot in behind the same
//! trait in later sessions, swapped at the boot boundary. This mirrors the
//! email-outbox / `StorefrontCredentialHandle` "abstraction-then-
//! implementations" pattern already proven across ABERP.
//!
//! # Scope (S344)
//!
//! - [`DigitalId`] — a resolved operator identity.
//! - [`Signature`] — an algorithm-tagged signature over arbitrary bytes.
//! - [`DigitalIdProvider`] — the swap-point trait.
//! - [`MockProvider`] — a deterministic test backend.
//!
//! S363 / PR-50 (ADR-0080) adds a SECOND deterministic, non-production
//! backend — [`UsDodCacProvider`] — purely to prove the trait abstracts: it
//! has a different signing persona (a certificate-bound digest, not a keyed
//! HMAC), session-based `current_operator()` (no card → no operator), and
//! cert-chain-membership verification (not MAC equality). It is still a stub;
//! real signing primitives stay un-wired until a real customer demands them.
//!
//! Out of scope (future work): real crypto backends, audit `EventKind`s that
//! populate the signer field (S346), electronic-signature ceremony UI.
//!
//! # ⚠️ The Mock is NOT production crypto
//!
//! [`MockProvider`] "signs" with a hand-rolled HMAC-SHA256 keyed on a
//! hardcoded, publicly-known test key ([`MOCK_TEST_KEY`]). It proves the
//! *shape* of sign/verify, nothing more. It logs a WARN on every
//! construction and must never back a production operator identity.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod cac;
// S441 / ADR-0086 — the DÁP eAzonosítás OpenID4VP transport seam (structural
// floor: trait + mock + `todo!` OIDC stub). See [`dap_transport`].
mod dap_transport;
mod identity;
mod mock;
mod provider;
mod signature;

pub use cac::{UsDodCacProvider, CAC_ALGORITHM, CAC_DEFAULT_EDIPI, CAC_ISSUER};
pub use dap_transport::{
    CallbackResponse, DapChallenge, DapError, DapIdentity, DapLoginContext, DapTransport,
    MockDapTransport, OidcDapTransport,
};
pub use identity::DigitalId;
pub use mock::{MockProvider, MOCK_ALGORITHM, MOCK_OPERATOR_ID, MOCK_TEST_KEY};
pub use provider::{DigitalIdProvider, ProviderError};
pub use signature::Signature;

#[cfg(test)]
mod tests;
