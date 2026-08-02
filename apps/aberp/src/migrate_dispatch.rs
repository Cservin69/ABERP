//! ADR-0108 Step 7 Part F — **the dispatch / shipment family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM) and E (QA/QC)
//! are already on this branch, so this is **Part F**. Named rather than silently
//! renumbered — the same call Parts D and E made.
//!
//! # The family — grepped, not recalled
//!
//! §3.2's own rule is that a table name in this plan is measured. A tree-wide
//! sweep of every `CREATE TABLE` in `apps/`, `crates/` and `modules/` returns
//! **no `shipments`, no `delivery_notes`, no `dispatch_lines`, no packing-list
//! table of any name**. ADR-0064 §"Out of scope" says why: partial shipments are
//! not modelled, so there is one dispatch per WO and therefore no line table.
//! The family is **two tables, two owning modules, 21 columns**:
//!
//! | table | built by | columns | the numbers in it |
//! |---|---|---|---|
//! | `dispatches` | `aberp_dispatch::ensure_schema` (V001) | 12 | none |
//! | `wo_part_marks` | `aberp::part_marking::ensure_schema` | 9 | `unit_index` (§3.2 F) |
//!
//! # Why `wo_part_marks` is in this family and not left behind
//!
//! **Because rule 14's fused boundary is measured here, not assumed.**
//! `part_marking.rs:504–505` (`trace_customer`) joins the two tables in **one
//! SQL statement** — `FROM wo_part_marks pm JOIN dispatches d ON d.wo_id =
//! pm.wo_id AND d.tenant_id = pm.tenant_id WHERE … d.state = 'shipped'` — and
//! `:496` calls `aberp_dispatch::ensure_schema` to guarantee the joined table
//! exists. A JOIN cannot span two engines. Crossing `dispatches` alone would
//! split a pair that one statement reads together, which is exactly the
//! half-migrated shape rule 14 names as *worse than unmigrated*.
//!
//! `wo_part_marks` is unclaimed by every prior Part: Part D crossed the four
//! tables the `aberp-work-orders` crate owns (`work_orders`, `boms`, `routings`,
//! `bom_revisions`) and this one is built by `apps/aberp/src/part_marking.rs`.
//! So it is not being taken from another family — it has simply not crossed.
//!
//! ⚠ **The fusion runs further than this commit does, and the §9 row says so.**
//! `repository.rs:249/726/788` read `work_orders` and `:283` reads `partners` in
//! statements that also touch `dispatches`. Both of those families crossed as
//! **migrator halves**, so every one of those statements still executes on a
//! single DuckDB connection and nothing is split today. What it means is that
//! the *Rust-side crossing* of dispatch cannot happen before work-orders' and
//! partners' — a sequencing constraint on the cutover, recorded in §9 rather
//! than discovered there.
//!
//! # No money, no quantity, no float, no boolean — measured
//!
//! §3.2's A, B, C, D, E, G and H-boolean categories are **all empty here**, and
//! that is a measurement rather than a hedge: neither table declares a
//! `DECIMAL`, a `DOUBLE`, a `BOOLEAN`, a `BIGINT` or a `BLOB`. Twenty of the 21
//! columns are `VARCHAR`; the twenty-first is `wo_part_marks.unit_index`, an
//! `INTEGER` per-unit ordinal — §3.2 F, `INTEGER` → `INTEGER`, unchanged.
//!
//! R1 and R2 therefore have nothing to bite on, and neither does §3.2 E's
//! [`finite_measurement`](crate::migrate_quality::finite_measurement): **this
//! family carries no float at all**, so Part E's NaN-is-NULL finding has no site
//! here. The unit pin [`tests::no_column_in_this_family_is_money_or_quantity`]
//! reds if a later session adds a freight cost, a shipped quantity or a weight
//! without deciding its representation — which is the moment R1/R2 start
//! applying to this family.
//!
//! The gate's arm is correspondingly **per column over all 21** rather than
//! money-first. On a dispatch record the payload is the record itself: which
//! part UID, with which heat lot, went to which customer on which date. That is
//! the defense/aerospace traceability chain `part_marking.rs`'s shipment gate
//! exists to protect, and "the important columns" would have been the wrong
//! subset to pick.
//!
//! # The refusal this family needs, and why it is not Part E's
//!
//! **`wo_part_marks` has no `PRIMARY KEY`, no `UNIQUE`, and no index of any
//! kind.** Its identity is the natural composite `(tenant_id, wo_id,
//! unit_index)` — `part_marking.rs:143` states the posture — and uniqueness is
//! enforced only in Rust, by a `COUNT(*)` inside the mark-parts transaction
//! (`:187`). Nothing in the DDL stops a duplicate.
//!
//! That matters because **the gate's per-row arm keys a `BTreeMap` on exactly
//! that composite**, the way every family since Step 5 has. A duplicate key
//! collapses two SQLite rows onto one map entry: the row **count** still
//! matches, the `typeof` sweep still passes, and the per-row comparison silently
//! checks one row twice instead of two rows once. That is the 14%-silently-
//! skipped shape rule 11 exists to stop, and it is invisible to every other
//! check in the set.
//!
//! So [`unique_natural_keys`] refuses a duplicate **before** the carry, naming
//! the table and the key — the same shape [`finite_measurement`] has for §3.2 E
//! and [`canonical_decimal`](crate::migrate_billing::canonical_decimal) has for
//! R2. It is the family's refusal arm, and the family test requires it to fire.
//!
//! It is applied to `dispatches` as well, where `dsp_id` **is** a DuckDB
//! `PRIMARY KEY`: there it costs one `BTreeSet` insert and asserts that the
//! source's own key held. One function, two uses — not two mechanisms.
//!
//! ⚠ **The same un-guarded composite-key shape exists in two already-landed
//! families** — `ncr_transitions` (`ncr_id#seq`, Part E) and `invoice_line`
//! (`invoice_id#ordinal`, Step 5) both key a `BTreeMap` on a composite that no
//! DDL enforces. **Not fixed here** (rule 3: this commit crosses one family, it
//! does not improve adjacent ones); recorded as its own §9 row.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5, 7B, 7C, 7D and 7E, and Ervin's same constraint: **the
//! migrator half only.** DuckDB remains the source of truth.
//! `crates/aberp-dispatch/src` and `apps/aberp/src/part_marking.rs` are
//! untouched — every one of their queries still runs against a DuckDB
//! `Connection`, and `migrations/V001__dispatch.sql` still builds the DuckDB
//! schema.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
//! arithmetic sites contain no dispatch statement and §6.3's row for this family
//! says "row-by-row carry" with no rebuild. The only aggregate anywhere in the
//! two modules is `COUNT(*)` (`repository.rs:760/787/811`,
//! `part_marking.rs:187`), which counts rows rather than folding a money, rate
//! or quantity column, so the T-8 pending-fold register is unchanged by this
//! commit — nothing joined it and nothing left it.
//!
//! **Nor does this family carry an M11-shaped hazard**: measured, not assumed —
//! `LOWER(` and `LIKE` both return **zero** hits against both tables across
//! `crates/aberp-dispatch/src` and `apps/aberp/src/part_marking.rs`, so the
//! ASCII-`LOWER()` fold that partners must fix at its cutover has no site here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use aberp_db::engine::{Connection as SqliteConn, ToSql};

use crate::migrate_billing::fold_i64;

// ---------------------------------------------------------------------------
// The STRICT DDL
// ---------------------------------------------------------------------------

/// `dispatches` as `V001__dispatch.sql` creates it, `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H) on all twelve columns. Both of V001's indexes
/// carry across; both key on base columns.
///
/// S234 / ADR-0064's `[[no-sql-specific]]` posture is preserved verbatim: there
/// is **no** DB-level `CHECK` on `state` or `carrier_kind`, and **no** DB-level
/// `UNIQUE` on `(tenant_id, wo_id)`. The closed vocabularies live in
/// `aberp_dispatch::types` and the one-dispatch-per-WO rule is an
/// application-layer gate inside `create_dispatch`'s transaction. `STRICT`
/// constrains storage classes, not vocabularies, and porting either invariant
/// into the DDL here would fork the two schemas — V001's own comment rules both
/// out by name.
const SQLITE_DISPATCHES_DDL: &str = "
CREATE TABLE IF NOT EXISTS dispatches (
    dsp_id             TEXT NOT NULL PRIMARY KEY,
    tenant_id          TEXT NOT NULL,
    wo_id              TEXT NOT NULL,
    partner_id         TEXT NOT NULL,
    state              TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    shipped_at         TEXT,
    cancelled_at       TEXT,
    carrier_kind       TEXT,
    tracking_number    TEXT,
    spawned_invoice_id TEXT,
    notes              TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS dispatches_tenant_state_created_idx
    ON dispatches (tenant_id, state, created_at);
CREATE INDEX IF NOT EXISTS dispatches_tenant_wo_idx
    ON dispatches (tenant_id, wo_id);
";

/// `wo_part_marks` as `part_marking.rs`'s `PART_MARKS_SCHEMA_SQL` creates it,
/// `STRICT`.
///
/// `unit_index` is §3.2 F's `INTEGER` → `INTEGER`. Everything else is
/// `VARCHAR` → `TEXT`; `heat_lot_reference` is the one nullable column.
///
/// **No `PRIMARY KEY` and no index, matching the DuckDB schema verbatim** —
/// `part_marking.rs:141–143` states the posture: a natural composite key, no
/// surrogate id, no `CHECK`, no `DEFAULT`. Adding a `PRIMARY KEY` here to make
/// the migrator's life easier would be an invariant moving into the DDL on one
/// engine and not the other; [`unique_natural_keys`] enforces it in Rust
/// instead, where the product already enforces it.
const SQLITE_WO_PART_MARKS_DDL: &str = "
CREATE TABLE IF NOT EXISTS wo_part_marks (
    tenant_id           TEXT    NOT NULL,
    wo_id               TEXT    NOT NULL,
    unit_index          INTEGER NOT NULL,
    part_uid            TEXT    NOT NULL,
    serial_number       TEXT    NOT NULL,
    data_matrix_payload TEXT    NOT NULL,
    heat_lot_reference  TEXT,
    marked_at_utc       TEXT    NOT NULL,
    marked_by_operator  TEXT    NOT NULL
) STRICT;
";

/// Build the whole family's SQLite schema.
///
/// **There is no `ensure_columns` ladder in this family, and that is measured
/// rather than assumed:** `ALTER TABLE` returns zero hits against both tables
/// across `apps/` and `crates/`. Each is created whole by its `CREATE TABLE IF
/// NOT EXISTS` and neither has ever been widened. So M8's post-condition has
/// nothing to exercise here — which is exactly why presence is held per
/// **table** (below) rather than per family: the schema evolution in this family
/// happened by *adding a table* (S432's `wo_part_marks`, three sprints after
/// S234's `dispatches`), not by adding columns.
pub fn ensure_dispatch_schema(conn: &SqliteConn) -> Result<()> {
    ensure_dispatches_table(conn)?;
    ensure_wo_part_marks_table(conn)?;
    Ok(())
}

fn ensure_dispatches_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_DISPATCHES_DDL)?;
    Ok(())
}

fn ensure_wo_part_marks_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_WO_PART_MARKS_DDL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The column lists — one list per table drives the SELECT, the INSERT, the
// per-row equality and the typeof() sweep, so the four can never drift apart.
// ---------------------------------------------------------------------------

/// `dispatches`'s 12 columns, every one `TEXT`. Six of them are nullable, and
/// the fixture exercises both states of each.
const DISPATCH_COLUMNS: &[&str] = &[
    "dsp_id",
    "tenant_id",
    "wo_id",
    "partner_id",
    "state",
    "created_at",
    "shipped_at",
    "cancelled_at",
    "carrier_kind",
    "tracking_number",
    "spawned_invoice_id",
    "notes",
];

/// `wo_part_marks`'s 8 `TEXT` columns; `unit_index` is the 9th and is typed
/// separately (§3.2 F).
///
/// **`tenant_id` and `wo_id` lead, in that order**, because [`mark_key`] builds
/// `tenant_id#wo_id#unit_index` from indices 0 and 1 plus the integer — the
/// three-part natural key `part_marking.rs` writes and reads by.
const MARK_TEXT_COLUMNS: &[&str] = &[
    "tenant_id",
    "wo_id",
    "part_uid",
    "serial_number",
    "data_matrix_payload",
    "heat_lot_reference",
    "marked_at_utc",
    "marked_by_operator",
];

// ---------------------------------------------------------------------------
// The family's one refusal
// ---------------------------------------------------------------------------

/// Refuse a duplicate natural key, naming the table and the key.
///
/// **This is the only way this family can lose a row silently.** Every column in
/// it is `TEXT` or `INTEGER`, both of which cross exactly, so there is no
/// representation hazard of the kind §3.2 E and R2 guard against. What there is
/// instead is a *keying* hazard, and it is specific to `wo_part_marks`: the
/// table has no `PRIMARY KEY`, no `UNIQUE` and no index, so nothing in the DDL
/// stops two rows sharing `(tenant_id, wo_id, unit_index)`.
///
/// The gate's per-row arm keys a `BTreeMap` on that composite. Two rows with one
/// key collapse to one map entry, and then:
///
/// * the row **count** check still passes — both rows crossed;
/// * the `typeof` sweep still passes — the storage classes are right;
/// * the per-row comparison checks the surviving row **twice** and the shadowed
///   row **never**.
///
/// A green gate over a row nobody compared. Refusing is rule 11; carrying it
/// because every individual check is green would be verifying the extraction
/// against itself (B4) one level down.
///
/// Called on the **DuckDB read side**, before anything is bound, so the message
/// names the source of truth's defect rather than the copy's symptom.
pub fn unique_natural_keys(keys: &[String], table: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for key in keys {
        if !seen.insert(key) {
            bail!(
                "{table} has two rows with the natural key {key}, and the table has no PRIMARY \
                 KEY, no UNIQUE and no index to have stopped it. The reconciliation gate keys its \
                 per-row comparison on exactly this composite, so a duplicate collapses two rows \
                 onto one map entry: the row count still matches, the typeof sweep still passes, \
                 and one row is compared twice while the other is never compared at all. Refusing \
                 to carry it rather than reporting PASS over a row nobody checked (ADR-0108 §6.3, \
                 CLAUDE.md rule 11)"
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One `wo_part_marks` row: the 8 `TEXT` columns plus §3.2 F's `unit_index`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkRow {
    text: Vec<Option<String>>,
    unit_index: i64,
}

/// What the dispatch carry did. As everywhere else in this migrator these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one of
/// them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchCarry {
    pub dispatches: u64,
    pub wo_part_marks: u64,
}

// ---------------------------------------------------------------------------
// Presence — per TABLE, because TWO modules build these two tables
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
const FAMILY_TABLES: &[&str] = &["dispatches", "wo_part_marks"];

/// Does this DuckDB database have **either** of the family's two tables?
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
/// rather than merely safer: two separate modules build the two tables
/// (`aberp_dispatch`'s V001 at S234, `part_marking.rs`'s batch at S432), so a
/// database predating S432 legitimately has `dispatches` and no `wo_part_marks`.
/// Creating an empty SQLite table for one the source does not have would
/// manufacture exactly the asymmetry the gate exists to detect.
pub fn carry_dispatch(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<DispatchCarry> {
    let dispatches = if present_duckdb(src, "dispatches")? {
        ensure_dispatches_table(dst)?;
        read_dispatches_duckdb(src)?
    } else {
        Vec::new()
    };
    let marks = if present_duckdb(src, "wo_part_marks")? {
        ensure_wo_part_marks_table(dst)?;
        read_marks_duckdb(src)?
    } else {
        Vec::new()
    };

    let dispatches_insert = insert_sql("dispatches", DISPATCH_COLUMNS, &[]);
    let marks_insert = insert_sql("wo_part_marks", MARK_TEXT_COLUMNS, &["unit_index"]);

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the dispatch carry transaction: {e}"))?;

    for row in &dispatches {
        let binds: Vec<&dyn ToSql> = row.iter().map(|v| v as &dyn ToSql).collect();
        tx.execute(&dispatches_insert, binds.as_slice())?;
    }
    for m in &marks {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(MARK_TEXT_COLUMNS.len() + 1);
        for v in &m.text {
            binds.push(v);
        }
        binds.push(&m.unit_index);
        tx.execute(&marks_insert, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(DispatchCarry {
        dispatches: dispatches.len() as u64,
        wo_part_marks: marks.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// `ORDER BY` the natural key so both engines hand back the same order, and so a
/// per-table read is deterministic for the gate as well as for the carry.
///
/// **There is no `CAST(… AS VARCHAR)` anywhere in this module** and that is a
/// statement about the family rather than an omission: no column in it is
/// `DECIMAL` on DuckDB, because there is no money and no quantity here.
fn dispatches_select() -> String {
    format!(
        "SELECT {} FROM dispatches ORDER BY dsp_id ASC",
        DISPATCH_COLUMNS.join(", ")
    )
}

fn marks_select() -> String {
    format!(
        "SELECT {}, unit_index FROM wo_part_marks ORDER BY tenant_id ASC, wo_id ASC, \
         unit_index ASC",
        MARK_TEXT_COLUMNS.join(", ")
    )
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// each read is written once per engine.
///
/// **The DuckDB side validates; the SQLite side does not.** A duplicate key is a
/// property of the source, and [`unique_natural_keys`] must name the source's
/// defect rather than the copy's symptom.
fn read_dispatches_duckdb(conn: &duckdb::Connection) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&dispatches_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(DISPATCH_COLUMNS.len());
        for i in 0..DISPATCH_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(text)
    })?;
    let rows: Vec<Vec<Option<String>>> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    unique_natural_keys(
        &rows.iter().map(|r| dispatch_key(r)).collect::<Vec<_>>(),
        "dispatches",
    )?;
    Ok(rows)
}

fn read_dispatches_sqlite(conn: &SqliteConn) -> Result<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&dispatches_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(DISPATCH_COLUMNS.len());
        for i in 0..DISPATCH_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(text)
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_marks_duckdb(conn: &duckdb::Connection) -> Result<Vec<MarkRow>> {
    let mut stmt = conn.prepare(&marks_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(MARK_TEXT_COLUMNS.len());
        for i in 0..MARK_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(MarkRow {
            text,
            unit_index: r.get(MARK_TEXT_COLUMNS.len())?,
        })
    })?;
    let rows: Vec<MarkRow> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    unique_natural_keys(
        &rows.iter().map(mark_key).collect::<Vec<_>>(),
        "wo_part_marks",
    )?;
    Ok(rows)
}

fn read_marks_sqlite(conn: &SqliteConn) -> Result<Vec<MarkRow>> {
    let mut stmt = conn.prepare(&marks_select())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(MARK_TEXT_COLUMNS.len());
        for i in 0..MARK_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(MarkRow {
            text,
            unit_index: r.get(MARK_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// `dispatches` carries its key at index 0 — `dsp_id`, a real `PRIMARY KEY` on
/// both engines.
fn dispatch_key(text: &[Option<String>]) -> String {
    text[0].clone().unwrap_or_default()
}

/// `wo_part_marks` has **no** primary key: its identity is the three-part
/// natural composite (`tenant_id`, `wo_id`, `unit_index`)
/// (`part_marking.rs:141–143`). Same shape as Step 5's `invoice_line` key
/// (`invoice_id#ordinal`) and Part E's `ncr_transitions` (`ncr_id#seq`), one
/// component wider — and unlike either of those, nothing at all enforces it,
/// which is what [`unique_natural_keys`] is for.
fn mark_key(row: &MarkRow) -> String {
    format!(
        "{}#{}#{}",
        row.text[0].clone().unwrap_or_default(),
        row.text[1].clone().unwrap_or_default(),
        row.unit_index
    )
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per table, per row, over **every** column. Every
/// number here is re-derived from disk by the gate (B4).
pub fn reconcile_dispatch(
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
            "✓ dispatch family absent on BOTH sides — nothing to reconcile (a database that has \
             never created a dispatch or marked a part is a legitimate shape; an asymmetry is not)"
                .to_string(),
        );
        return Ok(());
    }

    if both.contains(&"dispatches") {
        reconcile_dispatches(src, dst, checks, hard_stops)?;
    }
    if both.contains(&"wo_part_marks") {
        reconcile_marks(src, dst, checks, hard_stops)?;
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

/// `dispatches` — all twelve columns compared. No fold and no `typeof` class
/// other than `'text'` could catch a drift instead, which is why the arm
/// compares every column rather than the "important" ones: on a dispatch record
/// `partner_id` and `tracking_number` are the traceability chain, and neither
/// participates in any aggregate anywhere.
fn reconcile_dispatches(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_dispatches_duckdb(src)?;
    let lite: BTreeMap<String, Vec<Option<String>>> = read_dispatches_sqlite(dst)?
        .into_iter()
        .map(|r| (dispatch_key(&r), r))
        .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = dispatch_key(row);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("dispatch {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        drift += compare_text("dispatch", DISPATCH_COLUMNS, &key, row, got, hard_stops);
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every dispatches column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            DISPATCH_COLUMNS.len()
        ));
    }

    for col in DISPATCH_COLUMNS {
        typeof_sweep(dst, "dispatches", col, "text", checks, hard_stops)?;
    }
    Ok(())
}

/// `wo_part_marks` — keyed on the three-part composite
/// (`tenant_id`, `wo_id`, `unit_index`).
///
/// The `Σ unit_index` fold is an `i64` fold in Rust, never a SQL `SUM`: `SUM`
/// over `INTEGER` raises on i64 overflow, and the rule that keeps arithmetic out
/// of SQL does not get an exception for a column that happens to be small (§3.4
/// / T-8).
fn reconcile_marks(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_marks_duckdb(src)?;
    let lite: BTreeMap<String, MarkRow> = read_marks_sqlite(dst)?
        .into_iter()
        .map(|m| (mark_key(&m), m))
        .collect();

    let mut drift = 0usize;
    for m in &duck {
        let key = mark_key(m);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "part mark {key} is missing on the SQLite side — the mark IS the unit's \
                 defense/aerospace traceability record, and a gap in it means a shipped part \
                 whose heat lot can no longer be established"
            ));
            drift += 1;
            continue;
        };
        drift += compare_text(
            "part mark",
            MARK_TEXT_COLUMNS,
            &key,
            &m.text,
            &got.text,
            hard_stops,
        );
        // Part of the key, so a mismatch here cannot happen through the lookup
        // above — it is compared anyway, because `mark_key` reading the wrong
        // field would otherwise pair two rows silently.
        if got.unit_index != m.unit_index {
            hard_stops.push(format!(
                "part mark {key}: unit_index DuckDB {}, SQLite {}",
                m.unit_index, got.unit_index
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every wo_part_marks column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            MARK_TEXT_COLUMNS.len() + 1
        ));
    }

    let d = fold_i64(duck.iter().map(|m| m.unit_index), "unit_index")?;
    let l = fold_i64(lite.values().map(|m| m.unit_index), "unit_index")?;
    if d == l {
        checks.push(format!(
            "✓ Σ wo_part_marks.unit_index = {d} (i64 fold in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ wo_part_marks.unit_index: DuckDB {d}, SQLite {l}"
        ));
    }

    for col in MARK_TEXT_COLUMNS {
        typeof_sweep(dst, "wo_part_marks", col, "text", checks, hard_stops)?;
    }
    typeof_sweep(
        dst,
        "wo_part_marks",
        "unit_index",
        "integer",
        checks,
        hard_stops,
    )?;
    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// For `unit_index` this is the assertion that §3.2 F's `INTEGER` → `INTEGER`
/// actually held: a `'text'` there would mean the per-unit ordinal crossed as a
/// string and every later ordering would be lexicographic — `'10' < '9'`.
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
             distinct storage classes, so a wrong one here silently breaks lookups and, on an \
             ordinal column, turns a numeric ordering into a lexicographic one"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::{COUNT_INT, TEXT};

    /// Every table's carry list leads with the columns its key is built from.
    #[test]
    fn every_table_carries_its_key_first() {
        assert_eq!(DISPATCH_COLUMNS[0], "dsp_id");
        // The composite one: `mark_key` builds `tenant_id#wo_id#unit_index` from
        // indices 0 and 1 plus the separately-typed integer, and `marks_select`
        // orders on the same three in the same order.
        assert_eq!(MARK_TEXT_COLUMNS[0], "tenant_id");
        assert_eq!(MARK_TEXT_COLUMNS[1], "wo_id");
        assert!(marks_select().contains("ORDER BY tenant_id ASC, wo_id ASC, unit_index ASC"));
    }

    /// The two tables the carry, the gate and `FAMILY_TABLES` walk are the same
    /// two, in the same order.
    #[test]
    fn the_family_table_list_matches_the_carry() {
        assert_eq!(FAMILY_TABLES, &["dispatches", "wo_part_marks"]);
    }

    /// **The family has no money and no quantity**, and the DDL is what proves
    /// it rather than the module docs: not one column in it is declared with a
    /// type that §3 reserves for an exact value, and not one is a float.
    ///
    /// This is the pin that fails if a later session adds a freight cost, a
    /// shipped quantity or a package weight to a dispatch without deciding its
    /// representation — which is the moment R1/R2 start applying to this family.
    #[test]
    fn no_column_in_this_family_is_money_or_quantity() {
        for ddl in [SQLITE_DISPATCHES_DDL, SQLITE_WO_PART_MARKS_DDL] {
            for banned in [
                "DECIMAL", "NUMERIC", "REAL", "DOUBLE", "_minor", "price", "cost", "_huf", "qty",
                "weight",
            ] {
                assert!(
                    !ddl.to_lowercase().contains(&banned.to_lowercase()),
                    "the dispatch family carries no money and no quantity; {banned:?} appearing \
                     in its DDL means that stopped being true and R1/R2 now have something to \
                     bite on:\n{ddl}"
                );
            }
        }
    }

    /// The declared types this module uses are the vocabulary constants, spelled
    /// the same way, and both tables are `STRICT`.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        assert_eq!(COUNT_INT, "INTEGER");
        for (ddl, table) in [
            (SQLITE_DISPATCHES_DDL, "dispatches"),
            (SQLITE_WO_PART_MARKS_DDL, "wo_part_marks"),
        ] {
            assert!(!ddl.contains("VARCHAR"), "{table} must not declare VARCHAR");
            assert!(!ddl.contains("BIGINT"), "{table} must not declare BIGINT");
            assert!(ddl.contains("STRICT"), "{table} must be STRICT");
        }
        assert!(SQLITE_WO_PART_MARKS_DDL.contains("unit_index          INTEGER NOT NULL"));
    }

    /// **`wo_part_marks` must not gain a `PRIMARY KEY`**, because the DuckDB
    /// table has none and the two schemas must agree. `unique_natural_keys` is
    /// what enforces the key instead — this pin is what stops a later session
    /// "tidying" the DDL and silently deleting the refusal's reason to exist.
    #[test]
    fn wo_part_marks_has_no_primary_key_on_either_engine() {
        assert!(
            !SQLITE_WO_PART_MARKS_DDL.contains("PRIMARY KEY"),
            "the DuckDB table has no PRIMARY KEY (part_marking.rs:141-143); adding one here \
             would fork the two schemas and move an invariant into the DDL on one engine only"
        );
        assert!(
            !SQLITE_WO_PART_MARKS_DDL.contains("UNIQUE"),
            "same argument as the PRIMARY KEY above"
        );
        // And `dispatches` DOES have one, matching V001.
        assert!(SQLITE_DISPATCHES_DDL.contains("dsp_id             TEXT NOT NULL PRIMARY KEY"));
    }

    /// [`unique_natural_keys`] accepts a unique set and refuses a duplicate by
    /// name — both arms, in the smallest form.
    #[test]
    fn a_duplicate_natural_key_is_refused_by_name() {
        let unique: Vec<String> = ["test#wo-01#0", "test#wo-01#1", "test#wo-02#0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        unique_natural_keys(&unique, "wo_part_marks").expect("distinct keys must be accepted");

        let dup: Vec<String> = ["test#wo-01#0", "test#wo-01#1", "test#wo-01#0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let e = unique_natural_keys(&dup, "wo_part_marks")
            .expect_err("a duplicate natural key must be refused");
        let msg = e.to_string();
        assert!(msg.contains("wo_part_marks"), "must name the table: {msg}");
        assert!(msg.contains("test#wo-01#0"), "must name the key: {msg}");
    }

    /// `mark_key` reads all three components, so a change in any one of them
    /// produces a different key. A key built from two of the three would pair
    /// two genuinely different rows and the gate would compare the wrong pair.
    #[test]
    fn the_mark_key_uses_all_three_components() {
        let row = |tenant: &str, wo: &str, unit: i64| MarkRow {
            text: vec![
                Some(tenant.to_string()),
                Some(wo.to_string()),
                Some("uid".into()),
                Some("sn".into()),
                Some("dmx".into()),
                None,
                Some("2026-01-01T00:00:00Z".into()),
                Some("op".into()),
            ],
            unit_index: unit,
        };
        let base = mark_key(&row("test", "wo-01", 0));
        assert_eq!(base, "test#wo-01#0");
        assert_ne!(base, mark_key(&row("other", "wo-01", 0)));
        assert_ne!(base, mark_key(&row("test", "wo-02", 0)));
        assert_ne!(base, mark_key(&row("test", "wo-01", 1)));
    }
}
