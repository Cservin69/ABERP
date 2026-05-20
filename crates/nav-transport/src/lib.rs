//! `aberp-nav-transport` — NAV TLS transport + credential loading.
//!
//! See ADR-0009 §4 (NAV authentication and credentials), ADR-0020 §1-3
//! (transport / credential / threat-model correction), ADR-0021 §A14
//! (keyring + NAV trust-anchor pin amendment).
//!
//! ## What this crate provides
//!
//!   - [`NavEndpoint`] — the prod / test endpoint enum.
//!   - [`NavTransport`] — a constructed `reqwest::Client` with the two
//!     pinned trust anchors (`Microsec e-Szigno Root CA 2009` and
//!     `e-Szigno OV TLS CA 2023`) and the OS trust store disabled.
//!   - [`NavCredentials`] — the four-artifact credential bundle
//!     (login + password + xmlSignKey + xmlChangeKey) loaded from the
//!     OS keychain via the `keyring` crate (per ADR-0021 §A14).
//!
//! ## What this crate does NOT provide (PR-7-A scope discipline)
//!
//!   - No SOAP envelope or XML serialization (PR-7-B).
//!   - No `passwordHash` / `requestSignature` computation (PR-7-B).
//!   - No `tokenExchange` / `manageInvoice` / `queryTransactionStatus`
//!     (PR-7-B for the submit pair, PR-7-C for the poll).
//!   - No audit-ledger writes — those are the binary's responsibility,
//!     called from the NAV submission path in PR-7-B.
//!
//! PR-7-A's success criterion is: the transport CAN be constructed
//! against pinned trust anchors and the credentials CAN be loaded
//! from the keychain; both fail loud on any missing or malformed
//! input.

#![forbid(unsafe_code)]

pub mod credentials;
pub mod endpoint;
pub mod error;
pub mod trust;

mod client;

pub use client::NavTransport;
pub use credentials::NavCredentials;
pub use endpoint::NavEndpoint;
pub use error::NavTransportError;
