//! B3 / B3′ — ADR-0103 §3.1 (Invariant S — summary coverage).
//!
//! `write_summary` groups lines by `(vat_rate_kind, vat_rate_basis_points)`
//! and emits ONE `<summaryByVatRate>` per group, each carrying its own
//! group's net/vat/gross; the invoice-level totals are the sum over buckets.
//!
//! Before B3′ the emitter took the bucket from `lines.first()` and summed
//! across ALL lines — so a mixed-rate invoice sent NAV one bucket carrying
//! every line's money under the first line's rate (silently wrong ÁFA). The
//! local NAV XSD validator was built to match that wrong shape and bounced a
//! correct multi-bucket body with `ChildOrderViolation`; it was corrected in
//! lock-step. Ground truth: the published NAV OSA 3.0 `invoiceData.xsd`
//! defines `SummaryNormalType/summaryByVatRate` as `maxOccurs="unbounded"`.
//!
//! Each test names the MUTATION that must turn it red (all mutation-verified
//! in the implementation session).

use aberp::nav_xml::{
    self, CustomerAddress, CustomerInfo, CustomerVatStatus, NavParties, StornoReference,
    SupplierInfo,
};
use aberp_billing::{
    Currency, CustomerId, Huf, InvoiceId, LineItem, ReadyInvoice, SeriesCode, SeriesId, VatRateKind,
};
use aberp_nav_xsd_validator::validate_invoice_data;
use time::OffsetDateTime;

fn line(desc: &str, qty: i64, unit_price: i64, bp: u16, kind: VatRateKind) -> LineItem {
    LineItem {
        description: desc.to_string(),
        quantity: rust_decimal::Decimal::from(qty),
        unit_price: Huf(unit_price),
        vat_rate_basis_points: bp,
        vat_rate_kind: kind,
        note: None,
        unit: None,
    }
}

fn invoice_with_lines(lines: Vec<LineItem>) -> ReadyInvoice {
    ReadyInvoice {
        id: InvoiceId::new(),
        series_id: SeriesId::new(),
        customer_id: CustomerId::new(),
        sequence_number: 1,
        fiscal_year: 0,
        lines,
        issue_date: OffsetDateTime::now_utc(),
        payment_deadline: OffsetDateTime::now_utc().date(),
        delivery_date: OffsetDateTime::now_utc().date(),
    }
}

fn domestic_parties() -> NavParties {
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
            community_vat_number: None,
            customer_vat_status: CustomerVatStatus::Domestic,
            tax_number: Some("27952890-2-42".to_string()),
            name: "AZ9 Services".to_string(),
            address: Some(CustomerAddress {
                country_code: "HU".to_string(),
                postal_code: "1097".to_string(),
                city: "Budapest".to_string(),
                street: "Ulloi ut 1.".to_string(),
            }),
        },
    }
}

fn series() -> SeriesCode {
    SeriesCode::new("INV-default".to_string()).unwrap()
}

/// Render an invoice body and prove the (corrected) validator accepts it.
fn render(lines: Vec<LineItem>) -> String {
    let invoice = invoice_with_lines(lines);
    let xml = nav_xml::render_invoice_data(
        &invoice,
        &series(),
        &domestic_parties(),
        Currency::Huf,
        None,
    )
    .expect("emitter must succeed");
    validate_invoice_data(&xml).unwrap_or_else(|e| {
        panic!(
            "validator rejected multi-bucket body: {e}\n--- bytes ---\n{}\n--- end ---",
            String::from_utf8_lossy(&xml)
        )
    });
    String::from_utf8(xml).expect("emit is UTF-8")
}

/// Whitespace-free view so field-by-field bucket assertions do not depend on
/// the emitter's indentation.
fn compact(body: &str) -> String {
    body.chars().filter(|c| !c.is_whitespace()).collect()
}

fn bucket_count(body: &str) -> usize {
    body.matches("<summaryByVatRate>").count()
}

// ── T1 — two different Percent RATES on one invoice (the live B3′ case) ────

/// ⭐ The single most important pin in ADR-0103: 27% + 5% on ONE invoice,
/// both `Percent`. Two buckets, each carrying ITS OWN line's net/vat/gross
/// (NOT the sum), and the invoice-level totals equal the sum over buckets.
/// This case had ZERO coverage before 0103 and is SPA-reachable.
///
/// MUTATION: revert `write_summary` to `lines.first()` + sum-over-all — the
/// output collapses to ONE bucket carrying 30000 net under 27%.
#[test]
fn two_percent_rates_emit_two_buckets_each_with_its_own_totals() {
    // A: 27%, net 20000, vat 5400, gross 25400.  B: 5%, net 10000, vat 500, gross 10500.
    let body = render(vec![
        line("27% line", 2, 10_000, 2700, VatRateKind::Percent),
        line("5% line", 1, 10_000, 500, VatRateKind::Percent),
    ]);
    let c = compact(&body);
    assert_eq!(
        bucket_count(&body),
        2,
        "one bucket per distinct rate; body:\n{body}"
    );

    // 5% bucket (sorted first: same kind name, lower basis points) — full triple.
    assert!(
        c.contains(
            "<summaryByVatRate><vatRate><vatPercentage>0.05</vatPercentage></vatRate>\
             <vatRateNetData><vatRateNetAmount>10000</vatRateNetAmount><vatRateNetAmountHUF>10000</vatRateNetAmountHUF></vatRateNetData>\
             <vatRateVatData><vatRateVatAmount>500</vatRateVatAmount><vatRateVatAmountHUF>500</vatRateVatAmountHUF></vatRateVatData>\
             <vatRateGrossData><vatRateGrossAmount>10500</vatRateGrossAmount><vatRateGrossAmountHUF>10500</vatRateGrossAmountHUF></vatRateGrossData>\
             </summaryByVatRate>"
        ),
        "5% bucket must carry its OWN totals (500 VAT, not 5900); body:\n{body}"
    );
    // 27% bucket — full triple.
    assert!(
        c.contains(
            "<summaryByVatRate><vatRate><vatPercentage>0.27</vatPercentage></vatRate>\
             <vatRateNetData><vatRateNetAmount>20000</vatRateNetAmount><vatRateNetAmountHUF>20000</vatRateNetAmountHUF></vatRateNetData>\
             <vatRateVatData><vatRateVatAmount>5400</vatRateVatAmount><vatRateVatAmountHUF>5400</vatRateVatAmountHUF></vatRateVatData>\
             <vatRateGrossData><vatRateGrossAmount>25400</vatRateGrossAmount><vatRateGrossAmountHUF>25400</vatRateGrossAmountHUF></vatRateGrossData>\
             </summaryByVatRate>"
        ),
        "27% bucket must carry its OWN totals; body:\n{body}"
    );
    // Invoice-level totals are the sum OVER buckets.
    assert!(
        c.contains(
            "<invoiceNetAmount>30000</invoiceNetAmount><invoiceNetAmountHUF>30000</invoiceNetAmountHUF>\
             <invoiceVatAmount>5900</invoiceVatAmount><invoiceVatAmountHUF>5900</invoiceVatAmountHUF>"
        ),
        "invoice-level totals must equal the sum over buckets (5900 VAT); body:\n{body}"
    );
    assert!(
        c.contains("<invoiceGrossAmount>35900</invoiceGrossAmount><invoiceGrossAmountHUF>35900</invoiceGrossAmountHUF>"),
        "invoice gross must be 35900; body:\n{body}"
    );
}

// ── T2 — mixed KIND: a Percent line + an AAM-exempt line ───────────────────

/// One `Percent` 27% line + one `AamExempt` line → two buckets, each with the
/// correct `<vatRate>` choice element, and the exempt bucket's
/// `vatRateVatAmount` is 0.
///
/// MUTATION: revert `write_summary` to `lines.first()` — a single bucket keyed
/// on whichever line is first, carrying the other line's money.
#[test]
fn mixed_kind_percent_plus_aam_emits_two_buckets_exempt_vat_zero() {
    let body = render(vec![
        line("27% line", 2, 10_000, 2700, VatRateKind::Percent),
        line("AAM exempt line", 1, 10_000, 0, VatRateKind::AamExempt),
    ]);
    let c = compact(&body);
    assert_eq!(
        bucket_count(&body),
        2,
        "one Percent bucket + one exempt bucket; body:\n{body}"
    );

    // AamExempt bucket (sorted first by kind name): vatExemption/AAM, VAT = 0.
    assert!(
        c.contains("<summaryByVatRate><vatRate><vatExemption><case>AAM</case>"),
        "exempt bucket must emit the vatExemption/AAM choice element; body:\n{body}"
    );
    assert!(
        c.contains(
            "<vatRateNetData><vatRateNetAmount>10000</vatRateNetAmount><vatRateNetAmountHUF>10000</vatRateNetAmountHUF></vatRateNetData>\
             <vatRateVatData><vatRateVatAmount>0</vatRateVatAmount><vatRateVatAmountHUF>0</vatRateVatAmountHUF></vatRateVatData>\
             <vatRateGrossData><vatRateGrossAmount>10000</vatRateGrossAmount><vatRateGrossAmountHUF>10000</vatRateGrossAmountHUF></vatRateGrossData>"
        ),
        "exempt bucket VAT must be 0 and gross must equal net; body:\n{body}"
    );
    // Percent bucket carries its own 5400 VAT.
    assert!(
        c.contains("<vatRateVatData><vatRateVatAmount>5400</vatRateVatAmount>"),
        "the 27% bucket must carry 5400 VAT; body:\n{body}"
    );
    // Invoice VAT = 5400 (only the Percent line contributes).
    assert!(
        c.contains("<invoiceVatAmount>5400</invoiceVatAmount><invoiceVatAmountHUF>5400</invoiceVatAmountHUF>"),
        "invoice VAT must be 5400; body:\n{body}"
    );
}

// ── T3 — storno of a multi-rate base (B3's second reachable path) ──────────

/// A storno of a two-rate base emits the multi-bucket summary with NEGATED
/// per-bucket amounts. The storno render path shares `write_summary` (on the
/// negated lines), so the fix covers it automatically.
///
/// MUTATION: revert `write_summary` to `lines.first()` — one negated bucket
/// carrying the whole reversal under the first rate.
#[test]
fn storno_of_multi_rate_base_emits_negated_multi_bucket_summary() {
    let base = invoice_with_lines(vec![
        line("27% line", 2, 10_000, 2700, VatRateKind::Percent),
        line("5% line", 1, 10_000, 500, VatRateKind::Percent),
    ]);
    let reference = StornoReference {
        base_invoice_number: "INV-default/00001".to_string(),
        modification_index: 1,
        base_line_count: 2,
    };
    let xml = nav_xml::render_storno_data(
        &base,
        &series(),
        &domestic_parties(),
        &reference,
        Currency::Huf,
        None,
    )
    .expect("storno emitter must succeed");
    validate_invoice_data(&xml).unwrap_or_else(|e| {
        panic!(
            "validator rejected multi-bucket storno: {e}\n{}",
            String::from_utf8_lossy(&xml)
        )
    });
    let body = String::from_utf8(xml).unwrap();
    let c = compact(&body);
    assert_eq!(
        bucket_count(&body),
        2,
        "storno must mirror the base's two buckets; body:\n{body}"
    );
    // Negated per-bucket VAT.
    assert!(
        c.contains("<vatRateVatAmount>-500</vatRateVatAmount>"),
        "5% bucket must be negated to -500; body:\n{body}"
    );
    assert!(
        c.contains("<vatRateVatAmount>-5400</vatRateVatAmount>"),
        "27% bucket must be negated to -5400; body:\n{body}"
    );
    assert!(
        c.contains("<invoiceVatAmount>-5900</invoiceVatAmount>"),
        "invoice-level storno VAT must be -5900; body:\n{body}"
    );
}

// ── T4 — deterministic bucket order (independent of line order) ────────────

/// Two invoices with the SAME buckets but the lines in opposite order emit
/// BYTE-IDENTICAL summaries — the buckets are a stable sort on (kind, rate),
/// not first-appearance or `HashMap` order. This keeps the on-disk XML a
/// stable canonical record.
///
/// MUTATION: drop the stable sort (emit in first-appearance order) — the two
/// renders diverge.
#[test]
fn bucket_order_is_deterministic_regardless_of_line_order() {
    let forward = render(vec![
        line("27% line", 2, 10_000, 2700, VatRateKind::Percent),
        line("5% line", 1, 10_000, 500, VatRateKind::Percent),
    ]);
    let reversed = render(vec![
        line("5% line", 1, 10_000, 500, VatRateKind::Percent),
        line("27% line", 2, 10_000, 2700, VatRateKind::Percent),
    ]);
    // The <invoiceSummary> block must be byte-identical between the two.
    let extract = |b: &str| {
        let start = b.find("<invoiceSummary>").expect("summary present");
        let end = b.find("</invoiceSummary>").expect("summary end") + "</invoiceSummary>".len();
        b[start..end].to_string()
    };
    assert_eq!(
        extract(&forward),
        extract(&reversed),
        "summary must be identical regardless of line order"
    );
}

// ── T5 — single-bucket back-compat (byte-for-byte) ─────────────────────────

/// The load-bearing back-compat pin: a single-rate invoice — every invoice
/// issued to date — still emits EXACTLY one `<summaryByVatRate>` whose triple
/// and the invoice-level totals are unchanged from the pre-0103 output.
///
/// MUTATION: any change to the single-group path (this asserts the exact
/// bytes of the whole summary block for a known single-rate invoice).
#[test]
fn single_rate_invoice_emits_exactly_one_bucket_byte_identical() {
    let body = render(vec![line(
        "27% line",
        2,
        10_000,
        2700,
        VatRateKind::Percent,
    )]);
    assert_eq!(
        bucket_count(&body),
        1,
        "single-rate invoice must be single-bucket; body:\n{body}"
    );
    let c = compact(&body);
    assert!(
        c.contains(
            "<invoiceSummary><summaryNormal>\
             <summaryByVatRate><vatRate><vatPercentage>0.27</vatPercentage></vatRate>\
             <vatRateNetData><vatRateNetAmount>20000</vatRateNetAmount><vatRateNetAmountHUF>20000</vatRateNetAmountHUF></vatRateNetData>\
             <vatRateVatData><vatRateVatAmount>5400</vatRateVatAmount><vatRateVatAmountHUF>5400</vatRateVatAmountHUF></vatRateVatData>\
             <vatRateGrossData><vatRateGrossAmount>25400</vatRateGrossAmount><vatRateGrossAmountHUF>25400</vatRateGrossAmountHUF></vatRateGrossData>\
             </summaryByVatRate>\
             <invoiceNetAmount>20000</invoiceNetAmount><invoiceNetAmountHUF>20000</invoiceNetAmountHUF>\
             <invoiceVatAmount>5400</invoiceVatAmount><invoiceVatAmountHUF>5400</invoiceVatAmountHUF>\
             </summaryNormal>\
             <summaryGrossData><invoiceGrossAmount>25400</invoiceGrossAmount><invoiceGrossAmountHUF>25400</invoiceGrossAmountHUF></summaryGrossData>\
             </invoiceSummary>"
        ),
        "single-bucket summary must be byte-identical to the pre-0103 output; body:\n{body}"
    );
}

// ── T7 — exempt line never carries a non-zero VAT (emit-level B2 pin) ───────

/// An exempt line carrying a DELIBERATELY non-zero basis-points rate — the
/// gate-bypass state — still emits a zero `<lineVatAmount>` AND a zero
/// `vatRateVatAmount` in its summary bucket; the choice element is
/// `vatExemption`, never `vatPercentage`.
///
/// MUTATION: revert `vat_amount` to rate-only — the exempt line would emit a
/// phantom non-zero line VAT and bucket VAT.
#[test]
fn exempt_line_with_stray_rate_emits_zero_vat_at_line_and_summary() {
    // AamExempt but with a stray 2700 basis points (what a bypassed door admits).
    let body = render(vec![line(
        "AAM exempt",
        2,
        10_000,
        2700,
        VatRateKind::AamExempt,
    )]);
    let c = compact(&body);
    // Line-level VAT is 0.
    assert!(
        c.contains("<lineVatData><lineVatAmount>0</lineVatAmount><lineVatAmountHUF>0</lineVatAmountHUF></lineVatData>"),
        "exempt line must emit lineVatAmount 0 despite the stray rate; body:\n{body}"
    );
    // Summary bucket VAT is 0, and the choice is an exemption (never a percentage).
    assert!(
        c.contains("<vatRateVatData><vatRateVatAmount>0</vatRateVatAmount>"),
        "exempt summary bucket VAT must be 0; body:\n{body}"
    );
    assert!(
        c.contains("<vatExemption><case>AAM</case>") && !c.contains("<vatPercentage>"),
        "exempt line/summary must use vatExemption, never vatPercentage; body:\n{body}"
    );
}
