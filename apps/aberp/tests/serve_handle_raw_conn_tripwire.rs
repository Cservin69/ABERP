//! ADR-0099 H3 — post-migration regression: the invoice-family writers route
//! through the shared `Handle` and NO LONGER fork the DB (this SUPERSEDES the
//! session-6 raw-`Connection::open` tripwire oracle, whose job — proving the
//! pre-migration fork sites tripped — is spent now that those sites are gone).
//!
//! Before the invoice-family migration these fns fresh-opened the tenant DB
//! (`DuckDbBillingStore::open` / `Ledger::open` / four raw `Connection::open`),
//! so while a serve `Handle` was registered the SERVE_HANDLE_LIVE tripwire caught
//! them. After the migration every read is `db.read()` and every append is under
//! a `db.write()` guard on the ONE shared instance — there is no independent
//! opener left, so the tripwire (its chokepoint asserts still live in
//! `Ledger::open` / `DuckDbBillingStore::open`, and are exercised by
//! `serve_handle_tripwire.rs`) has NOTHING to fire on in these paths.
//!
//! This file now PINS that inversion: with a serve `Handle` registered on the
//! tenant DB, driving each migrated writer through the SAME `Handle` must NOT
//! panic with a tripwire message — it either succeeds or fails cleanly on the
//! (deliberately-absent) invoice, never on a fork. A regression that re-introduced
//! a fresh open on any of these paths would panic here (the tripwire would fire)
//! instead of passing. Debug/test only by construction (`assert_no_serve_handle`
//! is `debug_assertions`-gated); `cargo test` builds set it, so these run.

use std::path::PathBuf;

use aberp_audit_ledger::serve_tripwire::register_serve_handle;
use aberp_audit_ledger::{Actor, BinaryHash, Ledger, TenantId};
use aberp_db::Handle;
use aberp_nav_transport::{NavCredentials, NavEndpoint};
use ulid::Ulid;

use aberp::audit_payloads::PaymentMethod;
use aberp::mark_invoice_paid::{self, MarkPaidInput};
use aberp::{poll_ack, submit_invoice};

const BH: BinaryHash = BinaryHash::from_bytes([7u8; 32]);

fn tid() -> TenantId {
    TenantId::new("raw-conn-tripwire".to_string()).unwrap()
}

/// A unique temp DB path per test — the registry is process-global, so distinct
/// paths keep parallel tests from cross-contaminating. Creates the file (via a
/// throwaway `Ledger::open`, BEFORE any registration) so opening the `Handle`
/// on it is a genuine re-open of an existing tenant DB.
fn seeded_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aberp-raw-conn-{tag}-{}.duckdb", Ulid::new()));
    let _ = std::fs::remove_file(&p);
    drop(Ledger::open(&p, tid(), BH).expect("seed the tenant DB file"));
    p
}

/// A throwaway NAV credential blob. The migrated writers fail on the absent
/// invoice at the Handle READ — BEFORE any wire call — so these bytes are never
/// used against NAV.
fn dummy_credentials() -> NavCredentials {
    NavCredentials::from_parts(
        "raw-conn-tripwire",
        "TECHNICAL_LOGIN",
        "tech-password-01",
        "SIGN-KEY-32B-ASCII-XXXXXXXXXXXXX",
        "1234567890ABCDEF",
    )
}

/// `poll_ack_from_inputs` now loads the invoice through `db.read()` (was a raw
/// `Connection::open`). While a serve `Handle` is registered on the same path,
/// driving it through that `Handle` must NOT trip — it fails cleanly on the
/// absent invoice, BEFORE any NAV I/O. (A `#[should_panic]` here would mean a
/// fresh open crept back in.)
#[tokio::test]
async fn poll_ack_from_inputs_routes_through_handle_without_tripping() {
    let db_path = seeded_db("poll");
    let handle = Handle::open_default(&db_path, tid()).expect("open shared handle");
    let creds = dummy_credentials();
    let actor = Actor::from_local_cli(Ulid::new().to_string(), "raw-conn-test");
    let _guard = register_serve_handle(&db_path);
    let res = poll_ack::poll_ack_from_inputs(
        &handle,
        "raw-conn-tripwire",
        "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "12345678",
        NavEndpoint::Test,
        &creds,
        actor,
    )
    .await;
    // Reaches the Handle read (no tripwire panic), fails on the absent invoice.
    assert!(
        res.is_err(),
        "expected a clean Err on the absent invoice, not a tripwire panic"
    );
}

/// `submit_from_inputs` validates the InvoiceData XML (step 3a), then loads the
/// invoice through `db.read()` (was a raw `Connection::open`). Registered +
/// driven through the SAME `Handle`, it must NOT trip — it fails cleanly on the
/// absent invoice BEFORE the NAV token exchange.
#[tokio::test]
async fn submit_from_inputs_routes_through_handle_without_tripping() {
    let db_path = seeded_db("submit");
    let handle = Handle::open_default(&db_path, tid()).expect("open shared handle");
    let creds = dummy_credentials();
    let actor = Actor::from_local_cli(Ulid::new().to_string(), "raw-conn-test");
    let _guard = register_serve_handle(&db_path);
    let res = submit_invoice::submit_from_inputs(submit_invoice::SubmitFromInputs {
        db: &handle,
        tenant_str: "raw-conn-tripwire",
        invoice_id_str: "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        invoice_xml_origin: "raw-conn-test-fixture".to_string(),
        invoice_xml: MIN_VALID.as_bytes().to_vec(),
        tax_number_raw: "12345678",
        nav_endpoint: NavEndpoint::Test,
        endpoint_audit_label: "test",
        credentials: &creds,
        actor,
    })
    .await;
    assert!(
        res.is_err(),
        "expected a clean Err on the absent invoice, not a tripwire panic"
    );
}

/// `mark_paid` now reads the idempotency gate via `db.read()` and appends under a
/// `db.write()` guard (was `Ledger::open` + a raw `Connection::open`). Registered
/// + driven through the SAME `Handle`, it must NOT trip — it records the payment
/// and returns `Ok` (it does not require a pre-existing billing row).
#[test]
fn mark_paid_routes_through_handle_without_tripping() {
    let db_path = seeded_db("markpaid");
    let handle = Handle::open_default(&db_path, tid()).expect("open shared handle");
    let input = MarkPaidInput {
        invoice_id: "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        paid_at: "2026-05-20".to_string(),
        amount_minor: 1000,
        currency: "HUF".to_string(),
        method: PaymentMethod::BankTransfer,
        reference: None,
    };
    let _guard = register_serve_handle(&db_path);
    let res = mark_invoice_paid::mark_paid(&handle, tid(), BH, "raw-conn-test", input);
    assert!(
        res.is_ok(),
        "mark_paid must record the payment through the Handle without tripping: {res:?}"
    );
}

/// The validator's golden minimum-valid InvoiceData, copied verbatim from
/// `crates/nav-xsd-validator/src/validate.rs` (`MIN_VALID`). Only used to get
/// `submit_from_inputs` past its step-3a XSD walk so control reaches the guarded
/// raw open; the bytes never leave the process.
const MIN_VALID: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<InvoiceData xmlns="http://schemas.nav.gov.hu/OSA/3.0/data" xmlns:common="http://schemas.nav.gov.hu/OSA/3.0/base">
  <invoiceNumber>INV-default/00001</invoiceNumber>
  <invoiceIssueDate>2026-05-20</invoiceIssueDate>
  <completenessIndicator>false</completenessIndicator>
  <invoiceMain>
    <invoice>
      <invoiceHead>
        <supplierInfo>
          <supplierTaxNumber>
            <common:taxpayerId>12345678</common:taxpayerId>
            <common:vatCode>1</common:vatCode>
            <common:countyCode>42</common:countyCode>
          </supplierTaxNumber>
          <supplierName>ABERP Supplier Kft.</supplierName>
          <supplierAddress>
            <simpleAddress>
              <countryCode>HU</countryCode>
              <postalCode>1011</postalCode>
              <city>Budapest</city>
              <additionalAddressDetail>Fő utca 1.</additionalAddressDetail>
            </simpleAddress>
          </supplierAddress>
        </supplierInfo>
        <customerInfo>
          <customerVatStatus>DOMESTIC</customerVatStatus>
          <customerVatData>
            <customerTaxNumber>
              <common:taxpayerId>87654321</common:taxpayerId>
              <common:vatCode>1</common:vatCode>
              <common:countyCode>42</common:countyCode>
            </customerTaxNumber>
          </customerVatData>
          <customerName>Test Customer Zrt.</customerName>
          <customerAddress>
            <common:simpleAddress>
              <common:countryCode>HU</common:countryCode>
              <common:postalCode>1052</common:postalCode>
              <common:city>Budapest</common:city>
              <common:additionalAddressDetail>Váci utca 19.</common:additionalAddressDetail>
            </common:simpleAddress>
          </customerAddress>
        </customerInfo>
        <invoiceDetail>
          <invoiceCategory>NORMAL</invoiceCategory>
          <invoiceDeliveryDate>2026-05-20</invoiceDeliveryDate>
          <currencyCode>HUF</currencyCode>
          <exchangeRate>1</exchangeRate>
          <paymentMethod>TRANSFER</paymentMethod>
          <paymentDate>2026-05-20</paymentDate>
          <invoiceAppearance>ELECTRONIC</invoiceAppearance>
        </invoiceDetail>
      </invoiceHead>
      <invoiceLines>
        <mergedItemIndicator>false</mergedItemIndicator>
        <line>
          <lineNumber>1</lineNumber>
          <lineExpressionIndicator>false</lineExpressionIndicator>
          <lineDescription>Test widget</lineDescription>
          <quantity>2</quantity>
          <unitOfMeasure>PIECE</unitOfMeasure>
          <unitPrice>1000</unitPrice>
          <lineAmountsNormal>
            <lineNetAmountData>
              <lineNetAmount>2000</lineNetAmount>
              <lineNetAmountHUF>2000</lineNetAmountHUF>
            </lineNetAmountData>
            <lineVatRate>
              <vatPercentage>0.27</vatPercentage>
            </lineVatRate>
            <lineVatData>
              <lineVatAmount>540</lineVatAmount>
              <lineVatAmountHUF>540</lineVatAmountHUF>
            </lineVatData>
            <lineGrossAmountData>
              <lineGrossAmountNormal>2540</lineGrossAmountNormal>
              <lineGrossAmountNormalHUF>2540</lineGrossAmountNormalHUF>
            </lineGrossAmountData>
          </lineAmountsNormal>
        </line>
      </invoiceLines>
      <invoiceSummary>
        <summaryNormal>
          <summaryByVatRate>
            <vatRate>
              <vatPercentage>0.27</vatPercentage>
            </vatRate>
            <vatRateNetData>
              <vatRateNetAmount>2000</vatRateNetAmount>
              <vatRateNetAmountHUF>2000</vatRateNetAmountHUF>
            </vatRateNetData>
            <vatRateVatData>
              <vatRateVatAmount>540</vatRateVatAmount>
              <vatRateVatAmountHUF>540</vatRateVatAmountHUF>
            </vatRateVatData>
            <vatRateGrossData>
              <vatRateGrossAmount>2540</vatRateGrossAmount>
              <vatRateGrossAmountHUF>2540</vatRateGrossAmountHUF>
            </vatRateGrossData>
          </summaryByVatRate>
          <invoiceNetAmount>2000</invoiceNetAmount>
          <invoiceNetAmountHUF>2000</invoiceNetAmountHUF>
          <invoiceVatAmount>540</invoiceVatAmount>
          <invoiceVatAmountHUF>540</invoiceVatAmountHUF>
        </summaryNormal>
        <summaryGrossData>
          <invoiceGrossAmount>2540</invoiceGrossAmount>
          <invoiceGrossAmountHUF>2540</invoiceGrossAmountHUF>
        </summaryGrossData>
      </invoiceSummary>
    </invoice>
  </invoiceMain>
</InvoiceData>"#;
