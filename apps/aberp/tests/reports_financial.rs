//! S225 / PR-221 — integration tests for the financial-statistics
//! aggregator.
//!
//! Drives [`aberp::reports::compute_financial_report`] against an
//! in-process DuckDB fixture. The unit tests inside `reports.rs` pin
//! period parsing + helper math; these tests exercise the SQL path
//! end-to-end:
//!
//!   1. Empty DB → all aggregates zero, deferred notes present.
//!   2. Two AP rows (HUF + EUR) → expenses / VAT-paid populated;
//!      Irrelevant row excluded.
//!   3. Two restored_invoice rows → revenue/VAT-collected populated;
//!      partner_id-NULL surfaces in the hygiene panel.
//!   4. Past-deadline AP row → payable_past_deadline_count = 1.
//!   5. Outstanding AP row before-deadline → cashflow_forward picks it
//!      up; payable aging buckets correctly.
//!   6. Period filtering — a row outside the window is excluded.
//!
//! Outgoing-native invoice paths (which require the full billing
//! allocator + audit ledger) are exercised by the unit tests'
//! `aggregate_outgoing` + the helper-function pins; an end-to-end
//! native-invoice fixture would need to wire issue_invoice + the
//! audit ledger, which is large enough to belong in its own follow-on
//! integration test (deferred per CLAUDE.md rule 2).

use std::path::PathBuf;

use aberp::reports::{self, compute_financial_report, DateBasis, PeriodKind, ReportRequest};
use aberp_audit_ledger::{BinaryHash, TenantId};
use duckdb::{params, Connection};
use time::macros::date;
use ulid::Ulid;

const TEST_TENANT: &str = "reports_financial_test";

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-reports-financial")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn fresh_db(label: &str) -> PathBuf {
    test_dir(label).join("aberp.duckdb")
}

fn ensure_all_schemas(db_path: &PathBuf) {
    let conn = Connection::open(db_path).expect("open duckdb");
    aberp::incoming_invoices::ensure_schema(&conn).expect("ap_invoice schema");
    aberp::restore_from_nav_outgoing::ensure_schema(&conn).expect("restored_invoice schema");
    aberp_audit_ledger::ensure_schema(&conn).expect("audit-ledger schema");
}

/// Insert an `ap_invoice` row directly via SQL. The route-layer
/// ingestion path runs the audit-ledger write + idempotency check;
/// this fixture skips both because the aggregator reads the row
/// shape, not the audit history (per-row mark/audit transitions are
/// orthogonal to the aggregator's aggregates).
#[allow(clippy::too_many_arguments)]
fn insert_ap_row(
    db_path: &PathBuf,
    supplier_tax_number: &str,
    supplier_name: &str,
    nav_invoice_number: &str,
    issue_date: &str,
    payment_deadline: Option<&str>,
    net: i64,
    vat: i64,
    gross: i64,
    currency: &str,
    local_status: &str,
) {
    let conn = Connection::open(db_path).expect("open");
    let id = format!("apinv_{}", Ulid::new());
    conn.execute(
        "INSERT INTO ap_invoice (id, tenant_id, supplier_tax_number, supplier_name,
                                 supplier_address, nav_invoice_number, issue_date,
                                 delivery_date, payment_deadline, total_net_minor,
                                 total_vat_minor, total_gross_minor, currency,
                                 local_status, irrelevant_reason, nav_xml_path,
                                 created_at, updated_at)
         VALUES (?, ?, ?, ?, NULL, ?, ?, NULL, ?, ?, ?, ?, ?, ?, NULL, NULL,
                 '2026-06-01T00:00:00Z', '2026-06-01T00:00:00Z')",
        params![
            id,
            TEST_TENANT,
            supplier_tax_number,
            supplier_name,
            nav_invoice_number,
            issue_date,
            payment_deadline,
            net,
            vat,
            gross,
            currency,
            local_status,
        ],
    )
    .expect("insert ap row");
}

fn insert_restored_row(
    db_path: &PathBuf,
    source_nav_invoice_number: &str,
    issue_date: &str,
    net: i64,
    vat: i64,
    gross: i64,
    currency: &str,
    restore_year: i32,
    partner_id: Option<&str>,
    customer_name: Option<&str>,
) {
    let conn = Connection::open(db_path).expect("open");
    let id = format!("rinv_{}", Ulid::new());
    conn.execute(
        "INSERT INTO restored_invoice (id, tenant_id, source_nav_invoice_number,
                                       source_nav_transaction_id, issue_date,
                                       total_net_minor, total_vat_minor, total_gross_minor,
                                       currency, restore_year, created_at,
                                       customer_name, customer_tax_number,
                                       customer_vat_status, partner_id)
         VALUES (?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, '2026-06-01T00:00:00Z', ?, NULL, NULL, ?)",
        params![
            id,
            TEST_TENANT,
            source_nav_invoice_number,
            issue_date,
            net,
            vat,
            gross,
            currency,
            restore_year,
            customer_name,
            partner_id,
        ],
    )
    .expect("insert restored row");
}

fn build_request(period: PeriodKind, today: time::Date) -> ReportRequest {
    ReportRequest {
        period,
        date_basis: DateBasis::Teljesites,
        today,
    }
}

#[test]
fn empty_db_yields_zero_aggregates_with_deferred_notes() {
    let db_path = fresh_db("empty");
    ensure_all_schemas(&db_path);
    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Month(2026, 6), date!(2026 - 06 - 01)),
    )
    .expect("compute on empty DB");

    assert_eq!(report.revenue.huf.gross_minor, 0);
    assert_eq!(report.revenue.eur.gross_minor, 0);
    assert_eq!(report.expenses.huf.gross_minor, 0);
    assert_eq!(report.expenses.eur.gross_minor, 0);
    assert_eq!(report.vat_to_pay.huf_minor, 0);
    assert_eq!(report.gross_profit.huf_minor, 0);
    assert!(report.vat_breakdown_outgoing.is_empty());
    assert!(report.top_customers.is_empty());
    assert!(report.top_vendors.is_empty());
    assert!(
        !report.deferred_notes.is_empty(),
        "deferred notes always emitted"
    );
    assert_eq!(report.period.label, "2026-06");
    assert_eq!(report.period.date_basis, "teljesites");
}

#[test]
fn ap_rows_populate_expenses_and_vat_paid_excluding_irrelevant() {
    let db_path = fresh_db("ap-expenses");
    ensure_all_schemas(&db_path);

    // June 2026 HUF outstanding: 100_000 net + 27_000 vat = 127_000 gross
    insert_ap_row(
        &db_path,
        "11111111-1-42",
        "Vendor HUF Kft.",
        "VEND-001",
        "2026-06-10",
        Some("2026-07-10"),
        100_000,
        27_000,
        127_000,
        "HUF",
        "Outstanding",
    );
    // June 2026 EUR paid: 500_00 net + 135_00 vat = 635_00 gross (cents)
    insert_ap_row(
        &db_path,
        "22222222-1-42",
        "Vendor EUR GmbH",
        "VEND-002",
        "2026-06-15",
        Some("2026-07-15"),
        50_000,
        13_500,
        63_500,
        "EUR",
        "Paid",
    );
    // Irrelevant row — MUST NOT contribute to expenses or VAT-paid.
    insert_ap_row(
        &db_path,
        "33333333-1-42",
        "Spam Vendor",
        "SPAM-001",
        "2026-06-20",
        Some("2026-07-20"),
        9_999_999,
        2_699_999,
        12_699_998,
        "HUF",
        "Irrelevant",
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Month(2026, 6), date!(2026 - 06 - 25)),
    )
    .expect("compute");

    assert_eq!(report.expenses.huf.gross_minor, 127_000);
    assert_eq!(report.expenses.huf.count, 1);
    assert_eq!(report.expenses.eur.gross_minor, 63_500);
    assert_eq!(report.expenses.eur.count, 1);
    assert_eq!(report.vat_paid.huf.vat_minor, 27_000);
    assert_eq!(report.vat_paid.eur.vat_minor, 13_500);
    // Top vendors: the two non-Irrelevant entries.
    assert_eq!(report.top_vendors.len(), 2);
}

#[test]
fn restored_rows_populate_revenue_and_hygiene_no_partner() {
    let db_path = fresh_db("restored-revenue");
    ensure_all_schemas(&db_path);

    insert_restored_row(
        &db_path,
        "RESTORED-001",
        "2026-06-01",
        80_000,
        21_600,
        101_600,
        "HUF",
        2026,
        Some("prt_01ABCD"),
        Some("BSCE Kft."),
    );
    // Second row WITHOUT partner_id — surfaces in hygiene as
    // "restored_no_partner_count".
    insert_restored_row(
        &db_path,
        "RESTORED-002",
        "2026-06-15",
        40_000,
        10_800,
        50_800,
        "HUF",
        2026,
        None,
        None,
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Month(2026, 6), date!(2026 - 06 - 30)),
    )
    .expect("compute");

    assert_eq!(report.revenue.huf.gross_minor, 152_400);
    assert_eq!(report.revenue.huf.count, 2);
    assert_eq!(report.vat_collected.huf.vat_minor, 32_400);
    assert_eq!(report.hygiene.restored_no_partner_count, 1);
    // Top customers — only the labelled one.
    assert_eq!(report.top_customers.len(), 1);
    assert_eq!(report.top_customers[0].label, "BSCE Kft.");
}

#[test]
fn past_deadline_outstanding_ap_surfaces_in_hygiene_and_aging() {
    let db_path = fresh_db("ap-aging");
    ensure_all_schemas(&db_path);

    // Deadline 45 days ago (today = 2026-06-30). Should land in
    // days_31_60 aging bucket and bump payable_past_deadline_count.
    insert_ap_row(
        &db_path,
        "44444444-1-42",
        "Late Vendor Kft.",
        "LATE-001",
        "2026-05-01",
        Some("2026-05-16"),
        100_000,
        27_000,
        127_000,
        "HUF",
        "Outstanding",
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::All, date!(2026 - 06 - 30)),
    )
    .expect("compute");

    assert_eq!(report.payables.huf.gross_minor, 127_000);
    assert_eq!(report.hygiene.payable_past_deadline_count, 1);
    assert_eq!(report.payables_aging.days_31_60.gross_minor, 127_000);
    assert_eq!(report.payables_aging.days_1_30.gross_minor, 0);
}

#[test]
fn period_filter_excludes_out_of_window_rows() {
    let db_path = fresh_db("period-filter");
    ensure_all_schemas(&db_path);

    // June row.
    insert_ap_row(
        &db_path,
        "55555555-1-42",
        "June Vendor",
        "JUNE-001",
        "2026-06-15",
        Some("2026-07-15"),
        50_000,
        13_500,
        63_500,
        "HUF",
        "Outstanding",
    );
    // May row — excluded when window is June 2026.
    insert_ap_row(
        &db_path,
        "55555555-1-42",
        "May Vendor",
        "MAY-001",
        "2026-05-15",
        Some("2026-06-15"),
        99_999_999,
        26_999_999,
        126_999_998,
        "HUF",
        "Outstanding",
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Month(2026, 6), date!(2026 - 06 - 30)),
    )
    .expect("compute");

    assert_eq!(report.expenses.huf.gross_minor, 63_500);
    assert_eq!(report.expenses.huf.count, 1);
}

#[test]
fn date_basis_issued_uses_issue_date_axis() {
    let db_path = fresh_db("basis-issued");
    ensure_all_schemas(&db_path);

    // Row issued in June but delivery_date NULL (defaults to issue) —
    // both date bases pick it up under June.
    insert_ap_row(
        &db_path,
        "66666666-1-42",
        "Basis Vendor",
        "BASIS-001",
        "2026-06-20",
        Some("2026-07-20"),
        10_000,
        2_700,
        12_700,
        "HUF",
        "Outstanding",
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let req = ReportRequest {
        period: PeriodKind::Month(2026, 6),
        date_basis: DateBasis::Issued,
        today: date!(2026 - 06 - 30),
    };
    let report = compute_financial_report(&db_path, tenant, BinaryHash::from_bytes([0u8; 32]), req)
        .expect("compute");

    assert_eq!(report.expenses.huf.gross_minor, 12_700);
    assert_eq!(report.period.date_basis, "issued");
}

#[test]
fn quarter_period_resolves_three_month_window() {
    let db_path = fresh_db("quarter-window");
    ensure_all_schemas(&db_path);

    // April + May + June 2026 = Q2.
    insert_ap_row(
        &db_path,
        "77777777-1-42",
        "Q2 Vendor",
        "Q2-APR",
        "2026-04-10",
        None,
        10_000,
        2_700,
        12_700,
        "HUF",
        "Outstanding",
    );
    insert_ap_row(
        &db_path,
        "77777777-1-42",
        "Q2 Vendor",
        "Q2-JUN",
        "2026-06-15",
        None,
        20_000,
        5_400,
        25_400,
        "HUF",
        "Outstanding",
    );
    // March row excluded.
    insert_ap_row(
        &db_path,
        "77777777-1-42",
        "Q2 Vendor",
        "Q1-MAR",
        "2026-03-10",
        None,
        999_999,
        269_999,
        1_269_998,
        "HUF",
        "Outstanding",
    );

    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Quarter(2026, 2), date!(2026 - 06 - 30)),
    )
    .expect("compute");

    assert_eq!(report.expenses.huf.gross_minor, 38_100);
    assert_eq!(report.expenses.huf.count, 2);
}

#[test]
fn parse_period_round_trip_via_request() {
    // The route layer parses query strings via `reports::parse_period`;
    // pin the resolver→label→from→to chain end-to-end through a
    // computed report so a future refactor that drifts the wire-form
    // surfaces.
    let db_path = fresh_db("period-round-trip");
    ensure_all_schemas(&db_path);

    let kind = reports::parse_period("2026-Q2").expect("parse");
    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(kind, date!(2026 - 06 - 30)),
    )
    .expect("compute");

    assert_eq!(report.period.label, "2026-Q2");
    assert_eq!(report.period.kind, "quarter");
    assert_eq!(report.period.from.as_deref(), Some("2026-04-01"));
    assert_eq!(report.period.to.as_deref(), Some("2026-06-30"));
}

#[test]
fn annual_running_excludes_january_when_today_is_february() {
    let db_path = fresh_db("ytd");
    ensure_all_schemas(&db_path);
    // Restored row in Jan 2026.
    insert_restored_row(
        &db_path,
        "YTD-001",
        "2026-01-10",
        100_000,
        27_000,
        127_000,
        "HUF",
        2026,
        None,
        Some("YTD Customer"),
    );
    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let report = compute_financial_report(
        &db_path,
        tenant,
        BinaryHash::from_bytes([0u8; 32]),
        build_request(PeriodKind::Month(2026, 2), date!(2026 - 02 - 15)),
    )
    .expect("compute");
    // Current month is February — revenue zero (no Feb rows). But the
    // YTD annual_running panel must include the January row.
    assert_eq!(report.revenue.huf.gross_minor, 0);
    assert_eq!(report.annual_running.year, 2026);
    assert_eq!(report.annual_running.revenue.huf.gross_minor, 127_000);
}
