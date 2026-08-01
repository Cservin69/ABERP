//! ADR-0108 Step 7 Part B — **the partners family crosses.**
//!
//! Six pins, in the order they defend:
//!
//! 1. the family crosses and **no column drifts**, asserted against the
//!    fixture's own literals as well as through the gate;
//! 2. `ensure_partners_schema` lands all seven ladder columns and is
//!    idempotent (M8);
//! 3. `STRICT` refuses a float into `issued_invoice_count` — the family's one
//!    non-string column;
//! 4. the gate **hard-stops** when the family was not carried, rather than
//!    skipping its checks and reporting PASS;
//! 5. the per-row equality arm is **shown to go red** on a single changed
//!    column — a gate never shown to fail is not a gate;
//! 6. a partnerless source is *reported* as absent rather than passing silently.
//!
//! The fixture deliberately carries the shapes a partners table actually has
//! and a naive one would not: a Hungarian name outside ASCII (`Árvíztűrő
//! tükörfúrógép`), a `%` inside a stored value, a soft-deleted row, a
//! private-person row with a NULL `tax_number`, and a row where every
//! *genuinely nullable* ladder column is NULL — the pre-S361 / pre-S428 history
//! the ladders exist for.
//!
//! **This test does not pin M11 / T-12, and that is on purpose.** Nothing here
//! makes `partners.rs`'s `LOWER()` / `LIKE` dedup guard run against SQLite;
//! those queries still go to DuckDB. See the module docs of
//! `aberp::migrate_partners` and §9's row.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-partners-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The adversarial partner set, as `(id, display_name, legal_name,
/// tax_number, address_city, customer_type)`.
///
/// Every entry is here for a reason a reviewer can check:
///
/// * **`p-01`** — the non-ASCII case. `Árvíztűrő tükörfúrógép` is the Hungarian
///   pangram, and `ű` / `ő` are outside Latin-1 as well as outside ASCII. A
///   carry that round-trips through anything but UTF-8 mangles it.
/// * **`p-02`** — a literal `%` and `_` inside a stored value. They are
///   `LIKE` metacharacters, which is M11's surface; here they only have to
///   survive as bytes, but they are also the values a later M11 test needs to
///   already be in the DEV-shaped fixture.
/// * **`p-03`** — a private person: NULL `tax_number` (the column the pre-PR-97
///   schema declared `NOT NULL`), and NULL in all five genuinely-nullable
///   ladder columns.
/// * **`p-04`** — soft-deleted (`deleted_at` set). The gate must carry it: a
///   migration that dropped soft-deleted rows would silently change what the
///   dedup guard and the 8-year record contain.
/// * **`p-05`** — an EU partner with an `eu_vat_number` and no Hungarian tax
///   number (ADR-0102).
const PARTNER_CASES: &[(&str, &str, &str, Option<&str>, &str, &str)] = &[
    (
        "p-01",
        "Árvíztűrő tükörfúrógép Kft.",
        "ÁRVÍZTŰRŐ TÜKÖRFÚRÓGÉP KORLÁTOLT FELELŐSSÉGŰ TÁRSASÁG",
        Some("12345678-2-42"),
        "Budapest",
        "business",
    ),
    (
        "p-02",
        "100% Precision _ Machining",
        "100% PRECISION _ MACHINING LTD",
        Some("87654321-2-13"),
        "Győr",
        "business",
    ),
    (
        "p-03",
        "Kiss János",
        "Kiss János",
        None,
        "Szeged",
        "private",
    ),
    (
        "p-04",
        "Deleted Partner",
        "DELETED PARTNER KFT",
        Some("11112222-2-41"),
        "Pécs",
        "business",
    ),
    (
        "p-05",
        "Müller GmbH",
        "MÜLLER GMBH",
        None,
        "München",
        "business",
    ),
];

/// Seed a DEV-shaped DuckDB: the partners table through the **real**
/// `partners::ensure_schema` (so the SQLite side is compared against the schema
/// the product actually builds, not against a hand-written copy of it), then
/// the audit chain + mirror + tamper-evidence layer the Step-4 gate turns on.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::partners::ensure_schema(&conn).unwrap();
        for (i, (id, display, legal, tax, city, ctype)) in PARTNER_CASES.iter().enumerate() {
            // `p-04` is the soft-deleted row; `p-05` is the EU one; `p-03` is
            // the pre-ladder-history row.
            let deleted_at = if *id == "p-04" {
                Some("2026-07-30T10:00:00Z")
            } else {
                None
            };
            let eu_vat = if *id == "p-05" {
                Some("DE123456789")
            } else {
                None
            };
            // `p-03` carries the pre-S361 / pre-S428 history: every column that
            // is *genuinely* nullable on DuckDB stays NULL.
            //
            // MEASURED, and it narrows the fixture rather than the other way
            // round: `customer_vat_status` and `issued_invoice_count` CANNOT be
            // NULL on a database whose `partners` came from the base
            // `CREATE` — it declares both `NOT NULL DEFAULT`, and DuckDB
            // refuses the INSERT (`NOT NULL constraint failed:
            // partners.customer_vat_status`). Their nullable history exists
            // only on a genuinely pre-PR-97 table, where the ladder added them
            // unconstrained — and even there the ladder's follow-on `UPDATE`
            // backfills them on the next boot. The SQLite columns stay
            // nullable anyway, because nullable is the shape that carries both
            // histories and the carry must never be the thing that fails.
            let pre_ladder = *id == "p-03";
            conn.execute(
                "INSERT INTO partners
                   (id, tenant_id, display_name, legal_name, kind, tax_number, eu_vat_number,
                    address_street, address_postal_code, address_city, address_country,
                    bank_account, contact_email, contact_phone,
                    customer_vat_status, issued_invoice_count,
                    created_at, updated_at, deleted_at,
                    dpas_rating, eccn, export_screening_status, export_screened_at,
                    customer_type)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id,
                    TENANT,
                    display,
                    legal,
                    "Customer",
                    tax,
                    eu_vat,
                    Some(format!("Fő utca {}.", i + 1)),
                    Some("1011"),
                    city,
                    Some("HU"),
                    Some("11111111-22222222-33333333"),
                    Some(format!("{id}@example.test")),
                    Some("+36 1 234 5678"),
                    "Domestic",
                    i as i64,
                    "2026-01-01T00:00:00Z",
                    "2026-07-01T00:00:00Z",
                    deleted_at,
                    if pre_ladder { None } else { Some("DO-A1") },
                    if pre_ladder { None } else { Some("EAR99") },
                    if pre_ladder { None } else { Some("clear") },
                    if pre_ladder {
                        None
                    } else {
                        Some("2026-06-01T00:00:00Z")
                    },
                    if pre_ladder { None } else { Some(*ctype) },
                ],
            )
            .unwrap();
        }
        conn.close().unwrap();
    }

    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([7u8; 32]),
        )
        .unwrap();
        for i in 0..3 {
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
            SET session_id = 'sess-7',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-7', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
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

/// The partners family crosses, the gate passes, and every column read back
/// from SQLite is the value DuckDB held.
///
/// The read-back is deliberately done here as well as inside the gate: the gate
/// compares the two sides against each other, whereas the assertions below
/// compare SQLite against the literal constants the fixture was built from — so
/// two sides that were wrong in the same way would still be caught.
#[test]
fn the_partners_family_crosses_with_zero_drift() {
    let dir = scratch("family");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.partners.partners, PARTNER_CASES.len() as u64);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "unexpected hard stops: {:?}",
        r.hard_stops
    );
    for want in [
        "partners row count",
        "every partners column round-trips with ZERO drift",
        "Σ partners.issued_invoice_count",
        "typeof(partners.legal_name) = 'text'",
        "typeof(partners.issued_invoice_count) = 'integer'",
        "typeof(partners.customer_type) = 'text'",
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
            "SELECT id, display_name, legal_name, tax_number, address_city, customer_type,
                    deleted_at, eu_vat_number, issued_invoice_count
             FROM partners ORDER BY id ASC",
        )
        .unwrap();
    #[allow(clippy::type_complexity)]
    let got: Vec<(
        String,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(got.len(), PARTNER_CASES.len());
    for (got, want) in got.iter().zip(PARTNER_CASES) {
        assert_eq!(got.0, want.0);
        assert_eq!(
            got.1, want.1,
            "display_name must cross as UTF-8 verbatim — `ű` and `ő` are outside Latin-1, and \
             `%`/`_` are LIKE metacharacters that must survive as ordinary bytes"
        );
        assert_eq!(got.2, want.2);
        assert_eq!(
            got.3.as_deref(),
            want.3,
            "tax_number is NULL for a private person and is the buyer identifier on the NAV \
             filing for everyone else — the NULL arm is not filler"
        );
        assert_eq!(got.4, want.4);
    }

    // The soft-deleted row crossed rather than being quietly dropped.
    let deleted: Vec<&str> = got
        .iter()
        .filter(|g| g.6.is_some())
        .map(|g| g.0.as_str())
        .collect();
    assert_eq!(
        deleted,
        vec!["p-04"],
        "the soft-deleted partner must cross: dropping it would silently change what the \
         duplicate-partner guard and the 8-year record contain"
    );

    // The pre-ladder row's NULLs crossed as NULLs — not backfilled.
    let pre_ladder = got.iter().find(|g| g.0 == "p-03").unwrap();
    assert_eq!(
        pre_ladder.5, None,
        "customer_type must cross verbatim, NULL included: the read path resolves NULL, and \
         re-deriving the S428 backfill here is the `verify the extraction against itself` \
         shape B4 forbids"
    );

    // The EU partner's identifiers are the right way round (ADR-0102).
    let eu = got.iter().find(|g| g.0 == "p-05").unwrap();
    assert_eq!(eu.7.as_deref(), Some("DE123456789"));
    assert_eq!(eu.3, None);
}

// ---------------------------------------------------------------------------
// STRICT + ensure_columns
// ---------------------------------------------------------------------------

/// `ensure_partners_schema` lands all seven ladder columns, and running it
/// again changes nothing (M8).
///
/// The count is asserted as an exact 24, not as "at least the seven": a ladder
/// that added a column the DuckDB build does not have is as much a divergence
/// as one that added nothing.
#[test]
fn ensure_partners_schema_lands_all_seven_ladder_columns_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_partners::ensure_partners_schema(&conn).unwrap();
    let cols = |c: &aberp_db::engine::Connection| -> Vec<String> {
        let mut s = c
            .prepare("SELECT name FROM pragma_table_info('partners') ORDER BY name")
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let first = cols(&conn);
    assert_eq!(
        first.len(),
        24,
        "partners has 24 columns on DuckDB; got {first:?}"
    );
    for ladder in [
        "customer_vat_status",
        "issued_invoice_count",
        "dpas_rating",
        "eccn",
        "export_screening_status",
        "export_screened_at",
        "customer_type",
    ] {
        assert!(
            first.iter().any(|c| c == ladder),
            "the ladder column {ladder} did not land — `ensure_columns` must prove the column \
             is there, not merely report that an ALTER ran"
        );
    }

    aberp::migrate_partners::ensure_partners_schema(&conn).unwrap();
    assert_eq!(first, cols(&conn), "a second run must change nothing");
}

/// `STRICT` refuses a float into `issued_invoice_count`.
///
/// This is the family's **only** non-string column, so it is the only place
/// F-6a's storage-side float-coercion class can reach it at all. Asserting the
/// extended code rather than a substring is what stops this passing on some
/// other error: `SQLITE_CONSTRAINT` is 19 and the `DATATYPE` sub-code is 12, so
/// the extended code is `19 | (12 << 8)` = 3091 — and it exists **only** on a
/// `STRICT` table. Drop the `STRICT` suffix and the same statement succeeds
/// silently.
#[test]
fn strict_refuses_a_float_into_the_only_integer_column() {
    let dir = scratch("strict");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    aberp::migrate_partners::ensure_partners_schema(&conn).unwrap();

    let err = conn
        .execute(
            "INSERT INTO partners
               (id, tenant_id, display_name, legal_name, kind, created_at, updated_at,
                issued_invoice_count)
             VALUES ('x', 't', 'd', 'l', 'Customer', 'c', 'u', ?)",
            [1.5_f64],
        )
        .expect_err("STRICT must refuse a REAL into an INTEGER column");
    match err {
        aberp_db::engine::Error::SqliteFailure(e, msg) => {
            assert_eq!(
                e.extended_code,
                19 | (12 << 8),
                "expected SQLITE_CONSTRAINT_DATATYPE, got {e:?}: {msg:?}"
            );
            assert!(
                msg.clone()
                    .unwrap_or_default()
                    .contains("issued_invoice_count"),
                "the refusal must name the column: {msg:?}"
            );
        }
        other => panic!("expected a SQLite datatype constraint failure, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The gate's refusals
// ---------------------------------------------------------------------------

/// The silent-skip shape: the family exists in DuckDB, the SQLite side has no
/// `partners` table, and the gate must say so instead of skipping every check
/// below it and reporting PASS.
///
/// Mutation-shaped: this is what the gate does if a future edit drops the
/// `carry_partners` call from `migrate_families`.
#[test]
fn the_gate_hard_stops_when_the_partners_family_was_not_carried() {
    let dir = scratch("notcarried");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");

    // Drop the carried table — exactly the state a skipped carry leaves.
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("DROP TABLE partners").unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("exists in DuckDB but NOT in SQLite")),
        "the gate must name the silent-skip shape; it reported {:?}",
        r.hard_stops
    );
}

/// **The per-row equality arm goes red.** A gate that has never been shown to
/// fail is not a gate (ADR-0107 §4.1).
///
/// The mutation is the smallest one that matters and the one a real carry bug
/// would produce: a single column on a single row differs. `tax_number` is
/// chosen because it is the buyer identifier the NAV filing carries — and
/// because the row count, the `Σ issued_invoice_count` fold and the `typeof`
/// sweep are all **blind** to it, so this arm is the only thing standing
/// between a silently-corrupted partner record and a green gate.
#[test]
fn a_single_changed_column_on_a_single_row_reds_the_gate() {
    let dir = scratch("drift");
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert!(
        reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
        "the fixture must be green before it is mutated"
    );

    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE partners SET tax_number = '00000000-0-00' WHERE id = 'p-01'")
            .unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("partner p-01: tax_number")),
        "the gate must name the row AND the column; it reported {:?}",
        r.hard_stops
    );
    assert!(
        !r.checks.iter().any(|c| c.contains("ZERO drift")),
        "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
    );
}

/// A source with no `partners` table is a legitimate shape, and the gate says
/// so out loud rather than staying silent — the difference between "carried
/// nothing because there was nothing" and "carried nothing because the carry
/// was skipped".
#[test]
fn a_partnerless_source_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([9u8; 32]),
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
        "UPDATE audit_ledger
            SET session_id = 'sess-7', session_pubkey = 'pk', event_sig = 'sig';",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-7', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-07-31T00:00:00Z')",
        duckdb::params![TENANT, vec![7u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.partners.partners, 0);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a partnerless source is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("partners family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}
