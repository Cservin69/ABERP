//! ADR-0099 Addendum 2 (Defect 1) — equality pin.
//!
//! # The defect this pins
//!
//! `drain_pending_retries`, `retry_submission`, and `recover_from_nav`
//! all drive NAV's Layer-2 `queryInvoiceCheck` / `queryInvoiceData` with
//! a NAV-facing invoice number. Pre-Addendum-2 all three SYNTHESISED
//! that number as `"{series.code}/{seq:05}"`, where `series.code` is the
//! legacy `INV-default` literal — a string NAV has NEVER seen (the real
//! number lives only in the on-disk `<InvoiceData>` XML). NAV was asked
//! about a number it does not hold, so `queryInvoiceCheck` always
//! returned `Absent` and `Layer2Decision::SkipRePost` was unreachable
//! (the Layer-2 duplicate guard never worked), and `recover-from-nav`'s
//! derived-vs-recorded drift check silently agreed (both wrong the same
//! way).
//!
//! The fix replaced every Layer-2 / NAV-query use with
//! [`aberp::nav_xml::read_invoice_number_from_xml`] — the byte-exact
//! `<invoiceNumber>` NAV holds on file (written at issuance, never
//! re-rewritten; the S184 discipline `issue_storno` /
//! `issue_modification` / `observe_receiver_confirmation` already use).
//!
//! # What this file pins (behavioural, not a source grep)
//!
//! The string that ends up inside `<invoiceNumberQuery><invoiceNumber>`
//! of the real `queryInvoiceCheck` request MUST EQUAL the
//! `<invoiceNumber>` parsed from the on-disk `<InvoiceData>` XML — and
//! MUST NOT be the pre-Addendum-2 synthesised `INV-default/{seq:05}`
//! form. This is the exact seam all three modules now share: they read
//! the number from the on-disk XML via `read_invoice_number_from_xml`
//! and hand it to `query_invoice_check::build_request`. The fixture's
//! on-disk number is deliberately chosen to DIFFER from the synthesis
//! for the same sequence number, so a regression that re-introduces
//! synthesis at any of the three call sites would send the wrong number
//! and this equality would break.

use aberp::nav_xml::{
    self, CustomerAddress, CustomerInfo, CustomerVatStatus, NavParties, SupplierInfo,
};
use aberp_billing::{
    Currency, CustomerId, Huf, InvoiceId, LineItem, PaymentMethod, ReadyInvoice, SeriesCode,
    SeriesId,
};
use aberp_nav_transport::operations::query_invoice_check;
use aberp_nav_transport::soap::InvoiceDirection;
use aberp_nav_transport::NavCredentials;
use time::OffsetDateTime;

/// The legacy series literal every emit site carries (`numbering.rs`
/// `INV-default`); its synthesised NAV number for seq 42 is the shape
/// the three defect sites used to send.
const LEGACY_SERIES: &str = "INV-default";
const SEQ: u64 = 42;
/// Deliberately distinct from the synthesised `INV-default/00042` so the
/// equality pin fails the moment any module reverts to synthesis.
const ON_DISK_NUMBER: &str = "TEST-ADD2/2026/09999";

fn plain_invoice() -> ReadyInvoice {
    ReadyInvoice {
        id: InvoiceId::new(),
        series_id: SeriesId::new(),
        customer_id: CustomerId::new(),
        sequence_number: SEQ,
        fiscal_year: 2026,
        lines: vec![LineItem {
            description: "Test megnevezés".to_string(),
            quantity: rust_decimal::Decimal::from(1),
            unit_price: Huf(1000),
            vat_rate_basis_points: 2700,
            note: None,
            unit: None,
        }],
        issue_date: OffsetDateTime::now_utc(),
        payment_deadline: OffsetDateTime::now_utc().date(),
        delivery_date: OffsetDateTime::now_utc().date(),
    }
}

fn minimal_parties() -> NavParties {
    NavParties {
        supplier: SupplierInfo {
            tax_number: "12345678-1-42".to_string(),
            name: "ABERP Supplier Kft.".to_string(),
            address_country_code: "HU".to_string(),
            address_postal_code: "1011".to_string(),
            address_city: "Budapest".to_string(),
            address_street: "Fő utca 1.".to_string(),
        },
        customer: CustomerInfo {
            customer_vat_status: CustomerVatStatus::Domestic,
            tax_number: Some("87654321-1-42".to_string()),
            name: "Test Customer Zrt.".to_string(),
            address: Some(CustomerAddress {
                country_code: "HU".to_string(),
                postal_code: "1052".to_string(),
                city: "Budapest".to_string(),
                street: "Váci utca 19.".to_string(),
            }),
        },
    }
}

/// Fixture credentials — the byte shape of the request signature does
/// not matter here; we only parse the `<invoiceNumber>` element back out.
fn fixture_credentials() -> NavCredentials {
    NavCredentials::from_parts(
        "test-tenant",
        "TECHNICAL_LOGIN",
        "tech-password-01",
        "SIGN-KEY-32B-ASCII-XXXXXXXXXXXXX",
        "1234567890ABCDEF",
    )
}

fn unique_temp_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aberp-add2-eq-{tag}-{}-{}-{}.xml",
        std::process::id(),
        nanos,
        seq
    ))
}

#[test]
fn layer2_query_number_equals_on_disk_xml_number() {
    let invoice = plain_invoice();
    let series = SeriesCode::new(LEGACY_SERIES.to_string()).unwrap();
    let parties = minimal_parties();

    // 1. Render a realistic on-disk `<InvoiceData>` XML carrying a
    //    `<invoiceNumber>` that is NOT the synthesised INV-default form.
    let invoice_data_xml = nav_xml::render_invoice_data_with_number(
        &invoice,
        &series,
        &parties,
        Currency::Huf,
        None,
        PaymentMethod::default(),
        Some(ON_DISK_NUMBER),
    )
    .expect("render on-disk InvoiceData XML");
    let data_path = unique_temp_path("data");
    std::fs::write(&data_path, &invoice_data_xml).expect("write on-disk XML");

    // 2. The production number source (all three defect sites now use
    //    this exact call) reads the on-disk `<invoiceNumber>`.
    let number_from_disk =
        nav_xml::read_invoice_number_from_xml(&data_path).expect("read number from on-disk XML");
    assert_eq!(
        number_from_disk, ON_DISK_NUMBER,
        "read_invoice_number_from_xml must return the on-disk <invoiceNumber> verbatim"
    );

    // 3. Build the real Layer-2 `queryInvoiceCheck` request with that
    //    number — exactly as drain / retry / recover do.
    let request_xml = query_invoice_check::build_request(
        &fixture_credentials(),
        "12345678",
        &number_from_disk,
        InvoiceDirection::Outbound,
    )
    .expect("build queryInvoiceCheck request");
    let request_path = unique_temp_path("request");
    std::fs::write(&request_path, &request_xml).expect("write request XML");

    // 4. Parse the `<invoiceNumber>` back out of the request. In the
    //    `<QueryInvoiceCheckRequest>` the first (and only) `<invoiceNumber>`
    //    local element is the one inside `<invoiceNumberQuery>`, so the
    //    same namespace-tolerant reader extracts it.
    let number_in_request = nav_xml::read_invoice_number_from_xml(&request_path)
        .expect("parse <invoiceNumber> from queryInvoiceCheck request");

    // 5. Equality pin: the string placed in
    //    <invoiceNumberQuery><invoiceNumber> EQUALS the on-disk XML's
    //    <invoiceNumber>.
    assert_eq!(
        number_in_request, ON_DISK_NUMBER,
        "the queryInvoiceCheck <invoiceNumber> must equal the on-disk XML <invoiceNumber>"
    );
    assert_eq!(number_in_request, number_from_disk);

    // 6. And it is NOT the pre-Addendum-2 synthesised INV-default form —
    //    the number NAV never saw, which made SkipRePost unreachable.
    let synthesised = format!("{LEGACY_SERIES}/{SEQ:05}");
    assert_eq!(synthesised, "INV-default/00042");
    assert_ne!(
        number_in_request, synthesised,
        "the query number must NOT be the synthesised INV-default/{{seq:05}} form (Defect 1)"
    );

    let _ = std::fs::remove_file(&data_path);
    let _ = std::fs::remove_file(&request_path);
}
