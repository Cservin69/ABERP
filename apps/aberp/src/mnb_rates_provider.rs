//! MNB-rate provider abstraction for the issuance path (PR-44γ /
//! ADR-0037 §2 + §4 invariants C1 + C2 + C3 prerequisites).
//!
//! # Why a trait
//!
//! The issuance command (apps/aberp/src/issue_invoice.rs) needs to
//! fetch the MNB official mid-rate at issue time for non-HUF invoices.
//! In production the fetch reaches the real MNB SOAP endpoint via the
//! sibling `aberp-mnb-rates` crate (PR-44β). In tests the issuance
//! path must run fully offline — the live test path (`MNB_LIVE_TEST=1`)
//! from PR-44β is env-gated and not the default test surface.
//!
//! # A140 — trait over direct injection
//!
//! The session-51 brief named two alternatives: a trait, or inject the
//! rate value directly into the command. The trait choice has smaller
//! blast radius for two reasons:
//!
//! 1. **Walk-back loop owns retries.** ADR-0037 §2.b's D-1 walk-back
//!    (PR-44γ task #2) calls the fetcher in a bounded loop. Direct
//!    injection of a pre-fetched rate would push the walk-back logic
//!    into every caller; the trait keeps the walk-back local to the
//!    issuance path.
//!
//! 2. **Production wiring stays narrow.** Only the binary's `run()`
//!    constructs the real `MnbClient`-backed provider; the trait's
//!    `dyn` boxing keeps the per-call site (`issue_invoice::run_with_provider`)
//!    free of `MnbClient`-specific type ceremony.
//!
//! # PR-60 / session-80 — async-native trait
//!
//! Pre-PR-60 the trait was sync; the production impl owned a
//! current-thread tokio runtime and `block_on`'d the async
//! `MnbClient::fetch_official_rate` per call. That works for the CLI
//! (called from sync `main`, outside any runtime), but the
//! `POST /invoices/issue` route on the EUR branch reached this same
//! impl from inside axum's already-running multi-thread runtime — at
//! which point `block_on` panicked with the structural
//! "Cannot start a runtime from within a runtime" error (same shape
//! PR-56 / session-76 closed for the submit + poll path).
//!
//! Fix: lift the trait to `async fn fetch_official_rate(...)` via
//! `#[async_trait]` (preserves dyn-compatibility — the serve route
//! consumes the provider as `Box<dyn MnbRatesProvider>`). The
//! production impl now holds only the `MnbClient` and `.await`s the
//! crate's native-async fetch; the CLI's `issue_invoice::run` owns a
//! current-thread runtime at the top-level and `block_on`s
//! `issue_from_parsed` exactly once. The SPA's
//! `handle_issue_invoice` `.await`s the same async pipeline directly.

use aberp_billing::Currency;
use aberp_mnb_rates::{MnbClient, MnbError, MnbRate};
use async_trait::async_trait;
use time::Date;

/// Async abstraction over MNB-rate fetching. The production impl wraps
/// the real [`aberp_mnb_rates::MnbClient`]; the test impl returns
/// canned values from a `HashMap`.
///
/// The error type is the same [`MnbError`] the `aberp-mnb-rates` crate
/// emits — re-exported here without wrapping so the issuance path's
/// walk-back loop pattern-matches on the same variants
/// (`NoRateForCurrency` is the "walk back" signal; every other variant
/// is an immediate loud-fail per ADR-0037 §4 invariant C2).
///
/// The `Send + Sync` super-bounds + the `#[async_trait]` macro keep
/// this trait dyn-compatible (the serve route's
/// `build_live_provider_for_currency` returns
/// `Box<dyn MnbRatesProvider>`) AND keep the resulting future `Send`
/// (axum's handler bound requires `Future: Send`).
#[async_trait]
pub trait MnbRatesProvider: Send + Sync {
    /// Fetch the MNB official mid-rate for `currency` on `date`. The
    /// returned [`MnbRate::date`] may walk back to MNB's most-recent
    /// prior publication date if `date` was a non-publication day per
    /// ADR-0037 §2.b — consumers MUST read the returned date when
    /// populating the printed-invoice `Exchange-rate date` field per
    /// ADR-0037 §1.a.
    async fn fetch_official_rate(
        &self,
        currency: Currency,
        date: Date,
    ) -> Result<MnbRate, MnbError>;
}

/// Production impl backed by the real `aberp_mnb_rates::MnbClient`.
/// Holds the client only — no internal runtime (PR-60 / session-80
/// lifted the runtime ownership to the CLI's top-level `run`; the SPA
/// handler reaches this impl via `.await` on axum's existing runtime).
pub struct LiveMnbRatesProvider {
    client: MnbClient,
}

impl LiveMnbRatesProvider {
    /// Build a live provider targeting the public MNB endpoint with
    /// the default request timeout. Returns an `anyhow::Error` on
    /// client-build failure (lifted to anyhow at the binary boundary
    /// per ADR-0021 Part A item 2; client-build failure is rare TLS /
    /// reqwest territory and surfaces as the binary's top-level
    /// loud-fail).
    pub fn new() -> anyhow::Result<Self> {
        use anyhow::Context;
        let client = MnbClient::new().context("build MNB client")?;
        Ok(Self { client })
    }
}

#[async_trait]
impl MnbRatesProvider for LiveMnbRatesProvider {
    async fn fetch_official_rate(
        &self,
        currency: Currency,
        date: Date,
    ) -> Result<MnbRate, MnbError> {
        self.client.fetch_official_rate(currency, date).await
    }
}
