//! ADR-0108 Step 7 Part E — **the QA / QC family crosses.**
//!
//! (Ervin's brief called this "Part B"; B is partners, C is products/inventory
//! and D is work-orders/BOM, all three already on this branch. Named Part E
//! rather than silently renumbered — the same call Part D made.)
//!
//! Ten pins, in the order they defend:
//!
//! 1. all six tables cross and **no column drifts**, asserted against the
//!    fixture's own literals as well as through the gate;
//! 2. the eight §3.2 E measurements are `'real'` and **bit-identical**, and
//!    `qc_inspections.deviation` carries the value that makes the call
//!    load-bearing: a *derived* subtraction whose scale is far past R2's 6;
//! 3. the two §3.2 H booleans cross as `'integer'` with their values intact —
//!    the first booleans any family in this migration has carried;
//! 4. **the disjunction sweep** — 4096 generated measurements, every one either
//!    accepted and returned **bit-identically** or refused **loudly** naming the
//!    table, the column and the row. Both arms are required to fire;
//! 5. the same disjunction proved through **real storage**: 192 generated
//!    doubles seeded into DuckDB and read back from SQLite bit-for-bit;
//! 6. a non-finite measurement **fails the whole carry** rather than arriving as
//!    a `NULL` — SQLite has no `NaN`, and this is the only way this family can
//!    lose a value;
//! 7. `ensure_quality_schema` builds all six tables, is idempotent, and declares
//!    every column with the §3.2 vocabulary;
//! 8. the gate **hard-stops** per table when a table was not carried;
//! 9. the per-row equality arm is **shown to go red** on a single changed column
//!    on a single row — separately for a measurement, for a boolean, for an
//!    ordinary `TEXT` column, and for the composite-keyed transition log;
//! 10. a pre-S443 source with four of the six tables still crosses, which is why
//!     presence is held per table.
//!
//! **This test pins no §3.4 fold, because this family owes none**, and no
//! M11-shaped `LOWER()`/`LIKE` refusal, because measurement says the family has
//! no such site: both patterns return zero hits against all six tables.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_quality::finite_measurement;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

/// The family's six tables, in the order the migrator and the gate walk them.
const FAMILY: &[&str] = &[
    "ncrs",
    "ncr_transitions",
    "capas",
    "qa_inspections",
    "qc_inspection_plans",
    "qc_inspections",
];

/// The two tables V002 adds — absent on any database older than S443, which is
/// the legitimate partial shape pin 10 exercises.
const QC_TABLES: &[&str] = &["qc_inspection_plans", "qc_inspections"];

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-quality-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// `(ncr_id, severity, category, description, state, closed)`.
///
/// * **`ncr-01`** — open, Hungarian description with the accented characters
///   that a future `LOWER()`-based search would fold (there is none today; the
///   fixture carries the value so a T-12-shaped test has something to key on if
///   one is ever owed here).
/// * **`ncr-02`** — closed, so `closed_at_utc` / `closed_by_operator` are
///   populated and the nullable pair is exercised in both states.
/// * **`ncr-03`** — every JSON array column **empty**: `"[]"` is a value and
///   must cross as `"[]"`, not as `NULL` and not as `""`.
const NCR_CASES: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "ncr-01",
        "Major",
        "Dimensional",
        "Árvíztűrő tükörfúrógép — a furat átmérője tűrésen kívül",
        "Open",
        false,
    ),
    (
        "ncr-02",
        "Minor",
        "Surface",
        "scratch on face B",
        "Closed",
        true,
    ),
    (
        "ncr-03",
        "Critical",
        "Material",
        "wrong heat lot released",
        "InReview",
        false,
    ),
];

/// `(ncr_id, seq, from_state, to_state, note)`.
///
/// **`seq` restarts at 0 for every NCR**, so `ncr-01#0` and `ncr-02#0` both
/// exist — which is the point: the key is composite, and a migrator that keyed
/// on `seq` alone or on `ncr_id` alone would pair the wrong rows and the gate
/// would still be green.
const TRANSITION_CASES: &[(&str, i64, &str, &str, &str)] = &[
    ("ncr-01", 0, "New", "Open", "raised on the floor"),
    ("ncr-01", 1, "Open", "InReview", "escalated to QA"),
    ("ncr-02", 0, "New", "Open", "raised"),
    ("ncr-02", 1, "Open", "InReview", "reviewed"),
    (
        "ncr-02",
        2,
        "InReview",
        "Closed",
        "closed — rework accepted",
    ),
    ("ncr-03", 0, "New", "InReview", ""),
];

/// `(capa_id, ncr_id, verdict, fully_populated)`.
const CAPA_CASES: &[(&str, &str, &str, bool)] = &[
    ("capa-01", "ncr-01", "Pending", false),
    ("capa-02", "ncr-02", "Effective", true),
];

/// `(qa_id, wo_id, routing_op_id, state, decided, superseded_by)`.
const QA_CASES: &[(&str, &str, &str, &str, bool, Option<&str>)] = &[
    ("qa-01", "wo-01", "rop-01", "Pending", false, None),
    ("qa-02", "wo-01", "rop-02", "Passed", true, None),
    ("qa-03", "wo-01", "rop-02", "Rework", true, Some("qa-02")),
];

/// `(plan_id, feature_name, nominal, upper_tol, lower_tol, units, enabled,
/// archived)`.
const PLAN_CASES: &[(&str, &str, f64, f64, f64, &str, bool, bool)] = &[
    ("plan-01", "Ø bore", 25.0, 0.05, -0.05, "mm", true, false),
    (
        "plan-02", "Length", 100.5, 0.1, -0.1, "mm",
        false, // disabled AND archived — the boolean's other value
        true,
    ),
    // Tolerances at the edge of what a float renders shortly, and a non-ASCII
    // unit. `-0.0` is deliberate: it is a distinct bit pattern from `0.0`.
    (
        "plan-03",
        "Roughness",
        0.0,
        0.000001,
        -0.0,
        "µm",
        true,
        false,
    ),
];

/// `(qci_id, plan_id, actual_value, verdict, calibration_stale, auto_ncr)`.
///
/// **`deviation` is not in this table** — the fixture computes it exactly the
/// way `qc::verdict` does (`actual - plan.nominal_value`, in `f64`), because
/// that subtraction is the whole reason §3.2 E's `REAL` call is the right one:
/// `25.03 - 25.0` is `0.030000000000000426`, which needs a scale R2 refuses.
const INSPECTION_CASES: &[(&str, &str, f64, &str, bool, Option<&str>)] = &[
    ("qci-01", "plan-01", 25.03, "Fail", false, Some("ncr-01")),
    ("qci-02", "plan-01", 25.0, "Pass", false, None),
    ("qci-03", "plan-02", 100.45, "Warn", true, None),
    ("qci-04", "plan-03", 0.0, "Pass", false, None),
];

/// How many generated doubles pin 5 pushes through real storage.
const SWEPT_ROWS: usize = 192;

/// A deterministic xorshift, so a failure is reproducible from the file alone.
fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// A finite `f64` across the magnitudes and scales a dimensional measurement
/// actually takes — millimetres, microns, and the odd large coordinate.
fn swept_value(i: usize, state: &mut u64) -> f64 {
    let mantissa = (rng(state) % 1_000_000_000) as f64;
    let exp = (rng(state) % 13) as i32 - 6;
    let v = mantissa * 10f64.powi(exp);
    if i.is_multiple_of(3) {
        -v
    } else {
        v
    }
}

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`s, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it.
///
/// Two of them, because this family has two owning modules: `aberp::quality`
/// builds the three NCR/CAPA tables and `aberp_qa` builds V001 + V002.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::quality::ensure_schema(&conn).unwrap();
        aberp_qa::ensure_schema(&conn).unwrap();

        for (ncr_id, severity, category, description, state, closed) in NCR_CASES {
            conn.execute(
                "INSERT INTO ncrs
                   (ncr_id, tenant_id, discovered_at_utc, discovered_by_operator, severity,
                    category, description, affected_part_uids, affected_wo_ids,
                    affected_heat_lots, photos, state, closed_at_utc, closed_by_operator)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    ncr_id,
                    TENANT,
                    "2026-01-01T08:00:00Z",
                    "operator@example.test",
                    severity,
                    category,
                    description,
                    if *ncr_id == "ncr-03" {
                        "[]"
                    } else {
                        r#"["part-1","part-2"]"#
                    },
                    if *ncr_id == "ncr-03" {
                        "[]"
                    } else {
                        r#"["wo-01"]"#
                    },
                    if *ncr_id == "ncr-03" {
                        "[]"
                    } else {
                        r#"["heat-77"]"#
                    },
                    if *ncr_id == "ncr-03" {
                        "[]"
                    } else {
                        r#"["p.jpg"]"#
                    },
                    state,
                    closed.then_some("2026-02-01T09:00:00Z"),
                    closed.then_some("qa@example.test"),
                ],
            )
            .unwrap();
        }

        for (ncr_id, seq, from_state, to_state, note) in TRANSITION_CASES {
            conn.execute(
                "INSERT INTO ncr_transitions
                   (tenant_id, ncr_id, seq, from_state, to_state, operator, at_utc, note)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    TENANT,
                    ncr_id,
                    seq,
                    from_state,
                    to_state,
                    "operator@example.test",
                    format!("2026-01-0{}T10:00:00Z", seq + 1),
                    note,
                ],
            )
            .unwrap();
        }

        for (capa_id, ncr_id, verdict, full) in CAPA_CASES {
            conn.execute(
                "INSERT INTO capas
                   (capa_id, ncr_id, tenant_id, corrective_action_text, preventive_action_text,
                    responsible_operator, target_close_date, actual_close_date,
                    effectiveness_review_at_utc, effectiveness_verdict, effectiveness_comment,
                    approved_by_operator, approved_at_utc, created_at_utc, created_by_operator)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    capa_id,
                    ncr_id,
                    TENANT,
                    "Szerszámcsere és újramérés",
                    "napi kalibráció bevezetése",
                    "mérnök@example.test",
                    "2026-03-01",
                    full.then_some("2026-02-20"),
                    full.then_some("2026-02-25T00:00:00Z"),
                    verdict,
                    full.then_some("hatásos"),
                    full.then_some("plant.manager@example.test"),
                    full.then_some("2026-02-26T00:00:00Z"),
                    "2026-01-05T00:00:00Z",
                    "qa@example.test",
                ],
            )
            .unwrap();
        }

        for (qa_id, wo_id, routing_op_id, state, decided, superseded) in QA_CASES {
            conn.execute(
                "INSERT INTO qa_inspections
                   (qa_id, tenant_id, wo_id, routing_op_id, state, decided_at, decided_by,
                    reason, measurement, source_event_id, created_at, superseded_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    qa_id,
                    TENANT,
                    wo_id,
                    routing_op_id,
                    state,
                    decided.then_some("2026-02-02T12:00:00Z"),
                    decided.then_some("qa@example.test"),
                    decided.then_some("tűrésen kívül"),
                    // Free-form operator TEXT, not a number — the one column in
                    // this family a reader might mistake for a §3.2 quantity.
                    decided.then_some("25,03 mm (kézi mérés)"),
                    "evt-1",
                    "2026-02-02T11:00:00Z",
                    superseded,
                ],
            )
            .unwrap();
        }

        for (plan_id, feature, nominal, upper, lower, units, enabled, archived) in PLAN_CASES {
            conn.execute(
                "INSERT INTO qc_inspection_plans
                   (plan_id, tenant_id, product_id, feature_name, nominal_value, upper_tol,
                    lower_tol, units, optional_probe_cycle_id, enabled, created_at, archived_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    plan_id,
                    TENANT,
                    "prd-01",
                    feature,
                    nominal,
                    upper,
                    lower,
                    units,
                    (*plan_id == "plan-01").then_some("cycle-9"),
                    enabled,
                    "2026-01-01T00:00:00Z",
                    archived.then_some("2026-04-01T00:00:00Z"),
                ],
            )
            .unwrap();
        }

        for (qci_id, plan_id, actual, verdict, stale, auto_ncr) in INSPECTION_CASES {
            let (nominal, upper, lower, units) = plan_of(plan_id);
            insert_inspection(
                &conn,
                qci_id,
                plan_id,
                nominal,
                upper,
                lower,
                units,
                *actual,
                // Exactly `qc::verdict`'s own arithmetic.
                actual - nominal,
                verdict,
                *stale,
                *auto_ncr,
            );
        }

        // Pin 5's storage sweep: SWEPT_ROWS generated doubles, carried as real
        // rows so the round-trip is proved through DuckDB → SQLite storage and
        // not only through the validator.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for i in 0..SWEPT_ROWS {
            let actual = swept_value(i, &mut state);
            let nominal = swept_value(i + 7, &mut state);
            insert_inspection(
                &conn,
                &format!("qci-swept-{i:04}"),
                "plan-01",
                nominal,
                swept_value(i + 13, &mut state).abs(),
                -swept_value(i + 29, &mut state).abs(),
                "mm",
                actual,
                actual - nominal,
                "Pass",
                i % 2 == 0,
                None,
            );
        }

        conn.close().unwrap();
    }

    seed_ledger(&db);
    db
}

/// The plan a fixture inspection snapshots, so the denormalised copy on the
/// inspection row really is the plan's value.
fn plan_of(plan_id: &str) -> (f64, f64, f64, &'static str) {
    let p = PLAN_CASES.iter().find(|p| p.0 == plan_id).unwrap();
    (p.2, p.3, p.4, p.5)
}

#[allow(clippy::too_many_arguments)]
fn insert_inspection(
    conn: &duckdb::Connection,
    qci_id: &str,
    plan_id: &str,
    nominal: f64,
    upper: f64,
    lower: f64,
    units: &str,
    actual: f64,
    deviation: f64,
    verdict: &str,
    stale: bool,
    auto_ncr: Option<&str>,
) {
    conn.execute(
        "INSERT INTO qc_inspections
           (qci_id, tenant_id, measured_at_utc, source, source_event_id, inspection_plan_id,
            feature_name, nominal_value, upper_tol, lower_tol, units, actual_value, deviation,
            verdict, probe_serial, last_calibration_at_utc, calibration_stale_at_event,
            auto_ncr_id, linked_part_uid, linked_heat_lot, linked_wo_id, recorded_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        duckdb::params![
            qci_id,
            TENANT,
            "2026-02-03T07:30:00Z",
            "Manual",
            None::<&str>,
            plan_id,
            "Ø bore",
            nominal,
            upper,
            lower,
            units,
            actual,
            deviation,
            verdict,
            stale.then_some("probe-A"),
            stale.then_some("2025-12-01T00:00:00Z"),
            stale,
            auto_ncr,
            "part-1",
            "heat-77",
            "wo-01",
            "operator@example.test",
            "2026-02-03T07:31:00Z",
        ],
    )
    .unwrap();
}

/// The audit chain + mirror + tamper-evidence layer the Step-4 gate turns on.
fn seed_ledger(db: &Path) {
    {
        let mut ledger = Ledger::open(
            db,
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
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(db).unwrap();
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
}

/// Migrate a freshly-seeded fixture and return `(dir, duckdb, sqlite)`.
fn crossed(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(tag);
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.quality.ncrs, NCR_CASES.len() as u64);
    assert_eq!(out.quality.ncr_transitions, TRANSITION_CASES.len() as u64);
    assert_eq!(out.quality.capas, CAPA_CASES.len() as u64);
    assert_eq!(out.quality.qa_inspections, QA_CASES.len() as u64);
    assert_eq!(out.quality.qc_inspection_plans, PLAN_CASES.len() as u64);
    assert_eq!(
        out.quality.qc_inspections,
        (INSPECTION_CASES.len() + SWEPT_ROWS) as u64
    );
    (dir, db, lite)
}

fn sqlite_text(lite: &Path, sql: &str) -> Option<String> {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .unwrap()
}

fn sqlite_f64(lite: &Path, table: &str, col: &str, key_col: &str, key: &str) -> f64 {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        &format!("SELECT {col} FROM {table} WHERE {key_col} = ?"),
        [key],
        |r| r.get::<_, f64>(0),
    )
    .unwrap()
}

fn sqlite_typeof(lite: &Path, table: &str, col: &str, key_col: &str, key: &str) -> String {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        &format!("SELECT typeof({col}) FROM {table} WHERE {key_col} = ?"),
        [key],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// 1. The headline
// ---------------------------------------------------------------------------

/// All six tables cross, the gate passes, and every column read back from SQLite
/// is the value DuckDB held.
///
/// The read-back is done here as well as inside the gate: the gate compares the
/// two sides against each other, whereas the assertions below compare SQLite
/// against the literal constants the fixture was built from — so two sides that
/// were wrong in the same way would still be caught.
#[test]
fn the_qa_qc_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("family");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(r.hard_stops.is_empty(), "gate: {:?}", r.hard_stops);
    for table in FAMILY {
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(&format!("every {table} column round-trips with ZERO drift"))),
            "the gate must report the ZERO-drift arm for {table}; checks: {:?}",
            r.checks
        );
    }

    // The NCR record itself — ISO-9001 traceability, whose value is that it is
    // complete and unaltered.
    for (ncr_id, severity, category, description, state, closed) in NCR_CASES {
        assert_eq!(
            sqlite_text(
                &lite,
                &format!("SELECT description FROM ncrs WHERE ncr_id = '{ncr_id}'")
            )
            .as_deref(),
            Some(*description),
            "{ncr_id}: the description carries accented text verbatim"
        );
        for (col, want) in [
            ("severity", severity),
            ("category", category),
            ("state", state),
        ] {
            assert_eq!(
                sqlite_text(
                    &lite,
                    &format!("SELECT {col} FROM ncrs WHERE ncr_id = '{ncr_id}'")
                )
                .as_deref(),
                Some(*want),
                "{ncr_id}.{col}"
            );
        }
        assert_eq!(
            sqlite_text(
                &lite,
                &format!("SELECT closed_by_operator FROM ncrs WHERE ncr_id = '{ncr_id}'")
            )
            .is_some(),
            *closed,
            "{ncr_id}: the nullable close pair must keep its NULL-ness"
        );
    }

    // `"[]"` is a value. It must not become NULL and must not become "".
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT affected_part_uids FROM ncrs WHERE ncr_id = 'ncr-03'"
        )
        .as_deref(),
        Some("[]"),
        "an empty JSON array is a value, not an absence"
    );

    // The composite-keyed transition log: every (ncr_id, seq) pair present, with
    // its own note. `ncr-01#0` and `ncr-02#0` share a `seq` on purpose.
    for (ncr_id, seq, from_state, to_state, note) in TRANSITION_CASES {
        let got = sqlite_text(
            &lite,
            &format!(
                "SELECT from_state || '>' || to_state || '|' || note FROM ncr_transitions \
                 WHERE ncr_id = '{ncr_id}' AND seq = {seq}"
            ),
        );
        assert_eq!(
            got.as_deref(),
            Some(format!("{from_state}>{to_state}|{note}").as_str()),
            "{ncr_id}#{seq}"
        );
    }

    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT effectiveness_comment FROM capas WHERE capa_id = 'capa-02'"
        )
        .as_deref(),
        Some("hatásos")
    );
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT effectiveness_comment FROM capas WHERE capa_id = 'capa-01'"
        ),
        None,
        "capa-01's optional fields are NULL and must stay NULL"
    );

    // `qa_inspections.measurement` is operator TEXT, comma decimal separator and
    // all — the column a reader might mistake for a §3.2 quantity.
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT measurement FROM qa_inspections WHERE qa_id = 'qa-02'"
        )
        .as_deref(),
        Some("25,03 mm (kézi mérés)")
    );
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT superseded_by FROM qa_inspections WHERE qa_id = 'qa-03'"
        )
        .as_deref(),
        Some("qa-02"),
        "the cross-actor override pointer is the ADR-0063 §4 denormalisation"
    );
}

// ---------------------------------------------------------------------------
// 2. The eight measurements
// ---------------------------------------------------------------------------

/// **The eight §3.2 E measurements are `'real'` and bit-identical**, and
/// `deviation` carries the value that makes the call load-bearing.
///
/// §3.2 E note (b) took this call when the column was found and Part D's
/// distinguishing test confirms it: these have no exact counterpart anywhere in
/// the tree, so there is no rule-7 fork to close, and `deviation` is *derived by
/// subtraction* — `25.03 - 25.0` needs a scale far past the canonical quantity
/// scale of 6, so an R2 carry would have refused an ordinary inspection row.
#[test]
fn the_eight_measurements_stay_real_and_are_bit_identical() {
    let (_dir, _db, lite) = crossed("measure");

    for (plan_id, _, nominal, upper, lower, _, _, _) in PLAN_CASES {
        for (col, want) in [
            ("nominal_value", nominal),
            ("upper_tol", upper),
            ("lower_tol", lower),
        ] {
            assert_eq!(
                sqlite_f64(&lite, "qc_inspection_plans", col, "plan_id", plan_id),
                *want,
                "{plan_id}.{col}: DOUBLE → REAL must be bit-exact"
            );
            assert_eq!(
                sqlite_typeof(&lite, "qc_inspection_plans", col, "plan_id", plan_id),
                "real",
                "{plan_id}.{col}: §3.2 E keeps this a float"
            );
        }
    }

    for (qci_id, plan_id, actual, _, _, _) in INSPECTION_CASES {
        let (nominal, upper, lower, _) = plan_of(plan_id);
        for (col, want) in [
            ("nominal_value", nominal),
            ("upper_tol", upper),
            ("lower_tol", lower),
            ("actual_value", *actual),
            ("deviation", actual - nominal),
        ] {
            assert_eq!(
                sqlite_f64(&lite, "qc_inspections", col, "qci_id", qci_id),
                want,
                "{qci_id}.{col}: DOUBLE → REAL must be bit-exact"
            );
            assert_eq!(
                sqlite_typeof(&lite, "qc_inspections", col, "qci_id", qci_id),
                "real",
                "{qci_id}.{col}: §3.2 E keeps this a float"
            );
        }
    }

    // **The load-bearing value.** `deviation` on `qci-01` is what
    // `qc::verdict` computes, and its scale is past what R2 accepts — so
    // carrying these eight as R2 would have hard-failed the migration on an
    // ordinary inspection row rather than on a pathological one.
    let deviation = 25.03f64 - 25.0f64;
    assert_eq!(
        sqlite_f64(&lite, "qc_inspections", "deviation", "qci_id", "qci-01"),
        deviation
    );
    let scale = rust_decimal::Decimal::from_str_exact(&format!("{deviation}"))
        .expect("it renders as a decimal")
        .scale();
    assert!(
        scale > 6,
        "the fixture must actually exercise a derived deviation beyond the canonical quantity \
         scale of 6; got scale {scale} for {deviation}"
    );
}

// ---------------------------------------------------------------------------
// 3. The two booleans
// ---------------------------------------------------------------------------

/// **The first booleans this migration has carried** cross as `'integer'` with
/// their values intact (§3.2 H).
///
/// A `'text'` here would be a `"true"` bound as a string — which SQLite would
/// then read back as `0` in any numeric context, silently disabling every
/// enabled plan.
#[test]
fn the_two_booleans_cross_as_integer_with_their_values_intact() {
    let (_dir, _db, lite) = crossed("bool");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    for (plan_id, _, _, _, _, _, enabled, _) in PLAN_CASES {
        let got: bool = conn
            .query_row(
                "SELECT enabled FROM qc_inspection_plans WHERE plan_id = ?",
                [plan_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, *enabled, "{plan_id}.enabled");
        assert_eq!(
            sqlite_typeof(&lite, "qc_inspection_plans", "enabled", "plan_id", plan_id),
            "integer",
            "§3.2 H maps BOOLEAN to INTEGER; a 'text' here reads back as false"
        );
    }

    for (qci_id, _, _, _, stale, _) in INSPECTION_CASES {
        let got: bool = conn
            .query_row(
                "SELECT calibration_stale_at_event FROM qc_inspections WHERE qci_id = ?",
                [qci_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, *stale, "{qci_id}.calibration_stale_at_event");
        assert_eq!(
            sqlite_typeof(
                &lite,
                "qc_inspections",
                "calibration_stale_at_event",
                "qci_id",
                qci_id
            ),
            "integer"
        );
    }

    // Both values must actually occur, or the pin proves only that one of them
    // survives.
    assert!(PLAN_CASES.iter().any(|p| p.6));
    assert!(PLAN_CASES.iter().any(|p| !p.6));
}

// ---------------------------------------------------------------------------
// 4. The disjunction sweep
// ---------------------------------------------------------------------------

/// **Every measurement either round-trips bit-identically or is refused, loudly,
/// naming the table, the column and the row. Both arms are required to fire.**
///
/// This family's exactness rule is §3.2 E's bit-exact `REAL` rather than R2's
/// canonical decimal string, so the disjunction is over [`finite_measurement`]
/// rather than over `canonical_decimal` — but the shape and the requirement are
/// Part D's: a sweep in which nothing was ever refused proves only that the
/// happy path exists.
#[test]
fn every_carried_measurement_either_round_trips_bit_identically_or_is_refused() {
    let mut carried = 0usize;
    let mut refused = 0usize;

    // The adversarial table first: the shapes a dimensional measurement actually
    // takes, and the shapes it must never accept.
    let named: &[(f64, bool)] = &[
        (0.0, true),
        (-0.0, true),
        (25.0, true),
        (-273.15, true),
        (0.000001, true),
        // The derived subtraction `qc::verdict` performs. Carried, because it is
        // an ordinary inspection result — and refused by R2, which is the whole
        // §3.2 E argument in one row.
        (25.03 - 25.0, true),
        (f64::MIN, true),
        (f64::MAX, true),
        (f64::MIN_POSITIVE, true),
        // Subnormal — still finite, still a bit pattern SQLite stores exactly.
        (f64::from_bits(1), true),
        (f64::NAN, false),
        (-f64::NAN, false),
        (f64::INFINITY, false),
        (f64::NEG_INFINITY, false),
    ];
    for (v, ok) in named {
        match finite_measurement(*v, "qci-01", "qc_inspections", "deviation") {
            Ok(out) => {
                assert!(*ok, "{v} should have been refused, got {out}");
                assert_eq!(
                    out.to_bits(),
                    v.to_bits(),
                    "{v} must be returned BIT-identically, not renormalised"
                );
                carried += 1;
            }
            Err(e) => {
                assert!(!*ok, "{v} should have crossed: {e}");
                let msg = e.to_string();
                assert!(
                    msg.contains("qci-01"),
                    "the refusal must name the row: {msg}"
                );
                assert!(
                    msg.contains("qc_inspections.deviation"),
                    "the refusal must name the table and column: {msg}"
                );
                refused += 1;
            }
        }
    }

    // 4096 generated measurements across the magnitudes and scales a QC column
    // actually takes. Every 16th is replaced by a non-finite value — the shape a
    // divide-by-zero or a probe fault produces — so the refusal arm is exercised
    // on generated input too, not only on the named table.
    let mut state: u64 = 0xD1B54A32D192ED03;
    for i in 0..4096usize {
        let v = if i % 16 == 0 {
            match rng(&mut state) % 3 {
                0 => f64::NAN,
                1 => f64::INFINITY,
                _ => f64::NEG_INFINITY,
            }
        } else {
            swept_value(i, &mut state)
        };
        let key = format!("qci-{i:04}");
        match finite_measurement(v, &key, "qc_inspection_plans", "upper_tol") {
            Ok(out) => {
                assert_eq!(
                    out.to_bits(),
                    v.to_bits(),
                    "value {i} ({v}) must be returned bit-identically"
                );
                carried += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains(&key),
                    "value {i}: refusal must name the row: {msg}"
                );
                assert!(
                    msg.contains("qc_inspection_plans.upper_tol"),
                    "value {i}: refusal must name the table and column: {msg}"
                );
                refused += 1;
            }
        }
    }

    // **Both arms, required.** The thresholds sit below the constructed counts
    // (10 named carries + 3840 generated; 4 named refusals + 256 generated) so a
    // real change shows up as signal rather than as churn.
    assert!(carried > 3_800, "the carry arm must fire: {carried}");
    assert!(refused > 250, "the REFUSAL arm must fire: {refused}");
}

/// Pin 5 — **the same disjunction proved through real storage.** The validator
/// agreeing with itself is not the property; the property is that a value
/// DuckDB held comes back from SQLite as the same 64 bits.
#[test]
fn every_swept_measurement_survives_real_storage_bit_for_bit() {
    let (_dir, db, lite) = crossed("sweep");

    let duck = duckdb::Connection::open(&db).unwrap();
    let sql = "SELECT nominal_value, upper_tol, lower_tol, actual_value, deviation \
               FROM qc_inspections WHERE qci_id = ?";
    let lite_conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    let mut compared = 0usize;
    for i in 0..SWEPT_ROWS {
        let key = format!("qci-swept-{i:04}");
        let d: [f64; 5] = duck
            .query_row(sql, duckdb::params![key], |r| {
                Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])
            })
            .unwrap();
        let l: [f64; 5] = lite_conn
            .query_row(sql, [&key], |r| {
                Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])
            })
            .unwrap();
        for (j, (a, b)) in d.iter().zip(l.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{key} column {j}: DuckDB {a} ({:#x}) vs SQLite {b} ({:#x})",
                a.to_bits(),
                b.to_bits()
            );
            compared += 1;
        }
    }
    assert_eq!(
        compared,
        SWEPT_ROWS * 5,
        "every swept value must be compared"
    );
}

// ---------------------------------------------------------------------------
// 6. The refusal, end to end
// ---------------------------------------------------------------------------

/// **A non-finite measurement fails the whole carry**, naming the table, the
/// column and the row — rather than arriving in SQLite as a `NULL`.
///
/// This is the family's only loss channel and it is not hypothetical: SQLite has
/// no `NaN`, so a bound `f64::NAN` is stored as `NULL`. All eight measurement
/// columns are `NOT NULL`, so without the refusal this would surface as a bare
/// `NOT NULL constraint failed` naming neither the row nor the reason.
#[test]
fn a_non_finite_measurement_fails_the_migration() {
    for (col, bad) in [
        ("deviation", "CAST('nan' AS DOUBLE)"),
        ("actual_value", "CAST('inf' AS DOUBLE)"),
    ] {
        let dir = scratch("nonfinite");
        let db = seed(&dir);
        {
            let conn = duckdb::Connection::open(&db).unwrap();
            conn.execute_batch(&format!(
                "UPDATE qc_inspections SET {col} = {bad} WHERE qci_id = 'qci-01';"
            ))
            .unwrap();
            conn.close().unwrap();
        }

        let snap = run_snapshot(&db, TENANT, None).unwrap();
        let lite = dir.join("aberp.sqlite");
        let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
            .expect_err("a non-finite measurement must FAIL the carry");
        let msg = format!("{err:#}");
        assert!(msg.contains("qci-01"), "must name the row: {msg}");
        assert!(
            msg.contains(&format!("qc_inspections.{col}")),
            "must name the table and column: {msg}"
        );
        assert!(
            msg.contains("SQLite cannot store"),
            "must say what the rule is, not just that a value was odd: {msg}"
        );
    }
}

/// And the reason the refusal is not redundant: **SQLite really does turn a
/// bound `NaN` into `NULL`.** Measured here rather than asserted in a comment,
/// because the whole argument for [`finite_measurement`] rests on it.
#[test]
fn sqlite_stores_a_bound_nan_as_null() {
    let dir = scratch("nan-null");
    let lite = dir.join("probe.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    conn.execute_batch("CREATE TABLE probe (id TEXT NOT NULL, v REAL) STRICT;")
        .unwrap();
    conn.execute("INSERT INTO probe (id, v) VALUES ('a', ?)", [f64::NAN])
        .unwrap();

    let t: String = conn
        .query_row("SELECT typeof(v) FROM probe WHERE id = 'a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        t, "null",
        "SQLite has no NaN — this is why the carry refuses one instead of binding it"
    );

    // And the infinities, which DO round-trip: the refusal of those is a product
    // call (an infinite dimensional measurement is not a measurement), not a
    // storage limitation, and the difference is worth having measured.
    conn.execute("INSERT INTO probe (id, v) VALUES ('b', ?)", [f64::INFINITY])
        .unwrap();
    let got: f64 = conn
        .query_row("SELECT v FROM probe WHERE id = 'b'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got, f64::INFINITY);
}

// ---------------------------------------------------------------------------
// 7. The schema
// ---------------------------------------------------------------------------

/// `ensure_quality_schema` builds all six tables, is idempotent, and declares
/// every column with the §3.2 vocabulary.
///
/// **There is no `ensure_columns` ladder to exercise in this family** — measured:
/// `ALTER TABLE` returns zero hits against all six tables. The schema evolution
/// here happened by adding *tables*, across three migrations, which is exactly
/// why presence is per table.
#[test]
fn ensure_quality_schema_builds_all_six_tables_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("schema.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    for _ in 0..3 {
        aberp::migrate_quality::ensure_quality_schema(&conn).expect("idempotent");
    }

    for (table, col, want) in [
        ("ncrs", "ncr_id", "TEXT"),
        ("ncrs", "affected_part_uids", "TEXT"),
        ("ncrs", "closed_at_utc", "TEXT"),
        ("ncr_transitions", "seq", "INTEGER"),
        ("ncr_transitions", "note", "TEXT"),
        ("capas", "capa_id", "TEXT"),
        ("capas", "effectiveness_comment", "TEXT"),
        ("qa_inspections", "qa_id", "TEXT"),
        ("qa_inspections", "measurement", "TEXT"),
        ("qc_inspection_plans", "nominal_value", "REAL"),
        ("qc_inspection_plans", "upper_tol", "REAL"),
        ("qc_inspection_plans", "lower_tol", "REAL"),
        ("qc_inspection_plans", "enabled", "INTEGER"),
        ("qc_inspections", "nominal_value", "REAL"),
        ("qc_inspections", "upper_tol", "REAL"),
        ("qc_inspections", "lower_tol", "REAL"),
        ("qc_inspections", "actual_value", "REAL"),
        ("qc_inspections", "deviation", "REAL"),
        ("qc_inspections", "calibration_stale_at_event", "INTEGER"),
    ] {
        let got: String = conn
            .query_row(
                &format!("SELECT type FROM pragma_table_info('{table}') WHERE name = '{col}'"),
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("{table}.{col} must exist: {e}"));
        assert_eq!(got, want, "{table}.{col}");
    }

    // Every table is STRICT.
    for table in FAMILY {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("STRICT"), "{table} must be STRICT: {sql}");
    }

    // V001's and V002's indexes carry across.
    for idx in [
        "qa_inspections_tenant_state_created_idx",
        "qa_inspections_tenant_wo_routing_idx",
        "qc_inspection_plans_tenant_product_idx",
        "qc_inspections_tenant_wo_idx",
        "qc_inspections_tenant_part_idx",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
                [idx],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{idx} must land");
    }

    // And the three NCR/CAPA tables carry NO index, matching `quality.rs`'s
    // stated S341/S410 posture. Adding one here would fork the two schemas.
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name IN \
             ('ncrs', 'ncr_transitions', 'capas') AND sql IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "quality.rs declares no index on these three; the SQLite side must not invent one"
    );
}

// ---------------------------------------------------------------------------
// 8 + 9. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** per table when a table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops one arm of
/// the carry from `carry_quality`.
#[test]
fn the_gate_hard_stops_per_table_when_a_table_was_not_carried() {
    for table in FAMILY {
        let (_dir, db, lite) = crossed("notcarried");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!("DROP TABLE {table}")).unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.starts_with(table) && s.contains("exists in DuckDB but NOT in SQLite")),
            "dropping {table} must be named by the gate; it reported {:?}",
            r.hard_stops
        );
    }
}

/// **A single changed column on a single row reds the gate — for each of the
/// four column classes in this family.** A gate that has never been shown to
/// fail is not a gate (ADR-0107 §4.1).
///
/// The four mutations are chosen so nothing else can catch them:
///
/// * `qc_inspections.deviation` on `qci-01` — a measurement. There is no fold
///   over these at all (summing `f64`s is the operation this migration exists to
///   avoid) and the `typeof` sweep sees `'real'` either way, so the per-row arm
///   is the only thing between a rewritten verdict input and a green gate;
/// * `qc_inspection_plans.enabled` on `plan-01` — the boolean. `typeof` still
///   reads `'integer'`; only the value moved;
/// * `ncrs.description` on `ncr-01` — an ordinary `TEXT` column that no fold
///   touches, which is why the arm compares every column rather than the
///   "important" ones. On an ISO-9001 record the description **is** the payload;
/// * `ncr_transitions.note` on `ncr-01#1` — the composite-keyed table, mutated
///   so the `Σ seq` fold is provably **unchanged**, leaving the per-row arm
///   alone again.
#[test]
fn a_single_changed_column_on_a_single_row_reds_the_gate() {
    // --- a measurement ---
    let (_dir, db, lite) = crossed("drift-measure");
    assert!(
        reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
        "the fixture must be green before it is mutated"
    );
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        // One ULP away — the smallest change a REAL column can carry.
        let nudged = f64::from_bits((25.03f64 - 25.0f64).to_bits() + 1);
        conn.execute(
            "UPDATE qc_inspections SET deviation = ? WHERE qci_id = 'qci-01'",
            [nudged],
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("QC inspection qci-01: deviation")),
        "the gate must name the row AND the column; it reported {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("typeof(qc_inspections.deviation) = 'real'")),
        "the typeof sweep is BLIND to this — which is why the per-row arm exists: {:?}",
        r.checks
    );
    assert!(
        !r.checks
            .iter()
            .any(|c| c.contains("every qc_inspections column round-trips with ZERO drift")),
        "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
    );

    // --- the boolean ---
    let (_dir, db, lite) = crossed("drift-bool");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE qc_inspection_plans SET enabled = 0 WHERE plan_id = 'plan-01'")
            .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("QC plan plan-01: enabled")),
        "a disabled inspection plan is a silently skipped inspection: {:?}",
        r.hard_stops
    );

    // --- an ordinary TEXT column no fold touches ---
    let (_dir, db, lite) = crossed("drift-text");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE ncrs SET description = 'minor scuff' WHERE ncr_id = 'ncr-01'")
            .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("NCR ncr-01: description")),
        "no fold and no typeof sweep can see this; the per-row arm must: {:?}",
        r.hard_stops
    );

    // --- the composite-keyed table, with Σ provably unchanged ---
    let (_dir, db, lite) = crossed("drift-transition");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE ncr_transitions SET note = 'closed without review' \
             WHERE ncr_id = 'ncr-01' AND seq = 1",
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.checks.iter().any(|c| c.contains("Σ ncr_transitions.seq")),
        "the Σ fold must still agree — that is what makes this mutation the interesting one: {:?}",
        r.checks
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("NCR transition ncr-01#1: note")),
        "the per-row arm must name the COMPOSITE key; it reported {:?}",
        r.hard_stops
    );
}

/// A source with none of the family's tables is a legitimate shape, and the gate
/// says so out loud rather than staying silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.quality, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source with no NCRs and no inspections is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("QA/QC family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}

/// **A pre-S443 source has four of the six tables, and that is a legitimate
/// shape** — which is why presence is held per table rather than per family.
///
/// Without this, a per-family answer would say "the family is present" and then
/// hard-stop on the two QC tables, refusing to migrate a database whose only sin
/// is being older than V002.
#[test]
fn a_pre_s443_source_without_the_qc_tables_still_crosses() {
    let dir = scratch("preqc");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        for t in QC_TABLES {
            conn.execute_batch(&format!("DROP TABLE {t};")).unwrap();
        }
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.quality.ncrs, NCR_CASES.len() as u64);
    assert_eq!(out.quality.qa_inspections, QA_CASES.len() as u64);
    assert_eq!(out.quality.qc_inspection_plans, 0);
    assert_eq!(out.quality.qc_inspections, 0);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a pre-V002 source is legitimate, not an asymmetry: {:?}",
        r.hard_stops
    );

    // And the absent tables really are absent on both sides, not silently
    // created.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    for t in QC_TABLES {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 0,
            "creating an empty {t} the source does not have would manufacture the asymmetry the \
             gate exists to detect"
        );
    }
}

/// **B2 — the three keyless tables in this family must REFUSE a duplicate
/// natural key, not reconcile it away.**
///
/// `ncrs`, `capas` and `ncr_transitions` have no `PRIMARY KEY`, no `UNIQUE` and
/// no index **on either engine** (ADR-0019 keeps uniqueness in Rust), while the
/// reconciliation arm keys its per-row `BTreeMap` on `ncr_id` / `capa_id` /
/// `ncr_id#seq`. So a duplicate in the DuckDB source used to carry as two rows,
/// collapse onto one map entry, and pass: the row count matched (both sides had
/// the extra row), the `typeof` sweep matched, and the per-row arm compared one
/// row twice while never comparing the other at all.
///
/// The assertion below is on the *refusal*, and the two `assert_eq!`s before it
/// are what stop the test from passing vacuously: the plant really does add a
/// row, and it really does leave the distinct-key count where it was — i.e.
/// this is the collapse shape and not merely a bigger table.
///
/// It is the same guard, and the same call, Part F built for `wo_part_marks`
/// and Part G reused for the four purchasing tables.
#[test]
fn a_duplicate_natural_key_in_a_keyless_quality_table_is_refused() {
    for (table, plant, key_sql, named) in [
        (
            "ncrs",
            "INSERT INTO ncrs SELECT * FROM ncrs WHERE ncr_id = 'ncr-01';",
            "SELECT count(DISTINCT ncr_id) FROM ncrs",
            "ncr-01",
        ),
        (
            "capas",
            "INSERT INTO capas SELECT * FROM capas WHERE capa_id = 'capa-01';",
            "SELECT count(DISTINCT capa_id) FROM capas",
            "capa-01",
        ),
        (
            "ncr_transitions",
            "INSERT INTO ncr_transitions SELECT * FROM ncr_transitions \
             WHERE ncr_id = 'ncr-01' AND seq = 1;",
            "SELECT count(*) FROM (SELECT DISTINCT ncr_id, seq FROM ncr_transitions)",
            "ncr-01#1",
        ),
    ] {
        let dir = scratch(&format!("dupkey-{table}"));
        let db = seed(&dir);
        let (rows_before, distinct_before) = {
            let conn = duckdb::Connection::open(&db).unwrap();
            let r: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |x| x.get(0))
                .unwrap();
            let d: i64 = conn.query_row(key_sql, [], |x| x.get(0)).unwrap();
            conn.execute_batch(plant).unwrap();
            let r2: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |x| x.get(0))
                .unwrap();
            let d2: i64 = conn.query_row(key_sql, [], |x| x.get(0)).unwrap();
            conn.close().unwrap();
            assert_eq!(r2, r + 1, "{table}: the plant must add exactly one row");
            assert_eq!(
                d2, d,
                "{table}: the plant must NOT add a distinct key — that is the collapse shape"
            );
            (r2, d2)
        };
        assert_eq!(rows_before, distinct_before + 1);

        let snap = run_snapshot(&db, TENANT, None).unwrap();
        let lite = dir.join("aberp.sqlite");
        let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
            .expect_err("a duplicate natural key on a keyless table must FAIL the carry");
        let msg = format!("{err:#}");
        assert!(msg.contains(table), "must name the table: {msg}");
        assert!(msg.contains(named), "must name the duplicated key: {msg}");
        assert!(
            msg.contains("no PRIMARY KEY"),
            "must say WHY nothing stopped it, or the next reader adds a constraint to the DDL \
             instead of keeping the invariant in Rust: {msg}"
        );
    }
}
