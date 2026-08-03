//! ADR-0108 Step 7 Part E — **the QA / QC family crosses.**
//!
//! (Ervin's brief called this "Part B"; B is partners, C is products/inventory
//! and D is work-orders/BOM, all three already landed on this branch, so it is
//! **Part E**. Named here rather than silently renumbered — the same call Part D
//! made.)
//!
//! # The family
//!
//! Six tables, **two owning modules**, three construction paths, 84 columns:
//!
//! | table | built by | columns | the numbers in it |
//! |---|---|---|---|
//! | `ncrs` | `aberp::quality::ensure_schema` | 14 | none |
//! | `ncr_transitions` | `aberp::quality::ensure_schema` | 8 | `seq` (§3.2 F) |
//! | `capas` | `aberp::quality::ensure_schema` | 15 | none |
//! | `qa_inspections` | `aberp_qa::repository::ensure_schema` (V001) | 12 | none |
//! | `qc_inspection_plans` | `aberp_qa::qc::ensure_qc_schema` (V002) | 12 | 3 × `REAL` (§3.2 E), 1 × `BOOLEAN` |
//! | `qc_inspections` | `aberp_qa::qc::ensure_qc_schema` (V002) | 23 | 5 × `REAL` (§3.2 E), 1 × `BOOLEAN` |
//!
//! **This family has no money in it at all, and no quantity either.** §3.2's A,
//! B, C, D and G categories are all empty here: grepped, not recalled. Its only
//! numbers are one sequence integer (§3.2 F), eight dimensional measurements
//! (§3.2 E) and two booleans (§3.2 H). R1 and R2 have nothing to bite on, and
//! saying so is worth more than a hedge — it is why the gate's arm is **per
//! column over all 84** rather than money-first. On a table with no money the
//! regulatory payload is the NCR/CAPA record itself: ISO-9001 traceability,
//! whose value is that it is complete and unaltered.
//!
//! # The eight floats stay `REAL`, and this is §3.2 E's call, not a fresh one
//!
//! `qc_inspection_plans.{nominal_value, upper_tol, lower_tol}` and
//! `qc_inspections.{nominal_value, upper_tol, lower_tol, actual_value,
//! deviation}` are listed **by name** in §3.2 E among the non-money floats that
//! cross `DOUBLE` → `REAL` unchanged, and §3.2 E's note (b) already took the
//! call for `deviation` specifically: *"a derived float on a
//! dimensional-inspection record used for a pass/fail verdict. It is not money.
//! Keeping it `REAL` is the conservative no-change call, flagged."* It is
//! followed here and flagged again rather than re-litigated.
//!
//! **Part C moved five `DOUBLE`s out of category E; these eight do not follow
//! them, and Part D's distinguishing test is what decides it.** Part C's five
//! moved because they were one half of a *measured rule-7 divergence* — the same
//! physical quantity was `DECIMAL` in `stock_movements` and `DOUBLE` in
//! `inventory_balances`, so carrying both as-is would have blessed the fork.
//! These eight have no exact counterpart anywhere: a dimensional measurement in
//! `units` is stored in exactly one place in this product, and nothing else in
//! the tree models it as `Decimal`. **There is no fork to close.**
//!
//! And, as with `actual_machining_minutes`, the risk runs the other way — only
//! more sharply here, because `deviation` is *derived by subtraction*:
//! `qc::verdict` computes `deviation = actual - plan.nominal_value` in `f64`
//! (`crates/aberp-qa/src/qc/verdict.rs:103`). An ordinary pair like `0.3` and
//! `0.1` yields `0.19999999999999998` — scale 17. R2 refuses anything above the
//! canonical quantity scale of 6, so carrying these as R2 would **hard-fail the
//! whole migration on an ordinary inspection row**, refusing a value that
//! carries no exactness requirement, on a column that is not money and never
//! reaches a filing. `DOUBLE` → `REAL` is bit-exact and adds nothing new. The
//! gate proves it per row.
//!
//! # The one thing this family adds that no prior family had: a refusal on the float
//!
//! Part D compared its single float bit-for-bit at the gate and stopped there,
//! relying on `NaN != NaN` to hard-stop. That is not sufficient here, and the
//! reason is a storage-class behaviour rather than a preference: **SQLite has no
//! `NaN`.** A bound `f64::NAN` is stored as `NULL`. All eight columns are
//! `NOT NULL`, so a `NaN` would surface as a bare `NOT NULL constraint failed`
//! naming neither the row nor why — and on any future nullable measurement
//! column it would cross as a **silent `NULL`**, which is the 14%-silently-
//! skipped shape rule 11 exists to stop.
//!
//! So [`finite_measurement`] refuses a non-finite measurement **before** the
//! bind, naming the table, the column and the row — the same shape
//! [`canonical_decimal`](crate::migrate_billing::canonical_decimal) has for R2.
//! It is the family's refusal arm, and the family test requires it to fire.
//!
//! # Booleans — the first family to carry one
//!
//! `qc_inspection_plans.enabled` and `qc_inspections.calibration_stale_at_event`
//! are `BOOLEAN` on DuckDB. §3.2 H: `BOOLEAN` → `INTEGER`, because `rusqlite`
//! binds `bool` ↔ `INTEGER` 0/1 natively and `STRICT` has no boolean type. They
//! are read as `bool` and bound as `bool` on both sides, so the carry never sees
//! the integer at all; the gate's `typeof` sweep asserts `'integer'`, which is
//! what proves the mapping actually happened rather than a `'text'` `"true"`
//! sneaking in.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5, 7B, 7C and 7D, and Ervin's same constraint: **the
//! migrator half only.** DuckDB remains the source of truth.
//! `apps/aberp/src/quality.rs` and `crates/aberp-qa/src` are untouched — every
//! one of their queries still runs against a DuckDB `Connection`, and the two
//! `migrations/V00*.sql` files still build the DuckDB schema.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
//! arithmetic sites contain no QA/QC statement, §5's twelve mitigations name
//! none, and §6.3's row for this family says "row-by-row carry" with no rebuild.
//! The T-8 pending-fold register is unchanged by this commit — nothing joined it
//! and nothing left it. **Nor does this family carry an M11-shaped hazard**:
//! measured, not assumed — `LOWER(` and `LIKE` both return **zero** hits against
//! all six tables across `apps/aberp/src` and `crates/aberp-qa/src`, so the
//! ASCII-`LOWER()` fold that partners must fix at its cutover has no site here.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use aberp_db::engine::{Connection as SqliteConn, ToSql};

use crate::migrate_billing::fold_i64;
use crate::migrate_dispatch::unique_natural_keys;

// ---------------------------------------------------------------------------
// The STRICT DDL
// ---------------------------------------------------------------------------

/// The three NCR/CAPA tables as `aberp::quality::ensure_schema` creates them,
/// `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H) throughout; `ncr_transitions.seq` is §3.2 F's
/// `INTEGER` → `INTEGER`. The array-valued columns (`affected_part_uids`,
/// `affected_wo_ids`, `affected_heat_lots`, `photos`) are **JSON text** on
/// DuckDB — `quality.rs`'s own comment says so — so they are `TEXT` here and not
/// `BLOB`: R3 covers the four `audit_ledger` columns that are bound as
/// `Vec<u8>`, and binding a JSON string as bytes would be the storage-class fork
/// R3 exists to stop, applied in the wrong direction.
///
/// **No `PRIMARY KEY` and no index**, matching the DuckDB schema verbatim.
/// `quality.rs:256` states the posture: natural prefixed-ULID keys, no
/// surrogate id, no `CHECK`, no `DEFAULT`, no index (S341/S410 — scan and filter
/// in Rust). ADR-0019 keeps uniqueness in Rust; adding a `PRIMARY KEY` here to
/// make the migrator's life easier would be an invariant moving into the DDL.
///
/// ⚠ **And keeping uniqueness in Rust means the MIGRATOR has to keep it too —
/// added 2026-08-02 (B2).** `ncrs` and `capas` have no `PRIMARY KEY`, no
/// `UNIQUE` and no index **on either engine**, and the reconciliation arm keys
/// its per-row `BTreeMap` on column 0 (`ncr_id` / `capa_id`). So a duplicate
/// key in the DuckDB source carries as two rows, collapses onto one map entry,
/// and the gate compares one row twice while never comparing the other — with
/// the row count, the `typeof` sweep and the per-row arm all green. That is the
/// exact shape Part F built
/// [`unique_natural_keys`](crate::migrate_dispatch::unique_natural_keys) for,
/// and it is now called on all three keyless tables in this family (`ncrs`,
/// `capas`, `ncr_transitions`) before anything is written.
///
/// The other three (`qa_inspections`, `qc_inspection_plans`, `qc_inspections`)
/// need no such call and that is measured, not assumed: each declares a
/// single-column `PRIMARY KEY` below, the map is built from the **SQLite** side
/// where that key holds, and the carry's plain `INSERT INTO` would abort on a
/// duplicate rather than admit one. A guard there would be asserting a
/// constraint the engine already enforces.
const SQLITE_QUALITY_DDL: &str = "
CREATE TABLE IF NOT EXISTS ncrs (
    ncr_id                 TEXT NOT NULL,
    tenant_id              TEXT NOT NULL,
    discovered_at_utc      TEXT NOT NULL,
    discovered_by_operator TEXT NOT NULL,
    severity               TEXT NOT NULL,
    category               TEXT NOT NULL,
    description            TEXT NOT NULL,
    affected_part_uids     TEXT NOT NULL,
    affected_wo_ids        TEXT NOT NULL,
    affected_heat_lots     TEXT NOT NULL,
    photos                 TEXT NOT NULL,
    state                  TEXT NOT NULL,
    closed_at_utc          TEXT,
    closed_by_operator     TEXT
) STRICT;
";

/// `ncr_transitions` — the per-NCR state-machine history.
///
/// Its key is **composite** (`ncr_id`, `seq`) and there is no single-column PK,
/// which is why this migrator keys its per-row comparison on `ncr_id#seq` the
/// way Step 5 keys `invoice_line` on `invoice_id#ordinal`.
const SQLITE_NCR_TRANSITIONS_DDL: &str = "
CREATE TABLE IF NOT EXISTS ncr_transitions (
    tenant_id  TEXT    NOT NULL,
    ncr_id     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    from_state TEXT    NOT NULL,
    to_state   TEXT    NOT NULL,
    operator   TEXT    NOT NULL,
    at_utc     TEXT    NOT NULL,
    note       TEXT    NOT NULL
) STRICT;
";

/// `capas` — the corrective/preventive action record hanging off an NCR.
const SQLITE_CAPAS_DDL: &str = "
CREATE TABLE IF NOT EXISTS capas (
    capa_id                     TEXT NOT NULL,
    ncr_id                      TEXT NOT NULL,
    tenant_id                   TEXT NOT NULL,
    corrective_action_text      TEXT NOT NULL,
    preventive_action_text      TEXT NOT NULL,
    responsible_operator        TEXT NOT NULL,
    target_close_date           TEXT NOT NULL,
    actual_close_date           TEXT,
    effectiveness_review_at_utc TEXT,
    effectiveness_verdict       TEXT NOT NULL,
    effectiveness_comment       TEXT,
    approved_by_operator        TEXT,
    approved_at_utc             TEXT,
    created_at_utc              TEXT NOT NULL,
    created_by_operator         TEXT NOT NULL
) STRICT;
";

/// `qa_inspections` as V001 creates it, `STRICT`. Both of V001's indexes carry
/// across; both key on base columns.
///
/// S233 / ADR-0063's `[[no-sql-specific]]` posture is preserved verbatim: there
/// is **no** DB-level `CHECK` on `state`. The closed vocabulary lives in
/// `aberp_qa::state::next_qa_state`; `STRICT` constrains storage classes, not
/// vocabularies. The `superseded_by IS NULL` predicate stays a query-time
/// filter — V001's own comment rules out a partial index, and porting one here
/// would fork the two schemas.
///
/// `measurement` is free-form operator text (`VARCHAR`), **not** a number. It is
/// `TEXT` for that reason and not by default, and it is the one column in this
/// family a reader might mistake for a §3.2 quantity.
const SQLITE_QA_INSPECTIONS_DDL: &str = "
CREATE TABLE IF NOT EXISTS qa_inspections (
    qa_id           TEXT NOT NULL PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    wo_id           TEXT NOT NULL,
    routing_op_id   TEXT NOT NULL,
    state           TEXT NOT NULL,
    decided_at      TEXT,
    decided_by      TEXT,
    reason          TEXT,
    measurement     TEXT,
    source_event_id TEXT,
    created_at      TEXT NOT NULL,
    superseded_by   TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS qa_inspections_tenant_state_created_idx
    ON qa_inspections (tenant_id, state, created_at);
CREATE INDEX IF NOT EXISTS qa_inspections_tenant_wo_routing_idx
    ON qa_inspections (tenant_id, wo_id, routing_op_id);
";

/// `qc_inspection_plans` as V002 creates it, `STRICT`.
///
/// **`nominal_value` / `upper_tol` / `lower_tol` are `REAL`** — §3.2 E, `DOUBLE`
/// → `REAL` unchanged. See the module docs for why they do not follow Part C's
/// five. `enabled` is §3.2 H's `BOOLEAN` → `INTEGER`.
const SQLITE_QC_PLANS_DDL: &str = "
CREATE TABLE IF NOT EXISTS qc_inspection_plans (
    plan_id                 TEXT    NOT NULL PRIMARY KEY,
    tenant_id               TEXT    NOT NULL,
    product_id              TEXT    NOT NULL,
    feature_name            TEXT    NOT NULL,
    nominal_value           REAL    NOT NULL,
    upper_tol               REAL    NOT NULL,
    lower_tol               REAL    NOT NULL,
    units                   TEXT    NOT NULL,
    optional_probe_cycle_id TEXT,
    enabled                 INTEGER NOT NULL,
    created_at              TEXT    NOT NULL,
    archived_at             TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS qc_inspection_plans_tenant_product_idx
    ON qc_inspection_plans (tenant_id, product_id);
";

/// `qc_inspections` as V002 creates it, `STRICT`.
///
/// Five `REAL`s (§3.2 E) and one `INTEGER` boolean (§3.2 H). The plan's
/// nominal/tolerance/feature/units are **denormalised** onto every row on
/// purpose — V002's comment calls it an audit/traceability requirement, so the
/// row records what it was actually measured against even after the plan
/// changes. That makes the per-row arm below the check that matters: a drifted
/// snapshot is a rewritten inspection record, and no fold would see it.
const SQLITE_QC_INSPECTIONS_DDL: &str = "
CREATE TABLE IF NOT EXISTS qc_inspections (
    qci_id                     TEXT    NOT NULL PRIMARY KEY,
    tenant_id                  TEXT    NOT NULL,
    measured_at_utc            TEXT    NOT NULL,
    source                     TEXT    NOT NULL,
    source_event_id            TEXT,
    inspection_plan_id         TEXT    NOT NULL,
    feature_name               TEXT    NOT NULL,
    nominal_value              REAL    NOT NULL,
    upper_tol                  REAL    NOT NULL,
    lower_tol                  REAL    NOT NULL,
    units                      TEXT    NOT NULL,
    actual_value               REAL    NOT NULL,
    deviation                  REAL    NOT NULL,
    verdict                    TEXT    NOT NULL,
    probe_serial               TEXT,
    last_calibration_at_utc    TEXT,
    calibration_stale_at_event INTEGER NOT NULL,
    auto_ncr_id                TEXT,
    linked_part_uid            TEXT,
    linked_heat_lot            TEXT,
    linked_wo_id               TEXT,
    recorded_by                TEXT    NOT NULL,
    created_at                 TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS qc_inspections_tenant_wo_idx
    ON qc_inspections (tenant_id, linked_wo_id);
CREATE INDEX IF NOT EXISTS qc_inspections_tenant_part_idx
    ON qc_inspections (tenant_id, linked_part_uid);
";

/// Build the whole family's SQLite schema.
///
/// **There is no `ensure_columns` ladder in this family, and that is measured
/// rather than assumed:** `ALTER TABLE` returns zero hits against all six tables
/// across `apps/` and `crates/`. Every one of them is created whole by its
/// `CREATE TABLE IF NOT EXISTS` and has never been widened. So M8's
/// post-condition has nothing to exercise here — which is exactly why presence
/// is held per **table** (below) rather than per family: the schema evolution in
/// this family happened by *adding tables*, in three separate migrations, not by
/// adding columns.
pub fn ensure_quality_schema(conn: &SqliteConn) -> Result<()> {
    ensure_ncrs_table(conn)?;
    ensure_ncr_transitions_table(conn)?;
    ensure_capas_table(conn)?;
    ensure_qa_inspections_table(conn)?;
    ensure_qc_plans_table(conn)?;
    ensure_qc_inspections_table(conn)?;
    Ok(())
}

fn ensure_ncrs_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_QUALITY_DDL)?;
    Ok(())
}

fn ensure_ncr_transitions_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_NCR_TRANSITIONS_DDL)?;
    Ok(())
}

fn ensure_capas_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_CAPAS_DDL)?;
    Ok(())
}

fn ensure_qa_inspections_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_QA_INSPECTIONS_DDL)?;
    Ok(())
}

fn ensure_qc_plans_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_QC_PLANS_DDL)?;
    Ok(())
}

fn ensure_qc_inspections_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_QC_INSPECTIONS_DDL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The column lists — one list per table drives the SELECT, the INSERT, the
// per-row equality and the typeof() sweep, so the four can never drift apart.
// ---------------------------------------------------------------------------

/// `ncrs`'s 14 columns. Every one is `TEXT`; four of them are JSON arrays.
const NCR_COLUMNS: &[&str] = &[
    "ncr_id",
    "tenant_id",
    "discovered_at_utc",
    "discovered_by_operator",
    "severity",
    "category",
    "description",
    "affected_part_uids",
    "affected_wo_ids",
    "affected_heat_lots",
    "photos",
    "state",
    "closed_at_utc",
    "closed_by_operator",
];

/// `ncr_transitions`'s 7 `TEXT` columns; `seq` is the 8th and is typed
/// separately (§3.2 F).
///
/// **`ncr_id` leads, not `tenant_id`.** The DDL declares `tenant_id` first —
/// this list deliberately does not, because [`key_of`] reads index 0 and the
/// half of the composite key that identifies the NCR is the one an operator
/// looks for in a hard stop.
const TRANSITION_TEXT_COLUMNS: &[&str] = &[
    "ncr_id",
    "tenant_id",
    "from_state",
    "to_state",
    "operator",
    "at_utc",
    "note",
];

/// `capas`'s 15 columns, every one `TEXT`.
const CAPA_COLUMNS: &[&str] = &[
    "capa_id",
    "ncr_id",
    "tenant_id",
    "corrective_action_text",
    "preventive_action_text",
    "responsible_operator",
    "target_close_date",
    "actual_close_date",
    "effectiveness_review_at_utc",
    "effectiveness_verdict",
    "effectiveness_comment",
    "approved_by_operator",
    "approved_at_utc",
    "created_at_utc",
    "created_by_operator",
];

/// `qa_inspections`'s 12 columns, every one `TEXT` — including `measurement`,
/// which is free-form operator text and not a number.
const QA_COLUMNS: &[&str] = &[
    "qa_id",
    "tenant_id",
    "wo_id",
    "routing_op_id",
    "state",
    "decided_at",
    "decided_by",
    "reason",
    "measurement",
    "source_event_id",
    "created_at",
    "superseded_by",
];

/// `qc_inspection_plans`'s 8 `TEXT` columns. The three measurements and the
/// boolean are typed separately.
const PLAN_TEXT_COLUMNS: &[&str] = &[
    "plan_id",
    "tenant_id",
    "product_id",
    "feature_name",
    "units",
    "optional_probe_cycle_id",
    "created_at",
    "archived_at",
];

/// The plan's three §3.2 E measurements, in bind order.
const PLAN_MEASUREMENT_COLUMNS: &[&str] = &["nominal_value", "upper_tol", "lower_tol"];

/// `qc_inspections`'s 17 `TEXT` columns. The five measurements and the boolean
/// are typed separately.
const INSPECTION_TEXT_COLUMNS: &[&str] = &[
    "qci_id",
    "tenant_id",
    "measured_at_utc",
    "source",
    "source_event_id",
    "inspection_plan_id",
    "feature_name",
    "units",
    "verdict",
    "probe_serial",
    "last_calibration_at_utc",
    "auto_ncr_id",
    "linked_part_uid",
    "linked_heat_lot",
    "linked_wo_id",
    "recorded_by",
    "created_at",
];

/// The inspection's five §3.2 E measurements, in bind order. `deviation` is the
/// derived one — §3.2 E note (b), and the module docs.
const INSPECTION_MEASUREMENT_COLUMNS: &[&str] = &[
    "nominal_value",
    "upper_tol",
    "lower_tol",
    "actual_value",
    "deviation",
];

// ---------------------------------------------------------------------------
// §3.2 E's refusal — the family's one validation
// ---------------------------------------------------------------------------

/// Refuse a non-finite measurement, naming the table, the column and the row.
///
/// **`DOUBLE` → `REAL` is bit-exact for every finite `f64`, so this is the only
/// way the carry can lose a measurement — and without it the loss is silent or
/// unattributable.** SQLite has no `NaN`: a bound `f64::NAN` is stored as
/// `NULL`. All eight measurement columns in this family are `NOT NULL`, so a
/// `NaN` would surface as a bare `NOT NULL constraint failed` naming neither the
/// row nor the reason; on a nullable measurement column it would cross as a
/// silent `NULL` and the gate's own `IS NOT NULL`-scoped `typeof` sweep would
/// not see it either.
///
/// The infinities are refused with it. They *do* round-trip through SQLite —
/// measured, not assumed — but an infinite dimensional measurement is not a
/// measurement, and this column drives a pass/fail verdict (`qc::verdict`
/// computes `overage = max(0, |deviation| - t)`, which is `inf` for any finite
/// tolerance). Carrying one would migrate a quality record whose verdict no
/// longer means anything. Refusing is rule 11; carrying it because the bits
/// survive would be preserving a defect faithfully and calling that success.
pub fn finite_measurement(v: f64, key: &str, table: &str, column: &str) -> Result<f64> {
    if !v.is_finite() {
        bail!(
            "{table}.{column} on {key} is {v}, which SQLite cannot store as a REAL: it has no \
             NaN (a bound NaN becomes NULL) and an infinite dimensional measurement drives a \
             pass/fail verdict that can no longer mean anything. Refusing to carry it rather \
             than letting it arrive as a NULL or as an unattributable constraint error \
             (ADR-0108 §3.2 E)"
        );
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One `ncr_transitions` row: the 7 `TEXT` columns plus §3.2 F's `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TransitionRow {
    text: Vec<Option<String>>,
    seq: i64,
}

/// One `qc_inspection_plans` or `qc_inspections` row: the `TEXT` columns, the
/// §3.2 E measurements, and the §3.2 H boolean.
///
/// One struct serves both tables because the two have the same *shape* — text,
/// measurements, one flag — and the column lists are what differ. No `Eq`:
/// `f64` is not `Eq`, and the gate compares the measurements explicitly so a
/// `NaN` (which would compare unequal to itself) hard-stops rather than being
/// derived away.
#[derive(Debug, Clone, PartialEq)]
struct MeasuredRow {
    text: Vec<Option<String>>,
    measurements: Vec<f64>,
    flag: bool,
}

/// What the QA/QC carry did. As everywhere else in this migrator these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one of
/// them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QualityCarry {
    pub ncrs: u64,
    pub ncr_transitions: u64,
    pub capas: u64,
    pub qa_inspections: u64,
    pub qc_inspection_plans: u64,
    pub qc_inspections: u64,
}

// ---------------------------------------------------------------------------
// Presence — per TABLE, because THREE migrations build these six tables
// ---------------------------------------------------------------------------

fn present_duckdb(conn: &duckdb::Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = ?",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn present_sqlite(conn: &SqliteConn, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Every table in the family, so `migrate_families` and the gate iterate the
/// same list rather than two hand-kept copies.
const FAMILY_TABLES: &[&str] = &[
    "ncrs",
    "ncr_transitions",
    "capas",
    "qa_inspections",
    "qc_inspection_plans",
    "qc_inspections",
];

/// Does this DuckDB database have **any** of the family's six tables?
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    for t in FAMILY_TABLES {
        if present_duckdb(conn, t)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// The carry
// ---------------------------------------------------------------------------

fn insert_sql(table: &str, cols: &[&str], extra: &[&str]) -> String {
    let all: Vec<&str> = cols.iter().chain(extra.iter()).copied().collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        all.join(", "),
        vec!["?"; all.len()].join(", ")
    )
}

/// Carry the family from an already-open **read-only** DuckDB connection into an
/// already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// Schema creation is **per present table**, and here that is load-bearing
/// rather than merely safer: three separate migrations build these six tables
/// (`quality.rs`'s batch, V001, V002), so a database predating S443 legitimately
/// has four of the six and one predating S439 has only `qa_inspections`.
/// Creating an empty SQLite table for one the source does not have would
/// manufacture exactly the asymmetry the gate exists to detect.
pub fn carry_quality(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<QualityCarry> {
    let ncrs = if present_duckdb(src, "ncrs")? {
        ensure_ncrs_table(dst)?;
        let rows = read_text_table_duckdb(src, "ncrs", NCR_COLUMNS)?;
        unique_natural_keys(&rows.iter().map(|r| key_of(r)).collect::<Vec<_>>(), "ncrs")?;
        rows
    } else {
        Vec::new()
    };
    let transitions = if present_duckdb(src, "ncr_transitions")? {
        ensure_ncr_transitions_table(dst)?;
        let rows = read_transitions_duckdb(src)?;
        unique_natural_keys(
            &rows.iter().map(transition_key).collect::<Vec<_>>(),
            "ncr_transitions",
        )?;
        rows
    } else {
        Vec::new()
    };
    let capas = if present_duckdb(src, "capas")? {
        ensure_capas_table(dst)?;
        let rows = read_text_table_duckdb(src, "capas", CAPA_COLUMNS)?;
        unique_natural_keys(&rows.iter().map(|r| key_of(r)).collect::<Vec<_>>(), "capas")?;
        rows
    } else {
        Vec::new()
    };
    let qa = if present_duckdb(src, "qa_inspections")? {
        ensure_qa_inspections_table(dst)?;
        read_text_table_duckdb(src, "qa_inspections", QA_COLUMNS)?
    } else {
        Vec::new()
    };
    let plans = if present_duckdb(src, "qc_inspection_plans")? {
        ensure_qc_plans_table(dst)?;
        read_measured_duckdb(
            src,
            "qc_inspection_plans",
            PLAN_TEXT_COLUMNS,
            PLAN_MEASUREMENT_COLUMNS,
            "enabled",
        )?
    } else {
        Vec::new()
    };
    let inspections = if present_duckdb(src, "qc_inspections")? {
        ensure_qc_inspections_table(dst)?;
        read_measured_duckdb(
            src,
            "qc_inspections",
            INSPECTION_TEXT_COLUMNS,
            INSPECTION_MEASUREMENT_COLUMNS,
            "calibration_stale_at_event",
        )?
    } else {
        Vec::new()
    };

    let ncrs_insert = insert_sql("ncrs", NCR_COLUMNS, &[]);
    let transitions_insert = insert_sql("ncr_transitions", TRANSITION_TEXT_COLUMNS, &["seq"]);
    let capas_insert = insert_sql("capas", CAPA_COLUMNS, &[]);
    let qa_insert = insert_sql("qa_inspections", QA_COLUMNS, &[]);
    let plans_insert = {
        let extra: Vec<&str> = PLAN_MEASUREMENT_COLUMNS
            .iter()
            .copied()
            .chain(std::iter::once("enabled"))
            .collect();
        insert_sql("qc_inspection_plans", PLAN_TEXT_COLUMNS, &extra)
    };
    let inspections_insert = {
        let extra: Vec<&str> = INSPECTION_MEASUREMENT_COLUMNS
            .iter()
            .copied()
            .chain(std::iter::once("calibration_stale_at_event"))
            .collect();
        insert_sql("qc_inspections", INSPECTION_TEXT_COLUMNS, &extra)
    };

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the QA/QC carry transaction: {e}"))?;

    for (rows, sql) in [
        (&ncrs, &ncrs_insert),
        (&capas, &capas_insert),
        (&qa, &qa_insert),
    ] {
        for row in rows {
            let binds: Vec<&dyn ToSql> = row.iter().map(|v| v as &dyn ToSql).collect();
            tx.execute(sql, binds.as_slice())?;
        }
    }
    for t in &transitions {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(TRANSITION_TEXT_COLUMNS.len() + 1);
        for v in &t.text {
            binds.push(v);
        }
        binds.push(&t.seq);
        tx.execute(&transitions_insert, binds.as_slice())?;
    }
    for (rows, sql, texts, measures) in [
        (
            &plans,
            &plans_insert,
            PLAN_TEXT_COLUMNS,
            PLAN_MEASUREMENT_COLUMNS,
        ),
        (
            &inspections,
            &inspections_insert,
            INSPECTION_TEXT_COLUMNS,
            INSPECTION_MEASUREMENT_COLUMNS,
        ),
    ] {
        for row in rows {
            let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(texts.len() + measures.len() + 1);
            for v in &row.text {
                binds.push(v);
            }
            for v in &row.measurements {
                binds.push(v);
            }
            binds.push(&row.flag);
            tx.execute(sql, binds.as_slice())?;
        }
    }
    tx.commit()?;

    Ok(QualityCarry {
        ncrs: ncrs.len() as u64,
        ncr_transitions: transitions.len() as u64,
        capas: capas.len() as u64,
        qa_inspections: qa.len() as u64,
        qc_inspection_plans: plans.len() as u64,
        qc_inspections: inspections.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// `ORDER BY` the key so both engines hand back the same order, and so a
/// per-table read is deterministic for the gate as well as for the carry.
///
/// **There is no `CAST(… AS VARCHAR)` anywhere in this module** and that is a
/// statement about the family rather than an omission: no column in it is
/// `DECIMAL` on DuckDB, because there is no money and no quantity here.
fn text_select(table: &str, cols: &[&str], order_by: &str) -> String {
    format!(
        "SELECT {} FROM {table} ORDER BY {order_by}",
        cols.join(", ")
    )
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// the all-`TEXT` read is written once per engine and shared across the three
/// all-text tables rather than once per table.
fn read_text_table_duckdb(
    conn: &duckdb::Connection,
    table: &str,
    cols: &[&str],
) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&text_select(table, cols, cols[0]))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(cols.len());
        for i in 0..cols.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(text)
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_text_table_sqlite(
    conn: &SqliteConn,
    table: &str,
    cols: &[&str],
) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&text_select(table, cols, cols[0]))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(cols.len());
        for i in 0..cols.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(text)
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn transitions_select() -> String {
    format!(
        "SELECT {}, seq FROM ncr_transitions ORDER BY ncr_id ASC, seq ASC",
        TRANSITION_TEXT_COLUMNS.join(", ")
    )
}

fn read_transitions_duckdb(conn: &duckdb::Connection) -> Result<Vec<TransitionRow>> {
    let mut stmt = conn.prepare(&transitions_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(TRANSITION_TEXT_COLUMNS.len());
        for i in 0..TRANSITION_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(TransitionRow {
            text,
            seq: r.get(TRANSITION_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_transitions_sqlite(conn: &SqliteConn) -> Result<Vec<TransitionRow>> {
    let mut stmt = conn.prepare(&transitions_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(TRANSITION_TEXT_COLUMNS.len());
        for i in 0..TRANSITION_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(TransitionRow {
            text,
            seq: r.get(TRANSITION_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn measured_select(table: &str, texts: &[&str], measures: &[&str], flag: &str) -> String {
    format!(
        "SELECT {}, {}, {flag} FROM {table} ORDER BY {} ASC",
        texts.join(", "),
        measures.join(", "),
        texts[0]
    )
}

/// The DuckDB read of a measured table. **Every measurement is validated as it
/// is read** — [`finite_measurement`] refuses a non-finite one naming the table,
/// the column and the row, before anything is bound.
fn read_measured_duckdb(
    conn: &duckdb::Connection,
    table: &str,
    texts: &[&str],
    measures: &[&str],
    flag: &str,
) -> Result<Vec<MeasuredRow>> {
    let mut stmt = conn.prepare(&measured_select(table, texts, measures, flag))?;
    let rows: Vec<MeasuredRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(texts.len());
            for i in 0..texts.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut measurements = Vec::with_capacity(measures.len());
            for i in 0..measures.len() {
                measurements.push(r.get::<_, f64>(texts.len() + i)?);
            }
            Ok(MeasuredRow {
                text,
                measurements,
                flag: r.get(texts.len() + measures.len())?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for row in &rows {
        let key = key_of(&row.text);
        for (v, name) in row.measurements.iter().zip(measures) {
            finite_measurement(*v, &key, table, name)?;
        }
    }
    Ok(rows)
}

fn read_measured_sqlite(
    conn: &SqliteConn,
    table: &str,
    texts: &[&str],
    measures: &[&str],
    flag: &str,
) -> Result<Vec<MeasuredRow>> {
    let mut stmt = conn.prepare(&measured_select(table, texts, measures, flag))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(texts.len());
        for i in 0..texts.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        let mut measurements = Vec::with_capacity(measures.len());
        for i in 0..measures.len() {
            measurements.push(r.get::<_, f64>(texts.len() + i)?);
        }
        Ok(MeasuredRow {
            text,
            measurements,
            flag: r.get(texts.len() + measures.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Every table in this family carries its key at index 0 of its column list —
/// which for `ncr_transitions` is the *first half* of a composite key, so that
/// one gets [`transition_key`] instead.
fn key_of(text: &[Option<String>]) -> String {
    text[0].clone().unwrap_or_default()
}

/// `ncr_transitions` has no single-column primary key: its identity is
/// (`ncr_id`, `seq`). Same shape as Step 5's `invoice_line` key
/// (`invoice_id#ordinal`).
fn transition_key(row: &TransitionRow) -> String {
    format!("{}#{}", key_of(&row.text), row.seq)
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per table, per row, over **every** column. Every
/// number here is re-derived from disk by the gate (B4).
pub fn reconcile_quality(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let mut both = Vec::new();
    for table in FAMILY_TABLES {
        match (present_duckdb(src, table)?, present_sqlite(dst, table)?) {
            (false, false) => continue,
            (true, false) => {
                hard_stops.push(format!(
                    "{table} exists in DuckDB but NOT in SQLite — the table was not carried. \
                     This is the silent-skip shape: every check below would have been skipped \
                     and the gate would have reported PASS"
                ));
                continue;
            }
            (false, true) => {
                hard_stops.push(format!(
                    "{table} exists in SQLite but NOT in DuckDB — the copy has a table the \
                     source does not. There is no source of truth to reconcile against"
                ));
                continue;
            }
            (true, true) => both.push(*table),
        }
        row_count(src, dst, table, checks, hard_stops)?;
    }
    if both.is_empty() {
        checks.push(
            "✓ QA/QC family absent on BOTH sides — nothing to reconcile (a database that has \
             never raised an NCR or run an inspection is a legitimate shape; an asymmetry is not)"
                .to_string(),
        );
        return Ok(());
    }

    for (table, cols, noun) in [
        ("ncrs", NCR_COLUMNS, "NCR"),
        ("capas", CAPA_COLUMNS, "CAPA"),
        ("qa_inspections", QA_COLUMNS, "QA inspection"),
    ] {
        if both.contains(&table) {
            reconcile_text_table(src, dst, table, cols, noun, checks, hard_stops)?;
        }
    }
    if both.contains(&"ncr_transitions") {
        reconcile_transitions(src, dst, checks, hard_stops)?;
    }
    if both.contains(&"qc_inspection_plans") {
        reconcile_measured(
            src,
            dst,
            "qc_inspection_plans",
            PLAN_TEXT_COLUMNS,
            PLAN_MEASUREMENT_COLUMNS,
            "enabled",
            "QC plan",
            checks,
            hard_stops,
        )?;
    }
    if both.contains(&"qc_inspections") {
        reconcile_measured(
            src,
            dst,
            "qc_inspections",
            INSPECTION_TEXT_COLUMNS,
            INSPECTION_MEASUREMENT_COLUMNS,
            "calibration_stale_at_event",
            "QC inspection",
            checks,
            hard_stops,
        )?;
    }
    Ok(())
}

fn row_count(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    table: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let sql = format!("SELECT count(*) FROM {table}");
    let d: i64 = src.query_row(&sql, [], |r| r.get(0))?;
    let l: i64 = dst.query_row(&sql, [], |r| r.get(0))?;
    if d == l {
        checks.push(format!("✓ {table} row count = {d}"));
    } else {
        hard_stops.push(format!("{table} row count: DuckDB {d}, SQLite {l}"));
    }
    Ok(())
}

/// Per-row equality over one table's `TEXT` columns. Returns the drift count so
/// the caller can add its own typed columns to it before deciding whether to
/// emit the ZERO-drift check.
fn compare_text(
    noun: &str,
    columns: &[&str],
    key: &str,
    duck: &[Option<String>],
    lite: &[Option<String>],
    hard_stops: &mut Vec<String>,
) -> usize {
    let mut drift = 0usize;
    for (i, name) in columns.iter().enumerate() {
        if duck[i] != lite[i] {
            hard_stops.push(format!(
                "{noun} {key}: {name} DuckDB {:?}, SQLite {:?}",
                duck[i], lite[i]
            ));
            drift += 1;
        }
    }
    drift
}

/// The three all-`TEXT` tables. Every column is compared; none of them has a
/// fold or a `typeof` class other than `'text'` that could catch a drift
/// instead.
#[allow(clippy::too_many_arguments)]
fn reconcile_text_table(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    table: &str,
    cols: &[&str],
    noun: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_text_table_duckdb(src, table, cols)?;
    let lite: BTreeMap<String, Vec<Option<String>>> = read_text_table_sqlite(dst, table, cols)?
        .into_iter()
        .map(|r| (key_of(&r), r))
        .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = key_of(row);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("{noun} {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        drift += compare_text(noun, cols, &key, row, got, hard_stops);
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {table} column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            cols.len()
        ));
    }

    for col in cols {
        typeof_sweep(dst, table, col, "text", checks, hard_stops)?;
    }
    Ok(())
}

/// `ncr_transitions` — keyed on the composite (`ncr_id`, `seq`).
///
/// The `Σ seq` fold is an `i64` fold in Rust, never a SQL `SUM`: `SUM` over
/// `INTEGER` raises on i64 overflow, and the rule that keeps arithmetic out of
/// SQL does not get an exception for a column that happens to be small (§3.4 /
/// T-8).
fn reconcile_transitions(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_transitions_duckdb(src)?;
    let lite: BTreeMap<String, TransitionRow> = read_transitions_sqlite(dst)?
        .into_iter()
        .map(|t| (transition_key(&t), t))
        .collect();

    let mut drift = 0usize;
    for t in &duck {
        let key = transition_key(t);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "NCR transition {key} is missing on the SQLite side — the transition log IS the \
                 NCR's ISO-9001 history, and a gap in it is unrecoverable"
            ));
            drift += 1;
            continue;
        };
        drift += compare_text(
            "NCR transition",
            TRANSITION_TEXT_COLUMNS,
            &key,
            &t.text,
            &got.text,
            hard_stops,
        );
        // Part of the key, so a mismatch here cannot happen through the lookup
        // above — it is compared anyway, because `transition_key` reading the
        // wrong field would otherwise pair two rows silently.
        if got.seq != t.seq {
            hard_stops.push(format!(
                "NCR transition {key}: seq DuckDB {}, SQLite {}",
                t.seq, got.seq
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every ncr_transitions column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            TRANSITION_TEXT_COLUMNS.len() + 1
        ));
    }

    let d = fold_i64(duck.iter().map(|t| t.seq), "seq")?;
    let l = fold_i64(lite.values().map(|t| t.seq), "seq")?;
    if d == l {
        checks.push(format!(
            "✓ Σ ncr_transitions.seq = {d} (i64 fold in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!("Σ ncr_transitions.seq: DuckDB {d}, SQLite {l}"));
    }

    for col in TRANSITION_TEXT_COLUMNS {
        typeof_sweep(dst, "ncr_transitions", col, "text", checks, hard_stops)?;
    }
    typeof_sweep(dst, "ncr_transitions", "seq", "integer", checks, hard_stops)?;
    Ok(())
}

/// The two QC tables: text columns, §3.2 E measurements compared **bit for
/// bit**, and the §3.2 H boolean.
///
/// The measurements are deliberately **not** folded. Summing `f64`s is the
/// operation this migration exists to keep away from a stored value, and the
/// per-row bit-equality is strictly stronger than any Σ: it catches a swap
/// between two rows, which a sum cannot see.
#[allow(clippy::too_many_arguments)]
fn reconcile_measured(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    table: &str,
    texts: &[&str],
    measures: &[&str],
    flag: &str,
    noun: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_measured_duckdb(src, table, texts, measures, flag)?;
    let lite: BTreeMap<String, MeasuredRow> =
        read_measured_sqlite(dst, table, texts, measures, flag)?
            .into_iter()
            .map(|r| (key_of(&r.text), r))
            .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = key_of(&row.text);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("{noun} {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        drift += compare_text(noun, texts, &key, &row.text, &got.text, hard_stops);

        for (i, name) in measures.iter().enumerate() {
            // §3.2 E's floats, compared bit-for-bit rather than folded. A
            // `DOUBLE` that crosses to `REAL` is exact, so anything but equality
            // here is a real difference. `finite_measurement` has already
            // refused the non-finite values on the read side, so a `NaN`
            // reaching this comparison would itself be a carry defect — and it
            // would hard-stop, because `NaN != NaN`.
            if got.measurements[i] != row.measurements[i] {
                hard_stops.push(format!(
                    "{noun} {key}: {name} DuckDB {:?}, SQLite {:?} — §3.2 E carries this DOUBLE \
                     to REAL unchanged, so the two must be bit-identical; this column is a \
                     dimensional measurement that decides a pass/fail verdict",
                    row.measurements[i], got.measurements[i]
                ));
                drift += 1;
            }
        }
        if got.flag != row.flag {
            hard_stops.push(format!(
                "{noun} {key}: {flag} DuckDB {}, SQLite {}",
                row.flag, got.flag
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {table} column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            texts.len() + measures.len() + 1
        ));
    }

    for col in texts {
        typeof_sweep(dst, table, col, "text", checks, hard_stops)?;
    }
    for col in measures {
        typeof_sweep(dst, table, col, "real", checks, hard_stops)?;
    }
    // §3.2 H: `BOOLEAN` → `INTEGER`. A `'text'` here would be a `"true"` bound
    // as a string, which every comparison in Rust would then read as `false`.
    typeof_sweep(dst, table, flag, "integer", checks, hard_stops)?;
    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// For the eight measurement columns this is the assertion that §3.2 E's call
/// actually held: a `'text'` there would mean a measurement crossed as a string
/// and every later comparison would be lexicographic.
fn typeof_sweep(
    dst: &SqliteConn,
    table: &str,
    col: &str,
    want: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let bad: i64 = dst.query_row(
        &format!("SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
        [want],
        |r| r.get(0),
    )?;
    if bad == 0 {
        checks.push(format!("✓ typeof({table}.{col}) = '{want}' on every row"));
    } else {
        hard_stops.push(format!(
            "{table}.{col}: {bad} row(s) are not '{want}' — in SQLite TEXT, INTEGER and REAL are \
             distinct storage classes, so a wrong one here silently breaks lookups and, on a \
             measurement column, turns a numeric comparison into a lexicographic one"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::{COUNT_INT, NON_MONEY_FLOAT, TEXT};

    /// Every table's carry list leads with the column [`key_of`] reads and every
    /// `ORDER BY` sorts on.
    #[test]
    fn every_table_carries_its_key_first() {
        assert_eq!(NCR_COLUMNS[0], "ncr_id");
        assert_eq!(CAPA_COLUMNS[0], "capa_id");
        assert_eq!(QA_COLUMNS[0], "qa_id");
        assert_eq!(PLAN_TEXT_COLUMNS[0], "plan_id");
        assert_eq!(INSPECTION_TEXT_COLUMNS[0], "qci_id");
        // The composite one: `ncr_id` leads even though the DDL declares
        // `tenant_id` first, because `transition_key` builds `ncr_id#seq`.
        assert_eq!(TRANSITION_TEXT_COLUMNS[0], "ncr_id");
    }

    /// The six tables the carry, the gate and `FAMILY_TABLES` walk are the same
    /// six, in the same order.
    #[test]
    fn the_family_table_list_matches_the_carry() {
        assert_eq!(
            FAMILY_TABLES,
            &[
                "ncrs",
                "ncr_transitions",
                "capas",
                "qa_inspections",
                "qc_inspection_plans",
                "qc_inspections"
            ]
        );
    }

    /// **The family has no money and no quantity**, and the DDL is what proves
    /// it rather than the module docs: not one column in it is declared with a
    /// type that §3 reserves for an exact value.
    ///
    /// This is the pin that fails if a later session adds a cost, a price or a
    /// quantity to an NCR or an inspection without deciding its representation —
    /// which is the moment R1/R2 start applying to this family.
    #[test]
    fn no_column_in_this_family_is_money_or_quantity() {
        for ddl in [
            SQLITE_QUALITY_DDL,
            SQLITE_NCR_TRANSITIONS_DDL,
            SQLITE_CAPAS_DDL,
            SQLITE_QA_INSPECTIONS_DDL,
            SQLITE_QC_PLANS_DDL,
            SQLITE_QC_INSPECTIONS_DDL,
        ] {
            for banned in ["DECIMAL", "NUMERIC", "_minor", "price", "cost", "_huf"] {
                assert!(
                    !ddl.to_lowercase().contains(&banned.to_lowercase()),
                    "the QA/QC family carries no money and no quantity; {banned:?} appearing in \
                     its DDL means that stopped being true and R1/R2 now have something to bite \
                     on:\n{ddl}"
                );
            }
        }
    }

    /// The declared types this module uses are the vocabulary constants, spelled
    /// the same way. A `REAL` that drifted to `DOUBLE` would be a `STRICT`
    /// rejection at boot rather than a silent one, but the DDL is a string
    /// literal and nothing else checks it.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        assert_eq!(NON_MONEY_FLOAT, "REAL");
        assert_eq!(COUNT_INT, "INTEGER");
        for (ddl, table) in [
            (SQLITE_QC_PLANS_DDL, "qc_inspection_plans"),
            (SQLITE_QC_INSPECTIONS_DDL, "qc_inspections"),
        ] {
            assert!(!ddl.contains("DOUBLE"), "{table} must not declare DOUBLE");
            assert!(!ddl.contains("BOOLEAN"), "{table} must not declare BOOLEAN");
            assert!(ddl.contains("STRICT"), "{table} must be STRICT");
        }
    }

    /// [`finite_measurement`] refuses the three non-finite `f64`s and names the
    /// table, the column and the row when it does.
    #[test]
    fn a_non_finite_measurement_is_refused_by_name() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let e = finite_measurement(v, "qci-01", "qc_inspections", "deviation")
                .expect_err("a non-finite measurement must be refused");
            let msg = e.to_string();
            assert!(msg.contains("qci-01"), "must name the row: {msg}");
            assert!(
                msg.contains("qc_inspections.deviation"),
                "must name the table and column: {msg}"
            );
        }
        for v in [0.0, -0.0, 1.5, -273.15, f64::MIN, f64::MAX] {
            assert_eq!(
                finite_measurement(v, "qci-01", "qc_inspections", "deviation").unwrap(),
                v
            );
        }
    }
}
