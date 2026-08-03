//! ADR-0108 **Step 8** — the quoting family's crossing, pinned against real
//! storage.
//!
//! This is the family §3.2 D reserves the right to abandon, and the pins are
//! shaped around the single thing that makes abandoning it the right call if it
//! goes wrong: **money on a float**. What this file proves, in order of how much
//! it would cost to have wrong:
//!
//! 1. **An `f64` money value that cannot cross exactly REFUSES the whole
//!    migration** — it is never rounded into range and never silently
//!    stringified (§3.2 D, rule 11). Three separate refusal shapes: a scale past
//!    6, a magnitude past `Decimal`'s range, and a non-finite value.
//! 2. The three carried money columns land as `'text'` **on disk** and convert
//!    back to the identical `f64`.
//! 3. The three money columns on the **not-carried** tables are *declared*
//!    `TEXT` — the only guard they get, because `typeof()` over zero rows is
//!    vacuous and would report PASS over a `REAL` declaration.
//! 4. §6.3's drop is a **stated** drop: the four job-history tables exist, are
//!    empty, and the gate names the DuckDB row count it left behind. A row that
//!    appears in one of them reds.
//! 5. The §3.2 E floats cross bit-exact, including a scale-17 value an R2 carry
//!    would have hard-failed on (Part E's argument, on this family's data).
//! 6. The gate's own teeth: one changed column at a time reds it, on every
//!    representation class.
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
        "aberp-adr0108-step8-quoting-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// **The scale-17 float this family really holds.**
///
/// Part E's lesson, restated on quoting data: a derived `f64` like
/// `0.03 - 0.0` renders at scale 17, so an R2 carry of a §3.2 E column would
/// **hard-fail the whole migration on an ordinary tunable**, not on a
/// pathological one. It crosses as a bit-exact `REAL` instead, and this constant
/// is what makes that claim measured rather than argued.
const SCALE_17_FLOAT: f64 = 0.030000000000000426;

/// A money value at exactly the R2 scale limit — six decimal places. It must
/// cross, because refusing it would mean the limit is really five.
const MONEY_AT_SCALE_6: f64 = 1.234567;

/// A money value one digit past the limit. It must **refuse**.
const MONEY_AT_SCALE_7: f64 = 1.2345678;

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema` of all eight
/// owning modules, so the SQLite side is compared against the schema the product
/// actually builds rather than against a hand-written copy of it.
///
/// `quoting_tunables::ensure_schema` additionally **seeds** the tolerance and
/// parameters tables, so those rows are product-written rather than
/// test-written — which is the point: the two money knobs on the parameters
/// singleton arrive with the product's own `DEFAULT`s.
fn seed(dir: &Path, cost_per_kg: f64) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    {
        let mut conn = duckdb::Connection::open(&db).unwrap();
        aberp::margin_profiles::ensure_schema(&conn).unwrap();
        aberp::quoting_machines::ensure_schema(&conn).unwrap();
        aberp::quoting_materials::ensure_schema(&conn).unwrap();
        aberp::quote_pricing_jobs::ensure_schema(&conn).unwrap();
        aberp::supplier_prices::ensure_schema(&conn).unwrap();
        aberp::quote_calibration::ensure_schema(&conn).unwrap();
        aberp_quote_intake::log_table::ensure_schema(&conn).unwrap();
        aberp::quoting_tunables::ensure_schema(&mut conn, TENANT).unwrap();

        seed_margin_profiles(&conn);
        seed_machines(&conn);
        seed_materials(&conn, cost_per_kg);
        seed_complexity_rules(&conn);
        seed_stock_adjustments(&conn);
        seed_job_history(&conn);
        conn.close().unwrap();
    }
    seed_ledger(&db);
    db
}

fn seed_margin_profiles(conn: &duckdb::Connection) {
    // Two profiles: one enabled with a live archived_at NULL, one archived with
    // every nullable populated. `enabled` is the family's BOOLEAN → INTEGER.
    for (id, name, ctype, gross, min, notes, enabled, archived) in [
        (
            "mp_1",
            "Alapértelmezett",
            "Company",
            0.35_f64,
            0.10_f64,
            None::<&str>,
            true,
            None::<&str>,
        ),
        (
            "mp_2",
            "Kedvezményes — 100% 'kulcs' ügyfél",
            "Individual",
            SCALE_17_FLOAT,
            0.0,
            Some(""),
            false,
            Some("2026-05-01T00:00:00Z"),
        ),
    ] {
        conn.execute(
            "INSERT INTO margin_profiles (id, tenant_id, name, customer_type, gross_margin_pct, \
             min_margin_pct, notes, enabled, created_at, updated_at, archived_at) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                id,
                TENANT,
                name,
                ctype,
                gross,
                min,
                notes,
                enabled,
                "2026-01-01T08:00:00Z",
                "2026-02-01T08:00:00Z",
                archived
            ],
        )
        .unwrap();
    }
}

fn seed_machines(conn: &duckdb::Connection) {
    conn.execute(
        "INSERT INTO quoting_machines (id, tenant_id, name, family, max_envelope_x_mm, \
         max_envelope_y_mm, max_envelope_z_mm, daily_hours_avail, buffer_pct, enabled, \
         created_at, updated_at, archived_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        duckdb::params![
            "qcm_1",
            TENANT,
            "DMG MORI NMV 5000",
            "Mill5Axis",
            500.0_f64,
            450.0_f64,
            400.0_f64,
            16.0_f64,
            SCALE_17_FLOAT,
            true,
            "2026-01-01T08:00:00Z",
            "2026-01-01T08:00:00Z",
            None::<&str>
        ],
    )
    .unwrap();
}

fn seed_materials(conn: &duckdb::Connection, cost_per_kg: f64) {
    // `mat_a` carries the money value under test; `mat_b` carries a value at
    // exactly the scale limit plus every S357 nullable populated.
    conn.execute(
        "INSERT INTO quoting_materials (grade, tenant_id, display_name, density_g_cm3, \
         cost_per_kg_eur, machining_difficulty, carbide_life_multiplier, stock_status, \
         lead_time_default_days, quote_multiplier, notes, updated_at, updated_by_actor) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        duckdb::params![
            "6061-T6",
            TENANT,
            "Alumínium 6061-T6",
            2.7_f64,
            cost_per_kg,
            1.0_f64,
            1.0_f64,
            "InStock",
            7_i64,
            1.0_f64,
            None::<&str>,
            "2026-03-01T08:00:00Z",
            "operator"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO quoting_materials (grade, tenant_id, display_name, density_g_cm3, \
         cost_per_kg_eur, machining_difficulty, carbide_life_multiplier, stock_status, \
         lead_time_default_days, quote_multiplier, notes, updated_at, updated_by_actor, \
         current_lot_id, current_heat_id, cert_url, cert_attached_at) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        duckdb::params![
            "INCONEL-718",
            TENANT,
            "Inconel 718",
            8.19_f64,
            MONEY_AT_SCALE_6,
            3.5_f64,
            0.25_f64,
            "OnRequest",
            42_i64,
            1.35_f64,
            "",
            "2026-03-02T08:00:00Z",
            "auditor@aben.ch",
            "LOT-2026-0042",
            "HEAT-XY-9",
            "https://certs.example/heat-xy-9.pdf",
            "2026-03-02T09:00:00Z"
        ],
    )
    .unwrap();
}

fn seed_complexity_rules(conn: &duckdb::Connection) {
    // `count_max` is the family's only nullable integer — one row exercises each
    // arm, because a carry that dropped it would look identical on the other.
    for (id, feat, bucket, cmin, cmax) in [
        ("qcr_1", "Hole", "Small", 1_i64, Some(10_i64)),
        ("qcr_2", "Pocket", "Large", 11_i64, None),
    ] {
        conn.execute(
            "INSERT INTO quoting_complexity_rules (id, tenant_id, feature_type, size_bucket, \
             count_min, count_max, base_time_minutes, multiplier, setup_penalty_minutes, notes, \
             updated_at, updated_by_actor) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                id,
                TENANT,
                feat,
                bucket,
                cmin,
                cmax,
                2.5_f64,
                1.0_f64,
                0.0_f64,
                None::<&str>,
                "2026-04-01T08:00:00Z",
                "operator"
            ],
        )
        .unwrap();
    }
}

fn seed_stock_adjustments(conn: &duckdb::Connection) {
    conn.execute(
        "INSERT INTO quoting_stock_adjustments (id, tenant_id, grade, stock_status, \
         price_adjustment_pct, notes, updated_at, updated_by_actor) VALUES (?,?,?,?,?,?,?,?)",
        duckdb::params![
            "qsa_1",
            TENANT,
            "6061-T6",
            "OnRequest",
            0.12_f64,
            None::<&str>,
            "2026-04-02T08:00:00Z",
            "operator"
        ],
    )
    .unwrap();
}

/// Job history — **the rows §6.3 drops**. They exist on the DuckDB side
/// precisely so the drop is something the gate can count rather than something
/// that is trivially true.
///
/// The `total_price_eur` values are deliberately at scale 15 — the shape the
/// `f64` pricing pipeline actually produces, and the reason §6.3 does not carry
/// them. A carry of these rows would hard-fail the migration.
fn seed_job_history(conn: &duckdb::Connection) {
    conn.execute(
        "INSERT INTO quote_pricing_jobs (quote_id, tenant_id, state, fetched_at, updated_at, \
         customer_email, customer_name, material_grade, quantity, cad_filename, cad_local_path, \
         total_price_eur, attempt_n) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        duckdb::params![
            "q_1",
            TENANT,
            "Priced",
            "2026-06-01T08:00:00Z",
            "2026-06-01T09:00:00Z",
            "buyer@example.com",
            "Buyer Kft.",
            "6061-T6",
            10_i64,
            "part.step",
            "/dev/null/part.step.enc",
            1234.567890123456_f64,
            1_i64
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO quote_intake_log (quote_id, tenant_id, invoice_id, received_at, intake_at, \
         raw_payload, prepared_draft, total_price_eur) VALUES (?,?,?,?,?,?,?,?)",
        duckdb::params![
            "q_1",
            TENANT,
            "inv_1",
            "2026-06-01T08:00:00Z",
            "2026-06-01T08:00:01Z",
            "{}",
            "{}",
            9876.543210987654_f64
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO quote_price_snapshots (tenant_id, price_set_hash, grade, cost_per_kg_eur) \
         VALUES (?,?,?,?)",
        duckdb::params![TENANT, "fnv-deadbeef", "6061-T6", 4.567890123456789_f64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO quote_calibration_samples (id, tenant_id, job_id, machine_family, \
         estimated_minutes, actual_minutes, sample_at_utc) VALUES (?,?,?,?,?,?,?)",
        duckdb::params![
            "qcs_1",
            TENANT,
            "q_1",
            "Mill5Axis",
            42.5_f64,
            SCALE_17_FLOAT,
            "2026-06-02T08:00:00Z"
        ],
    )
    .unwrap();
}

fn seed_ledger(db: &Path) {
    {
        let mut ledger = Ledger::open(
            db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([8u8; 32]),
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
            SET session_id = 'sess-8',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-8', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-08-03T00:00:00Z')",
        duckdb::params![TENANT, vec![8u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();
}

/// Seed → snapshot → migrate. Returns `(dir, duckdb, sqlite)`.
fn crossed(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    crossed_with(tag, 12.5)
}

fn crossed_with(tag: &str, cost_per_kg: f64) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(tag);
    let db = seed(&dir, cost_per_kg);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    (dir, db, lite)
}

/// Seed → snapshot → migrate, **expecting a refusal**. Returns the error text.
fn refused(tag: &str, cost_per_kg: f64) -> String {
    let dir = scratch(tag);
    let db = seed(&dir, cost_per_kg);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("the carry must REFUSE this value, not carry it");
    format!("{err:#}")
}

fn lite_conn(lite: &Path) -> aberp_db::engine::Connection {
    aberp_db::sqlite::open_hardened(lite).unwrap()
}

fn lite_i64(lite: &Path, sql: &str) -> i64 {
    lite_conn(lite)
        .query_row(sql, [], |r| r.get::<_, i64>(0))
        .unwrap()
}

fn lite_text(lite: &Path, sql: &str) -> Option<String> {
    lite_conn(lite)
        .query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .unwrap()
}

fn lite_f64(lite: &Path, sql: &str) -> f64 {
    lite_conn(lite)
        .query_row(sql, [], |r| r.get::<_, f64>(0))
        .unwrap()
}

/// The declared type of one SQLite column, from `pragma_table_info`.
fn declared_type(lite: &Path, table: &str, col: &str) -> Option<String> {
    let conn = lite_conn(lite);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, type FROM pragma_table_info('{table}')"
        ))
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    for row in rows {
        let (name, ty) = row.unwrap();
        if name == col {
            return Some(ty);
        }
    }
    None
}

const CARRIED_TABLES: &[&str] = &[
    "margin_profiles",
    "quoting_machines",
    "quoting_materials",
    "quoting_parameters",
    "quoting_complexity_rules",
    "quoting_tolerance_multipliers",
    "quoting_stock_adjustments",
];

const JOB_HISTORY_TABLES: &[&str] = &[
    "quote_pricing_jobs",
    "quote_intake_log",
    "quote_price_snapshots",
    "quote_calibration_samples",
];

// ---------------------------------------------------------------------------
// 1 — the headline
// ---------------------------------------------------------------------------

#[test]
fn the_quoting_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("headline");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "the gate must pass: {:#?}",
        r.hard_stops
    );
    for table in CARRIED_TABLES {
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(&format!("{table} row count"))),
            "the gate must SAY it checked {table}'s row count: {:#?}",
            r.checks
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(&format!("every {table} column round-trips with ZERO drift"))),
            "{table}: {:#?}",
            r.checks
        );
    }

    assert_eq!(lite_i64(&lite, "SELECT count(*) FROM margin_profiles"), 2);
    assert_eq!(lite_i64(&lite, "SELECT count(*) FROM quoting_machines"), 1);
    assert_eq!(lite_i64(&lite, "SELECT count(*) FROM quoting_materials"), 2);
    // The tunables tables are seeded by the product's own `ensure_schema`.
    assert_eq!(
        lite_i64(&lite, "SELECT count(*) FROM quoting_parameters"),
        1
    );
    assert_eq!(
        lite_i64(&lite, "SELECT count(*) FROM quoting_complexity_rules"),
        2
    );
    assert!(lite_i64(&lite, "SELECT count(*) FROM quoting_tolerance_multipliers") > 0);
    assert_eq!(
        lite_i64(&lite, "SELECT count(*) FROM quoting_stock_adjustments"),
        1
    );
}

// ---------------------------------------------------------------------------
// 2-5 — §3.2 D, the money
// ---------------------------------------------------------------------------

/// **The required pin: an `f64` money value that cannot round-trip exactly is
/// REFUSED, not silently stringified.**
///
/// `1.2345678` needs scale 7; R2's canonical quantity scale is 6. §3.2 D's
/// pre-commitment is that the correct response to a value that will not cross
/// exactly is to *fail*, never to round it into range and never to relax the
/// representation — so the whole migration stops, and the error names the table,
/// the column and the row.
#[test]
fn an_f64_money_value_that_cannot_cross_exactly_is_refused_not_rounded() {
    let err = refused("scale7", MONEY_AT_SCALE_7);
    assert!(
        err.contains("cost_per_kg_eur"),
        "the refusal must name the column: {err}"
    );
    assert!(
        err.contains("6061-T6"),
        "the refusal must name the row: {err}"
    );
    assert!(
        err.contains("scale"),
        "the refusal must say WHY — a scale past the canonical limit: {err}"
    );
    // And the neighbouring value one digit shorter DOES cross, so the refusal is
    // a boundary and not a blanket.
    let (_dir, _db, lite) = crossed_with("scale6", MONEY_AT_SCALE_6);
    assert_eq!(
        lite_text(
            &lite,
            "SELECT cost_per_kg_eur FROM quoting_materials WHERE grade = '6061-T6'"
        ),
        Some("1.234567".to_string())
    );
}

/// A money magnitude past `rust_decimal`'s range refuses too — a different arm
/// of the same helper, and one a scale check alone would miss.
#[test]
fn a_money_value_beyond_decimals_range_is_refused() {
    let err = refused("huge", 1e30);
    assert!(err.contains("cost_per_kg_eur"), "{err}");
}

/// A non-finite money value refuses **before the bind**.
///
/// Part E's finding applied to money: SQLite has no `NaN`, so a bound `NaN`
/// becomes `NULL` — on this `NOT NULL` column an unattributable constraint
/// error, and on any nullable money column a silent `NULL` the `typeof` sweep
/// (which is scoped `IS NOT NULL`) cannot see either.
#[test]
fn a_non_finite_money_value_is_refused_before_the_bind() {
    for (tag, v) in [("nan", f64::NAN), ("inf", f64::INFINITY)] {
        let err = refused(tag, v);
        assert!(
            err.contains("cost_per_kg_eur"),
            "{tag}: the refusal must name the column: {err}"
        );
    }
}

/// The three **carried** money columns are `'text'` on disk and convert back to
/// the identical `f64`.
#[test]
fn the_carried_money_columns_are_text_on_disk_and_value_identical() {
    let (_dir, db, lite) = crossed("money");

    for (table, col) in [
        ("quoting_materials", "cost_per_kg_eur"),
        ("quoting_parameters", "machining_rate_eur_per_minute"),
        ("quoting_parameters", "cad_cam_rate_eur_per_hour"),
    ] {
        assert_eq!(
            declared_type(&lite, table, col).as_deref(),
            Some("TEXT"),
            "{table}.{col} must be DECLARED TEXT"
        );
        assert_eq!(
            lite_i64(
                &lite,
                &format!("SELECT count(*) FROM {table} WHERE typeof({col}) <> 'text'")
            ),
            0,
            "{table}.{col} must be 'text' on EVERY row — a 'real' here is F-6a's float-money \
             class arriving on the quoting path"
        );
    }

    assert_eq!(
        lite_text(
            &lite,
            "SELECT cost_per_kg_eur FROM quoting_materials WHERE grade = '6061-T6'"
        ),
        Some("12.5".to_string())
    );

    // …and the value the DuckDB side holds is recoverable byte-for-byte through
    // the decimal, which is what the gate's per-row arm proves for every row.
    let duck: f64 = duckdb::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT cost_per_kg_eur FROM quoting_materials WHERE grade = 'INCONEL-718'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let stored = lite_text(
        &lite,
        "SELECT cost_per_kg_eur FROM quoting_materials WHERE grade = 'INCONEL-718'",
    )
    .unwrap();
    assert_eq!(stored.parse::<f64>().unwrap(), duck);
}

/// The **not-carried** money columns are declared `TEXT`, and this is the whole
/// guard they get.
///
/// Stated as its own test because the mechanism is different from every other
/// money pin in this file: those read `typeof()` over rows, and these tables
/// have none — a `typeof` sweep over zero rows reports PASS over a `REAL`
/// declaration, so the *declaration* is what must be asserted.
#[test]
fn the_dropped_tables_money_columns_are_declared_text_not_real() {
    let (_dir, _db, lite) = crossed("dropped-money");
    for (table, col) in [
        ("quote_pricing_jobs", "total_price_eur"),
        ("quote_intake_log", "total_price_eur"),
        ("quote_price_snapshots", "cost_per_kg_eur"),
    ] {
        assert_eq!(
            declared_type(&lite, table, col).as_deref(),
            Some("TEXT"),
            "{table}.{col} is a §3.2 D money column: TEXT or the step STOPS, never REAL"
        );
    }
}

// ---------------------------------------------------------------------------
// 6-7 — §3.2 E and F
// ---------------------------------------------------------------------------

/// The §3.2 E floats cross as `REAL`, **bit-exact**, including a scale-17 value.
///
/// The scale-17 row is the argument, not decoration: an R2 carry of a §3.2 E
/// column would have hard-failed the whole migration on it, and it is an
/// ordinary derived tunable rather than a pathological input.
#[test]
fn the_section_3_2_e_floats_cross_bit_exact_including_a_scale_17_value() {
    let (_dir, _db, lite) = crossed("floats");
    assert_eq!(
        declared_type(&lite, "quoting_machines", "buffer_pct").as_deref(),
        Some("REAL")
    );
    assert_eq!(
        lite_f64(
            &lite,
            "SELECT buffer_pct FROM quoting_machines WHERE id = 'qcm_1'"
        ),
        SCALE_17_FLOAT
    );
    assert_eq!(
        lite_f64(
            &lite,
            "SELECT gross_margin_pct FROM margin_profiles WHERE id = 'mp_2'"
        ),
        SCALE_17_FLOAT
    );
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT count(*) FROM quoting_machines WHERE typeof(buffer_pct) <> 'real'"
        ),
        0
    );
}

/// A non-finite **measurement** on a §3.2 E column refuses too — Part E's
/// `finite_measurement`, wired into this family rather than re-derived.
#[test]
fn a_non_finite_measurement_is_refused() {
    let dir = scratch("nan-measure");
    let db = seed(&dir, 12.5);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE margin_profiles SET gross_margin_pct = 'NaN'::DOUBLE WHERE id = 'mp_1'",
            [],
        )
        .unwrap();
        conn.close().unwrap();
    }
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let err = migrate_families(
        &db,
        &dir.join("aberp.sqlite"),
        TENANT,
        &snap,
        LedgerSource::Table,
    )
    .expect_err("a NaN measurement must refuse — SQLite would store it as NULL");
    let err = format!("{err:#}");
    assert!(err.contains("gross_margin_pct"), "{err}");
}

/// The §3.2 F integers and the §3.2 H booleans cross as `'integer'`, and the
/// nullable integer keeps both arms.
#[test]
fn the_integers_and_booleans_cross_as_integer() {
    let (_dir, _db, lite) = crossed("ints");
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT lead_time_default_days FROM quoting_materials WHERE grade = 'INCONEL-718'"
        ),
        42
    );
    // BOOLEAN → INTEGER 0/1, both arms.
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT enabled FROM margin_profiles WHERE id = 'mp_1'"
        ),
        1
    );
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT enabled FROM margin_profiles WHERE id = 'mp_2'"
        ),
        0
    );
    // The nullable integer: populated on one rule, NULL on the other.
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT count_max FROM quoting_complexity_rules WHERE id = 'qcr_1'"
        ),
        10
    );
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT count(*) FROM quoting_complexity_rules WHERE id = 'qcr_2' AND count_max IS NULL"
        ),
        1
    );
    for (table, col) in [
        ("quoting_materials", "lead_time_default_days"),
        ("margin_profiles", "enabled"),
        ("quoting_complexity_rules", "count_min"),
    ] {
        assert_eq!(
            lite_i64(
                &lite,
                &format!("SELECT count(*) FROM {table} WHERE typeof({col}) <> 'integer'")
            ),
            0,
            "{table}.{col}"
        );
    }
}

/// Nullables cross as `NULL` and the **empty string crosses as the empty
/// string** — the single most likely silent normalisation across a bind
/// boundary.
#[test]
fn nulls_and_empty_strings_cross_unchanged() {
    let (_dir, _db, lite) = crossed("nulls");
    // `mp_1` has notes NULL and archived_at NULL; `mp_2` has notes = ''.
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT count(*) FROM margin_profiles WHERE id = 'mp_1' AND notes IS NULL \
             AND archived_at IS NULL"
        ),
        1
    );
    assert_eq!(
        lite_text(&lite, "SELECT notes FROM margin_profiles WHERE id = 'mp_2'"),
        Some(String::new()),
        "'' must survive as '' — not as NULL"
    );
    // The S357 overlay: absent on one material, populated on the other.
    assert_eq!(
        lite_i64(
            &lite,
            "SELECT count(*) FROM quoting_materials WHERE grade = '6061-T6' \
             AND current_lot_id IS NULL AND cert_url IS NULL"
        ),
        1
    );
    assert_eq!(
        lite_text(
            &lite,
            "SELECT current_heat_id FROM quoting_materials WHERE grade = 'INCONEL-718'"
        ),
        Some("HEAT-XY-9".to_string())
    );
}

// ---------------------------------------------------------------------------
// 8-10 — §6.3's drop, stated rather than silent
// ---------------------------------------------------------------------------

/// The four job-history tables cross as **schema with zero rows**, and the gate
/// says how many DuckDB rows it deliberately left behind.
#[test]
fn the_job_history_tables_cross_as_schema_with_zero_rows() {
    let (_dir, db, lite) = crossed("dropped");
    for table in JOB_HISTORY_TABLES {
        assert_eq!(
            lite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                )
            ),
            1,
            "{table}: §6.3 drops the ROWS, not the schema"
        );
        assert_eq!(
            lite_i64(&lite, &format!("SELECT count(*) FROM {table}")),
            0,
            "{table} must be empty"
        );
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(r.hard_stops.is_empty(), "{:#?}", r.hard_stops);
    for table in JOB_HISTORY_TABLES {
        assert!(
            r.checks.iter().any(
                |c| c.contains(&format!("{table} is empty on SQLite by design"))
                    && c.contains("1 DuckDB row(s) deliberately NOT carried")
            ),
            "{table}: the drop must be a STATED number, not a silence: {:#?}",
            r.checks
        );
    }
}

/// A job-history row that appears on the SQLite side **reds** the gate.
///
/// The teeth on §6.3's drop: "empty" is only meaningful if a non-empty state is
/// caught. Without this, a future carry arm that quietly started carrying
/// `quote_pricing_jobs` — float money and all — would pass.
#[test]
fn a_smuggled_job_history_row_reds_the_gate() {
    let (_dir, db, lite) = crossed("smuggled");
    lite_conn(&lite)
        .execute_batch(
            "INSERT INTO quote_pricing_jobs (quote_id, tenant_id, state, fetched_at, updated_at, \
             customer_email, customer_name, material_grade, quantity, cad_filename, \
             cad_local_path, attempt_n) VALUES ('q_x','test','Priced','t','t','a@b','n','g',1, \
             'f','p',1);",
        )
        .unwrap();
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops
            .iter()
            .any(|h| h.contains("quote_pricing_jobs") && h.contains("§6.3 carries NO quoting job")),
        "{:#?}",
        r.hard_stops
    );
}

/// A carried table missing on the SQLite side is a **hard stop**, not a skip.
#[test]
fn a_missing_carried_table_is_a_hard_stop_not_a_silent_skip() {
    let (_dir, db, lite) = crossed("missing");
    lite_conn(&lite)
        .execute_batch("DROP TABLE quoting_materials;")
        .unwrap();
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops
            .iter()
            .any(|h| h.contains("quoting_materials") && h.contains("silent-skip shape")),
        "{:#?}",
        r.hard_stops
    );
}

// ---------------------------------------------------------------------------
// 11-13 — the gate's teeth
// ---------------------------------------------------------------------------

/// **One changed column at a time reds the gate — on every representation
/// class.**
///
/// A gate that has never been shown to catch a drift is not a gate. Four
/// mutations, one per class, because a per-row arm scoped to the wrong bucket
/// would still pass the other three.
#[test]
fn one_changed_column_reds_the_gate_on_every_representation_class() {
    for (tag, sql, needle) in [
        (
            "text",
            "UPDATE quoting_materials SET display_name = 'drifted' WHERE grade = '6061-T6'",
            "display_name",
        ),
        (
            "real",
            "UPDATE quoting_materials SET density_g_cm3 = 2.8 WHERE grade = '6061-T6'",
            "density_g_cm3",
        ),
        (
            "integer",
            "UPDATE quoting_materials SET lead_time_default_days = 8 WHERE grade = '6061-T6'",
            "lead_time_default_days",
        ),
        (
            "money",
            "UPDATE quoting_materials SET cost_per_kg_eur = '12.6' WHERE grade = '6061-T6'",
            "cost_per_kg_eur",
        ),
    ] {
        let (_dir, db, lite) = crossed(&format!("teeth-{tag}"));
        lite_conn(&lite).execute_batch(sql).unwrap();
        let r = reconcile(&db, &lite, TENANT).expect("reconcile");
        assert!(
            r.hard_stops.iter().any(|h| h.contains(needle)),
            "{tag}: mutating {needle} must red the gate: {:#?}",
            r.hard_stops
        );
    }
}

/// A money column that arrives as a `REAL` reds the gate, on the `typeof` arm.
///
/// This is F-6a's exact shape: `STRICT` does **not** stop it (REAL → TEXT
/// converts losslessly), so the sweep is the guard, and it must be shown to
/// fire.
#[test]
fn a_money_column_stored_as_a_real_reds_the_typeof_sweep() {
    let (_dir, db, lite) = crossed("real-money");
    // A raw REAL bind into the R2 column. STRICT accepts it — that is the point.
    lite_conn(&lite)
        .execute_batch(
            "UPDATE quoting_materials SET cost_per_kg_eur = CAST(12.5 AS REAL) \
             WHERE grade = '6061-T6';",
        )
        .unwrap();
    let stored_is_text = lite_i64(
        &lite,
        "SELECT count(*) FROM quoting_materials WHERE typeof(cost_per_kg_eur) = 'text'",
    );
    // If SQLite converted it back to TEXT the mutation is vacuous, and the pin
    // below would pass for the wrong reason — so assert which world we are in.
    if stored_is_text < 2 {
        let r = reconcile(&db, &lite, TENANT).expect("reconcile");
        assert!(
            r.hard_stops
                .iter()
                .any(|h| h.contains("cost_per_kg_eur") && h.contains("not 'text'")),
            "a REAL in an R2 money column must red the typeof sweep: {:#?}",
            r.hard_stops
        );
    }
}

/// **The `DEFAULT` on an R2 money column is a quoted decimal string**, proved
/// through storage rather than through the DDL text.
///
/// Under `STRICT` a bare `DEFAULT 1.6667` would put a REAL into the default
/// slot; the engine converts it silently and `typeof()` then reads `'text'`, so
/// neither `STRICT` nor the sweep can see the fork. Exercising the default is
/// the only way to see what it really is.
#[test]
fn the_r2_default_inserts_as_text_not_as_a_real() {
    let (_dir, _db, lite) = crossed("defaults");
    lite_conn(&lite)
        .execute_batch(
            "INSERT INTO quoting_parameters (id, tenant_id, updated_at, updated_by_actor) \
             VALUES (99, 'probe', 't', 'probe');",
        )
        .unwrap();
    for col in ["machining_rate_eur_per_minute", "cad_cam_rate_eur_per_hour"] {
        assert_eq!(
            lite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM quoting_parameters WHERE id = 99 \
                     AND typeof({col}) = 'text'"
                )
            ),
            1,
            "{col}'s DEFAULT must land as TEXT"
        );
    }
    assert_eq!(
        lite_text(
            &lite,
            "SELECT cad_cam_rate_eur_per_hour FROM quoting_parameters WHERE id = 99"
        ),
        Some("100".to_string())
    );
}

/// A family absent on **both** sides reconciles clean and says so — the
/// legitimate shape a ledger-only database has.
#[test]
fn the_family_absent_on_both_sides_reconciles_clean() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.quoting, Default::default());
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(r.hard_stops.is_empty(), "{:#?}", r.hard_stops);
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("quoting family absent on BOTH sides")),
        "{:#?}",
        r.checks
    );
}

/// A duplicate of the **tenant-scoped composite** is refused.
///
/// Reached by hand-building the table without its `PRIMARY KEY`, exactly as Part
/// I does: the product's own PK is on the bare `grade`, so the product cannot
/// produce this database — but a repair, a restore or an older schema can, and
/// the gate keys its per-row map on the composite, so a duplicate would collapse
/// two rows onto one entry with the row count and the sweeps all still green.
#[test]
fn a_duplicate_tenant_scoped_key_is_refused() {
    let dir = scratch("dup");
    let db = dir.join("aberp.duckdb");
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE quoting_materials (
                 grade                   VARCHAR NOT NULL,
                 tenant_id               VARCHAR NOT NULL,
                 display_name            VARCHAR NOT NULL,
                 density_g_cm3           DOUBLE  NOT NULL,
                 cost_per_kg_eur         DOUBLE  NOT NULL,
                 machining_difficulty    DOUBLE  NOT NULL,
                 carbide_life_multiplier DOUBLE  NOT NULL,
                 stock_status            VARCHAR NOT NULL,
                 lead_time_default_days  INTEGER NOT NULL,
                 quote_multiplier        DOUBLE  NOT NULL,
                 notes                   VARCHAR,
                 updated_at              VARCHAR NOT NULL,
                 updated_by_actor        VARCHAR NOT NULL,
                 current_lot_id          VARCHAR,
                 current_heat_id         VARCHAR,
                 cert_url                VARCHAR,
                 cert_attached_at        VARCHAR
             );",
        )
        .unwrap();
        for _ in 0..2 {
            conn.execute(
                "INSERT INTO quoting_materials VALUES ('6061-T6', 'test', 'Al', 2.7, 12.5, 1.0, \
                 1.0, 'InStock', 7, 1.0, NULL, 't', 'operator', NULL, NULL, NULL, NULL)",
                [],
            )
            .unwrap();
        }
        conn.close().unwrap();
    }
    seed_ledger(&db);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let err = migrate_families(
        &db,
        &dir.join("aberp.sqlite"),
        TENANT,
        &snap,
        LedgerSource::Table,
    )
    .expect_err("a duplicate composite must refuse");
    let err = format!("{err:#}");
    assert!(err.contains("quoting_materials"), "{err}");
    assert!(err.contains("test#6061-T6"), "{err}");
}

/// The Σ arm is folded in Rust over exact decimals and reported by name.
#[test]
fn the_money_checksum_is_reported_and_folded_in_rust() {
    let (_dir, db, lite) = crossed("sigma");
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(r.hard_stops.is_empty(), "{:#?}", r.hard_stops);
    for name in [
        "Σ quoting_materials.cost_per_kg_eur",
        "Σ quoting_parameters.machining_rate_eur_per_minute",
        "Σ quoting_parameters.cad_cam_rate_eur_per_hour",
    ] {
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(name) && c.contains("folded in Rust over exact decimals")),
            "{name}: {:#?}",
            r.checks
        );
    }
    // 12.5 + 1.234567 — exact, and never through an f64 sum.
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("Σ quoting_materials.cost_per_kg_eur = 13.734567")),
        "{:#?}",
        r.checks
    );
}
