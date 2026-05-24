//! Live TLS handshake against `api-test.onlineszamla.nav.gov.hu`.
//!
//! Env-gated by `ABERP_NAV_LIVE_TEST=1`. CI skips it by default because
//! it depends on NAV's test-endpoint uptime; a developer runs it locally
//! before opening a NAV-touching PR. The unconditional negation guard
//! lives in `trust_negation.rs`.
//!
//! What this test verifies:
//!
//!   1. `NavTransport::new(NavEndpoint::Test)` constructs without error
//!      against the embedded PEMs.
//!   2. An HTTPS GET against the NAV test endpoint completes the TLS
//!      handshake — i.e., NAV's leaf cert validates under our pin set.
//!   3. The response shape is *something* (the endpoint may return 404
//!      or a SOAP fault for a GET; that's fine — we're not asking it
//!      to do anything, just to handshake).
//!
//! What this test does NOT verify:
//!
//!   - Anything about NAV's application protocol (PR-7-B / PR-7-C).
//!   - Anything about credential loading (covered by unit tests inside
//!     `src/credentials/`).

use aberp_nav_transport::{NavEndpoint, NavTransport};

#[test]
fn tls_handshake_against_nav_test_endpoint() {
    if std::env::var("ABERP_NAV_LIVE_TEST").is_err() {
        eprintln!(
            "skipping live-NAV handshake test: set ABERP_NAV_LIVE_TEST=1 \
             to run (depends on NAV test-endpoint uptime)"
        );
        return;
    }

    let transport = NavTransport::new(NavEndpoint::Test).expect("transport must construct");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime must build");

    let result = rt.block_on(async {
        // The base URL points at the v3 invoiceService. A bare GET
        // there is enough to drive the TLS handshake; the application
        // response is not part of this test's contract.
        transport
            .client()
            .get(transport.endpoint().base_url())
            .send()
            .await
    });

    match result {
        Ok(resp) => {
            // TLS handshake succeeded — that's what we're proving.
            // Application-level status is documentary, not asserted.
            eprintln!(
                "TLS handshake to {} succeeded; HTTP status was {}",
                transport.endpoint().hostname(),
                resp.status()
            );
        }
        Err(e) => {
            // If we got here under ABERP_NAV_LIVE_TEST=1, something is
            // wrong with the pin or the network. Fail loud, don't
            // silently pass (CLAUDE.md rule 12).
            panic!(
                "TLS handshake to {} FAILED: {e:?}. \
                 Either: (a) the network is partitioned, (b) the pinned \
                 trust anchors have been rotated and the vendored PEMs \
                 are stale, or (c) the pin construction in src/client.rs \
                 has regressed. See trust.rs for the rotation playbook.",
                transport.endpoint().hostname()
            );
        }
    }
}
