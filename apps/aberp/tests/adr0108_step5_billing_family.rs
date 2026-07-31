//! ADR-0108 Step 5 — **the invoice family crosses, and every monetary value
//! round-trips with zero drift.**
//!
//! This is the first family whose storage representation changes, and the one
//! the whole exercise exists to protect: `invoice.huf_equivalent_total` moves
//! `DECIMAL(18,0)` → `INTEGER` (R1), `invoice.exchange_rate` and
//! `invoice_line.quantity` move `DECIMAL(18,6)` → `TEXT` (R2), all under
//! `STRICT`.
//!
//! # Why the fixture issues through the REAL allocator
//!
//! Every invoice here is created by `DuckDbBillingStore::allocate_and_insert`
//! — the production writer, on the production schema built by the production
//! `ensure_schema`. A hand-written `INSERT` fixture would let the test pass
//! against a schema the application never produces, and the whole question
//! being asked is whether *the application's own rows* cross intact.
//!
//! # The values are adversarial on purpose
//!
//! Six-decimal quantities, six-decimal rates, a trailing-zero form
//! (`1.500000`), a value below `0.000001`-adjacent scale, a negative HUF
//! equivalent (the storno shape), and a `unit_price` near `i64::MAX / 2`.
//! Every one of them is a value an `f64` round-trip would visibly damage —
//! which is what makes the zero-drift assertion mean something.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};
use std::str::FromStr;

use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use aberp_billing::{
    self as billing, AllocateArgs, BillingStore, CustomerId, DraftInvoice, DuckDbBillingStore, Huf,
    IdempotencyKey, InvoiceId, InvoiceSeries, LineItem, RateMetadata, ResetPolicy, SeriesCode,
    SeriesId,
};
use rust_decimal::Decimal;
use time::macros::{date, datetime};
use time::OffsetDateTime;

const TENANT: &str = "test";
const ISSUE_AT: OffsetDateTime = datetime!(2026-07-31 09:00:00 UTC);

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step5-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The adversarial money set, as `(quantity, unit_price, rate, huf_total)`.
///
/// `rate`/`huf_total` are `None` for the HUF invoice — which is not a filler
/// case: a HUF row carries NO rate stamp by design (ADR-0037 §1's C10
/// byte-identity prerequisite), so it is the row that proves the NULL arms of
/// the carry.
#[allow(clippy::type_complexity)]
const MONEY_CASES: &[(&str, i64, Option<&str>, Option<i64>)] = &[
    // HUF: no rate metadata at all.
    ("1.000000", 1_000, None, None),
    // A trailing-zero decimal form. DuckDB renders DECIMAL(18,6) `1.5` as
    // `1.500000`; the migrator must carry those bytes, not re-render them.
    ("1.500000", 12_345, Some("405.230000"), Some(5_065)),
    // Full six-decimal precision on BOTH the quantity and the rate.
    ("0.333333", 999_999, Some("398.765432"), Some(40_523)),
    // The storno shape: a negative HUF equivalent.
    ("2.250000", 7, Some("410.000000"), Some(-40_523)),
    // The largest price the writer will accept. The ceiling is NOT `i64::MAX`:
    // `allocate_in_tx` refuses any line whose `gross_total()` overflows, and
    // gross is computed as `net × (10000 + basis_points)`, so the real bound is
    // `unit_price × quantity < i64::MAX / 12700 ≈ 7.3e14`. Measured, not
    // assumed — the first draft of this fixture used `i64::MAX / 2` and the
    // allocator refused it with `MoneyOverflow`.
    ("10.000000", 12_345_678_901_234, Some("1.000001"), Some(1)),
];

/// Seed a DEV-shaped DuckDB: the invoice family through the real store, then
/// the audit chain + its mirror + the tamper-evidence layer the Step-4 gate
/// turns on.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    let series_id = SeriesId::new();

    {
        let mut store = DuckDbBillingStore::open(&db).unwrap();
        store.ensure_schema().unwrap();
        store
            .create_series(&InvoiceSeries {
                id: series_id,
                code: SeriesCode::new("S5".to_string()).unwrap(),
                reset_policy: ResetPolicy::AnnualOnFiscalYear,
                fiscal_year: None,
                created_at: ISSUE_AT,
            })
            .unwrap();

        for (qty, price, rate, huf) in MONEY_CASES {
            let rate_metadata = rate.map(|r| RateMetadata {
                rate: Decimal::from_str(r).unwrap(),
                source: "MNB".to_string(),
                date: date!(2026 - 07 - 30),
                huf_equivalent_total: huf.unwrap(),
            });
            let currency = if rate_metadata.is_some() {
                billing::Currency::Eur
            } else {
                billing::Currency::Huf
            };
            let id = InvoiceId::new();
            store
                .allocate_and_insert(
                    AllocateArgs {
                        series_id,
                        draft: DraftInvoice {
                            id,
                            series_id,
                            customer_id: CustomerId::new(),
                            lines: vec![LineItem {
                                description: format!("qty {qty}"),
                                quantity: Decimal::from_str(qty).unwrap(),
                                unit_price: Huf(*price),
                                vat_rate_basis_points: 2700,
                                vat_rate_kind: billing::VatRateKind::Percent,
                                note: None,
                                unit: None,
                            }],
                            issue_date: ISSUE_AT,
                            payment_deadline: ISSUE_AT.date(),
                            delivery_date: ISSUE_AT.date(),
                        },
                        idempotency_key: IdempotencyKey::new(),
                        currency,
                        rate_metadata,
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
        }
    }

    // The ledger + its mirror. The migrator refuses without a mirror, and the
    // B1 gate refuses without signature and anchor coverage.
    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([5u8; 32]),
        )
        .unwrap();
        for i in 0..4 {
            ledger
                .append(
                    EventKind::DbAutoRecovered,
                    format!(r#"{{"n":{i}}}"#).into_bytes(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
        }
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(&db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(&db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger
            SET session_id = 'sess-5',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-5', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-07-31T00:00:00Z')",
        duckdb::params![TENANT, vec![7u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();
    db
}

// ---------------------------------------------------------------------------
// The headline: the family crosses and nothing drifts
// ---------------------------------------------------------------------------

/// The invoice family crosses, the gate passes, and **every monetary value
/// read back from SQLite is bit-for-bit the value DuckDB held.**
///
/// The read-back is deliberately done here as well as inside the gate: the
/// gate could in principle compare two sides that are both wrong in the same
/// way, whereas the assertions below compare SQLite against the literal
/// constants the fixture was built from.
#[test]
fn the_invoice_family_crosses_with_zero_money_drift() {
    let dir = scratch("family");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.billing.invoices, MONEY_CASES.len() as u64);
    assert_eq!(out.billing.invoice_lines, MONEY_CASES.len() as u64);
    assert_eq!(out.billing.reservations, MONEY_CASES.len() as u64);
    assert_eq!(out.billing.series, 1);
    assert_eq!(out.billing.sequence_state, 1);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "unexpected hard stops: {:?}",
        r.hard_stops
    );
    for want in [
        "invoice row count",
        "invoice_line row count",
        "ZERO drift",
        "Σ invoice_line.unit_price",
        "Σ invoice.huf_equivalent_total",
        "Σ invoice_line.quantity",
        "typeof(invoice.huf_equivalent_total) = 'integer'",
        "typeof(invoice.exchange_rate) = 'text'",
        "typeof(invoice_line.quantity) = 'text'",
        "typeof(invoice_line.unit_price) = 'integer'",
        "invoice_sequence_state agrees",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(want)),
            "the gate never ran a `{want}` check; it has {:?}",
            r.checks
        );
    }

    // --- the read-back, against the fixture's own literals ---
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT l.quantity, l.unit_price, i.exchange_rate, i.huf_equivalent_total
             FROM invoice_line l JOIN invoice i ON i.id = l.invoice_id
             ORDER BY i.sequence_number ASC",
        )
        .unwrap();
    let got: Vec<(String, i64, Option<String>, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(got.len(), MONEY_CASES.len());
    for (got, want) in got.iter().zip(MONEY_CASES) {
        assert_eq!(
            got.0.as_str(),
            want.0,
            "quantity must cross as the canonical string VERBATIM — re-rendering it makes the \
             migration's trailing-zero behaviour depend on when the row was written"
        );
        assert_eq!(got.1, want.1, "unit_price (R1, INTEGER minor units)");
        assert_eq!(
            got.2.as_deref(),
            want.2,
            "exchange_rate (R2, TEXT decimal) must be byte-identical"
        );
        assert_eq!(
            got.3, want.3,
            "huf_equivalent_total (R1, INTEGER) — the HUF figure on the NAV filing"
        );
    }

    // And the SQLite values still parse to exactly the Decimals the fixture
    // constructed, which is the property the PDF/NAV formatters depend on.
    for (got, want) in got.iter().zip(MONEY_CASES) {
        assert_eq!(
            Decimal::from_str(&got.0).unwrap(),
            Decimal::from_str(want.0).unwrap()
        );
        if let (Some(g), Some(w)) = (got.2.as_deref(), want.2) {
            assert_eq!(Decimal::from_str(g).unwrap(), Decimal::from_str(w).unwrap());
        }
    }
}

// ---------------------------------------------------------------------------
// STRICT + ensure_columns
// ---------------------------------------------------------------------------

/// The refusal must be `SQLITE_CONSTRAINT_DATATYPE` **and** name the column.
///
/// Asserting the extended code rather than a substring is what stops this
/// passing on some *other* failure that happens to be an error — a `NOT NULL`
/// violation, a typo'd column, a locked database. `SQLITE_CONSTRAINT` is 19
/// and the `DATATYPE` sub-code is 12, so the extended code is `19 | (12 << 8)`
/// = 3091, and it exists **only** on a `STRICT` table: drop the `STRICT`
/// suffix from the DDL and the same statement succeeds silently, which is
/// exactly PR #49 F-6a.
fn assert_datatype_refusal(err: &aberp_db::engine::Error, column: &str) {
    match err {
        aberp_db::engine::Error::SqliteFailure(e, msg) => {
            assert_eq!(
                e.extended_code,
                19 | (12 << 8),
                "expected SQLITE_CONSTRAINT_DATATYPE, got {e:?}: {msg:?}"
            );
            let msg = msg.clone().unwrap_or_default();
            assert!(
                msg.contains(column),
                "the refusal must name {column}: {msg}"
            );
        }
        other => panic!("expected a SQLite datatype constraint failure, got {other:?}"),
    }
}

/// **T-1, on this family's own columns**, and the boundary of what `STRICT`
/// actually buys.
///
/// `STRICT` refuses a fractional float into `huf_equivalent_total` — that is
/// F-6a's closure, asserted on the migrated schema rather than on a synthetic
/// table. It does **not** protect `quantity`: REAL → TEXT is a lossless
/// conversion, so SQLite accepts the float and stringifies it, and a
/// `typeof()` sweep still reads `'text'`.
///
/// The general form of that is measured in
/// `crates/aberp-db/tests/adr0108_money_representation.rs`; it is re-asserted
/// here on the real column because ADR-0108 §3.1 reads as though `STRICT` were
/// the R2 mitigation, and it is not — the Rust `Decimal` bind and T-8's
/// no-arithmetic-in-SQL gate are.
#[test]
fn strict_refuses_a_float_into_the_money_columns_but_not_into_the_decimal_ones() {
    let dir = scratch("strict");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();

    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM invoice LIMIT 1", [], |r| r.get(0))
        .unwrap();

    let err = conn
        .execute(
            "UPDATE invoice SET huf_equivalent_total = 1234.56 WHERE id = ?",
            [&id],
        )
        .expect_err("STRICT must refuse a REAL into an INTEGER money column");
    assert_datatype_refusal(&err, "invoice.huf_equivalent_total");

    // The value is untouched by the refused statement.
    let huf: Option<i64> = conn
        .query_row(
            "SELECT huf_equivalent_total FROM invoice WHERE id = ?",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(huf, Some(1_234), "the refused UPDATE must not have landed");

    // R2, the other way: a float INTO the quantity column is ACCEPTED and
    // silently stringified. Pinned as a fact, not as a wish — the guard that
    // does hold is that nothing in this tree binds an f64 there.
    conn.execute(
        "UPDATE invoice_line SET quantity = 0.1 + 0.2 WHERE ordinal = 0",
        [],
    )
    .expect("STRICT accepts REAL into TEXT — the conversion is lossless");
    let (q, ty): (String, String) = conn
        .query_row(
            "SELECT quantity, typeof(quantity) FROM invoice_line LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ty, "text", "the typeof() sweep cannot see this");
    assert!(
        q.starts_with("0.30000"),
        "what landed is the float's rendering, not the exact decimal: {q:?}"
    );
}

/// **M8's post-condition, on this family.** `ensure_billing_schema` is
/// idempotent, and every one of the 16 ladder columns is present afterwards —
/// asserted by name, because "the ALTERs ran" is what
/// `ADD COLUMN IF NOT EXISTS` could report and it is not the same claim.
///
/// Mutation-verify: delete any one `ensure_columns` call from
/// `ensure_billing_schema` and this goes red on that column's name.
#[test]
fn ensure_billing_schema_lands_all_sixteen_ladder_columns_and_is_idempotent() {
    let dir = scratch("ladder");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_billing::ensure_billing_schema(&conn).unwrap();
    aberp::migrate_billing::ensure_billing_schema(&conn).expect("idempotent — it runs every boot");

    let cols = |table: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?)")
            .unwrap();
        stmt.query_map([table], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    let invoice = cols("invoice");
    for c in [
        "currency",
        "exchange_rate",
        "exchange_rate_source",
        "exchange_rate_date",
        "huf_equivalent_total",
        "bank_account_id",
        "bank_account_currency",
        "bank_account_number",
        "bank_account_bank_name",
        "bank_account_swift_bic",
        "invoice_note",
        "payment_deadline",
        "delivery_date",
        "email_recipient_override",
    ] {
        assert!(invoice.contains(&c.to_string()), "invoice.{c} is missing");
    }
    let line = cols("invoice_line");
    for c in ["note", "vat_rate_kind"] {
        assert!(line.contains(&c.to_string()), "invoice_line.{c} is missing");
    }

    // §3.2 C — the S157 widen ladder's scratch column must never exist here.
    assert!(!line.contains(&"quantity_dec".to_string()));

    // Every table carries STRICT. Without it the four representation rules are
    // declarations rather than constraints.
    let mut stmt = conn
        .prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table'")
        .unwrap();
    let tables: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables.len(), 5, "five tables: {tables:?}");
    for (name, sql) in &tables {
        assert!(sql.contains("STRICT"), "{name} is not STRICT: {sql}");
    }
}

// ---------------------------------------------------------------------------
// The gate's symmetry arm
// ---------------------------------------------------------------------------

/// **The silent-skip shape, refused.** A DuckDB side that HAS the family while
/// the SQLite side does not is a hard stop, not a skipped section.
///
/// This is the arm that makes "the family is optional" safe. Without it, a
/// carry that never ran would produce a gate whose billing checks simply do
/// not appear — and a gate with fewer checks reports PASS exactly as loudly as
/// one with more.
///
/// Mutation-verify: change the `(true, false)` arm of `reconcile_billing` to
/// `return Ok(())` and this goes red.
#[test]
fn the_gate_hard_stops_when_the_family_was_not_carried() {
    let dir = scratch("asym");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();

    // Drop the family from the SQLite side — the state a skipped carry leaves.
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        for t in [
            "invoice_line",
            "invoice",
            "invoice_sequence_reservation",
            "invoice_sequence_state",
            "invoice_series",
        ] {
            conn.execute_batch(&format!("DROP TABLE {t};")).unwrap();
        }
    }

    let r = reconcile(&db, &lite, TENANT).expect("the gate itself must still run");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("exists in DuckDB but NOT in SQLite")),
        "an uncarried family must hard-stop, not vanish from the report: {:?}",
        r.hard_stops
    );
}

/// A ledger-only source is a legitimate shape and must NOT hard-stop — but it
/// must say so out loud, so "absent on both sides" is never indistinguishable
/// from "checked and fine".
#[test]
fn a_ledger_only_source_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("ledgeronly");
    let db = dir.join("aberp.duckdb");
    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([5u8; 32]),
        )
        .unwrap();
        ledger
            .append(
                EventKind::DbAutoRecovered,
                b"{}".to_vec(),
                Actor::test_only(),
                None,
            )
            .unwrap();
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(&db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(&db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger SET session_id='s', session_pubkey='p', event_sig='sig';",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 's', 'session_close', 'dead', ?, 'tsa', 'ok', '2026-07-31T00:00:00Z')",
        duckdb::params![TENANT, vec![1u8; 4]],
    )
    .unwrap();
    conn.close().unwrap();

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();
    assert_eq!(out.billing, aberp::migrate_billing::BillingCarry::default());

    let r = reconcile(&db, &lite, TENANT).unwrap();
    assert!(r.hard_stops.is_empty(), "{:?}", r.hard_stops);
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("invoice family absent on BOTH sides")),
        "the absence must be stated, not inferred from missing checks: {:?}",
        r.checks
    );
}
