//! ADR-0099 H3 Addendum 3 — SERVE_HANDLE_LIVE tripwire, RAW `Connection::open`
//! coverage (session 6).
//!
//! The tripwire's chokepoint asserts live inside `Ledger::open` and
//! `DuckDbBillingStore::open`. But the invoice family also fresh-opens the DB
//! through FOUR raw `duckdb::Connection::open` sites — a foreign fn with no
//! chokepoint, so the oracle was BLIND to them (session 5 FINDING 1):
//!   * `mark_invoice_paid.rs:127`  (shadowed at runtime by the `Ledger::open`
//!     idempotency gate at :114 — see the shadow note on the mark_paid test)
//!   * `submit_invoice.rs:484`
//!   * `poll_ack.rs:361`           (poll_ack_from_inputs)
//!   * `poll_ack.rs:1008`          (write_daemon_terminal_ack — private; proven
//!     by a unit test inside `poll_ack.rs` at its own module)
//!
//! Session 6 wired an explicit `serve_tripwire::assert_no_serve_handle(path,
//! "Connection::open @ …")` immediately before each raw open. This file proves
//! the guard has TEETH at the reachable-first sites — while a serve `Handle` is
//! registered on the tenant DB, driving the fn to its raw open panics with the
//! site-labelled tripwire message BEFORE any NAV interaction.
//!
//! Debug/test only by construction (`assert_no_serve_handle` is
//! `debug_assertions`-gated); `cargo test` builds set it, so these run.

use std::path::PathBuf;

use aberp_audit_ledger::serve_tripwire::register_serve_handle;
use aberp_audit_ledger::{Actor, BinaryHash, Ledger, TenantId};
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
/// throwaway `Ledger::open`) so a later open is a genuine independent RE-open,
/// not a create.
fn seeded_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aberp-raw-conn-{tag}-{}.duckdb", Ulid::new()));
    let _ = std::fs::remove_file(&p);
    drop(Ledger::open(&p, tid(), BH).expect("seed the tenant DB file"));
    p
}

/// A throwaway NAV credential blob. The tripwire fires at the DB OPEN, which is
/// reached BEFORE any wire call, so these bytes are never used against NAV.
fn dummy_credentials() -> NavCredentials {
    NavCredentials::from_parts(
        "raw-conn-tripwire",
        "TECHNICAL_LOGIN",
        "tech-password-01",
        "SIGN-KEY-32B-ASCII-XXXXXXXXXXXXX",
        "1234567890ABCDEF",
    )
}

/// `poll_ack_from_inputs` reaches its raw `Connection::open` (poll_ack.rs:361)
/// right after parsing the tenant + tax number — BEFORE the NAV token exchange.
/// While a serve Handle is registered, that open must trip with the site label.
#[tokio::test]
#[should_panic(expected = "Connection::open @ poll_ack_from_inputs")]
async fn poll_ack_from_inputs_raw_open_trips_while_serve_handle_registered() {
    let db = seeded_db("poll");
    let creds = dummy_credentials();
    let actor = Actor::from_local_cli(Ulid::new().to_string(), "raw-conn-test");
    let _guard = register_serve_handle(&db);
    // Reaches poll_ack.rs:361 -> the guarded raw open -> panic before any NAV I/O.
    let _ = poll_ack::poll_ack_from_inputs(
        &db,
        "raw-conn-tripwire",
        "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "12345678",
        NavEndpoint::Test,
        &creds,
        actor,
    )
    .await;
}

/// `submit_from_inputs` validates the InvoiceData XML (step 3a) then reaches its
/// raw `Connection::open` (submit_invoice.rs:484) — still BEFORE the NAV token
/// exchange. `MIN_VALID` is the validator's own golden positive fixture (copied
/// so this test does not depend on a `#[cfg(test)]` const in another crate), so
/// step 3a passes and control reaches the guarded open, which must trip.
#[tokio::test]
#[should_panic(expected = "Connection::open @ submit_invoice")]
async fn submit_from_inputs_raw_open_trips_while_serve_handle_registered() {
    let db = seeded_db("submit");
    let creds = dummy_credentials();
    let actor = Actor::from_local_cli(Ulid::new().to_string(), "raw-conn-test");
    let _guard = register_serve_handle(&db);
    // Reaches submit_invoice.rs:484 -> the guarded raw open -> panic before NAV.
    let _ = submit_invoice::submit_from_inputs(submit_invoice::SubmitFromInputs {
        db: &db,
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
}

/// `mark_paid` is CAUGHT by the oracle — but at its `Ledger::open` idempotency
/// gate (mark_invoice_paid.rs:114), which runs BEFORE the raw open at :127. So
/// the runtime panic here names `Ledger::open`, not the raw-open label. This
/// test pins that mark_paid trips at all while registered (oracle completeness);
/// the raw-open guard at :127 becomes load-bearing only once :114 is migrated to
/// the Handle, and its exact label is proven by the session-6 injection
/// experiment recorded in the handoff (the :114 shadow removed transiently).
#[test]
#[should_panic(expected = "SERVE_HANDLE_LIVE tripwire")]
fn mark_paid_is_caught_while_serve_handle_registered() {
    let db = seeded_db("markpaid");
    let input = MarkPaidInput {
        invoice_id: "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        paid_at: "2026-05-20".to_string(),
        amount_minor: 1000,
        currency: "HUF".to_string(),
        method: PaymentMethod::BankTransfer,
        reference: None,
    };
    let _guard = register_serve_handle(&db);
    // Trips at mark_invoice_paid.rs:114 (Ledger::open) — the fn is caught.
    let _ = mark_invoice_paid::mark_paid(&db, tid(), BH, "raw-conn-test", input);
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
