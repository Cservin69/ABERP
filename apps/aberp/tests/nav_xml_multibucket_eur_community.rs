//! Pre-cut integration pin (PROD_v2.33.0) — ADR-0103's B3′ multi-bucket
//! summary crossed with the features it was never tested against.
//!
//! Every B3′ test in `nav_xml_summary_multibucket.rs` renders a DOMESTIC
//! buyer in `Currency::Huf`. That makes `huf_equivalent_for` the identity
//! function and never emits `<communityVatNumber>`, so two whole
//! interactions were uncovered:
//!
//!   * **EUR × multi-bucket** — the HUF mirror is now computed PER BUCKET
//!     and summed (ADR-0037 §1.c "invoice-level total HUF amount … the sum
//!     of the per-VAT-rate HUF amounts, NOT by converting the EUR invoice
//!     total directly"). With one bucket the two formulations coincide; the
//!     moment there are two they do not.
//!   * **`CustomerVatStatus::Other` (ADR-0102) × multi-bucket** — a
//!     foreign-EU buyer is buyer-agnostic for `Percent` lines, so a
//!     community-VAT invoice with several rates is reachable and emits
//!     `<communityVatNumber>` beside N buckets.
//!
//! ⚠ KNOWN DIVERGENCE, deliberately NOT asserted here (see
//! `docs/findings/precut-integration-v2.33.0-2026-07-27.md`, finding I1):
//! `RateMetadata.huf_equivalent_total` — the value stored on the invoice
//! row, printed on the PDF and shown in the SPA — is computed by
//! `issue_invoice::finalize_rate` as ONE round-half-even of the invoice's
//! gross cents, which is precisely the direct conversion §1.c forbids. For
//! the body below it is 7169 while the wire carries 7170. The WIRE is the
//! §1.c-correct one. Fixing the books side means reworking `finalize_rate`
//! and the two chain-inheritance call sites, which is its own PR.

use aberp::nav_xml::{
    self, CustomerAddress, CustomerInfo, CustomerVatStatus, NavParties, SupplierInfo,
};
use aberp_billing::{
    Currency, CustomerId, Huf, InvoiceId, LineItem, RateMetadata, ReadyInvoice, SeriesCode,
    SeriesId, VatRateKind,
};
use aberp_nav_xsd_validator::validate_invoice_data;
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::date;
use time::OffsetDateTime;

fn line(desc: &str, unit_price: i64, bp: u16) -> LineItem {
    LineItem {
        description: desc.to_string(),
        quantity: Decimal::from(1),
        unit_price: Huf(unit_price),
        vat_rate_basis_points: bp,
        vat_rate_kind: VatRateKind::Percent,
        note: None,
        unit: None,
    }
}

/// ADR-0102 foreign-EU business buyer: no Hungarian tax number, a
/// `<communityVatNumber>` instead.
fn community_parties() -> NavParties {
    NavParties {
        supplier: SupplierInfo {
            tax_number: "24904362-2-41".to_string(),
            name: "Aben Consulting Kft".to_string(),
            address_country_code: "HU".to_string(),
            address_postal_code: "1037".to_string(),
            address_city: "Budapest".to_string(),
            address_street: "Visszatero koz 6".to_string(),
        },
        customer: CustomerInfo {
            community_vat_number: Some("ATU12345678".to_string()),
            customer_vat_status: CustomerVatStatus::Other,
            tax_number: None,
            name: "Wiener Metallbau GmbH".to_string(),
            address: Some(CustomerAddress {
                country_code: "AT".to_string(),
                postal_code: "1010".to_string(),
                city: "Wien".to_string(),
                street: "Kaerntner Ring 1".to_string(),
            }),
        },
    }
}

/// Text content of every `<tag>…</tag>` in document order.
fn text_of(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let j = after.find(&close).expect("closing tag");
        out.push(after[..j].to_string());
        rest = &after[j + close.len()..];
    }
    out
}

/// EUR + foreign-EU buyer + two `Percent` rates in one body.
///
/// The two line prices are chosen so the per-bucket HUF roundings do NOT
/// agree with a single rounding of the invoice total (bucket grosses 1000
/// and 1010 cents → 3567 + 3603 = 7170 HUF, while one rounding of 2010
/// cents gives 7169). That is what makes this body a real test of the §1.c
/// rule rather than a case where both formulations happen to coincide.
///
/// MUTATIONS that must turn this red: revert `write_summary` to a single
/// `lines.first()` bucket (bucket count + per-bucket money); make the
/// invoice-level HUF a fresh conversion of the native grand total
/// (`invoiceGrossAmountHUF != Σ vatRateGrossAmountHUF`); drop the
/// `CustomerVatStatus::Other` arm's `<communityVatNumber>` emit.
#[test]
fn eur_community_buyer_multirate_validates_and_reconciles_per_bucket() {
    let rate = Decimal::from_str("356.690000").unwrap();
    // 27% on net 788 → vat 212, gross 1000.  5% on net 962 → vat 48, gross 1010.
    let invoice = ReadyInvoice {
        id: InvoiceId::new(),
        series_id: SeriesId::new(),
        customer_id: CustomerId::new(),
        sequence_number: 7,
        fiscal_year: 2026,
        lines: vec![line("27% part", 788, 2700), line("5% book", 962, 500)],
        issue_date: OffsetDateTime::now_utc(),
        payment_deadline: OffsetDateTime::now_utc().date(),
        delivery_date: OffsetDateTime::now_utc().date(),
    };
    let meta = RateMetadata {
        rate,
        source: "MNB".to_string(),
        date: date!(2026 - 05 - 08),
        // Not read by `write_summary`'s amounts — only the rate is. Set to
        // the wire's own figure so this fixture does not silently encode
        // the finding-I1 divergence as if it were correct.
        huf_equivalent_total: 7170,
    };

    let xml = nav_xml::render_invoice_data(
        &invoice,
        &SeriesCode::new("INV-default".to_string()).unwrap(),
        &community_parties(),
        Currency::Eur,
        Some(&meta),
    )
    .expect("emitter must succeed");
    validate_invoice_data(&xml).unwrap_or_else(|e| {
        panic!(
            "validator rejected a multi-bucket EUR community body: {e}\n{}",
            String::from_utf8_lossy(&xml)
        )
    });
    let compact: String = String::from_utf8(xml)
        .expect("emit is UTF-8")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    // Two distinct rates → two buckets, and the ADR-0102 buyer block.
    assert_eq!(
        compact.matches("<summaryByVatRate>").count(),
        2,
        "two Percent rates must emit two buckets; body:\n{compact}"
    );
    assert!(
        compact.contains("<communityVatNumber>ATU12345678</communityVatNumber>"),
        "the Other-buyer arm must emit communityVatNumber; body:\n{compact}"
    );
    assert!(
        !compact.contains("customerTaxNumber"),
        "an Other buyer must NOT carry the domestic structured tax block"
    );

    // Buckets are sorted (kind, basis points), so 5% precedes 27%.
    assert_eq!(text_of(&compact, "vatRateNetAmount"), ["9.62", "7.88"]);
    assert_eq!(text_of(&compact, "vatRateVatAmount"), ["0.48", "2.12"]);
    assert_eq!(text_of(&compact, "vatRateGrossAmount"), ["10.10", "10.00"]);

    // Native invoice-level totals are the sum over buckets.
    assert_eq!(text_of(&compact, "invoiceNetAmount"), ["17.50"]);
    assert_eq!(text_of(&compact, "invoiceVatAmount"), ["2.60"]);
    assert_eq!(text_of(&compact, "invoiceGrossAmount"), ["20.10"]);

    // ADR-0037 §1.c — the invoice-level HUF figures are the SUM of the
    // per-bucket HUF figures. 3603 + 3567 = 7170; a direct conversion of
    // 2010 cents would give 7169, which is what this pin exists to catch.
    assert_eq!(text_of(&compact, "vatRateGrossAmountHUF"), ["3603", "3567"]);
    let isum = |tag: &str| -> i64 {
        text_of(&compact, tag)
            .iter()
            .map(|s| s.parse::<i64>().unwrap())
            .sum()
    };
    for (bucket_tag, invoice_tag) in [
        ("vatRateNetAmountHUF", "invoiceNetAmountHUF"),
        ("vatRateVatAmountHUF", "invoiceVatAmountHUF"),
        ("vatRateGrossAmountHUF", "invoiceGrossAmountHUF"),
    ] {
        assert_eq!(
            isum(invoice_tag),
            isum(bucket_tag),
            "{invoice_tag} must equal Σ {bucket_tag} (ADR-0037 §1.c)"
        );
    }
    assert_eq!(isum("invoiceGrossAmountHUF"), 7170);
}
