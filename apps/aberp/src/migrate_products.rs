//! ADR-0108 Step 7 Part C — **the products/inventory family crosses.** This is
//! the family §7 flagged as the risky one, and the risk is not its size: it is
//! that the tree holds **two different representations of one physical
//! quantity**, and a migration that carried both as-is under `STRICT` would
//! make the divergence look sanctioned.
//!
//! # The family
//!
//! Four tables, two owning modules, 46 columns:
//!
//! | table | owner | columns | the numbers in it |
//! |---|---|---|---|
//! | `products` | `apps/aberp/src/products.rs` + `aberp-inventory/migrations/V001__inventory.sql` | 14 | `unit_price_minor` (R1), `stock_qty` / `min_stock` (R2) |
//! | `stock_movements` | `V001__inventory.sql` | 11 | `qty_delta` (R2) |
//! | `inventory_balances` | `apps/aberp/src/material_inventory.rs` | 12 | `on_hand_qty` / `reserved_qty` / `committed_qty` / `consumed_qty` (**were `DOUBLE`**) |
//! | `inventory_reservations` | `apps/aberp/src/material_inventory.rs` | 9 | `qty` (**was `DOUBLE`**) |
//!
//! The four tables are created by **two independent `ensure_schema` calls**, so
//! unlike the partners family their presence is not one answer. Presence is
//! therefore checked and held symmetrically **per table**, not per family: a
//! products-only database is a legitimate shape, and so is a
//! material-inventory-only one.
//!
//! # THE RULE-7 RESOLUTION, which is the whole point of this commit
//!
//! §3.2 E records the divergence and declines to fix it: `inventory_balances`'s
//! four `*_qty` columns and `inventory_reservations.qty` are `DOUBLE`, while
//! `stock_movements.qty_delta`, `products.stock_qty` and `products.min_stock`
//! are `DECIMAL(18,6)`. Two representations of "how much of this thing is
//! there", one product, neither source document noticing. §9 carries the row.
//!
//! **Resolved conservatively here: every one of them crosses as R2 — `TEXT`
//! holding the canonical `rust_decimal::Decimal` string — with ZERO value
//! change.** Not `REAL`. The alternative (carry the `DOUBLE`s as `REAL`, per
//! §3.2 E's "unchanged" column) is what the register's own comment names as the
//! mechanism by which "temporary" becomes "sanctioned": after this commit there
//! would be a `STRICT` table, blessed by a green reconciliation gate, declaring
//! that a quantity is a float in one half of the product and exact in the
//! other.
//!
//! **Zero value change is enforced, not asserted.** [`canonical_decimal_from_f64`]
//! carries each `DOUBLE` through the shortest decimal string that round-trips
//! the `f64` (Rust's `Display`), parses that to a `Decimal`, and then **refuses
//! the carry** unless *both*:
//!
//! * the decimal fits the canonical quantity scale ([`QUANTITY_SCALE`] = 6, the
//!   scale `DECIMAL(18,6)` already uses for the other half of the divergence);
//!   and
//! * converting the `Decimal` back yields the same `f64`.
//!
//! A value that fails either is a **hard error in the carry**, never a rounded
//! one — the rule-11 call. The one bit-level difference the equality admits is
//! the sign of zero (`-0.0` stores as `"0"`), which carries no quantity
//! meaning; nothing else can differ, because a shortest-round-trip string is
//! by construction the one that maps back to the same float.
//!
//! **What this commit deliberately does NOT do.** It does not touch the
//! app-layer modelling: `material_inventory.rs` still holds these values as
//! `f64` in Rust (`Balance.on_hand_qty: f64`, the `InsufficientMaterial` error's
//! four `f64` fields), and `aberp-inventory` still holds its side as `Decimal`.
//! That is a real pre-existing divergence and fixing it is a separate change to
//! a saga's arithmetic and its callers — rule 3. It gets its **own §9 row**,
//! naming the exact tables and columns, rather than being folded in here where
//! it would be indistinguishable from the storage change.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5 and 7B, and Ervin's same constraint: the **migrator
//! half only**. DuckDB remains the source of truth. `products.rs`,
//! `material_inventory.rs` and `aberp-inventory/src/repository.rs` are
//! untouched; every one of their queries still runs against a DuckDB
//! `Connection`.
//!
//! **§3.4's three cache-rebuild folds do NOT land here, and the T-8 pending-fold
//! register keeps its two entries.** They are `SUM(qty_delta)` inside
//! `aberp_inventory::repository::{record_movement, rebuild_stock_cache_for_tenant}`
//! — a *write* path that computes `products.stock_qty`. §7's Step-7 line asks
//! for them in this commit; that line is corrected in §9 on exactly the
//! precedent Part B set for M11/T-12. The argument is the same one: nothing
//! here makes those statements run against SQLite, so their exposure is still
//! zero (on DuckDB `DECIMAL` is a real decimal type and `SUM` does not coerce),
//! and the mutation that proves the fold matters — a float `stock_qty` written
//! back into the cache — is only reachable once the queries cross. The register
//! is a ratchet and it stays honest: both entries still match a live statement,
//! so T8-3 and T8-4 are green because the sites are *there*, not because the
//! gate cannot see them.
//!
//! **The migrator's own code holds to T-8 unconditionally**: not one `SUM`,
//! `AVG`, `*`, `+` or `-` on a money or quantity column appears in the SQL
//! below. Every fold in this file is Rust over `Decimal` / `i64`.
//!
//! # §6.3's "rebuild the cache" is NOT what this does, and that is deliberate
//!
//! §6.3's inventory row says: carry `stock_movements`, but **rebuild**
//! `products.stock_qty` from `SUM(qty_delta)` in Rust. This module **carries
//! `stock_qty` verbatim**, like every other column.
//!
//! Rebuilding would make the gate unable to do its job. The reconciliation arm
//! compares the two sides row-for-row; if the migrator re-derived `stock_qty`,
//! then a DuckDB cache that had legitimately drifted (which is the exact
//! condition ADR-0061 §3's `rebuild-stock-cache` recovery path exists for)
//! would show as a per-row difference, and the only way to keep the gate green
//! would be to teach it the transformation — i.e. to verify the extraction
//! against itself, which is what B4 forbids. It would also make the migration a
//! repair tool, which §6.3's own mirror argument rules out three paragraphs
//! earlier: *a migration is not a repair tool.* A cache that is wrong on DuckDB
//! must cross as wrong and be repaired by the path that owns that repair.
//! §6.3's row is corrected in §9.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use aberp_db::engine::{Connection as SqliteConn, ToSql};
use aberp_db::schema::{ensure_columns, EXACT_DECIMAL, TEXT};

use crate::migrate_billing::{canonical_decimal, fold_decimal, fold_i64};

/// The canonical scale of a physical quantity in this product: `DECIMAL(18,6)`.
///
/// It is not a preference. It is the scale `stock_movements.qty_delta`,
/// `products.stock_qty` and `products.min_stock` are already declared at, and
/// the rule-7 resolution is to bring the `DOUBLE` half **to** that scale rather
/// than to invent a third representation.
pub const QUANTITY_SCALE: u32 = 6;

// ---------------------------------------------------------------------------
// The STRICT DDL
// ---------------------------------------------------------------------------

/// `products`, pre-inventory-ladder, `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H); `BIGINT unit_price_minor` → `INTEGER` (§3.2 A,
/// R1 — the representation is unchanged, it was already `i64` in Rust). Both
/// indexes carry across; both key on base columns.
///
/// S410 / `[[no-sql-specific]]` preserved verbatim: no DB-level `CHECK` on
/// `unit_kind` or `currency`. Both are closed vocabularies enforced in Rust
/// (`unit_from_db_columns`, `currency_from_db`), and `STRICT` constrains
/// storage classes, not vocabularies.
const SQLITE_PRODUCTS_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS products (
    id               TEXT    NOT NULL PRIMARY KEY,
    tenant_id        TEXT    NOT NULL,
    name             TEXT    NOT NULL,
    unit_kind        TEXT    NOT NULL,
    unit_value       TEXT    NOT NULL,
    currency         TEXT    NOT NULL,
    unit_price_minor INTEGER NOT NULL,
    created_at       TEXT    NOT NULL,
    updated_at       TEXT    NOT NULL,
    deleted_at       TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS products_tenant_deleted_idx
    ON products (tenant_id, deleted_at);
CREATE INDEX IF NOT EXISTS products_tenant_name_idx
    ON products (tenant_id, name);
";

/// S231 / PR-227 / ADR-0061 (`V001__inventory.sql`) — the denormalised
/// inventory cache on `products`.
///
/// The base `CREATE` omits all four so the ladder actually runs and M8's
/// post-condition is exercised, the same F-1c call Steps 5 and 7B made.
///
/// `stock_qty` / `min_stock` are R2 (`EXACT_DECIMAL` = `TEXT`) and **nullable**,
/// exactly as on DuckDB: the migration's own comment records that DuckDB
/// rejects `ADD COLUMN ... NOT NULL DEFAULT 0`, so the columns are nullable at
/// the schema layer and the app treats NULL as 0. Re-deriving that backfill
/// here would be B4's forbidden shape.
const PRODUCTS_INVENTORY_CACHE: &[(&str, &str)] = &[
    ("stock_qty", EXACT_DECIMAL),
    ("min_stock", EXACT_DECIMAL),
    ("bin_location", TEXT),
    ("last_movement_at", TEXT),
];

/// The append-only movement ledger. `qty_delta` is R2, `NOT NULL` on both
/// engines. The per-product chronological index carries across.
const SQLITE_STOCK_MOVEMENTS_DDL: &str = "
CREATE TABLE IF NOT EXISTS stock_movements (
    movement_id     TEXT NOT NULL PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    product_id      TEXT NOT NULL,
    qty_delta       TEXT NOT NULL,
    reason          TEXT NOT NULL,
    ref_kind        TEXT,
    ref_id          TEXT,
    at_iso8601      TEXT NOT NULL,
    operator        TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    notes           TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS stock_movements_tenant_product_at_idx
    ON stock_movements (tenant_id, product_id, at_iso8601);
";

/// S262 / ADR-0084's per-grade balance row, pre-S432, `STRICT`.
///
/// **The four `*_qty` columns are `TEXT`, not `REAL`** — the rule-7 resolution.
/// They keep their `NOT NULL DEFAULT`, with the default written as the canonical
/// decimal string `'0'`: under `STRICT` a bare `DEFAULT 0` would put an INTEGER
/// into a `TEXT` column's default slot, which is the storage-class fork R3
/// exists to stop, one indirection away.
///
/// The natural composite `(tenant_id, material_grade)` PK is preserved
/// (`material_inventory.rs`'s own PUSHBACK note explains why there is no
/// surrogate `id`).
const SQLITE_INVENTORY_BALANCES_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS inventory_balances (
    tenant_id        TEXT NOT NULL,
    material_grade   TEXT NOT NULL,
    on_hand_qty      TEXT NOT NULL DEFAULT '0',
    reserved_qty     TEXT NOT NULL DEFAULT '0',
    committed_qty    TEXT NOT NULL DEFAULT '0',
    consumed_qty     TEXT NOT NULL DEFAULT '0',
    unit_of_measure  TEXT NOT NULL,
    last_updated     TEXT NOT NULL,
    PRIMARY KEY (tenant_id, material_grade)
) STRICT;
";

/// S432 / ADR-0085 — the heat-lot traceability overlay. All four nullable with
/// no SQL `DEFAULT`, exactly as on DuckDB: a missing heat lot is the signal the
/// defense WO-start gate reads, so a backfill here would silently open that
/// gate.
const INVENTORY_BALANCES_S432_HEAT: &[(&str, &str)] = &[
    ("heat_lot_number", TEXT),
    ("mill_test_report_url", TEXT),
    ("heat_assigned_at_utc", TEXT),
    ("heat_assigned_by_operator", TEXT),
];

/// The DEAL saga's reservation rows, pre-S275, `STRICT`. `qty` is the fifth
/// `DOUBLE` the rule-7 resolution moves to R2.
const SQLITE_INVENTORY_RESERVATIONS_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS inventory_reservations (
    reservation_id   TEXT NOT NULL PRIMARY KEY,
    tenant_id        TEXT NOT NULL,
    quote_id         TEXT NOT NULL,
    material_grade   TEXT NOT NULL,
    qty              TEXT NOT NULL,
    state            TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    transitioned_at  TEXT NOT NULL
) STRICT;
";

/// S275 / PR-264 F1 — what `qty` on this row actually means (`units` | `kg`).
/// Nullable and with no SQL `DEFAULT`, as on DuckDB: reads treat NULL on a
/// pre-S275 row as `units`.
const INVENTORY_RESERVATIONS_S275: &[(&str, &str)] = &[("qty_unit_kind", TEXT)];

/// Build the whole family's SQLite schema. Ladder order is the DuckDB build's
/// order, so the two schemas line up statement for statement.
pub fn ensure_products_schema(conn: &SqliteConn) -> Result<()> {
    ensure_products_table(conn)?;
    ensure_stock_movements_table(conn)?;
    ensure_inventory_balances_table(conn)?;
    ensure_inventory_reservations_table(conn)?;
    Ok(())
}

fn ensure_products_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_PRODUCTS_BASE_DDL)?;
    ensure_columns(conn, "products", PRODUCTS_INVENTORY_CACHE)?;
    Ok(())
}

fn ensure_stock_movements_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_STOCK_MOVEMENTS_DDL)?;
    Ok(())
}

fn ensure_inventory_balances_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_INVENTORY_BALANCES_BASE_DDL)?;
    ensure_columns(conn, "inventory_balances", INVENTORY_BALANCES_S432_HEAT)?;
    Ok(())
}

fn ensure_inventory_reservations_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_INVENTORY_RESERVATIONS_BASE_DDL)?;
    ensure_columns(conn, "inventory_reservations", INVENTORY_RESERVATIONS_S275)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The column lists — one list per table drives the SELECT, the INSERT, the
// per-row equality and the typeof() sweep, so the four can never drift apart.
// ---------------------------------------------------------------------------

/// `products`'s 13 `TEXT` columns, in carry order. `unit_price_minor` is the
/// 14th and is typed separately (R1, `i64`).
///
/// `stock_qty` and `min_stock` are in here because R2 *is* text: they cross as
/// the canonical decimal string, verbatim, exactly like `invoice_line.quantity`
/// did in Step 5.
const PRODUCT_TEXT_COLUMNS: &[&str] = &[
    "id",
    "tenant_id",
    "name",
    "unit_kind",
    "unit_value",
    "currency",
    "created_at",
    "updated_at",
    "deleted_at",
    "stock_qty",
    "min_stock",
    "bin_location",
    "last_movement_at",
];

/// Which of [`PRODUCT_TEXT_COLUMNS`] are `DECIMAL` on DuckDB and so need a
/// `CAST(… AS VARCHAR)` on the read side.
const PRODUCT_DECIMAL_COLUMNS: &[&str] = &["stock_qty", "min_stock"];

/// Their positions in [`PRODUCT_TEXT_COLUMNS`], as a const rather than a
/// runtime `position().expect(…)`: an index that is wrong is a boot abort in a
/// migrator, and a unit test pins the two lists against each other instead.
const PRODUCT_DECIMAL_INDICES: &[usize] = &[9, 10];

/// `qty_delta`'s position in [`MOVEMENT_COLUMNS`], for the same reason.
const MOVEMENT_QTY_DELTA_INDEX: usize = 3;

/// All 11 `stock_movements` columns. Every one of them is text after the carry
/// — `qty_delta` because R2 says so.
const MOVEMENT_COLUMNS: &[&str] = &[
    "movement_id",
    "tenant_id",
    "product_id",
    "qty_delta",
    "reason",
    "ref_kind",
    "ref_id",
    "at_iso8601",
    "operator",
    "idempotency_key",
    "notes",
];

const MOVEMENT_DECIMAL_COLUMNS: &[&str] = &["qty_delta"];

/// `inventory_balances`'s 8 non-quantity columns, in carry order.
const BALANCE_TEXT_COLUMNS: &[&str] = &[
    "tenant_id",
    "material_grade",
    "unit_of_measure",
    "last_updated",
    "heat_lot_number",
    "mill_test_report_url",
    "heat_assigned_at_utc",
    "heat_assigned_by_operator",
];

/// **The four `DOUBLE`s.** These are the rule-7 columns: `f64` on DuckDB, R2
/// `TEXT` after the carry, and the reconciliation arm proves each one converts
/// back to the identical `f64`.
const BALANCE_QTY_COLUMNS: &[&str] = &[
    "on_hand_qty",
    "reserved_qty",
    "committed_qty",
    "consumed_qty",
];

/// `inventory_reservations`'s 8 non-quantity columns, in carry order.
const RESERVATION_TEXT_COLUMNS: &[&str] = &[
    "reservation_id",
    "tenant_id",
    "quote_id",
    "material_grade",
    "state",
    "created_at",
    "transitioned_at",
    "qty_unit_kind",
];

/// The fifth `DOUBLE`.
const RESERVATION_QTY_COLUMNS: &[&str] = &["qty"];

// ---------------------------------------------------------------------------
// R2 for a value that was never a decimal string — the rule-7 carry
// ---------------------------------------------------------------------------

/// Carry one `DOUBLE` to its canonical R2 decimal string, or **refuse**.
///
/// The three steps, and why each is there:
///
/// 1. **Shortest round-trip rendering.** Rust's `Display` for `f64` emits the
///    shortest decimal string that parses back to the same float. That is the
///    only rendering with the property this carry needs; `{:.6}` would round,
///    and `Decimal::from_f64_retain` would carry the float's full binary
///    expansion (`0.1` → 17 digits of noise) into a column whose whole purpose
///    is to be exact.
/// 2. **Scale ≤ [`QUANTITY_SCALE`].** The canonical quantity scale in this
///    product is 6. A value needing more than that cannot be expressed in the
///    representation the other half of the divergence already uses, so
///    canonicalising it would mean rounding — the thing this function exists
///    not to do.
/// 3. **Round-trip proof, per value.** The `Decimal` must convert back to the
///    same `f64`. Steps 1 and 2 make this true by construction; asserting it
///    anyway is what turns "by construction" into something a reviewer can see
///    fail, and it is the property the family's property test sweeps.
///
/// Non-finite input is refused first: `NaN` and the infinities have no decimal
/// form at all, and `format!` would emit `NaN` / `inf`, which `Decimal::from_str`
/// rejects with a message about parsing rather than about the value.
pub fn canonical_decimal_from_f64(v: f64, key: &str, column: &str) -> Result<String> {
    if !v.is_finite() {
        bail!(
            "{column} on {key} is {v}, which has no decimal representation at all. ADR-0108 R2 \
             stores an exact decimal string; refusing to carry a non-finite quantity rather \
             than inventing one"
        );
    }
    let shortest = format!("{v}");
    let d = Decimal::from_str_exact(&shortest).map_err(|e| {
        anyhow::anyhow!(
            "{column} on {key} renders as {shortest:?}, which rust_decimal cannot represent \
             ({e}). ADR-0108's rule-7 resolution carries this DOUBLE to the same exact decimal \
             representation stock_movements.qty_delta already uses — refusing rather than \
             rounding it into range"
        )
    })?;
    if d.scale() > QUANTITY_SCALE {
        bail!(
            "{column} on {key} is {v} (exactly {shortest}), which needs scale {} — more than \
             the canonical quantity scale of {QUANTITY_SCALE} that DECIMAL(18,6) gives \
             stock_movements.qty_delta. Carrying it would mean ROUNDING a stocked quantity, so \
             the carry refuses instead (CLAUDE.md rule 11)",
            d.scale()
        );
    }
    let back = d.to_f64().ok_or_else(|| {
        anyhow::anyhow!("{column} on {key}: {d} does not convert back to an f64 at all")
    })?;
    if back != v {
        bail!(
            "{column} on {key}: {v} rendered as {shortest:?} and read back as {back}. The carry \
             must be value-neutral; refusing rather than silently moving a stocked quantity"
        );
    }
    Ok(d.to_string())
}

/// The inverse, used **only by the reconciliation gate**: take the string that
/// is on the SQLite side and give back the `f64` it must equal.
///
/// This is deliberately not the inverse of the migrator's own transform running
/// forwards again (B4): the gate parses what is actually on disk and converts
/// it, so a carry that wrote the wrong string is caught by the comparison
/// rather than reproduced by it.
fn f64_from_canonical_decimal(raw: &str, key: &str, column: &str) -> Result<f64> {
    let d = Decimal::from_str_exact(raw).map_err(|e| {
        anyhow::anyhow!(
            "{column} on {key} is {raw:?} on the SQLite side, which is not a decimal ({e})"
        )
    })?;
    d.to_f64().ok_or_else(|| {
        anyhow::anyhow!("{column} on {key}: {raw:?} does not convert back to an f64")
    })
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One `products` row: the 13 `TEXT` columns plus the R1 money column.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductRow {
    text: Vec<Option<String>>,
    /// §3.2 A, R1. `BIGINT` → `INTEGER`, `NOT NULL` on both engines.
    unit_price_minor: i64,
}

/// One row of a table whose columns are all text after the carry, plus zero or
/// more quantities that were `DOUBLE` on DuckDB.
///
/// `stock_movements` uses it with no quantities (its `qty_delta` is already a
/// decimal string and lives in `text`); `inventory_balances` with four;
/// `inventory_reservations` with one.
#[derive(Debug, Clone)]
struct QtyRow {
    text: Vec<Option<String>>,
    /// The `f64` each quantity column must equal — **the DuckDB value on the
    /// DuckDB side, and the value re-derived from the stored string on the
    /// SQLite side.** Equality of these is the round-trip proof.
    qty: Vec<f64>,
}

/// What the products/inventory carry did. As everywhere else in this migrator
/// these are **not** the numbers the gate compares (B4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProductsCarry {
    pub products: u64,
    pub stock_movements: u64,
    pub inventory_balances: u64,
    pub inventory_reservations: u64,
}

// ---------------------------------------------------------------------------
// Presence — per TABLE, because two modules own the four
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
    "products",
    "stock_movements",
    "inventory_balances",
    "inventory_reservations",
];

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
/// `CAST(… AS VARCHAR)` so the canonical string crosses verbatim (the same
/// thing `duckdb_store.rs` already does on every read of `invoice_line.quantity`).
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

/// Carry the family from an already-open **read-only** DuckDB connection into
/// an already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// Schema creation is **per present table**, not per family: the four are built
/// by two independent `ensure_schema` calls on DuckDB, so a database can
/// legitimately have some and not others, and creating an empty SQLite table
/// for one the source does not have would manufacture exactly the asymmetry the
/// gate exists to detect.
pub fn carry_products(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<ProductsCarry> {
    let has_products = present_duckdb(src, "products")?;
    let has_movements = present_duckdb(src, "stock_movements")?;
    let has_balances = present_duckdb(src, "inventory_balances")?;
    let has_reservations = present_duckdb(src, "inventory_reservations")?;

    let products = if has_products {
        ensure_products_table(dst)?;
        read_products_duckdb(src)?
    } else {
        Vec::new()
    };
    let movements = if has_movements {
        ensure_stock_movements_table(dst)?;
        read_movements_duckdb(src)?
    } else {
        Vec::new()
    };
    let balances = if has_balances {
        ensure_inventory_balances_table(dst)?;
        read_balances_duckdb(src)?
    } else {
        Vec::new()
    };
    let reservations = if has_reservations {
        ensure_inventory_reservations_table(dst)?;
        read_reservations_duckdb(src)?
    } else {
        Vec::new()
    };

    let products_insert = insert_sql("products", PRODUCT_TEXT_COLUMNS, &["unit_price_minor"]);
    let movements_insert = insert_sql("stock_movements", MOVEMENT_COLUMNS, &[]);
    let balances_insert = insert_sql(
        "inventory_balances",
        BALANCE_TEXT_COLUMNS,
        BALANCE_QTY_COLUMNS,
    );
    let reservations_insert = insert_sql(
        "inventory_reservations",
        RESERVATION_TEXT_COLUMNS,
        RESERVATION_QTY_COLUMNS,
    );

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the products/inventory carry transaction: {e}"))?;

    for p in &products {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(PRODUCT_TEXT_COLUMNS.len() + 1);
        for v in &p.text {
            binds.push(v);
        }
        binds.push(&p.unit_price_minor);
        tx.execute(&products_insert, binds.as_slice())?;
    }
    for m in &movements {
        let binds: Vec<&dyn ToSql> = m.text.iter().map(|v| v as &dyn ToSql).collect();
        tx.execute(&movements_insert, binds.as_slice())?;
    }
    // The rule-7 columns. The `DOUBLE` never reaches the bind: what is bound is
    // the canonical decimal string [`canonical_decimal_from_f64`] proved
    // value-neutral, so a float cannot enter an R2 column even by accident.
    for (b, quantities) in &balances {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(BALANCE_TEXT_COLUMNS.len() + 4);
        for v in &b.text {
            binds.push(v);
        }
        for q in quantities {
            binds.push(q);
        }
        tx.execute(&balances_insert, binds.as_slice())?;
    }
    for (r, quantities) in &reservations {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(RESERVATION_TEXT_COLUMNS.len() + 1);
        for v in &r.text {
            binds.push(v);
        }
        for q in quantities {
            binds.push(q);
        }
        tx.execute(&reservations_insert, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(ProductsCarry {
        products: products.len() as u64,
        stock_movements: movements.len() as u64,
        inventory_balances: balances.len() as u64,
        inventory_reservations: reservations.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

fn products_select(for_duckdb: bool) -> String {
    let cols = if for_duckdb {
        duck_projection(PRODUCT_TEXT_COLUMNS, PRODUCT_DECIMAL_COLUMNS)
    } else {
        PRODUCT_TEXT_COLUMNS.join(", ")
    };
    format!("SELECT {cols}, unit_price_minor FROM products ORDER BY id ASC")
}

fn movements_select(for_duckdb: bool) -> String {
    let cols = if for_duckdb {
        duck_projection(MOVEMENT_COLUMNS, MOVEMENT_DECIMAL_COLUMNS)
    } else {
        MOVEMENT_COLUMNS.join(", ")
    };
    format!("SELECT {cols} FROM stock_movements ORDER BY movement_id ASC")
}

fn balances_select() -> String {
    format!(
        "SELECT {}, {} FROM inventory_balances ORDER BY tenant_id ASC, material_grade ASC",
        BALANCE_TEXT_COLUMNS.join(", "),
        BALANCE_QTY_COLUMNS.join(", ")
    )
}

fn reservations_select() -> String {
    format!(
        "SELECT {}, {} FROM inventory_reservations ORDER BY reservation_id ASC",
        RESERVATION_TEXT_COLUMNS.join(", "),
        RESERVATION_QTY_COLUMNS.join(", ")
    )
}

fn read_products_duckdb(conn: &duckdb::Connection) -> Result<Vec<ProductRow>> {
    let mut stmt = conn.prepare(&products_select(true))?;
    let rows: Vec<ProductRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(PRODUCT_TEXT_COLUMNS.len());
            for i in 0..PRODUCT_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(ProductRow {
                text,
                unit_price_minor: r.get(PRODUCT_TEXT_COLUMNS.len())?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // R2's validation: prove every carried decimal string is one `rust_decimal`
    // can read back, before it crosses — not on first read from SQLite.
    for p in &rows {
        let key = p.text[0].clone().unwrap_or_default();
        for (col, i) in PRODUCT_DECIMAL_COLUMNS.iter().zip(PRODUCT_DECIMAL_INDICES) {
            if let Some(raw) = &p.text[*i] {
                canonical_decimal(raw, &key, &format!("products.{col}"))?;
            }
        }
    }
    Ok(rows)
}

fn read_products_sqlite(conn: &SqliteConn) -> Result<Vec<ProductRow>> {
    let mut stmt = conn.prepare(&products_select(false))?;
    let rows = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(PRODUCT_TEXT_COLUMNS.len());
            for i in 0..PRODUCT_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(ProductRow {
                text,
                unit_price_minor: r.get(PRODUCT_TEXT_COLUMNS.len())?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn read_movements_duckdb(conn: &duckdb::Connection) -> Result<Vec<QtyRow>> {
    let mut stmt = conn.prepare(&movements_select(true))?;
    let rows: Vec<QtyRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(MOVEMENT_COLUMNS.len());
            for i in 0..MOVEMENT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(QtyRow {
                text,
                qty: Vec::new(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for m in &rows {
        let key = m.text[0].clone().unwrap_or_default();
        let raw = m.text[MOVEMENT_QTY_DELTA_INDEX].as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "stock_movements.qty_delta is NULL on {key}, but the column is NOT NULL"
            )
        })?;
        canonical_decimal(raw, &key, "stock_movements.qty_delta")?;
    }
    Ok(rows)
}

fn read_movements_sqlite(conn: &SqliteConn) -> Result<Vec<QtyRow>> {
    let mut stmt = conn.prepare(&movements_select(false))?;
    let rows = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(MOVEMENT_COLUMNS.len());
            for i in 0..MOVEMENT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(QtyRow {
                text,
                qty: Vec::new(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Read `inventory_balances` from DuckDB and canonicalise its four `DOUBLE`s.
///
/// Returns the row **and** the strings that will be bound, so the carry binds
/// exactly what this function proved rather than re-deriving it.
fn read_balances_duckdb(conn: &duckdb::Connection) -> Result<Vec<(QtyRow, Vec<String>)>> {
    let mut stmt = conn.prepare(&balances_select())?;
    let raw: Vec<QtyRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(BALANCE_TEXT_COLUMNS.len());
            for i in 0..BALANCE_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut qty = Vec::with_capacity(BALANCE_QTY_COLUMNS.len());
            for i in 0..BALANCE_QTY_COLUMNS.len() {
                qty.push(r.get::<_, f64>(BALANCE_TEXT_COLUMNS.len() + i)?);
            }
            Ok(QtyRow { text, qty })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    raw.into_iter()
        .map(|row| {
            let key = balance_key(&row);
            let strings = BALANCE_QTY_COLUMNS
                .iter()
                .zip(&row.qty)
                .map(|(col, v)| {
                    canonical_decimal_from_f64(*v, &key, &format!("inventory_balances.{col}"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((row, strings))
        })
        .collect()
}

/// Read `inventory_balances` back from SQLite, converting each R2 string to the
/// `f64` it must equal. **This is the round-trip proof**, and it runs on what
/// is on disk rather than on anything the migrator held.
fn read_balances_sqlite(conn: &SqliteConn) -> Result<Vec<QtyRow>> {
    let mut stmt = conn.prepare(&balances_select())?;
    let rows: Vec<(Vec<Option<String>>, Vec<String>)> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(BALANCE_TEXT_COLUMNS.len());
            for i in 0..BALANCE_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut qty = Vec::with_capacity(BALANCE_QTY_COLUMNS.len());
            for i in 0..BALANCE_QTY_COLUMNS.len() {
                qty.push(r.get::<_, String>(BALANCE_TEXT_COLUMNS.len() + i)?);
            }
            Ok((text, qty))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    rows.into_iter()
        .map(|(text, strings)| {
            let probe = QtyRow {
                text,
                qty: Vec::new(),
            };
            let key = balance_key(&probe);
            let qty = BALANCE_QTY_COLUMNS
                .iter()
                .zip(&strings)
                .map(|(col, s)| {
                    f64_from_canonical_decimal(s, &key, &format!("inventory_balances.{col}"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(QtyRow { qty, ..probe })
        })
        .collect()
}

fn read_reservations_duckdb(conn: &duckdb::Connection) -> Result<Vec<(QtyRow, Vec<String>)>> {
    let mut stmt = conn.prepare(&reservations_select())?;
    let raw: Vec<QtyRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(RESERVATION_TEXT_COLUMNS.len());
            for i in 0..RESERVATION_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut qty = Vec::with_capacity(RESERVATION_QTY_COLUMNS.len());
            for i in 0..RESERVATION_QTY_COLUMNS.len() {
                qty.push(r.get::<_, f64>(RESERVATION_TEXT_COLUMNS.len() + i)?);
            }
            Ok(QtyRow { text, qty })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    raw.into_iter()
        .map(|row| {
            let key = row.text[0].clone().unwrap_or_default();
            let strings = RESERVATION_QTY_COLUMNS
                .iter()
                .zip(&row.qty)
                .map(|(col, v)| {
                    canonical_decimal_from_f64(*v, &key, &format!("inventory_reservations.{col}"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((row, strings))
        })
        .collect()
}

fn read_reservations_sqlite(conn: &SqliteConn) -> Result<Vec<QtyRow>> {
    let mut stmt = conn.prepare(&reservations_select())?;
    let rows: Vec<(Vec<Option<String>>, Vec<String>)> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(RESERVATION_TEXT_COLUMNS.len());
            for i in 0..RESERVATION_TEXT_COLUMNS.len() {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut qty = Vec::with_capacity(RESERVATION_QTY_COLUMNS.len());
            for i in 0..RESERVATION_QTY_COLUMNS.len() {
                qty.push(r.get::<_, String>(RESERVATION_TEXT_COLUMNS.len() + i)?);
            }
            Ok((text, qty))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    rows.into_iter()
        .map(|(text, strings)| {
            let key = text[0].clone().unwrap_or_default();
            let qty = RESERVATION_QTY_COLUMNS
                .iter()
                .zip(&strings)
                .map(|(col, s)| {
                    f64_from_canonical_decimal(s, &key, &format!("inventory_reservations.{col}"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(QtyRow { text, qty })
        })
        .collect()
}

/// `inventory_balances`'s key is the natural composite, so error messages name
/// the row the operator would look for.
fn balance_key(row: &QtyRow) -> String {
    format!(
        "{}/{}",
        row.text[0].as_deref().unwrap_or(""),
        row.text[1].as_deref().unwrap_or("")
    )
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per table, per row, over **every** column. Every
/// number here is re-derived from disk by the gate (B4).
pub fn reconcile_products(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let mut any = false;
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
            (true, true) => any = true,
        }
        row_count(src, dst, table, checks, hard_stops)?;
    }
    if !any {
        checks.push(
            "✓ products/inventory family absent on BOTH sides — nothing to reconcile (a \
             database without products is a legitimate shape; an asymmetry is not)"
                .to_string(),
        );
        return Ok(());
    }

    if present_duckdb(src, "products")? && present_sqlite(dst, "products")? {
        reconcile_products_table(src, dst, checks, hard_stops)?;
    }
    if present_duckdb(src, "stock_movements")? && present_sqlite(dst, "stock_movements")? {
        reconcile_movements(src, dst, checks, hard_stops)?;
    }
    if present_duckdb(src, "inventory_balances")? && present_sqlite(dst, "inventory_balances")? {
        reconcile_balances(src, dst, checks, hard_stops)?;
    }
    if present_duckdb(src, "inventory_reservations")?
        && present_sqlite(dst, "inventory_reservations")?
    {
        reconcile_reservations(src, dst, checks, hard_stops)?;
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

fn reconcile_products_table(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_products_duckdb(src)?;
    let lite: BTreeMap<String, ProductRow> = read_products_sqlite(dst)?
        .into_iter()
        .map(|p| (p.text[0].clone().unwrap_or_default(), p))
        .collect();

    let mut drift = 0usize;
    for p in &duck {
        let key = p.text[0].clone().unwrap_or_default();
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("product {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        for (i, name) in PRODUCT_TEXT_COLUMNS.iter().enumerate() {
            if got.text[i] != p.text[i] {
                hard_stops.push(format!(
                    "product {key}: {name} DuckDB {:?}, SQLite {:?}",
                    p.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
        if got.unit_price_minor != p.unit_price_minor {
            hard_stops.push(format!(
                "product {key}: unit_price_minor DuckDB {}, SQLite {}",
                p.unit_price_minor, got.unit_price_minor
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every products column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            PRODUCT_TEXT_COLUMNS.len() + 1
        ));
    }

    // R1 money, folded in Rust on both sides — never SQL SUM (§3.4 / T-8).
    let duck_price = fold_i64(duck.iter().map(|p| p.unit_price_minor), "unit_price_minor")?;
    let lite_price = fold_i64(
        lite.values().map(|p| p.unit_price_minor),
        "unit_price_minor",
    )?;
    if duck_price == lite_price {
        checks.push(format!(
            "✓ Σ products.unit_price_minor = {duck_price} (i64 fold in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ products.unit_price_minor: DuckDB {duck_price}, SQLite {lite_price}"
        ));
    }

    // R2 quantities, folded over `Decimal`. `SUM` on a TEXT column coerces to
    // REAL in SQLite, which is the class this migration exists to remove.
    for (col, i) in PRODUCT_DECIMAL_COLUMNS.iter().zip(PRODUCT_DECIMAL_INDICES) {
        let d = fold_decimal(duck.iter().filter_map(|p| p.text[*i].as_deref()))?;
        let l = fold_decimal(lite.values().filter_map(|p| p.text[*i].as_deref()))?;
        if d == l {
            checks.push(format!(
                "✓ Σ products.{col} = {d} (Decimal fold in Rust on both sides)"
            ));
        } else {
            hard_stops.push(format!("Σ products.{col}: DuckDB {d}, SQLite {l}"));
        }
    }

    for col in PRODUCT_TEXT_COLUMNS {
        typeof_sweep(dst, "products", col, "text", checks, hard_stops)?;
    }
    typeof_sweep(
        dst,
        "products",
        "unit_price_minor",
        "integer",
        checks,
        hard_stops,
    )?;
    Ok(())
}

fn reconcile_movements(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck = read_movements_duckdb(src)?;
    let lite: BTreeMap<String, QtyRow> = read_movements_sqlite(dst)?
        .into_iter()
        .map(|m| (m.text[0].clone().unwrap_or_default(), m))
        .collect();

    let mut drift = 0usize;
    for m in &duck {
        let key = m.text[0].clone().unwrap_or_default();
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "stock_movement {key} is missing on the SQLite side — the movement ledger is \
                 append-only and is what the stock cache is derived from"
            ));
            drift += 1;
            continue;
        };
        for (i, name) in MOVEMENT_COLUMNS.iter().enumerate() {
            if got.text[i] != m.text[i] {
                hard_stops.push(format!(
                    "stock_movement {key}: {name} DuckDB {:?}, SQLite {:?}",
                    m.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every stock_movements column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            MOVEMENT_COLUMNS.len()
        ));
    }

    let d = fold_decimal(
        duck.iter()
            .filter_map(|m| m.text[MOVEMENT_QTY_DELTA_INDEX].as_deref()),
    )?;
    let l = fold_decimal(
        lite.values()
            .filter_map(|m| m.text[MOVEMENT_QTY_DELTA_INDEX].as_deref()),
    )?;
    if d == l {
        checks.push(format!(
            "✓ Σ stock_movements.qty_delta = {d} (Decimal fold in Rust on both sides; this is \
             the quantity the stock cache is defined as)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ stock_movements.qty_delta: DuckDB {d}, SQLite {l}"
        ));
    }

    for col in MOVEMENT_COLUMNS {
        typeof_sweep(dst, "stock_movements", col, "text", checks, hard_stops)?;
    }
    Ok(())
}

fn reconcile_balances(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck: Vec<QtyRow> = read_balances_duckdb(src)?
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let lite: BTreeMap<String, QtyRow> = read_balances_sqlite(dst)?
        .into_iter()
        .map(|b| (balance_key(&b), b))
        .collect();

    let mut drift = 0usize;
    for b in &duck {
        let key = balance_key(b);
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "inventory_balance {key} is missing on the SQLite side"
            ));
            drift += 1;
            continue;
        };
        for (i, name) in BALANCE_TEXT_COLUMNS.iter().enumerate() {
            if got.text[i] != b.text[i] {
                hard_stops.push(format!(
                    "inventory_balance {key}: {name} DuckDB {:?}, SQLite {:?}",
                    b.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
        // THE rule-7 round-trip. `got.qty` was derived by parsing the stored
        // decimal string and converting back; `b.qty` is the DOUBLE DuckDB
        // holds. Equal means the canonicalisation moved nothing.
        for (i, name) in BALANCE_QTY_COLUMNS.iter().enumerate() {
            if got.qty[i] != b.qty[i] {
                hard_stops.push(format!(
                    "inventory_balance {key}: {name} DuckDB {}, SQLite {} — ADR-0108's rule-7 \
                     resolution carries this DOUBLE to an exact decimal string and it MUST \
                     convert back to the identical value",
                    b.qty[i], got.qty[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every inventory_balances column round-trips with ZERO drift ({} row(s) × {} \
             columns, incl. the four rule-7 quantities proved value-identical to their DOUBLEs)",
            duck.len(),
            BALANCE_TEXT_COLUMNS.len() + BALANCE_QTY_COLUMNS.len()
        ));
    }

    for col in BALANCE_TEXT_COLUMNS {
        typeof_sweep(dst, "inventory_balances", col, "text", checks, hard_stops)?;
    }
    // The load-bearing sweep of this commit: these four were `DOUBLE`. A
    // `'real'` here means the rule-7 resolution did not happen.
    for col in BALANCE_QTY_COLUMNS {
        typeof_sweep(dst, "inventory_balances", col, "text", checks, hard_stops)?;
    }
    Ok(())
}

fn reconcile_reservations(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let duck: Vec<QtyRow> = read_reservations_duckdb(src)?
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let lite: BTreeMap<String, QtyRow> = read_reservations_sqlite(dst)?
        .into_iter()
        .map(|r| (r.text[0].clone().unwrap_or_default(), r))
        .collect();

    let mut drift = 0usize;
    for r in &duck {
        let key = r.text[0].clone().unwrap_or_default();
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!(
                "inventory_reservation {key} is missing on the SQLite side"
            ));
            drift += 1;
            continue;
        };
        for (i, name) in RESERVATION_TEXT_COLUMNS.iter().enumerate() {
            if got.text[i] != r.text[i] {
                hard_stops.push(format!(
                    "inventory_reservation {key}: {name} DuckDB {:?}, SQLite {:?}",
                    r.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in RESERVATION_QTY_COLUMNS.iter().enumerate() {
            if got.qty[i] != r.qty[i] {
                hard_stops.push(format!(
                    "inventory_reservation {key}: {name} DuckDB {}, SQLite {} — ADR-0108's \
                     rule-7 resolution carries this DOUBLE to an exact decimal string and it \
                     MUST convert back to the identical value",
                    r.qty[i], got.qty[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every inventory_reservations column round-trips with ZERO drift ({} row(s) × {} \
             columns, incl. the rule-7 qty proved value-identical to its DOUBLE)",
            duck.len(),
            RESERVATION_TEXT_COLUMNS.len() + RESERVATION_QTY_COLUMNS.len()
        ));
    }

    for col in RESERVATION_TEXT_COLUMNS {
        typeof_sweep(
            dst,
            "inventory_reservations",
            col,
            "text",
            checks,
            hard_stops,
        )?;
    }
    for col in RESERVATION_QTY_COLUMNS {
        typeof_sweep(
            dst,
            "inventory_reservations",
            col,
            "text",
            checks,
            hard_stops,
        )?;
    }
    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// For the five rule-7 columns this is the assertion that the divergence is
/// actually closed: they were `DOUBLE`, and a single `'real'` row here means
/// something wrote a float into a column this commit declared exact.
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
             quantity column, reinstates the float this migration exists to remove"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two const lists must agree, because `read_products_duckdb` indexes
    /// one by the other and an `expect` in a migrator is a boot abort.
    #[test]
    fn the_decimal_column_lists_are_subsets_of_their_text_lists() {
        assert_eq!(PRODUCT_DECIMAL_COLUMNS.len(), PRODUCT_DECIMAL_INDICES.len());
        for (c, i) in PRODUCT_DECIMAL_COLUMNS.iter().zip(PRODUCT_DECIMAL_INDICES) {
            assert_eq!(
                PRODUCT_TEXT_COLUMNS[*i], *c,
                "PRODUCT_DECIMAL_INDICES[{i}] must name {c}; a wrong index would validate and \
                 fold the wrong column while everything still looked green"
            );
        }
        for c in MOVEMENT_DECIMAL_COLUMNS {
            assert!(MOVEMENT_COLUMNS.contains(c), "{c} is not carried");
        }
        assert_eq!(
            MOVEMENT_COLUMNS[MOVEMENT_QTY_DELTA_INDEX], "qty_delta",
            "MOVEMENT_QTY_DELTA_INDEX is used by the NOT NULL check and the Σ fold"
        );
    }

    /// The rule-7 carry is value-neutral on the values a quantity column
    /// actually holds, and **refuses** the ones it cannot carry exactly.
    #[test]
    fn the_rule7_carry_is_value_neutral_or_it_refuses() {
        for (v, want) in [
            (0.0_f64, "0"),
            (-0.0_f64, "0"),
            (1.0, "1"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (2.5, "2.5"),
            (-3.25, "-3.25"),
            (0.000001, "0.000001"),
            (1234.567891, "1234.567891"),
        ] {
            let got = canonical_decimal_from_f64(v, "k", "c").unwrap();
            assert_eq!(got, want, "{v} must canonicalise to {want}");
        }

        // Beyond the canonical scale — refuse, never round.
        let err = canonical_decimal_from_f64(0.0000001, "k", "c").unwrap_err();
        assert!(
            err.to_string().contains("canonical quantity scale"),
            "the refusal must name the scale: {err}"
        );
        // 0.1 + 0.2 is the float this whole rule exists for: it is
        // 0.30000000000000004, needs scale 17, and must not become "0.3".
        let err = canonical_decimal_from_f64(0.1 + 0.2, "k", "c").unwrap_err();
        assert!(
            err.to_string().contains("canonical quantity scale"),
            "{err}"
        );
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = canonical_decimal_from_f64(bad, "k", "c").unwrap_err();
            assert!(
                err.to_string().contains("no decimal representation"),
                "{bad} must be refused: {err}"
            );
        }
    }

    /// A carried string converts back to the `f64` it came from — the property
    /// the gate's per-row arm asserts, checked here without a database.
    #[test]
    fn every_carried_quantity_converts_back_to_the_same_f64() {
        for v in [0.0_f64, 1.0, 1.5, 0.1, 12.75, -4.125, 999999.999999] {
            let s = canonical_decimal_from_f64(v, "k", "c").unwrap();
            assert_eq!(f64_from_canonical_decimal(&s, "k", "c").unwrap(), v);
        }
    }
}
