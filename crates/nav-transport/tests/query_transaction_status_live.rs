//! Live `queryTransactionStatus` conformance test against NAV `api-test`.
//!
//! ENV-GATED. The test body runs only when `ABERP_NAV_LIVE_TEST=1` is
//! set; otherwise it returns early. Matches the PR-7-A
//! `tls_handshake.rs` / PR-7-B-2 `token_exchange_live.rs` shape so CI
//! does not need NAV creds and offline contributors do not have a
//! flaky-by-design test.
//!
//! Required environment when ABERP_NAV_LIVE_TEST=1 is set:
//!
//!   ABERP_NAV_LIVE_TEST=1
//!   ABERP_NAV_TENANT_ID=<tenant id whose keychain is populated>
//!   ABERP_NAV_TEST_TAX_NUMBER=<8-digit base of the test taxpayer>
//!   ABERP_NAV_TEST_TRANSACTION_ID=<a recent transactionId for this taxpayer>
//!
//! The transactionId is operator-supplied because this test is a
//! standalone "does the query op work" check; the full
//! issue → submit → poll → terminal pipeline lives in
//! `apps/aberp/tests/poll_ack_live.rs` (PR-7-C-2). A recent
//! `transactionId` can be lifted from the most recent
//! `InvoiceSubmissionResponse` audit entry of any prior `submit-invoice`
//! run against `api-test`.

use aberp_nav_transport::{
    operations::query_transaction_status::{self, ProcessingStatus},
    NavCredentials, NavEndpoint, NavTransport,
};

#[tokio::test(flavor = "current_thread")]
async fn query_transaction_status_against_api_test() {
    if std::env::var("ABERP_NAV_LIVE_TEST").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping query_transaction_status_against_api_test \
             (set ABERP_NAV_LIVE_TEST=1 + ABERP_NAV_TENANT_ID + \
             ABERP_NAV_TEST_TAX_NUMBER + ABERP_NAV_TEST_TRANSACTION_ID to run)"
        );
        return;
    }

    let tenant_id = std::env::var("ABERP_NAV_TENANT_ID")
        .expect("ABERP_NAV_TENANT_ID must be set when ABERP_NAV_LIVE_TEST=1");
    let tax_number_8 = std::env::var("ABERP_NAV_TEST_TAX_NUMBER")
        .expect("ABERP_NAV_TEST_TAX_NUMBER must be set when ABERP_NAV_LIVE_TEST=1");
    let transaction_id = std::env::var("ABERP_NAV_TEST_TRANSACTION_ID").expect(
        "ABERP_NAV_TEST_TRANSACTION_ID must be set when ABERP_NAV_LIVE_TEST=1 — \
         lift a recent value from the audit ledger's InvoiceSubmissionResponse entry",
    );
    assert_eq!(
        tax_number_8.len(),
        8,
        "tax number base must be 8 digits, got {tax_number_8:?}"
    );
    assert!(
        !transaction_id.is_empty(),
        "ABERP_NAV_TEST_TRANSACTION_ID must not be empty"
    );

    let credentials = NavCredentials::load_from_keychain(&tenant_id)
        .expect("NAV credentials must be present in the OS keychain for this tenant");
    let transport = NavTransport::new(NavEndpoint::Test).expect("transport must construct");

    let outcome =
        query_transaction_status::call(&transport, &credentials, &tax_number_8, &transaction_id)
            .await
            .expect("queryTransactionStatus must succeed against api-test");

    // The returned status MUST be one of the four ADR-0009 §2 values.
    // The round-trip property is unit-tested separately; this assertion
    // is the live-call corollary — if NAV returns an unknown enumeration
    // value at runtime, the call() above already loud-fails into
    // QueryTransactionStatusResponseParse, so reaching this line means
    // the value is one of the four.
    let parsed = outcome.processing_status;
    assert!(
        matches!(
            parsed,
            ProcessingStatus::Received
                | ProcessingStatus::Processing
                | ProcessingStatus::Saved
                | ProcessingStatus::Aborted
        ),
        "processing_status must be one of the four ADR-0009 §2 values, got {parsed:?}"
    );

    assert!(
        !outcome.request_xml.is_empty(),
        "request_xml must be captured for audit"
    );
    assert!(
        !outcome.response_xml.is_empty(),
        "response_xml must be captured for audit"
    );

    eprintln!(
        "queryTransactionStatus live: transactionId={} -> {}",
        transaction_id,
        parsed.as_nav_str()
    );
}
