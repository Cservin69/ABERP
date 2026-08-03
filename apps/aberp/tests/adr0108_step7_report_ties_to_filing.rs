//! **ADR-0108 T-5(c), restated by M-2.** The ÁFA report's VAT figures are the
//! figures that were **filed**.
//!
//! # What T-5(c) used to assert, and why that was the wrong pin
//!
//! §8 T-5(c) originally read: "`unit_price × quantity` folded in Rust equals
//! the pre-migration DuckDB `DECIMAL(38,6)` aggregate for every invoice". That
//! pins the fold to the *old report*, and the old report did not agree with the
//! filing. Step 6 found the contradiction (M-2): §3.4 prescribes a fold through
//! `Money::checked_mul_decimal`, which rounds **per line**, while
//! `reports.rs` summed the unrounded products and rounded **once**. Executing
//! §3.4 literally would have made T-5(c) go red, and the cheapest repair would
//! have been to weaken whichever side complained.
//!
//! **Ervin's M-2 ruling is per-line: the ÁFA report shows what was filed.** So
//! T-5(c) is restated to what actually matters, and this file is that pin:
//!
//! > For the same invoice, the report's `(net, vat)` per VAT-rate bucket equals
//! > the `<vatRateNetAmount>` / `<vatRateVatAmount>` NAV was sent.
//!
//! # Two divergences, not one
//!
//! The memo's sharp point, and the reason "just round per line" is not enough:
//! the two paths differed in **granularity** *and* in **rounding mode**.
//!
//! | | net per line | VAT |
//! |---|---|---|
//! | filed (`write_summary` → `LineItem::{net_total,vat_amount}`) | round-half-even **per line** | `floor(net × bp / 10_000)` **per line**, truncating |
//! | report, before M-2 | sum unrounded, round-half-even **once** | `round_half_even(group_net × bp / 10_000)` |
//!
//! So the report is not merely rounded per line now — it calls
//! `aberp_billing::domain::invoice::{line_net_total, line_vat_amount}`, the
//! *same two functions* `nav_xml::write_summary` sums. Equality is structural,
//! not coincidental, and [`the_pre_m2_group_rounding_is_a_different_number`]
//! shows the pin can still go red.
//!
//! # Scope
//!
//! DuckDB only, deliberately. `modules/billing` has not crossed the engine seam
//! (Step 5 landed the migrator half), so the report runs on DuckDB on both
//! sides of this branch. Byte-identity *across* the engines is T-4's job
//! (`adr0108_step6_nav_byte_identity.rs`); this file pins report-vs-filing on
//! one engine, which is the claim M-2 is actually about.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use aberp::audit_payloads;
use aberp::nav_xml::{
    self, ChainOperationReference, CustomerAddress, CustomerInfo, CustomerVatStatus, NavParties,
    SupplierInfo,
};
use aberp::reports::{compute_financial_report, DateBasis, PeriodKind, ReportRequest};
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use aberp_billing::{
    AllocateArgs, AllocateOutcome, BillingStore, Currency, CustomerId, DraftInvoice,
    DuckDbBillingStore, Huf, IdempotencyKey, InvoiceId, InvoiceSeries, LineItem, PaymentMethod,
    ReadyInvoice, ResetPolicy, SeriesCode, SeriesId, VatRateKind,
};
use rust_decimal::Decimal;
use time::macros::{date, datetime};
use time::OffsetDateTime;

const TENANT: &str = "test";
const ISSUE_AT: OffsetDateTime = datetime!(2026-07-15 09:00:00 UTC);

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct SeedLine {
    quantity: &'static str,
    unit_price: i64,
    basis_points: u16,
    kind: VatRateKind,
}

const fn l(quantity: &'static str, unit_price: i64, basis_points: u16) -> SeedLine {
    SeedLine {
        quantity,
        unit_price,
        basis_points,
        kind: VatRateKind::Percent,
    }
}

/// **The M-2 regression fixture, named in the brief.** Two 27% lines of 50 Ft
/// net each.
///
/// * filed: `50 × 2700 / 10_000 = 13` (truncated from 13.5), twice → **26 Ft**
/// * report before M-2: `round_half_even(100 × 2700 / 10_000)` → **27 Ft**
///
/// One forint, on one invoice. It scales with the line count, it always errs
/// upward on a `.5` remainder, and it is the difference between a dashboard the
/// bookkeeper can reconcile against the bevallás and one they cannot.
const TWO_27_LINES: &[SeedLine] = &[l("1", 50, 2700), l("1", 50, 2700)];

/// All four legal Hungarian rates, with fractional quantities so the per-line
/// half-even net rounding is exercised too — `2.5 × 333` is `832.5`, which is
/// exactly the tie half-even resolves downward to 832. A single-rounding fold
/// would never see that tie.
const MIXED_RATE: &[SeedLine] = &[
    l("2.5", 333, 2700),
    l("1.5", 777, 1800),
    l("3.5", 111, 500),
    l("1", 4321, 0),
];

/// A non-`Percent` line alongside a `Percent` one: the exempt line must
/// contribute zero VAT to *both* sides (ADR-0103 Invariant V).
const WITH_EXEMPT_LINE: &[SeedLine] = &[
    l("1", 1000, 2700),
    SeedLine {
        quantity: "1",
        unit_price: 5000,
        basis_points: 0,
        kind: VatRateKind::AamExempt,
    },
];

fn lines_of(seed: &[SeedLine]) -> Vec<LineItem> {
    seed.iter()
        .enumerate()
        .map(|(i, s)| LineItem {
            description: format!("line {i}"),
            quantity: Decimal::from_str(s.quantity).unwrap(),
            unit_price: Huf(s.unit_price),
            vat_rate_basis_points: s.basis_points,
            vat_rate_kind: s.kind,
            note: None,
            unit: None,
        })
        .collect()
}

/// Seed one invoice plus the audit trail that makes the report **count** it.
///
/// Each seed is `(lines, is_storno)`. When `is_storno`, an
/// `InvoiceStornoIssued` chain entry is appended so `walk_ledger` marks the
/// invoice `is_storno_self` and the report flips its sign, exactly as in
/// production.
fn seed(dir: &Path, seeds: &[(&[SeedLine], bool)]) -> (PathBuf, Vec<(String, ReadyInvoice)>) {
    let db = dir.join("aberp.duckdb");
    let series_id = SeriesId::new();
    let mut issued: Vec<(String, ReadyInvoice)> = Vec::new();

    {
        let mut store = DuckDbBillingStore::open(&db).unwrap();
        store.ensure_schema().unwrap();
        store
            .create_series(&InvoiceSeries {
                id: series_id,
                code: SeriesCode::new("S7".to_string()).unwrap(),
                reset_policy: ResetPolicy::AnnualOnFiscalYear,
                fiscal_year: None,
                created_at: ISSUE_AT,
            })
            .unwrap();
        for (lines, _) in seeds {
            let id = InvoiceId::new();
            let outcome = store
                .allocate_and_insert(
                    AllocateArgs {
                        series_id,
                        draft: DraftInvoice {
                            id,
                            series_id,
                            customer_id: CustomerId::new(),
                            lines: lines_of(lines),
                            issue_date: ISSUE_AT,
                            payment_deadline: ISSUE_AT.date(),
                            delivery_date: ISSUE_AT.date(),
                        },
                        idempotency_key: IdempotencyKey::new(),
                        currency: Currency::Huf,
                        rate_metadata: None,
                        bank_snapshot: None,
                        invoice_note: None,
                        email_recipient_override: None,
                        start_value: 1,
                        sequence_floor: None,
                        durable_high_water: None,
                    },
                    ISSUE_AT,
                )
                .unwrap();
            let AllocateOutcome::Fresh { invoice, .. } = outcome else {
                panic!("each seeded invoice burns a fresh number")
            };
            issued.push((id.to_prefixed_string(), invoice));
        }
    }

    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([7u8; 32]),
        )
        .unwrap();
        for ((id, ready), (_, is_storno)) in issued.iter().zip(seeds.iter()) {
            // A `SAVED` ack is what `ReportTrace::classify` needs to count the
            // invoice; without it the report reports nothing and every
            // assertion below would pass vacuously (rule 9).
            let ack =
                audit_payloads::InvoiceAckStatusPayload::new(id, "txn-1", "SAVED", Vec::new());
            ledger
                .append(
                    EventKind::InvoiceAckStatus,
                    serde_json::to_vec(&ack).unwrap(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
            if *is_storno {
                // The production payload type, serialized the production way
                // (`to_bytes`) — a hand-written JSON object here would drift
                // from the struct the reader deserializes, and the report would
                // silently treat the invoice as a plain sale.
                let link = audit_payloads::InvoiceStornoIssuedPayload::new(
                    id,
                    ready.sequence_number,
                    "res-1",
                    IdempotencyKey::new(),
                    "inv_BASE_OUTSIDE_THE_WINDOW",
                    1,
                    1,
                );
                ledger
                    .append(
                        EventKind::InvoiceStornoIssued,
                        link.to_bytes(),
                        Actor::test_only(),
                        None,
                    )
                    .unwrap();
            }
        }
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(&db))
            .unwrap();
    }
    (db, issued)
}

// ---------------------------------------------------------------------------
// The filed side
// ---------------------------------------------------------------------------

fn parties() -> NavParties {
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

/// `(net, vat)` per `<summaryByVatRate>` bucket, read out of the filed body.
///
/// Deliberately parsed out of the **emitted XML** rather than recomputed from
/// the `LineItem`s: recomputing would compare the report against a second copy
/// of the same arithmetic, which is exactly the vacuous shape §6.3 warns about
/// for the migrator gate. These are the bytes NAV received.
fn filed_buckets(xml: &[u8]) -> Vec<(i64, i64)> {
    let s = String::from_utf8(xml.to_vec()).expect("the NAV body is UTF-8");
    let mut out = Vec::new();
    for chunk in s.split("<summaryByVatRate>").skip(1) {
        let bucket = chunk
            .split("</summaryByVatRate>")
            .next()
            .expect("closed bucket");
        out.push((
            text_of(bucket, "vatRateNetAmount"),
            text_of(bucket, "vatRateVatAmount"),
        ));
    }
    assert!(!out.is_empty(), "the body must carry at least one bucket");
    out
}

fn text_of(bucket: &str, tag: &str) -> i64 {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let after = bucket
        .split(&open)
        .nth(1)
        .unwrap_or_else(|| panic!("bucket has no <{tag}>"));
    let raw = after
        .split(&close)
        .next()
        .unwrap_or_else(|| panic!("bucket has no </{tag}>"));
    // HUF renders as an integer string; the report is in whole minor units.
    raw.parse()
        .unwrap_or_else(|e| panic!("<{tag}> is not an integer: {raw:?} ({e})"))
}

fn render_fresh(inv: &ReadyInvoice) -> Vec<u8> {
    nav_xml::render_invoice_data_with_number(
        inv,
        &SeriesCode::new("S7".to_string()).unwrap(),
        &parties(),
        Currency::Huf,
        None,
        PaymentMethod::Transfer,
        None,
    )
    .expect("render the fresh-issuance body")
}

fn render_modification(inv: &ReadyInvoice) -> Vec<u8> {
    nav_xml::render_modification_data_with_number(
        inv,
        &SeriesCode::new("S7".to_string()).unwrap(),
        &parties(),
        &ChainOperationReference {
            base_invoice_number: format!("S7/{:05}", inv.sequence_number),
            modification_index: 1,
            base_line_count: inv.lines.len(),
        },
        Currency::Huf,
        None,
        PaymentMethod::Transfer,
        None,
    )
    .expect("render the modification body")
}

fn render_storno(inv: &ReadyInvoice) -> Vec<u8> {
    nav_xml::render_storno_data_with_number(
        inv,
        &SeriesCode::new("S7".to_string()).unwrap(),
        &parties(),
        &ChainOperationReference {
            base_invoice_number: format!("S7/{:05}", inv.sequence_number),
            modification_index: 1,
            base_line_count: inv.lines.len(),
        },
        Currency::Huf,
        None,
        PaymentMethod::Transfer,
        None,
        &inv.lines,
    )
    .expect("render the storno body")
}

// ---------------------------------------------------------------------------
// The reported side
// ---------------------------------------------------------------------------

/// `(net, vat)` per VAT-rate bucket as the ÁFA report publishes them, through
/// the **public** `compute_financial_report` — the whole read path, not an
/// internal helper, so nothing between the SQL and the JSON is bypassed.
fn reported_buckets(db: &Path) -> Vec<(i64, i64)> {
    let report = compute_financial_report(
        db,
        TenantId::new(TENANT.to_string()).unwrap(),
        BinaryHash::from_bytes([7u8; 32]),
        ReportRequest {
            period: PeriodKind::Month(2026, 7),
            date_basis: DateBasis::Teljesites,
            today: date!(2026 - 07 - 31),
            top_n: 10,
        },
    )
    .expect("compute the financial report");
    report
        .vat_breakdown_outgoing
        .iter()
        .map(|e| (e.net_minor, e.vat_minor))
        .collect()
}

fn sorted(mut v: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    v.sort_unstable();
    v
}

fn total_vat(v: &[(i64, i64)]) -> i64 {
    v.iter().map(|(_, vat)| *vat).sum()
}

// ---------------------------------------------------------------------------
// T-5(c)
// ---------------------------------------------------------------------------

/// **T-5(c), the named regression.** Two 27% lines of 50 Ft: the filing carries
/// 26 Ft of VAT and so does the report. 27 here means the group net was rounded
/// instead of the lines.
#[test]
fn report_ties_to_the_filing_for_the_two_27_percent_lines() {
    let dir = scratch("m2-fixture");
    let (db, issued) = seed(&dir, &[(TWO_27_LINES, false)]);
    let filed = filed_buckets(&render_fresh(&issued[0].1));

    assert_eq!(
        filed,
        vec![(100, 26)],
        "the filing truncates per line: 13 + 13"
    );
    assert_eq!(
        sorted(reported_buckets(&db)),
        sorted(filed),
        "the ÁFA report must publish the filed figure, not 27"
    );
}

/// The witness that this pin can go red: the pre-M-2 arithmetic on the same
/// fixture is a **different number**. Without this, `report == filed` could be
/// satisfied by two identically-wrong implementations and nobody would know the
/// fold had done anything.
#[test]
fn the_pre_m2_group_rounding_is_a_different_number() {
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::RoundingStrategy;
    // Exactly what `reports.rs` did before this commit: sum the unrounded
    // products across the group, round once, then round-half-even the VAT.
    let group_net = Decimal::from(50) * Decimal::ONE + Decimal::from(50) * Decimal::ONE;
    let net = group_net
        .round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven)
        .to_i64()
        .unwrap();
    let vat = (Decimal::from(net) * Decimal::from(2700) / Decimal::from(10_000))
        .round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven)
        .to_i64()
        .unwrap();
    assert_eq!(net, 100);
    assert_eq!(
        vat, 27,
        "the old report published 27 where the filing carried 26 — if this ever \
         reads 26, the two arithmetics have converged and T-5(c) has stopped \
         discriminating"
    );
}

/// Mixed rates + fractional quantities: every bucket ties, not just the total.
/// A fold that got the grand total right by cancelling errors across buckets
/// would fail here.
#[test]
fn report_ties_to_the_filing_bucket_by_bucket_on_a_mixed_rate_invoice() {
    let dir = scratch("mixed");
    let (db, issued) = seed(&dir, &[(MIXED_RATE, false)]);
    let filed = filed_buckets(&render_fresh(&issued[0].1));
    assert_eq!(filed.len(), 4, "all four legal rates bucket separately");
    assert_eq!(sorted(reported_buckets(&db)), sorted(filed));
}

/// The **modification** body re-files the same line set, so its summary is the
/// same one the report shows. Pinned rather than assumed: `write_summary` is
/// shared, but the modification path reaches it through a different renderer.
#[test]
fn report_ties_to_the_filed_modification_summary() {
    let dir = scratch("modification");
    let (db, issued) = seed(&dir, &[(MIXED_RATE, false)]);
    assert_eq!(
        sorted(reported_buckets(&db)),
        sorted(filed_buckets(&render_modification(&issued[0].1))),
    );
}

/// **Storno.** The filing negates `quantity` per line (`negate_line`, S381);
/// the report negates the folded group (`aggregate_outgoing`'s
/// `is_storno_self` sign). Those are only the same number because negation
/// commutes with both roundings — half-even is symmetric about zero and Rust's
/// `i64 / i64` truncates *toward* zero. This test is what makes that argument
/// checkable rather than a comment.
#[test]
fn report_ties_to_the_filed_storno_summary() {
    let dir = scratch("storno");
    let (db, issued) = seed(&dir, &[(MIXED_RATE, true)]);
    let filed = filed_buckets(&render_storno(&issued[0].1));
    assert!(
        total_vat(&filed) < 0,
        "a storno body must carry negative VAT, or this test proves nothing"
    );
    assert_eq!(sorted(reported_buckets(&db)), sorted(filed));
}

/// An `AamExempt` line contributes zero VAT on both sides. Before M-2 the
/// report derived VAT from the stored basis points alone, with no knowledge of
/// the kind — so an exempt line that a gate-bypassing door admitted with a
/// non-zero rate would have been reported with VAT the filing did not carry.
#[test]
fn report_ties_to_the_filing_with_a_non_percent_line() {
    let dir = scratch("exempt");
    let (db, issued) = seed(&dir, &[(WITH_EXEMPT_LINE, false)]);
    let filed = filed_buckets(&render_fresh(&issued[0].1));
    assert_eq!(
        total_vat(&filed),
        270,
        "only the 27% line carries VAT: 1000 x 2700 / 10_000"
    );
    assert_eq!(sorted(reported_buckets(&db)), sorted(filed));
}

/// Several invoices in one window: the report aggregates across them and still
/// ties to the sum of what was filed.
#[test]
fn report_ties_to_the_filing_across_a_whole_window() {
    let dir = scratch("window");
    let (db, issued) = seed(
        &dir,
        &[
            (TWO_27_LINES, false),
            (MIXED_RATE, false),
            (WITH_EXEMPT_LINE, false),
        ],
    );
    let filed: i64 = issued
        .iter()
        .map(|(_, inv)| total_vat(&filed_buckets(&render_fresh(inv))))
        .sum();
    assert_eq!(total_vat(&reported_buckets(&db)), filed);
}
