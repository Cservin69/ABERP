//! ADR-0108 Step 7 Part D — **the work-orders / BOM family crosses.**
//!
//! (Ervin's brief called this "Part B"; B is the partners family and C is
//! products/inventory, both already landed on this branch, so it is **Part D**.
//! Named here rather than silently renumbered.)
//!
//! # The family
//!
//! Four tables, one owning crate, 43 columns:
//!
//! | table | migration | columns | the numbers in it |
//! |---|---|---|---|
//! | `work_orders` | V001 + V002 + V003 | 16 | `qty_target` (R2), `actual_machining_minutes` (§3.2 E, `REAL`) |
//! | `boms` | V001 + V003 | 8 | `qty_per_unit` (R2) |
//! | `routings` | V001 | 10 | `est_cost_huf` (R2 — **the one money column that does not follow R1**), `sequence` / `est_time_min` (§3.2 F) |
//! | `bom_revisions` | V003 | 9 | `rev_number` / `line_count` (§3.2 F) |
//!
//! All four are built by a **single** `aberp_work_orders::ensure_schema`, so
//! unlike products/inventory the family has one construction path. Presence is
//! nonetheless held **per table**, not per family, and that is a deliberate
//! choice between the two shapes already in this migrator (rule 7, pick one and
//! say why): `bom_revisions` arrives with V003 (ADR-0105) and the two
//! `bom_rev_id` columns with it, so a DuckDB file written before that migration
//! legitimately has three of the four tables. A per-family answer would read
//! that shape as "the family is present" and then hard-stop on a table the
//! source never had. Per-table is strictly the safer of the two: an asymmetry
//! is still a hard stop, and only the genuinely legitimate partial shape passes.
//!
//! # The money column, and why it is R2 and not R1
//!
//! `routings.est_cost_huf` is money — HUF, at scale 2 — and it crosses as
//! **`TEXT`** (R2), not as `INTEGER` minor units (R1). This is not a fresh
//! decision taken here: §3.2 B settled it when the column was found, and the
//! reasoning is on the record. HUF has no subunit in ABERP's model, so "minor
//! units" for a scale-2 estimate is a product question, not a storage one; the
//! Rust type is `Option<Decimal>`, not `Money`; and the value never reaches NAV,
//! the invoice PDF, or the ledger totals — it is an operator's estimate on a
//! routing op.
//!
//! **It is flagged here as loudly as §3.2 B flags it, because it is the single
//! exception to R1 in the whole tree.** Two things make the exception safe
//! rather than merely stated: the value is carried **verbatim** through
//! [`canonical_decimal`], which refuses anything `rust_decimal` cannot parse; and
//! the gate folds it in Rust over `Decimal`, never as a SQL `SUM` — which is the
//! guard R2 actually has (§3.1's corrected note: `STRICT` does not protect an R2
//! column, the Rust-side bind and T-8 do).
//!
//! # The one `REAL` in the family
//!
//! `work_orders.actual_machining_minutes` stays a float: §3.2 E lists it by name
//! among the non-money floats that cross `DOUBLE` → `REAL` unchanged, and its
//! Rust type is `Option<f64>` (`repository.rs:614`; `Eq` was dropped from
//! `WorkOrder` in S429 for exactly that reason).
//!
//! **Part C moved five `DOUBLE`s out of category E and this one does not follow
//! them, so the difference is worth stating.** Part C's five were moved because
//! they were one half of a *measured rule-7 divergence*: the same physical
//! quantity was `DECIMAL` in `stock_movements` and `DOUBLE` in
//! `inventory_balances`, and carrying both as-is under `STRICT` would have
//! blessed the fork. `actual_machining_minutes` has no exact counterpart —
//! `routings.est_time_min` is an `INTEGER` *estimate* at a different granularity
//! (per-op, not per-WO), and the calibration consumer's own columns
//! (`quote_calibration.estimated_minutes` / `actual_minutes`) are `DOUBLE` too.
//! There is no fork to close.
//!
//! And the direction of the risk is the opposite one: R2 refuses any value above
//! the canonical quantity scale of 6, so carrying a *measured duration* as R2
//! would make the migration **hard-fail** on an ordinary DEV row like
//! `12.3456789` — refusing a value that carries no exactness requirement, on a
//! column that is not money. `REAL` is bit-exact for an `f64` and adds nothing
//! new. The gate proves that per row: the `f64` read back from SQLite must equal
//! the `f64` DuckDB held.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5, 7B and 7C, and Ervin's same constraint: **the migrator
//! half only.** DuckDB remains the source of truth.
//! `crates/aberp-work-orders/src/repository.rs` is untouched — every one of its
//! queries still runs against a DuckDB `Connection`, and the three
//! `migrations/V00*.sql` files still build the DuckDB schema.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
//! arithmetic sites contain no work-orders statement, §5's twelve mitigations
//! name none, and §6.3's row for this family says "row-by-row carry" with no
//! rebuild. The T-8 pending-fold register is therefore unchanged by this commit
//! — nothing joined it and nothing left it. The migrator's own SQL holds to T-8
//! unconditionally: not one `SUM`, `AVG`, `*`, `+` or `-` on a money or quantity
//! column appears below. Every fold in this file is Rust over `Decimal` / `i64`.

use std::collections::BTreeMap;

use anyhow::Result;

use aberp_db::engine::{Connection as SqliteConn, ToSql};
use aberp_db::schema::{ensure_columns, NON_MONEY_FLOAT, TEXT};

use crate::migrate_billing::{canonical_decimal, fold_decimal, fold_i64};

// ---------------------------------------------------------------------------
// The STRICT DDL
// ---------------------------------------------------------------------------

/// `work_orders` as V001 creates it, `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H); `DECIMAL(18,6) qty_target` → `TEXT` (§3.2 C,
/// R2). Both indexes carry across; both key on base columns.
///
/// The base `CREATE` omits V002's and V003's columns, so both ladders actually
/// run through [`ensure_columns`] and M8's post-condition is exercised rather
/// than no-op'd — the same F-1c call Steps 5, 7B and 7C made.
///
/// S232 / ADR-0062's `[[no-sql-specific]]` posture is preserved verbatim: there
/// is **no** DB-level `CHECK` on `state`. The closed vocabulary lives in
/// `aberp_work_orders::state::next_state` and the transition handler refuses an
/// illegal transition with 400; `STRICT` constrains storage classes, not
/// vocabularies.
const SQLITE_WORK_ORDERS_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS work_orders (
    wo_id        TEXT NOT NULL PRIMARY KEY,
    tenant_id    TEXT NOT NULL,
    wo_number    TEXT NOT NULL,
    product_id   TEXT NOT NULL,
    qty_target   TEXT NOT NULL,
    state        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    released_at  TEXT,
    started_at   TEXT,
    completed_at TEXT,
    cancelled_at TEXT,
    hold_reason  TEXT,
    notes        TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS work_orders_tenant_state_created_idx
    ON work_orders (tenant_id, state, created_at);
CREATE INDEX IF NOT EXISTS work_orders_tenant_wo_number_idx
    ON work_orders (tenant_id, wo_number);
";

/// S429 / V002 — the closed-loop calibration link.
///
/// `actual_machining_minutes` is the family's only float and the only column in
/// it that is not `TEXT` or `INTEGER` after the carry. §3.2 E, `DOUBLE` →
/// `REAL`; see the module docs for why it does not follow Part C's five.
const WORK_ORDERS_V002_CALIBRATION: &[(&str, &str)] = &[
    ("source_quote_id", TEXT),
    ("actual_machining_minutes", NON_MONEY_FLOAT),
];

/// ADR-0105 / V003 — which BOM revision the WO was released against.
///
/// Nullable with no SQL `DEFAULT`, exactly as on DuckDB. V003's own comment is
/// the reason a backfill here would be a lie: a pre-migration row has no
/// recorded author or reason, so NULL means "legacy / unrevisioned" and the
/// Release path warns on it rather than pinning a fabricated revision.
const WORK_ORDERS_V003_BOM_REV: &[(&str, &str)] = &[("bom_rev_id", TEXT)];

/// `boms` as V001 creates it, `STRICT`. `qty_per_unit` is R2.
///
/// ADR-0062 §6: BOM lines are **soft-retired** via `retired_at`, never DELETEd,
/// so a retired row is history and must cross like any other.
const SQLITE_BOMS_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS boms (
    bom_line_id  TEXT NOT NULL PRIMARY KEY,
    tenant_id    TEXT NOT NULL,
    product_id   TEXT NOT NULL,
    component_id TEXT NOT NULL,
    qty_per_unit TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    retired_at   TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS boms_tenant_product_idx
    ON boms (tenant_id, product_id);
";

/// V003's line → revision link.
const BOMS_V003_BOM_REV: &[(&str, &str)] = &[("bom_rev_id", TEXT)];

/// V003's `boms_tenant_rev_idx`, kept **out** of [`SQLITE_BOMS_BASE_DDL`]
/// because it keys on a ladder column.
///
/// DuckDB tolerates the V003 ordering (`ALTER TABLE … ADD COLUMN` then
/// `CREATE INDEX`) inside one `execute_batch`; SQLite would too, but only if the
/// column is already there. Running it after [`ensure_columns`] — which
/// *proves* the column landed rather than reporting that a statement ran — is
/// what makes the index creation unconditionally safe.
const SQLITE_BOMS_REV_INDEX_DDL: &str = "
CREATE INDEX IF NOT EXISTS boms_tenant_rev_idx
    ON boms (tenant_id, bom_rev_id);
";

/// `routings` as V001 creates it, `STRICT`.
///
/// **`est_cost_huf` is `TEXT`** — R2, per §3.2 B's note, and the one money
/// column in the tree that does not follow R1. See the module docs.
///
/// `sequence` and `est_time_min` are §3.2 F integers: `INTEGER` → `INTEGER`,
/// unchanged. SQLite's `INTEGER` is 64-bit and DuckDB's is 32-bit, so the carry
/// widens; widening is lossless and the per-row arm proves it per value.
///
/// Same `[[no-sql-specific]]` posture on `state` as `work_orders`: no `CHECK`.
const SQLITE_ROUTINGS_DDL: &str = "
CREATE TABLE IF NOT EXISTS routings (
    routing_op_id TEXT    NOT NULL PRIMARY KEY,
    tenant_id     TEXT    NOT NULL,
    wo_id         TEXT    NOT NULL,
    sequence      INTEGER NOT NULL,
    op_name       TEXT    NOT NULL,
    est_time_min  INTEGER,
    est_cost_huf  TEXT,
    state         TEXT    NOT NULL,
    started_at    TEXT,
    completed_at  TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS routings_tenant_wo_sequence_idx
    ON routings (tenant_id, wo_id, sequence);
";

/// `bom_revisions` as V003 creates it, `STRICT`.
///
/// `content_hash` is FNV-1a rendered as a **hex string** by
/// `aberp_work_orders::repository` (`VARCHAR` on DuckDB, `String` in Rust), so
/// it is R2's neighbour `TEXT` and **not** R3 `BLOB`. R3 covers the four
/// `audit_ledger` columns that are bound as `Vec<u8>`; binding this one as
/// bytes would be the storage-class fork R3 exists to stop, applied in the wrong
/// direction.
const SQLITE_BOM_REVISIONS_DDL: &str = "
CREATE TABLE IF NOT EXISTS bom_revisions (
    bom_rev_id   TEXT    NOT NULL PRIMARY KEY,
    tenant_id    TEXT    NOT NULL,
    product_id   TEXT    NOT NULL,
    rev_number   INTEGER NOT NULL,
    created_at   TEXT    NOT NULL,
    author       TEXT    NOT NULL,
    reason       TEXT,
    line_count   INTEGER NOT NULL,
    content_hash TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS bom_revisions_tenant_product_rev_idx
    ON bom_revisions (tenant_id, product_id, rev_number);
";

/// Build the whole family's SQLite schema. Ladder order is
/// `aberp_work_orders::ensure_schema`'s order (V001 → V002 → V003), so the two
/// schemas line up statement for statement.
pub fn ensure_work_orders_schema(conn: &SqliteConn) -> Result<()> {
    ensure_work_orders_table(conn)?;
    ensure_boms_table(conn)?;
    ensure_routings_table(conn)?;
    ensure_bom_revisions_table(conn)?;
    Ok(())
}

fn ensure_work_orders_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_WORK_ORDERS_BASE_DDL)?;
    ensure_columns(conn, "work_orders", WORK_ORDERS_V002_CALIBRATION)?;
    ensure_columns(conn, "work_orders", WORK_ORDERS_V003_BOM_REV)?;
    Ok(())
}

fn ensure_boms_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_BOMS_BASE_DDL)?;
    ensure_columns(conn, "boms", BOMS_V003_BOM_REV)?;
    conn.execute_batch(SQLITE_BOMS_REV_INDEX_DDL)?;
    Ok(())
}

fn ensure_routings_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_ROUTINGS_DDL)?;
    Ok(())
}

fn ensure_bom_revisions_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_BOM_REVISIONS_DDL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The column lists — one list per table drives the SELECT, the INSERT, the
// per-row equality and the typeof() sweep, so the four can never drift apart.
// ---------------------------------------------------------------------------

/// `work_orders`'s 15 `TEXT` columns, in carry order.
/// `actual_machining_minutes` is the 16th and is typed separately.
///
/// `qty_target` is in here because R2 *is* text: it crosses as the canonical
/// decimal string, verbatim, exactly like `invoice_line.quantity` did in Step 5.
const WORK_ORDER_TEXT_COLUMNS: &[&str] = &[
    "wo_id",
    "tenant_id",
    "wo_number",
    "product_id",
    "qty_target",
    "state",
    "created_at",
    "released_at",
    "started_at",
    "completed_at",
    "cancelled_at",
    "hold_reason",
    "notes",
    "source_quote_id",
    "bom_rev_id",
];

/// Which of [`WORK_ORDER_TEXT_COLUMNS`] are `DECIMAL` on DuckDB and so need a
/// `CAST(… AS VARCHAR)` on the read side.
const WORK_ORDER_DECIMAL_COLUMNS: &[&str] = &["qty_target"];

/// Their positions, as a const rather than a runtime `position().expect(…)`: an
/// index that is wrong is a boot abort in a migrator, and a unit test pins the
/// two lists against each other instead.
const WORK_ORDER_DECIMAL_INDICES: &[usize] = &[4];

/// `boms`'s 8 columns. Every one is text after the carry — `qty_per_unit`
/// because R2 says so.
const BOM_COLUMNS: &[&str] = &[
    "bom_line_id",
    "tenant_id",
    "product_id",
    "component_id",
    "qty_per_unit",
    "created_at",
    "retired_at",
    "bom_rev_id",
];

const BOM_DECIMAL_COLUMNS: &[&str] = &["qty_per_unit"];
const BOM_DECIMAL_INDICES: &[usize] = &[4];

/// `routings`'s 8 `TEXT` columns. `sequence` and `est_time_min` are the 9th and
/// 10th and are typed separately (§3.2 F).
const ROUTING_TEXT_COLUMNS: &[&str] = &[
    "routing_op_id",
    "tenant_id",
    "wo_id",
    "op_name",
    "est_cost_huf",
    "state",
    "started_at",
    "completed_at",
];

/// **The money column.** R2, so it reads through the same `CAST(… AS VARCHAR)`
/// as every other exact non-integer in this migrator.
const ROUTING_DECIMAL_COLUMNS: &[&str] = &["est_cost_huf"];
const ROUTING_DECIMAL_INDICES: &[usize] = &[4];

/// `bom_revisions`'s 7 `TEXT` columns; `rev_number` and `line_count` are the 8th
/// and 9th.
const REVISION_TEXT_COLUMNS: &[&str] = &[
    "bom_rev_id",
    "tenant_id",
    "product_id",
    "created_at",
    "author",
    "reason",
    "content_hash",
];

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One `work_orders` row: the 15 `TEXT` columns plus the family's single float.
///
/// No `Eq`, for the same reason `aberp_work_orders::WorkOrder` has none since
/// S429: `Option<f64>` is not `Eq`. The gate compares the field explicitly, and
/// a `NaN` — which would compare unequal to itself and so hard-stop — is the
/// correct outcome for a duration that is not a number.
#[derive(Debug, Clone, PartialEq)]
struct WorkOrderRow {
    text: Vec<Option<String>>,
    /// §3.2 E. `DOUBLE` → `REAL`, bit-exact.
    actual_machining_minutes: Option<f64>,
}

/// One `routings` row: the 8 `TEXT` columns plus the two §3.2 F integers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoutingRow {
    text: Vec<Option<String>>,
    /// `NOT NULL` on both engines — the linear order of the operation.
    sequence: i64,
    est_time_min: Option<i64>,
}

/// One `bom_revisions` row: the 7 `TEXT` columns plus the two counts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RevisionRow {
    text: Vec<Option<String>>,
    rev_number: i64,
    line_count: i64,
}

/// What the work-orders/BOM carry did. As everywhere else in this migrator
/// these are **not** the numbers the gate compares (B4) — the gate re-derives
/// every one of them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkOrdersCarry {
    pub work_orders: u64,
    pub boms: u64,
    pub routings: u64,
    pub bom_revisions: u64,
}

// ---------------------------------------------------------------------------
// Presence — per TABLE, because V003 is a later migration than V001
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
const FAMILY_TABLES: &[&str] = &["work_orders", "boms", "routings", "bom_revisions"];

/// Does this DuckDB database have **any** of the family's four tables?
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

/// The `SELECT` list for a DuckDB read: `DECIMAL` columns get a
/// `CAST(… AS VARCHAR)` so the canonical string crosses verbatim — the same
/// thing `aberp_work_orders::repository` already does on every read of
/// `qty_target`, `qty_per_unit` and `est_cost_huf`.
fn duck_projection(cols: &[&str], decimals: &[&str]) -> String {
    cols.iter()
        .map(|c| {
            if decimals.contains(c) {
                format!("CAST({c} AS VARCHAR)")
            } else {
                (*c).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn insert_sql(table: &str, cols: &[&str], extra: &[&str]) -> String {
    let all: Vec<&str> = cols.iter().chain(extra.iter()).copied().collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        all.join(", "),
        vec!["?"; all.len()].join(", ")
    )
}

/// Validate the R2 columns of one already-read row **in place**, refusing
/// anything `rust_decimal` cannot restore.
///
/// The value bound is the **verbatim** DuckDB string, never a re-rendering of
/// it: DuckDB's `DECIMAL(18,6)` read-back renders `1.5` as `"1.500000"`, and
/// §3.2 C's trailing-zero note is explicit that a migrated row keeps those while
/// a freshly-written one does not. Re-rendering here would make the migration's
/// output depend on when the row happened to be written, and the formatters'
/// `.normalize()` is what makes both shapes emit the same bytes.
///
/// So this is a *validation*, not a transformation — and it is the family's
/// refusal arm: a value the SQLite read path could not turn back into a
/// `Decimal` fails the carry, naming the column and the row, rather than
/// crossing as an opaque string into a column the product will later
/// `Decimal::from_str`.
fn validate_decimals(
    row: &[Option<String>],
    indices: &[usize],
    columns: &[&str],
    table: &str,
    key: &str,
) -> Result<()> {
    for (i, name) in indices.iter().zip(columns) {
        if let Some(raw) = row[*i].as_deref() {
            canonical_decimal(raw, key, &format!("{table}.{name}"))?;
        }
    }
    Ok(())
}

/// Carry the family from an already-open **read-only** DuckDB connection into
/// an already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// Schema creation is **per present table**: creating an empty SQLite table for
/// one the source does not have would manufacture exactly the asymmetry the gate
/// exists to detect.
pub fn carry_work_orders(
    src: &duckdb::Connection,
    dst: &mut SqliteConn,
) -> Result<WorkOrdersCarry> {
    let work_orders = if present_duckdb(src, "work_orders")? {
        ensure_work_orders_table(dst)?;
        read_work_orders_duckdb(src)?
    } else {
        Vec::new()
    };
    let boms = if present_duckdb(src, "boms")? {
        ensure_boms_table(dst)?;
        read_boms_duckdb(src)?
    } else {
        Vec::new()
    };
    let routings = if present_duckdb(src, "routings")? {
        ensure_routings_table(dst)?;
        read_routings_duckdb(src)?
    } else {
        Vec::new()
    };
    let revisions = if present_duckdb(src, "bom_revisions")? {
        ensure_bom_revisions_table(dst)?;
        read_revisions_duckdb(src)?
    } else {
        Vec::new()
    };

    let work_orders_insert = insert_sql(
        "work_orders",
        WORK_ORDER_TEXT_COLUMNS,
        &["actual_machining_minutes"],
    );
    let boms_insert = insert_sql("boms", BOM_COLUMNS, &[]);
    let routings_insert = insert_sql(
        "routings",
        ROUTING_TEXT_COLUMNS,
        &["sequence", "est_time_min"],
    );
    let revisions_insert = insert_sql(
        "bom_revisions",
        REVISION_TEXT_COLUMNS,
        &["rev_number", "line_count"],
    );

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the work-orders/BOM carry transaction: {e}"))?;

    for w in &work_orders {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(WORK_ORDER_TEXT_COLUMNS.len() + 1);
        for v in &w.text {
            binds.push(v);
        }
        binds.push(&w.actual_machining_minutes);
        tx.execute(&work_orders_insert, binds.as_slice())?;
    }
    for b in &boms {
        let binds: Vec<&dyn ToSql> = b.iter().map(|v| v as &dyn ToSql).collect();
        tx.execute(&boms_insert, binds.as_slice())?;
    }
    for r in &routings {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(ROUTING_TEXT_COLUMNS.len() + 2);
        for v in &r.text {
            binds.push(v);
        }
        binds.push(&r.sequence);
        binds.push(&r.est_time_min);
        tx.execute(&routings_insert, binds.as_slice())?;
    }
    for v in &revisions {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(REVISION_TEXT_COLUMNS.len() + 2);
        for t in &v.text {
            binds.push(t);
        }
        binds.push(&v.rev_number);
        binds.push(&v.line_count);
        tx.execute(&revisions_insert, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(WorkOrdersCarry {
        work_orders: work_orders.len() as u64,
        boms: boms.len() as u64,
        routings: routings.len() as u64,
        bom_revisions: revisions.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

fn work_orders_select(for_duckdb: bool) -> String {
    let cols = if for_duckdb {
        duck_projection(WORK_ORDER_TEXT_COLUMNS, WORK_ORDER_DECIMAL_COLUMNS)
    } else {
        WORK_ORDER_TEXT_COLUMNS.join(", ")
    };
    format!("SELECT {cols}, actual_machining_minutes FROM work_orders ORDER BY wo_id ASC")
}

fn boms_select(for_duckdb: bool) -> String {
    let cols = if for_duckdb {
        duck_projection(BOM_COLUMNS, BOM_DECIMAL_COLUMNS)
    } else {
        BOM_COLUMNS.join(", ")
    };
    format!("SELECT {cols} FROM boms ORDER BY bom_line_id ASC")
}

fn routings_select(for_duckdb: bool) -> String {
    let cols = if for_duckdb {
        duck_projection(ROUTING_TEXT_COLUMNS, ROUTING_DECIMAL_COLUMNS)
    } else {
        ROUTING_TEXT_COLUMNS.join(", ")
    };
    format!("SELECT {cols}, sequence, est_time_min FROM routings ORDER BY routing_op_id ASC")
}

fn revisions_select() -> String {
    format!(
        "SELECT {}, rev_number, line_count FROM bom_revisions ORDER BY bom_rev_id ASC",
        REVISION_TEXT_COLUMNS.join(", ")
    )
}

fn read_work_orders_duckdb(conn: &duckdb::Connection) -> Result<Vec<WorkOrderRow>> {
    let mut stmt = conn.prepare(&work_orders_select(true))?;
    let rows = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(WORK_ORDER_TEXT_COLUMNS.len());
            for i in 0..WORK_ORDER_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(WorkOrderRow {
                text,
                actual_machining_minutes: r.get(WORK_ORDER_TEXT_COLUMNS.len())?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for w in &rows {
        validate_decimals(
            &w.text,
            WORK_ORDER_DECIMAL_INDICES,
            WORK_ORDER_DECIMAL_COLUMNS,
            "work_orders",
            &key_of(&w.text),
        )?;
    }
    Ok(rows)
}

fn read_work_orders_sqlite(conn: &SqliteConn) -> Result<Vec<WorkOrderRow>> {
    let mut stmt = conn.prepare(&work_orders_select(false))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(WORK_ORDER_TEXT_COLUMNS.len());
        for i in 0..WORK_ORDER_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(WorkOrderRow {
            text,
            actual_machining_minutes: r.get(WORK_ORDER_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_boms_duckdb(conn: &duckdb::Connection) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&boms_select(true))?;
    let rows = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(BOM_COLUMNS.len());
            for i in 0..BOM_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(text)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for b in &rows {
        validate_decimals(
            b,
            BOM_DECIMAL_INDICES,
            BOM_DECIMAL_COLUMNS,
            "boms",
            &key_of(b),
        )?;
    }
    Ok(rows)
}

fn read_boms_sqlite(conn: &SqliteConn) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&boms_select(false))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(BOM_COLUMNS.len());
        for i in 0..BOM_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(text)
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_routings_duckdb(conn: &duckdb::Connection) -> Result<Vec<RoutingRow>> {
    let mut stmt = conn.prepare(&routings_select(true))?;
    let rows = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(ROUTING_TEXT_COLUMNS.len());
            for i in 0..ROUTING_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(RoutingRow {
                text,
                sequence: r.get(ROUTING_TEXT_COLUMNS.len())?,
                est_time_min: r.get(ROUTING_TEXT_COLUMNS.len() + 1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for r in &rows {
        validate_decimals(
            &r.text,
            ROUTING_DECIMAL_INDICES,
            ROUTING_DECIMAL_COLUMNS,
            "routings",
            &key_of(&r.text),
        )?;
    }
    Ok(rows)
}

fn read_routings_sqlite(conn: &SqliteConn) -> Result<Vec<RoutingRow>> {
    let mut stmt = conn.prepare(&routings_select(false))?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(ROUTING_TEXT_COLUMNS.len());
        for i in 0..ROUTING_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(RoutingRow {
            text,
            sequence: r.get(ROUTING_TEXT_COLUMNS.len())?,
            est_time_min: r.get(ROUTING_TEXT_COLUMNS.len() + 1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_revisions_duckdb(conn: &duckdb::Connection) -> Result<Vec<RevisionRow>> {
    let mut stmt = conn.prepare(&revisions_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(REVISION_TEXT_COLUMNS.len());
        for i in 0..REVISION_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(RevisionRow {
            text,
            rev_number: r.get(REVISION_TEXT_COLUMNS.len())?,
            line_count: r.get(REVISION_TEXT_COLUMNS.len() + 1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_revisions_sqlite(conn: &SqliteConn) -> Result<Vec<RevisionRow>> {
    let mut stmt = conn.prepare(&revisions_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(REVISION_TEXT_COLUMNS.len());
        for i in 0..REVISION_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(RevisionRow {
            text,
            rev_number: r.get(REVISION_TEXT_COLUMNS.len())?,
            line_count: r.get(REVISION_TEXT_COLUMNS.len() + 1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Every table in this family has its primary key at index 0, so one accessor
/// serves all four and an error message always names the row an operator would
/// look for.
fn key_of(text: &[Option<String>]) -> String {
    text[0].clone().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per table, per row, over **every** column. Every
/// number here is re-derived from disk by the gate (B4).
pub fn reconcile_work_orders(
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
            "✓ work-orders/BOM family absent on BOTH sides — nothing to reconcile (a database \
             without work orders is a legitimate shape; an asymmetry is not)"
                .to_string(),
        );
        return Ok(());
    }

    if both.contains(&"work_orders") {
        reconcile_work_orders_table(src, dst, checks, hard_stops)?;
    }
    if both.contains(&"boms") {
        reconcile_boms(src, dst, checks, hard_stops)?;
    }
    if both.contains(&"routings") {
        reconcile_routings(src, dst, checks, hard_stops)?;
    }
    if both.contains(&"bom_revisions") {
        reconcile_revisions(src, dst, checks, hard_stops)?;
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

fn reconcile_work_orders_table(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_work_orders_duckdb(src)?;
    let lite: BTreeMap<String, WorkOrderRow> = read_work_orders_sqlite(dst)?
        .into_iter()
        .map(|w| (key_of(&w.text), w))
        .collect();

    let mut drift = 0usize;
    for w in &duck {
        let key = key_of(&w.text);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("work order {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        drift += compare_text(
            "work order",
            WORK_ORDER_TEXT_COLUMNS,
            &key,
            &w.text,
            &got.text,
            hard_stops,
        );
        // §3.2 E's float, compared bit-for-bit rather than folded. A `DOUBLE`
        // that crosses to `REAL` is exact, so anything but equality here is a
        // real difference — including a `NaN`, which compares unequal to itself
        // and so hard-stops, which is the right answer for a duration that is
        // not a number.
        if got.actual_machining_minutes != w.actual_machining_minutes {
            hard_stops.push(format!(
                "work order {key}: actual_machining_minutes DuckDB {:?}, SQLite {:?} — §3.2 E \
                 carries this DOUBLE to REAL unchanged, so the two must be bit-identical",
                w.actual_machining_minutes, got.actual_machining_minutes
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every work_orders column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            WORK_ORDER_TEXT_COLUMNS.len() + 1
        ));
    }

    // R2 `qty_target`, folded over `Decimal`. `SUM` on a `TEXT` column coerces
    // to `REAL` in SQLite, which is the class this migration exists to remove.
    for (col, i) in WORK_ORDER_DECIMAL_COLUMNS
        .iter()
        .zip(WORK_ORDER_DECIMAL_INDICES)
    {
        let d = fold_decimal(duck.iter().filter_map(|w| w.text[*i].as_deref()))?;
        let l = fold_decimal(lite.values().filter_map(|w| w.text[*i].as_deref()))?;
        if d == l {
            checks.push(format!(
                "✓ Σ work_orders.{col} = {d} (Decimal fold in Rust on both sides)"
            ));
        } else {
            hard_stops.push(format!("Σ work_orders.{col}: DuckDB {d}, SQLite {l}"));
        }
    }
    // `actual_machining_minutes` is deliberately NOT folded: summing `f64`s is
    // the operation this migration exists to keep away from a stored value, and
    // the per-row bit-equality above is a strictly stronger check than any Σ.

    for col in WORK_ORDER_TEXT_COLUMNS {
        typeof_sweep(dst, "work_orders", col, "text", checks, hard_stops)?;
    }
    typeof_sweep(
        dst,
        "work_orders",
        "actual_machining_minutes",
        "real",
        checks,
        hard_stops,
    )?;
    Ok(())
}

fn reconcile_boms(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_boms_duckdb(src)?;
    let lite: BTreeMap<String, Vec<Option<String>>> = read_boms_sqlite(dst)?
        .into_iter()
        .map(|b| (key_of(&b), b))
        .collect();

    let mut drift = 0usize;
    for b in &duck {
        let key = key_of(b);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "bom line {key} is missing on the SQLite side — BOM lines are soft-retired and \
                 never deleted (ADR-0062 §6), so a missing one is lost revision history"
            ));
            drift += 1;
            continue;
        };
        drift += compare_text("bom line", BOM_COLUMNS, &key, b, got, hard_stops);
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every boms column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            BOM_COLUMNS.len()
        ));
    }

    for (col, i) in BOM_DECIMAL_COLUMNS.iter().zip(BOM_DECIMAL_INDICES) {
        let d = fold_decimal(duck.iter().filter_map(|b| b[*i].as_deref()))?;
        let l = fold_decimal(lite.values().filter_map(|b| b[*i].as_deref()))?;
        if d == l {
            checks.push(format!(
                "✓ Σ boms.{col} = {d} (Decimal fold in Rust on both sides)"
            ));
        } else {
            hard_stops.push(format!("Σ boms.{col}: DuckDB {d}, SQLite {l}"));
        }
    }

    for col in BOM_COLUMNS {
        typeof_sweep(dst, "boms", col, "text", checks, hard_stops)?;
    }
    Ok(())
}

fn reconcile_routings(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_routings_duckdb(src)?;
    let lite: BTreeMap<String, RoutingRow> = read_routings_sqlite(dst)?
        .into_iter()
        .map(|r| (key_of(&r.text), r))
        .collect();

    let mut drift = 0usize;
    for r in &duck {
        let key = key_of(&r.text);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("routing op {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        drift += compare_text(
            "routing op",
            ROUTING_TEXT_COLUMNS,
            &key,
            &r.text,
            &got.text,
            hard_stops,
        );
        if got.sequence != r.sequence {
            hard_stops.push(format!(
                "routing op {key}: sequence DuckDB {}, SQLite {}",
                r.sequence, got.sequence
            ));
            drift += 1;
        }
        if got.est_time_min != r.est_time_min {
            hard_stops.push(format!(
                "routing op {key}: est_time_min DuckDB {:?}, SQLite {:?}",
                r.est_time_min, got.est_time_min
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every routings column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            ROUTING_TEXT_COLUMNS.len() + 2
        ));
    }

    // **The money fold.** `est_cost_huf` is the one money column in the tree
    // that is R2 rather than R1 (§3.2 B), so it folds over `Decimal` exactly
    // like a quantity — and never as a SQL `SUM`, which on a `TEXT` column
    // coerces to `REAL` (§3.4 / T-8).
    for (col, i) in ROUTING_DECIMAL_COLUMNS.iter().zip(ROUTING_DECIMAL_INDICES) {
        let d = fold_decimal(duck.iter().filter_map(|r| r.text[*i].as_deref()))?;
        let l = fold_decimal(lite.values().filter_map(|r| r.text[*i].as_deref()))?;
        if d == l {
            checks.push(format!(
                "✓ Σ routings.{col} = {d} (Decimal fold in Rust on both sides — this is MONEY, \
                 and it never touches a SQL SUM)"
            ));
        } else {
            hard_stops.push(format!("Σ routings.{col}: DuckDB {d}, SQLite {l}"));
        }
    }

    let d = fold_i64(duck.iter().filter_map(|r| r.est_time_min), "est_time_min")?;
    let l = fold_i64(lite.values().filter_map(|r| r.est_time_min), "est_time_min")?;
    if d == l {
        checks.push(format!(
            "✓ Σ routings.est_time_min = {d} (i64 fold in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!("Σ routings.est_time_min: DuckDB {d}, SQLite {l}"));
    }

    for col in ROUTING_TEXT_COLUMNS {
        typeof_sweep(dst, "routings", col, "text", checks, hard_stops)?;
    }
    for col in ["sequence", "est_time_min"] {
        typeof_sweep(dst, "routings", col, "integer", checks, hard_stops)?;
    }
    Ok(())
}

fn reconcile_revisions(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_revisions_duckdb(src)?;
    let lite: BTreeMap<String, RevisionRow> = read_revisions_sqlite(dst)?
        .into_iter()
        .map(|v| (key_of(&v.text), v))
        .collect();

    let mut drift = 0usize;
    for v in &duck {
        let key = key_of(&v.text);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "bom revision {key} is missing on the SQLite side — this is the row a released \
                 WO's bom_rev_id points at, i.e. the traceability answer to 'what BOM was this \
                 batch built to'"
            ));
            drift += 1;
            continue;
        };
        drift += compare_text(
            "bom revision",
            REVISION_TEXT_COLUMNS,
            &key,
            &v.text,
            &got.text,
            hard_stops,
        );
        if got.rev_number != v.rev_number {
            hard_stops.push(format!(
                "bom revision {key}: rev_number DuckDB {}, SQLite {}",
                v.rev_number, got.rev_number
            ));
            drift += 1;
        }
        if got.line_count != v.line_count {
            hard_stops.push(format!(
                "bom revision {key}: line_count DuckDB {}, SQLite {}",
                v.line_count, got.line_count
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every bom_revisions column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            REVISION_TEXT_COLUMNS.len() + 2
        ));
    }

    let d = fold_i64(duck.iter().map(|v| v.line_count), "line_count")?;
    let l = fold_i64(lite.values().map(|v| v.line_count), "line_count")?;
    if d == l {
        checks.push(format!(
            "✓ Σ bom_revisions.line_count = {d} (i64 fold in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ bom_revisions.line_count: DuckDB {d}, SQLite {l}"
        ));
    }

    for col in REVISION_TEXT_COLUMNS {
        typeof_sweep(dst, "bom_revisions", col, "text", checks, hard_stops)?;
    }
    for col in ["rev_number", "line_count"] {
        typeof_sweep(dst, "bom_revisions", col, "integer", checks, hard_stops)?;
    }
    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// For `routings.est_cost_huf` this is the assertion that the tree's one
/// R2-money column really is `TEXT`: a `'real'` here would be F-6a's
/// float-money class arriving through the exception §3.2 B granted.
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
             money or quantity column, reinstates the float this migration exists to remove"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decimal index consts must name the columns they claim to, because
    /// the validation, the fold and the `CAST` all index one list by the other
    /// and an `expect` in a migrator is a boot abort.
    #[test]
    fn the_decimal_indices_name_the_columns_they_claim_to() {
        for (cols, idx, list, what) in [
            (
                WORK_ORDER_DECIMAL_COLUMNS,
                WORK_ORDER_DECIMAL_INDICES,
                WORK_ORDER_TEXT_COLUMNS,
                "work_orders",
            ),
            (
                BOM_DECIMAL_COLUMNS,
                BOM_DECIMAL_INDICES,
                BOM_COLUMNS,
                "boms",
            ),
            (
                ROUTING_DECIMAL_COLUMNS,
                ROUTING_DECIMAL_INDICES,
                ROUTING_TEXT_COLUMNS,
                "routings",
            ),
        ] {
            assert_eq!(cols.len(), idx.len(), "{what}");
            for (c, i) in cols.iter().zip(idx) {
                assert_eq!(
                    list[*i], *c,
                    "{what}: index {i} must name {c}; a wrong index would validate and fold the \
                     WRONG column while everything still looked green"
                );
            }
        }
    }

    /// Every table's key is at index 0, which is what [`key_of`] assumes and
    /// what every `ORDER BY` in this module sorts on.
    #[test]
    fn every_table_carries_its_primary_key_first() {
        assert_eq!(WORK_ORDER_TEXT_COLUMNS[0], "wo_id");
        assert_eq!(BOM_COLUMNS[0], "bom_line_id");
        assert_eq!(ROUTING_TEXT_COLUMNS[0], "routing_op_id");
        assert_eq!(REVISION_TEXT_COLUMNS[0], "bom_rev_id");
    }

    /// The four tables the carry, the gate and `FAMILY_TABLES` walk are the
    /// same four, in the same order.
    #[test]
    fn the_family_table_list_matches_the_carry() {
        assert_eq!(
            FAMILY_TABLES,
            &["work_orders", "boms", "routings", "bom_revisions"]
        );
    }
}
