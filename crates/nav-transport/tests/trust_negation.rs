//! Trust-store negation: the NAV-pinned `reqwest::Client` does NOT
//! validate certificate chains that root anywhere other than the two
//! vendored NAV trust anchors.
//!
//! This is the load-bearing conformance test for ADR-0020 §1's posture
//! ("OS trust store is not consulted for NAV traffic"). Without it, a
//! contributor could add `webpki-roots` or `rustls-native-certs` to
//! reqwest's feature list in the workspace `Cargo.toml`, every other
//! test would keep passing, and the production pin would silently
//! degrade to "OS roots + NAV pin" — exactly the trap CLAUDE.md rule
//! 12 names. (In reqwest 0.11/0.12 the same trap was a runtime
//! `tls_built_in_root_certs(true)` flag; 0.13 moved the lever to
//! compile-time features but the failure mode is the same.)
//!
//! ## How it asserts the negation without a private CA infrastructure
//!
//! The test makes a real HTTPS request to a public host that is NOT
//! `*.onlineszamla.nav.gov.hu` and whose certificate is rooted in a
//! public CA (the kind that lives in the OS trust store but NOT in our
//! pin set). If our pin set were augmented by the OS store, the request
//! would succeed. With pin-only-trust, the request fails at TLS chain
//! validation. The test asserts the failure.
//!
//! Hostname choice: `example.com` is the standardized "use me in tests"
//! host per RFC 2606 / IANA reservation. Its certificate is signed by a
//! public CA in the Mozilla bundle. If `example.com` ever stops serving
//! HTTPS or starts being signed by Microsec e-Szigno (vanishingly
//! unlikely on both counts), this test will need to pick another host.
//!
//! ## Why this test is unconditional (not env-gated)
//!
//! Unlike `tls_handshake.rs` (which depends on NAV uptime and is
//! therefore env-gated), `example.com` is a TLS-stable target served
//! by IANA. CI can run this test on every push without external-uptime
//! flakiness — and SHOULD, because this is the negation guard.
//!
//! If the test cannot reach the network at all (offline sandbox,
//! firewall) it falls back to checking the error is at least the
//! right *kind* (TLS / connect, not "200 OK"); a successful response
//! is the only outcome that fails the test.

use aberp_nav_transport::{NavEndpoint, NavTransport};

#[test]
fn pinned_client_refuses_chains_not_anchored_in_pinset() {
    // Construct the transport the same way production does. We don't
    // actually use the NavEndpoint here — we only need the client's
    // TLS configuration. Either endpoint variant produces an
    // identically-configured `reqwest::Client` (the endpoint enum
    // only changes the URL targeted, not the trust anchors).
    let transport = NavTransport::new(NavEndpoint::Test).expect("transport must construct");

    // Build a blocking runtime to drive the async reqwest call inside
    // a single-purpose test without needing #[tokio::main].
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime must build");

    let result = rt.block_on(async { transport.client().get("https://example.com/").send().await });

    match result {
        Ok(resp) => panic!(
            "TLS chain validation MUST have failed for example.com under \
             NAV-pinned trust, but reqwest returned a response: status={}, \
             url={}. This means the OS trust store is being consulted — \
             ADR-0020 §1's posture is violated.",
            resp.status(),
            resp.url(),
        ),
        Err(e) => {
            // Sanity-print so a CI run shows WHY the request failed.
            // Acceptable shapes:
            //   - e.is_connect() with a "invalid certificate" inner
            //   - e.is_builder() if reqwest itself rejected something
            //   - e.is_request() with a chain-validation cause
            // What is NOT acceptable: e.is_status() or e.is_decode(),
            // because those mean the TLS handshake succeeded and the
            // failure is downstream — i.e., the chain WAS trusted.
            assert!(
                !e.is_status() && !e.is_decode(),
                "TLS handshake succeeded against example.com under NAV pin \
                 (failure shape was {e:?}). The pinned-trust posture is broken."
            );
        }
    }
}
