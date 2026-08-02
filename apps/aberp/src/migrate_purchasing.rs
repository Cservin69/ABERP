//! ADR-0108 Step 7 Part G — **the purchasing / purchase-order family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM), E (QA/QC)
//! and F (dispatch) are already on this branch, so this is **Part G**. Named
//! rather than silently renumbered — the same call Parts D, E and F made. §7's
//! own family list puts purchasing after dispatch and before email/relay.
//!
//! # The family — grepped, not recalled
//!
//! §3.2's rule is that a table name in this plan is measured. A tree-wide sweep
//! of every `CREATE TABLE` in `apps/`, `crates/` and `modules/` returns **four**
//! purchasing tables, all four built by one `CREATE TABLE IF NOT EXISTS` batch
//! in one module (`apps/aberp/src/purchasing.rs:171–225`):
//!
//! | table | columns | the numbers in it |
//! |---|---|---|
//! | `purchase_orders` | 18 | 3 × money (§3.2 A), `vat_rate_pct` (§3.2 F) |
//! | `purchase_order_lines` | 12 | 2 × money (§3.2 A), 3 × §3.2 F, 1 × `BOOLEAN` |
//! | `purchase_order_receipts` | 12 | 1 × §3.2 F, 1 × `BOOLEAN` |
//! | `po_number_state` | 4 | 2 × §3.2 F |
//!
//! **There is no supplier-price table and no vendor-price table in this family**,
//! and that is a measurement rather than an omission. §6.3's quoting row names
//! `supplier_prices`; a `CREATE TABLE` sweep returns **zero hits for it anywhere
//! in the tree** — ADR-0104's supplier price ingestion writes
//! `products.unit_price_minor`, which crossed with Part C. A session that went
//! looking for `supplier_prices` here would find nothing and could read the zero
//! hits as "already done", which is the silent-skip shape rule 11 exists to stop.
//!
//! # What is NOT in this family, and why — rule 14's boundary, measured
//!
//! **`avl_vendors` stays out.** `create_po` reads it (`purchasing.rs:600–611`,
//! `resolve_avl`) — but it reads it as its **own separate `SELECT`** on the
//! shared writer guard, *before* the PO transaction opens. `JOIN` returns
//! **zero** hits across `purchasing.rs`, and every one of the module's fifteen
//! SQL statements names exactly one purchasing table. Part F's boundary test was
//! *one statement spanning two tables* (`trace_customer`'s JOIN); nothing here
//! meets it. Same for `ncrs` (`create_ncr` at `:1160`, a cross-module call on
//! the same guard) and for `audit_ledger` (`append_event`, which every family
//! does).
//!
//! ⚠ **That is a boundary for the MIGRATOR half and a sequencing constraint for
//! the cutover, and the two must not be confused.** Those same-guard calls mean
//! the *Rust-side* crossing of purchasing cannot precede `avl_vendors`' — and
//! `avl_vendors` **has not crossed in any Part** — nor quality's, which crossed
//! as a migrator half in Part E. Recorded in §9 rather than discovered there.
//!
//! Three more columns point out of the family and **none of them is a JOIN**:
//! `purchase_orders.vendor_partner_id` → `partners` (Part B),
//! `purchase_order_lines.product_id` → `products` (Part C),
//! `purchase_order_receipts.ncr_id` → `ncrs` (Part E). All three are resolved in
//! Rust by the callers, never in SQL.
//!
//! # Money — §3.2 A, pre-decided by name, and this family is where most of it is
//!
//! **Five money columns, and the ADR already named every one of them:** §3.2 A
//! lists `purchase_order_lines.unit_price_minor` / `line_total_minor` and
//! `purchase_orders.subtotal_minor` / `vat_minor` / `total_minor` as `BIGINT`
//! `i64` → **`INTEGER`, representation unchanged**. So R1 applies and R2 does
//! not: there is no `DECIMAL` and no `DOUBLE` anywhere in the family, and
//! therefore **nothing for [`canonical_decimal`](crate::migrate_billing::canonical_decimal),
//! `canonical_decimal_from_f64` or
//! [`finite_measurement`](crate::migrate_quality::finite_measurement) to guard.**
//!
//! ⚠ **The two quantity columns are INTEGER, not R2 `TEXT`, and that is the one
//! place this family diverges from the shape the other families took.** It is a
//! measurement, not a preference: `purchase_order_lines.quantity`,
//! `.received_quantity` and `purchase_order_receipts.received_quantity` are
//! `BIGINT` on DuckDB and **`i64` in Rust** (`PoLine`/`PoReceiptLine`,
//! `purchasing.rs:267/272/285`) — a whole-unit count, never a fractional one.
//! Part C's five `DOUBLE` quantities became R2 `TEXT` precisely because they were
//! floats; converting an exact `i64` count to a decimal string would *introduce*
//! a representation, not preserve one, and would break the running product's
//! `received_quantity = received_quantity + ?` at cutover. **They cross as
//! §3.2 F `INTEGER`.** Flagged rather than assumed, because §3.2 F's enumerated
//! list does not name them — see the census-gap row in §9.
//!
//! `vat_rate_pct` is likewise §3.2 F, and it is worth one sentence because it
//! differs from the invoice family's: purchasing stores a **whole percent**
//! (`i32`, validated 0..=100 at `purchasing.rs:627`), where billing stores
//! `vat_rate_basis_points`. Two representations of one concept in one tree
//! (rule 7) — **not this commit's to reconcile**, and not a storage hazard
//! either way, since both are exact integers on both engines.
//!
//! # The first two booleans outside QA/QC — §3.2 H
//!
//! `purchase_order_lines.expected_heat_lot_required` and
//! `purchase_order_receipts.inspection_pass` are `BOOLEAN` on DuckDB. §3.2 H:
//! `BOOLEAN` → `INTEGER`, because `rusqlite` binds `bool` ↔ `INTEGER` 0/1
//! natively and `STRICT` has no boolean type. They are read as `bool` and bound
//! as `bool` on both sides, so the carry never sees a string. `inspection_pass`
//! is load-bearing: it is what mints the S439 auto-NCR, so a `"false"` bound as
//! text (which every Rust comparison would then read as `false`) would silently
//! turn every passing incoming inspection into a failure.
//!
//! # The refusal — identity again, and here it can re-issue a PO number
//!
//! **None of the four tables has a `PRIMARY KEY`, a `UNIQUE` or an index.**
//! `purchasing.rs:169–171` states the posture in as many words: *"NO surrogate
//! id (natural prefixed-ULID / `(tenant, year)` PK), NO CHECK / NO DEFAULT, NO
//! index"*. So this family has Part F's keying hazard on **all four** tables,
//! and [`unique_natural_keys`](crate::migrate_dispatch::unique_natural_keys) is
//! reused verbatim rather than re-implemented — its message already says "no
//! PRIMARY KEY, no UNIQUE and no index", which is true of every table here.
//!
//! **`po_number_state` is the one where a duplicate is not merely a gate defect.**
//! Its key is the genuine composite `(tenant_id, year)`, nothing enforces it, and
//! `reserve_po_number` (`purchasing.rs:569–588`) reads it with `rows.next()` —
//! it takes the **first** row and advances that one. Two rows for one
//! `(tenant, year)` therefore means two independent counters issuing **the same
//! `PO-YYYY-NNNN` twice**, which is the exact shape of the invoice-number reuse
//! fixed in v2.33.4 (`c053cd0`). Carrying such a source silently is refused.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5, 7B, 7C, 7D, 7E and 7F, and Ervin's same constraint:
//! **the migrator half only.** DuckDB remains the source of truth.
//! `apps/aberp/src/purchasing.rs` is untouched — every one of its queries still
//! runs against a DuckDB `Connection`, and its `ensure_schema` still builds the
//! DuckDB schema.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
//! arithmetic sites contain no purchasing statement and §6.3's row for
//! "partners / products / **purchasing**" says "row-by-row carry". The module
//! has exactly one piece of SQL-side arithmetic —
//! `UPDATE purchase_order_lines SET received_quantity = received_quantity + ?3`
//! (`purchasing.rs:1109`) — and it is **not** a §3.4 site, because §3.4's class
//! is arithmetic over a column that R2 turns into `TEXT`. This column is
//! `INTEGER` on both engines, so the addition stays exact. The T-8 pending-fold
//! register is **unchanged by this commit**: nothing joined it and nothing left
//! it. (One narrow cutover note on that statement lives in §9; it is about
//! SQLite's integer-overflow behaviour, not about a fold.)
//!
//! **Nor does this family carry an M11-shaped hazard**: measured, not assumed —
//! `LOWER(` and `LIKE` both return **zero** hits across `purchasing.rs`, so the
//! ASCII-`LOWER()` fold that partners must fix at its cutover has no site here.

use std::collections::BTreeMap;

use anyhow::Result;

use aberp_db::engine::{Connection as SqliteConn, ToSql};

use crate::migrate_billing::fold_i64;
use crate::migrate_dispatch::unique_natural_keys;

// ---------------------------------------------------------------------------
// The STRICT DDL — column-for-column, in declaration order, with §3.2's renames
// ---------------------------------------------------------------------------

/// `purchase_orders` as `PURCHASING_SCHEMA_SQL` creates it, `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H) on fourteen columns; the four `BIGINT`/`INTEGER`
/// numbers → `INTEGER` (§3.2 A for the three money columns, §3.2 F for
/// `vat_rate_pct`).
///
/// **No `PRIMARY KEY`, no `UNIQUE`, no index, no `CHECK`, no `DEFAULT`** —
/// `purchasing.rs:169–171`'s `[[no-sql-specific]]` posture, preserved verbatim.
/// The closed `state` vocabulary lives in `PoState`, and the currency rule in
/// `validate_currency`; `STRICT` constrains storage classes, not vocabularies,
/// and porting either into the DDL here would fork the two schemas.
const SQLITE_PURCHASE_ORDERS_DDL: &str = "
CREATE TABLE IF NOT EXISTS purchase_orders (
    po_id                  TEXT    NOT NULL,
    tenant_id              TEXT    NOT NULL,
    po_number              TEXT    NOT NULL,
    vendor_partner_id      TEXT    NOT NULL,
    currency               TEXT    NOT NULL,
    subtotal_minor         INTEGER NOT NULL,
    vat_rate_pct           INTEGER NOT NULL,
    vat_minor              INTEGER NOT NULL,
    total_minor            INTEGER NOT NULL,
    state                  TEXT    NOT NULL,
    vendor_avl_status      TEXT,
    issued_at_utc          TEXT,
    expected_delivery_utc  TEXT,
    notes                  TEXT    NOT NULL,
    requested_by_operator  TEXT    NOT NULL,
    approved_by_operator   TEXT,
    approved_at_utc        TEXT,
    created_at_utc         TEXT    NOT NULL
) STRICT;
";

/// `purchase_order_lines`, `STRICT`.
///
/// `unit_price_minor` / `line_total_minor` are §3.2 A money; `seq`, `quantity`
/// and `received_quantity` are §3.2 F counts; `expected_heat_lot_required` is
/// §3.2 H's `BOOLEAN` → `INTEGER`.
const SQLITE_PURCHASE_ORDER_LINES_DDL: &str = "
CREATE TABLE IF NOT EXISTS purchase_order_lines (
    pol_id                      TEXT    NOT NULL,
    po_id                       TEXT    NOT NULL,
    tenant_id                   TEXT    NOT NULL,
    seq                         INTEGER NOT NULL,
    product_id                  TEXT,
    description                 TEXT    NOT NULL,
    quantity                    INTEGER NOT NULL,
    unit_price_minor            INTEGER NOT NULL,
    currency                    TEXT    NOT NULL,
    line_total_minor            INTEGER NOT NULL,
    expected_heat_lot_required  INTEGER NOT NULL,
    received_quantity           INTEGER NOT NULL
) STRICT;
";

/// `purchase_order_receipts`, `STRICT`.
///
/// `inspection_pass` is §3.2 H's second `BOOLEAN`; `received_quantity` is
/// §3.2 F. `heat_lot_assigned` and `ncr_id` are the two nullable columns — the
/// heat lot because a non-defense part is received without one, the NCR id
/// because it is stamped only when the inspection failed (S439).
const SQLITE_PURCHASE_ORDER_RECEIPTS_DDL: &str = "
CREATE TABLE IF NOT EXISTS purchase_order_receipts (
    por_id                TEXT    NOT NULL,
    po_id                 TEXT    NOT NULL,
    pol_id                TEXT    NOT NULL,
    tenant_id             TEXT    NOT NULL,
    received_at_utc       TEXT    NOT NULL,
    received_by_operator  TEXT    NOT NULL,
    delivery_note_number  TEXT    NOT NULL,
    received_quantity     INTEGER NOT NULL,
    inspection_pass       INTEGER NOT NULL,
    inspection_notes      TEXT    NOT NULL,
    heat_lot_assigned     TEXT,
    ncr_id                TEXT
) STRICT;
";

/// `po_number_state`, `STRICT`. §3.2 F names `next_number` explicitly.
///
/// **The `(tenant_id, year)` key is a comment in the source, not a constraint**
/// (`purchasing.rs:169`), and it is not made one here: adding a `PRIMARY KEY`
/// would move an invariant into the DDL on one engine and not the other.
/// [`unique_natural_keys`] enforces it in Rust, where the product already
/// relies on it.
const SQLITE_PO_NUMBER_STATE_DDL: &str = "
CREATE TABLE IF NOT EXISTS po_number_state (
    tenant_id    TEXT    NOT NULL,
    year         INTEGER NOT NULL,
    next_number  INTEGER NOT NULL,
    updated_at   TEXT    NOT NULL
) STRICT;
";

// ---------------------------------------------------------------------------
// The table specs — one spec per table drives the SELECT, the INSERT, the
// per-row equality, the Σ folds and the typeof() sweep, so the five can never
// drift apart.
// ---------------------------------------------------------------------------

/// One carried row: the `TEXT` columns, the `INTEGER` columns (§3.2 A money and
/// §3.2 F counts alike — both are exact `i64` on both engines) and the §3.2 H
/// booleans.
///
/// One struct serves all four tables because the four have the same *shape* —
/// text, integers, flags — and the column lists are what differ. `Eq` holds:
/// **this family carries no float at all**, so there is no value in it that
/// compares unequal to itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PurchasingRow {
    text: Vec<Option<String>>,
    ints: Vec<i64>,
    flags: Vec<bool>,
}

/// Everything the carry and the gate need to know about one table.
struct TableSpec {
    name: &'static str,
    /// Human-readable singular, used in hard-stop messages.
    noun: &'static str,
    /// `TEXT` columns, **key components first** — see [`TableSpec::key`].
    texts: &'static [&'static str],
    /// `INTEGER` columns: §3.2 A money and §3.2 F counts.
    ints: &'static [&'static str],
    /// §3.2 H booleans, bound and read as `bool`.
    flags: &'static [&'static str],
    /// `ORDER BY` clause, on the natural key, so both engines hand back the
    /// same order and a per-table read is deterministic.
    order_by: &'static str,
    /// The natural composite key. A `fn` per table rather than a column-index
    /// list because `po_number_state`'s key spans a `TEXT` and an `INTEGER`.
    key: fn(&PurchasingRow) -> String,
}

/// `tenant_id#<second text column>` — the shape three of the four tables use.
fn key_tenant_and_id(row: &PurchasingRow) -> String {
    format!(
        "{}#{}",
        row.text[0].clone().unwrap_or_default(),
        row.text[1].clone().unwrap_or_default()
    )
}

/// `po_number_state`'s key is `(tenant_id, year)` — a `TEXT` and an `INTEGER`.
/// This is the one key in the family that is genuinely composite rather than a
/// tenant-scoped ULID, and it is the one whose duplicate re-issues a PO number.
fn key_tenant_and_year(row: &PurchasingRow) -> String {
    format!(
        "{}#{}",
        row.text[0].clone().unwrap_or_default(),
        row.ints[0]
    )
}

/// Every table in the family, in the order the carry and the gate walk them.
///
/// The order is the source's own declaration order, which is also dependency
/// order: header → lines → receipts → the sequence bucket.
const FAMILY: &[TableSpec] = &[
    TableSpec {
        name: "purchase_orders",
        noun: "purchase order",
        texts: &[
            "tenant_id",
            "po_id",
            "po_number",
            "vendor_partner_id",
            "currency",
            "state",
            "vendor_avl_status",
            "issued_at_utc",
            "expected_delivery_utc",
            "notes",
            "requested_by_operator",
            "approved_by_operator",
            "approved_at_utc",
            "created_at_utc",
        ],
        ints: &["subtotal_minor", "vat_rate_pct", "vat_minor", "total_minor"],
        flags: &[],
        order_by: "tenant_id ASC, po_id ASC",
        key: key_tenant_and_id,
    },
    TableSpec {
        name: "purchase_order_lines",
        noun: "PO line",
        texts: &[
            "tenant_id",
            "pol_id",
            "po_id",
            "product_id",
            "description",
            "currency",
        ],
        ints: &[
            "seq",
            "quantity",
            "unit_price_minor",
            "line_total_minor",
            "received_quantity",
        ],
        flags: &["expected_heat_lot_required"],
        order_by: "tenant_id ASC, pol_id ASC",
        key: key_tenant_and_id,
    },
    TableSpec {
        name: "purchase_order_receipts",
        noun: "PO receipt",
        texts: &[
            "tenant_id",
            "por_id",
            "po_id",
            "pol_id",
            "received_at_utc",
            "received_by_operator",
            "delivery_note_number",
            "inspection_notes",
            "heat_lot_assigned",
            "ncr_id",
        ],
        ints: &["received_quantity"],
        flags: &["inspection_pass"],
        order_by: "tenant_id ASC, por_id ASC",
        key: key_tenant_and_id,
    },
    TableSpec {
        name: "po_number_state",
        noun: "PO number bucket",
        texts: &["tenant_id", "updated_at"],
        ints: &["year", "next_number"],
        flags: &[],
        order_by: "tenant_id ASC, year ASC",
        key: key_tenant_and_year,
    },
];

/// The DDL for one table, looked up by name so the schema and the spec list
/// cannot name different sets of tables.
fn ddl_for(table: &str) -> &'static str {
    match table {
        "purchase_orders" => SQLITE_PURCHASE_ORDERS_DDL,
        "purchase_order_lines" => SQLITE_PURCHASE_ORDER_LINES_DDL,
        "purchase_order_receipts" => SQLITE_PURCHASE_ORDER_RECEIPTS_DDL,
        "po_number_state" => SQLITE_PO_NUMBER_STATE_DDL,
        other => unreachable!("no DDL for {other}; FAMILY and ddl_for must name the same tables"),
    }
}

/// Build the whole family's SQLite schema.
///
/// **There is no `ensure_columns` ladder in this family, and that is measured
/// rather than assumed:** `ALTER TABLE` returns zero hits against all four
/// tables across `apps/`, `crates/` and `modules/`. All four are created whole
/// by one `CREATE TABLE IF NOT EXISTS` batch and none has ever been widened.
pub fn ensure_purchasing_schema(conn: &SqliteConn) -> Result<()> {
    for spec in FAMILY {
        conn.execute_batch(ddl_for(spec.name))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// What the carry did
// ---------------------------------------------------------------------------

/// As everywhere else in this migrator these are **not** the numbers the gate
/// compares (B4) — the gate re-derives every one of them from disk in a separate
/// invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PurchasingCarry {
    pub purchase_orders: u64,
    pub purchase_order_lines: u64,
    pub purchase_order_receipts: u64,
    pub po_number_state: u64,
}

// ---------------------------------------------------------------------------
// Presence — per TABLE
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

/// Does this DuckDB database have **any** of the family's four tables?
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    for spec in FAMILY {
        if present_duckdb(conn, spec.name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// The carry
// ---------------------------------------------------------------------------

fn insert_sql(spec: &TableSpec) -> String {
    let all: Vec<&str> = spec
        .texts
        .iter()
        .chain(spec.ints.iter())
        .chain(spec.flags.iter())
        .copied()
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        spec.name,
        all.join(", "),
        vec!["?"; all.len()].join(", ")
    )
}

/// Carry the family from an already-open **read-only** DuckDB connection into an
/// already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// Schema creation is **per present table**. Unlike Parts C, D, E and F — where
/// two or three modules built the family's tables at different sprints, so a
/// partial source is a shape the *product* can produce — all four tables here
/// come from one batch and a real source has either four or none. The per-table
/// hold is therefore about detecting a **carry** asymmetry rather than a source
/// one: creating an empty SQLite table for one the source does not have would
/// manufacture exactly the asymmetry the gate exists to detect, and the gate's
/// per-table hard stop is what names a dropped arm of this function.
pub fn carry_purchasing(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<PurchasingCarry> {
    let mut tables: Vec<(&TableSpec, Vec<PurchasingRow>)> = Vec::with_capacity(FAMILY.len());
    for spec in FAMILY {
        if present_duckdb(src, spec.name)? {
            dst.execute_batch(ddl_for(spec.name))?;
            tables.push((spec, read_duckdb(src, spec)?));
        } else {
            tables.push((spec, Vec::new()));
        }
    }

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the purchasing carry transaction: {e}"))?;
    for (spec, rows) in &tables {
        let sql = insert_sql(spec);
        for row in rows {
            let mut binds: Vec<&dyn ToSql> =
                Vec::with_capacity(spec.texts.len() + spec.ints.len() + spec.flags.len());
            for v in &row.text {
                binds.push(v);
            }
            for v in &row.ints {
                binds.push(v);
            }
            // §3.2 H: bound as `bool`, which `rusqlite` maps to INTEGER 0/1. A
            // `&str` here would store `"true"`, and every Rust `bool` read of it
            // afterwards would be `false`.
            for v in &row.flags {
                binds.push(v);
            }
            tx.execute(&sql, binds.as_slice())?;
        }
    }
    tx.commit()?;

    let count = |name: &str| {
        tables
            .iter()
            .find(|(s, _)| s.name == name)
            .map(|(_, r)| r.len() as u64)
            .unwrap_or(0)
    };
    Ok(PurchasingCarry {
        purchase_orders: count("purchase_orders"),
        purchase_order_lines: count("purchase_order_lines"),
        purchase_order_receipts: count("purchase_order_receipts"),
        po_number_state: count("po_number_state"),
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// **There is no `CAST(… AS VARCHAR)` anywhere in this module** and that is a
/// statement about the family rather than an omission: no column in it is
/// `DECIMAL` or `DOUBLE` on DuckDB, so no value in it needs an R2 projection.
fn select_sql(spec: &TableSpec) -> String {
    let all: Vec<&str> = spec
        .texts
        .iter()
        .chain(spec.ints.iter())
        .chain(spec.flags.iter())
        .copied()
        .collect();
    format!(
        "SELECT {} FROM {} ORDER BY {}",
        all.join(", "),
        spec.name,
        spec.order_by
    )
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// each read is written once per engine.
///
/// **The DuckDB side validates; the SQLite side does not.** A duplicate key is a
/// property of the source, and [`unique_natural_keys`] must name the source's
/// defect rather than the copy's symptom.
fn read_duckdb(conn: &duckdb::Connection, spec: &TableSpec) -> Result<Vec<PurchasingRow>> {
    let mut stmt = conn.prepare(&select_sql(spec))?;
    let rows = stmt.query_map([], |r| decode_duckdb(r, spec))?;
    let rows: Vec<PurchasingRow> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    unique_natural_keys(&rows.iter().map(spec.key).collect::<Vec<_>>(), spec.name)?;
    Ok(rows)
}

fn read_sqlite(conn: &SqliteConn, spec: &TableSpec) -> Result<Vec<PurchasingRow>> {
    let mut stmt = conn.prepare(&select_sql(spec))?;
    let rows = stmt.query_map([], |r| decode_sqlite(r, spec))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn decode_duckdb(r: &duckdb::Row, spec: &TableSpec) -> duckdb::Result<PurchasingRow> {
    let mut text = Vec::with_capacity(spec.texts.len());
    for i in 0..spec.texts.len() {
        text.push(r.get::<_, Option<String>>(i)?);
    }
    let mut ints = Vec::with_capacity(spec.ints.len());
    for i in 0..spec.ints.len() {
        ints.push(r.get::<_, i64>(spec.texts.len() + i)?);
    }
    let mut flags = Vec::with_capacity(spec.flags.len());
    for i in 0..spec.flags.len() {
        flags.push(r.get::<_, bool>(spec.texts.len() + spec.ints.len() + i)?);
    }
    Ok(PurchasingRow { text, ints, flags })
}

fn decode_sqlite(
    r: &aberp_db::engine::Row,
    spec: &TableSpec,
) -> aberp_db::engine::Result<PurchasingRow> {
    let mut text = Vec::with_capacity(spec.texts.len());
    for i in 0..spec.texts.len() {
        text.push(r.get::<_, Option<String>>(i)?);
    }
    let mut ints = Vec::with_capacity(spec.ints.len());
    for i in 0..spec.ints.len() {
        ints.push(r.get::<_, i64>(spec.texts.len() + i)?);
    }
    let mut flags = Vec::with_capacity(spec.flags.len());
    for i in 0..spec.flags.len() {
        flags.push(r.get::<_, bool>(spec.texts.len() + spec.ints.len() + i)?);
    }
    Ok(PurchasingRow { text, ints, flags })
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per table, per row, over **every** column. Every
/// number here is re-derived from disk by the gate (B4).
pub fn reconcile_purchasing(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let mut both = Vec::new();
    for spec in FAMILY {
        match (
            present_duckdb(src, spec.name)?,
            present_sqlite(dst, spec.name)?,
        ) {
            (false, false) => continue,
            (true, false) => {
                hard_stops.push(format!(
                    "{} exists in DuckDB but NOT in SQLite — the table was not carried. This is \
                     the silent-skip shape: every check below would have been skipped and the \
                     gate would have reported PASS",
                    spec.name
                ));
                continue;
            }
            (false, true) => {
                hard_stops.push(format!(
                    "{} exists in SQLite but NOT in DuckDB — the copy has a table the source does \
                     not. There is no source of truth to reconcile against",
                    spec.name
                ));
                continue;
            }
            (true, true) => both.push(spec),
        }
        row_count(src, dst, spec.name, checks, hard_stops)?;
    }
    if both.is_empty() {
        checks.push(
            "✓ purchasing family absent on BOTH sides — nothing to reconcile (a database in \
             which no purchase order has ever been raised is a legitimate shape; an asymmetry \
             is not)"
                .to_string(),
        );
        return Ok(());
    }

    for spec in both {
        reconcile_table(src, dst, spec, checks, hard_stops)?;
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

/// One table's per-row equality over **every** column, then the Σ folds, then
/// the `typeof` sweep.
///
/// The per-row arm compares the whole column set rather than "the important
/// ones": on a PO the money and the quantity are what the vendor is owed, and
/// `delivery_note_number` / `heat_lot_assigned` are the incoming-goods
/// traceability chain that ADR-0105's heat-lot rule and the S439 auto-NCR both
/// hang off. No fold and no `typeof` class can catch a drift in either.
fn reconcile_table(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    spec: &TableSpec,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_duckdb(src, spec)?;
    let lite: BTreeMap<String, PurchasingRow> = read_sqlite(dst, spec)?
        .into_iter()
        .map(|r| ((spec.key)(&r), r))
        .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = (spec.key)(row);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("{} {key} is missing on the SQLite side", spec.noun));
            drift += 1;
            continue;
        };
        for (i, name) in spec.texts.iter().enumerate() {
            if row.text[i] != got.text[i] {
                hard_stops.push(format!(
                    "{} {key}: {name} DuckDB {:?}, SQLite {:?}",
                    spec.noun, row.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in spec.ints.iter().enumerate() {
            if row.ints[i] != got.ints[i] {
                hard_stops.push(format!(
                    "{} {key}: {name} DuckDB {}, SQLite {}",
                    spec.noun, row.ints[i], got.ints[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in spec.flags.iter().enumerate() {
            if row.flags[i] != got.flags[i] {
                hard_stops.push(format!(
                    "{} {key}: {name} DuckDB {}, SQLite {}",
                    spec.noun, row.flags[i], got.flags[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {} column round-trips with ZERO drift ({} row(s) × {} columns)",
            spec.name,
            duck.len(),
            spec.texts.len() + spec.ints.len() + spec.flags.len()
        ));
    }

    // §6.3's per-money-column exact sum. Every INTEGER column is folded, not
    // only the five money ones: an i64 fold in Rust is the cheapest cross-check
    // that exists, and the counts and ordinals beside the money are the values
    // the money was computed from. **Never a SQL `SUM`** — `SUM` over INTEGER
    // raises on i64 overflow on one engine and silently yields REAL on the
    // other, and the rule that keeps arithmetic out of SQL does not get an
    // exception for a column that happens to be small (§3.4 / T-8).
    for (i, name) in spec.ints.iter().enumerate() {
        let d = fold_i64(duck.iter().map(|r| r.ints[i]), name)?;
        let l = fold_i64(lite.values().map(|r| r.ints[i]), name)?;
        if d == l {
            checks.push(format!(
                "✓ Σ {}.{name} = {d} (i64 fold in Rust on both sides)",
                spec.name
            ));
        } else {
            hard_stops.push(format!("Σ {}.{name}: DuckDB {d}, SQLite {l}", spec.name));
        }
    }

    for col in spec.texts {
        typeof_sweep(dst, spec.name, col, "text", checks, hard_stops)?;
    }
    for col in spec.ints {
        typeof_sweep(dst, spec.name, col, "integer", checks, hard_stops)?;
    }
    // §3.2 H: `BOOLEAN` → `INTEGER`. A `'text'` here would be a `"true"` bound
    // as a string, which every comparison in Rust would then read as `false` —
    // and on `inspection_pass` that turns every passing incoming inspection into
    // a failure that mints an auto-NCR.
    for col in spec.flags {
        typeof_sweep(dst, spec.name, col, "integer", checks, hard_stops)?;
    }
    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// For the money columns this is the assertion that §3.2 A's call actually held:
/// a `'text'` or a `'real'` on `total_minor` would mean a PO total crossed as
/// something other than exact minor units, which is the whole class R1 exists to
/// close.
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
             money or count column, turns an exact value into a float"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::{COUNT_INT, MONEY_MINOR, TEXT};

    /// Every table's carry list leads with the columns its key is built from,
    /// and its `ORDER BY` names the same components in the same order.
    #[test]
    fn every_table_carries_its_key_first() {
        for spec in FAMILY {
            assert_eq!(
                spec.texts[0], "tenant_id",
                "{} must lead with tenant_id — every read in purchasing.rs is tenant-scoped",
                spec.name
            );
            assert!(
                spec.order_by.starts_with("tenant_id ASC,"),
                "{} must order on its key: {}",
                spec.name,
                spec.order_by
            );
        }
        // The three tenant-scoped-ULID tables.
        assert_eq!(FAMILY[0].texts[1], "po_id");
        assert_eq!(FAMILY[1].texts[1], "pol_id");
        assert_eq!(FAMILY[2].texts[1], "por_id");
        // And the genuinely composite one, whose second component is an INTEGER.
        assert_eq!(FAMILY[3].ints[0], "year");
        assert_eq!(FAMILY[3].order_by, "tenant_id ASC, year ASC");
    }

    /// The four tables the carry, the gate and `ddl_for` walk are the same four,
    /// in the same order.
    #[test]
    fn the_family_table_list_matches_the_carry() {
        let names: Vec<&str> = FAMILY.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "purchase_orders",
                "purchase_order_lines",
                "purchase_order_receipts",
                "po_number_state",
            ]
        );
        // `ddl_for` panics on an unknown name, so this also proves the two lists
        // cannot name different sets.
        for spec in FAMILY {
            assert!(ddl_for(spec.name).contains(&format!("EXISTS {}", spec.name)));
        }
    }

    /// **The family's money is INTEGER minor units and its quantities are
    /// INTEGER counts** — the DDL is what proves it rather than the module docs.
    ///
    /// This is the pin that fails if a later session adds a fractional PO
    /// quantity, a unit cost on a float, or a supplier price in a decimal
    /// column, without deciding its representation — which is the moment R2 and
    /// `finite_measurement` start applying to this family.
    #[test]
    fn no_column_in_this_family_is_a_float_or_a_decimal() {
        for spec in FAMILY {
            let ddl = ddl_for(spec.name);
            for banned in ["DECIMAL", "NUMERIC", "REAL", "DOUBLE", "FLOAT"] {
                assert!(
                    !ddl.contains(banned),
                    "the purchasing family carries no float and no decimal; {banned:?} appearing \
                     in {}'s DDL means that stopped being true and R2 now has something to bite \
                     on:\n{ddl}",
                    spec.name
                );
            }
        }
        // And the five money columns really are declared INTEGER, by name.
        for (table, col) in [
            ("purchase_orders", "subtotal_minor"),
            ("purchase_orders", "vat_minor"),
            ("purchase_orders", "total_minor"),
            ("purchase_order_lines", "unit_price_minor"),
            ("purchase_order_lines", "line_total_minor"),
        ] {
            let ddl = ddl_for(table);
            assert!(
                ddl.lines()
                    .any(|l| l.trim_start().starts_with(col) && l.contains(MONEY_MINOR)),
                "§3.2 A names {table}.{col} as BIGINT → INTEGER:\n{ddl}"
            );
        }
    }

    /// The declared types this module uses are the vocabulary constants, spelled
    /// the same way, and all four tables are `STRICT`.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        assert_eq!(COUNT_INT, "INTEGER");
        assert_eq!(MONEY_MINOR, "INTEGER");
        for spec in FAMILY {
            let ddl = ddl_for(spec.name);
            for banned in ["VARCHAR", "BIGINT", "BOOLEAN"] {
                assert!(
                    !ddl.contains(banned),
                    "{} must not declare {banned}",
                    spec.name
                );
            }
            assert!(ddl.contains("STRICT"), "{} must be STRICT", spec.name);
        }
        // §3.2 H's two booleans really did become INTEGER.
        assert!(SQLITE_PURCHASE_ORDER_LINES_DDL
            .contains("expected_heat_lot_required  INTEGER NOT NULL"));
        assert!(
            SQLITE_PURCHASE_ORDER_RECEIPTS_DDL.contains("inspection_pass       INTEGER NOT NULL")
        );
    }

    /// **No table in this family may gain a `PRIMARY KEY`, a `UNIQUE` or an
    /// index**, because none of the DuckDB tables has one
    /// (`purchasing.rs:169–171`) and the two schemas must agree.
    /// [`unique_natural_keys`] is what enforces the keys instead — this pin is
    /// what stops a later session "tidying" the DDL and silently deleting the
    /// refusal's reason to exist.
    #[test]
    fn no_table_in_this_family_has_a_primary_key_on_either_engine() {
        for spec in FAMILY {
            let ddl = ddl_for(spec.name);
            for banned in ["PRIMARY KEY", "UNIQUE", "CREATE INDEX", "CHECK", "DEFAULT"] {
                assert!(
                    !ddl.contains(banned),
                    "the DuckDB schema declares no {banned} on {} (purchasing.rs:169-171's \
                     [[no-sql-specific]] posture); adding one here would fork the two schemas \
                     and move an invariant into the DDL on one engine only",
                    spec.name
                );
            }
        }
    }

    /// `po_number_state`'s key reads **both** components, so a change in either
    /// produces a different key — and it is the one key in the family built from
    /// an `INTEGER` rather than from two strings.
    ///
    /// A key built from `tenant_id` alone would pair a tenant's 2025 bucket with
    /// its 2026 bucket, and the gate would compare one of them twice while never
    /// comparing the other — over the counter that decides whether a PO number
    /// can be re-issued.
    #[test]
    fn the_po_number_state_key_uses_both_components() {
        let row = |tenant: &str, year: i64| PurchasingRow {
            text: vec![
                Some(tenant.to_string()),
                Some("2026-01-01T00:00:00Z".into()),
            ],
            ints: vec![year, 7],
            flags: vec![],
        };
        let base = key_tenant_and_year(&row("test", 2026));
        assert_eq!(base, "test#2026");
        assert_ne!(base, key_tenant_and_year(&row("other", 2026)));
        assert_ne!(base, key_tenant_and_year(&row("test", 2025)));
    }

    /// The generated SQL names every column of the table exactly once, in the
    /// same order, in both statements — which is what makes the positional
    /// decode in [`decode_duckdb`] / [`decode_sqlite`] safe.
    #[test]
    fn the_select_and_the_insert_agree_on_column_order() {
        for spec in FAMILY {
            let cols: Vec<&str> = spec
                .texts
                .iter()
                .chain(spec.ints.iter())
                .chain(spec.flags.iter())
                .copied()
                .collect();
            let joined = cols.join(", ");
            assert!(
                select_sql(spec).starts_with(&format!("SELECT {joined} FROM {}", spec.name)),
                "{}: {}",
                spec.name,
                select_sql(spec)
            );
            assert!(
                insert_sql(spec).starts_with(&format!("INSERT INTO {} ({joined})", spec.name)),
                "{}: {}",
                spec.name,
                insert_sql(spec)
            );
            // And the column set is exactly the DDL's, no more and no fewer.
            let declared = ddl_for(spec.name)
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .filter(|w| cols.contains(w))
                .count();
            assert_eq!(
                declared,
                cols.len(),
                "{} carries {} columns but its DDL declares {declared} of them",
                spec.name,
                cols.len()
            );
        }
    }
}
