//! NAV `<InvoiceData>` v3.0 runtime invariant validator (ADR-0022).
//!
//! Hand-rolled structural check that walks an in-memory `<InvoiceData>`
//! XML payload against an allowlist of required elements, ordering,
//! cardinalities, and ASCII-shape constraints on numeric and date
//! fields. On any divergence the validator returns a typed
//! [`NavXsdValidationError`] — the call site is expected to convert
//! that into a loud-fail per CLAUDE.md rule 12.
//!
//! # Scope
//!
//! This crate is NOT a generic XSD 1.0 validator. ADR-0022 (Option B —
//! "hand-rolled invariant check") explicitly picks the hand-rolled path
//! over libxml2 FFI (single-static-binary posture per ADR-0001 +
//! ADR-0007 §Supply chain) and over `xmlschema-rs` (immature as of
//! 2026-05). The crate name carries `xsd` because the *role* is XSD-
//! style validation at runtime; the *implementation* is the
//! hand-rolled allowlist. Future replacement with a real XSD library
//! is a one-call-site swap because the public entry point is a single
//! function with a typed error — see ADR-0022 §"What we lock
//! ourselves into."
//!
//! # NAV version pin
//!
//! The allowlist is hand-written against NAV Online Számla v3.0 as
//! exercised by `apps/aberp/src/nav_xml.rs`. A future NAV v3.x or v4
//! migration extends the allowlist in the same PR that extends the
//! emitter; see [`NAV_XSD_VERSION`].
//!
//! # Wiring per ADR-0022 + ADR-0026
//!
//! `validate_invoice_data` is consumed at three call sites (ADR-0022):
//!
//! 1. `issue_invoice::run` after rendering, before writing to disk.
//! 2. `submit_invoice::run` after `std::fs::read`, before any NAV call.
//! 3. `retry_submission::run` mirroring `submit_invoice::run`.
//!
//! `validate_annulment_data` is consumed at one call site today
//! (ADR-0026 §4):
//!
//! 1. `submit_annulment::run` after `std::fs::read`, before any NAV
//!    call. Same loud-fail discipline (CLAUDE.md rule 12) — no
//!    `tokenExchange` happens if the on-disk annulment XML diverges
//!    from the v3.0 `<InvoiceAnnulment>` allowlist.
//!
//! `request_technical_annulment::run` does NOT call
//! `validate_annulment_data` — instead it runs a minimal call-site
//! sanity check (`check_annulment_xml_minimum` in
//! `apps/aberp/src/request_technical_annulment.rs`) per ADR-0025 §4.
//! The request-side check is intentionally narrower than the wire-
//! side validator; ADR-0026 §4 names this asymmetry and the
//! rationale (loud-fail matters most where the bytes hit NAV).
//!
//! # Trap-doors against drift
//!
//! The validator's allowlist is the source of truth for "what the NAV
//! XML builder is allowed to emit." A second source of truth is the
//! builder itself (`apps/aberp/src/nav_xml.rs`). Divergence between
//! the two is exactly the failure mode CLAUDE.md rule 7 names. Two
//! trap-doors close the divergence:
//!
//! - `apps/aberp/tests/round_trip_invoice_data.rs` — issue an invoice
//!   from `fixtures/invoice_minimal.json`, validate the produced XML.
//!   If the builder emits something the validator rejects, the test
//!   fails loud at commit time.
//! - `apps/aberp/tests/nav_xsd_validator_annulment_round_trip.rs`
//!   (PR-13, ADR-0026 §4) — render a fresh `<InvoiceAnnulment>` body
//!   via `nav_xml::render_annulment_data` and validate it with
//!   `validate_annulment_data`. A future emitter change that
//!   diverges from the annulment allowlist fails this test loud at
//!   commit time. Same posture as the `InvoiceData` round-trip
//!   above.
//! - The variant pairwise-distinct test in this crate
//!   (`error_variants_have_distinct_display`) catches the case where a
//!   merge accidentally collapses two error classes into one.

#![forbid(unsafe_code)]

mod error;
mod validate;

pub use error::NavXsdValidationError;
pub use validate::{validate_annulment_data, validate_invoice_data};

/// The NAV Online Számla schema version this validator targets.
///
/// ADR-0022 §"What this ADR does NOT cover" requires the version be
/// surfaced as a public constant so a future contributor reading the
/// crate sees the version pin without grepping. A future NAV v3.x or
/// v4 migration bumps this constant in the same PR that extends the
/// allowlist (both `<InvoiceData>` and `<InvoiceAnnulment>` per
/// ADR-0026 §4 — the version pin covers both body shapes).
pub const NAV_XSD_VERSION: &str = "3.0";

/// The NAV namespace for `<InvoiceData>` payloads at v3.0. Returned
/// from this crate so the call site can include it in error context
/// without re-hard-coding the URI.
pub const NAV_NS_DATA: &str = "http://schemas.nav.gov.hu/OSA/3.0/data";

/// The NAV namespace for `<InvoiceAnnulment>` payloads at v3.0 per
/// ADR-0026 §4 (ADR-0025 §"Surfaced conflict 1"'s chosen reading).
/// The `manageAnnulment` body counterpart to `NAV_NS_DATA`. Same
/// surfacing rationale: callers include the URI in error context
/// without re-hard-coding.
pub const NAV_NS_ANNUL: &str = "http://schemas.nav.gov.hu/OSA/3.0/annul";
