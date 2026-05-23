//! Env-gated live MNB integration test per ADR-0037 §2.a.
//!
//! Disabled by default — the gate normally runs offline. Set
//! `MNB_LIVE_TEST=1` to exercise the real
//! `https://www.mnb.hu/arfolyamok.asmx` endpoint and prove the
//! crate's three transport details (URL, SOAPAction,
//! Content-Type) match MNB's current contract.
//!
//! Usage:
//!
//! ```sh
//! MNB_LIVE_TEST=1 cargo test -p aberp-mnb-rates --test live_mnb -- --nocapture
//! ```
//!
//! The assertion is intentionally weak (rate > 0 + value parses as
//! decimal): pinning a specific historical rate value here would
//! make this test brittle against MNB's occasional prior-day
//! republications, and the parse-side fixtures already pin the
//! contract precisely. This test exists to catch transport-layer
//! drift (MNB renames the SOAPAction, switches namespaces, etc.),
//! not to re-verify the parse.

use aberp_billing::Currency;
use aberp_mnb_rates::MnbClient;
use time::macros::date;

#[tokio::test(flavor = "current_thread")]
async fn live_mnb_returns_eur_rate_for_known_past_date() {
    if std::env::var("MNB_LIVE_TEST").ok().as_deref() != Some("1") {
        eprintln!("skipping live MNB test — set MNB_LIVE_TEST=1 to exercise");
        return;
    }

    // Use a Thursday well in the past — any weekday MNB published
    // on. The walked-back-fallback rule per ADR-0037 §2.b means
    // even a non-publication day would resolve to *some* prior
    // date; pinning a weekday keeps the returned date predictable
    // for the assertion below.
    let requested_date = date!(2024 - 03 - 21);

    let client = MnbClient::new().expect("MnbClient builds");
    let rate = client
        .fetch_official_rate(Currency::Eur, requested_date)
        .await
        .expect("live MNB call must succeed");

    assert_eq!(rate.currency, Currency::Eur);
    assert!(rate.unit >= 1, "MNB EUR unit is always >= 1");

    // Value MUST parse as a positive decimal. EUR/HUF has not
    // traded below 100 HUF since the 1990s; the loose floor below
    // catches a parse-disaster (empty string, sign flip, etc.)
    // without pinning a specific rate this test would have to
    // chase across MNB republications.
    let parsed: f64 = rate
        .value
        .parse()
        .expect("MNB-returned value must parse as decimal");
    assert!(
        parsed > 100.0,
        "EUR/HUF rate {parsed} below sanity floor — transport / parse drift?"
    );

    // Returned date is on or before the requested date (the
    // walked-back-fallback rule per ADR-0037 §2.b allows equal-or-
    // prior, never later).
    assert!(
        rate.date <= requested_date,
        "MNB returned date {} is later than requested {}",
        rate.date,
        requested_date,
    );

    eprintln!(
        "live MNB EUR/HUF on {}: {} (unit {}, returned date {})",
        requested_date, rate.value, rate.unit, rate.date,
    );
}
