//! ADR-0108 Step 7 Part I — **the approved-vendor-list (AVL) family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM), E (QA/QC),
//! F (dispatch), G (purchasing) and H (email/relay) are already on this branch,
//! so this is **Part I**. Named rather than silently renumbered — the same call
//! Parts D–H made.
//!
//! ⚠ **Part H's closing sentence — "§7's own family list puts email/relay last
//! among the non-quoting families, so this closes Step 7" — was WRONG, and this
//! Part is the correction.** `avl_vendors` had crossed in **no Part at all**,
//! and §9's Part-G row says so in as many words. §7's family list simply does
//! not name it: the table is owned by a module that no family's boundary reaches
//! (`purchasing.rs` has zero JOINs and every one of its statements names exactly
//! one purchasing table), so each Part correctly excluded it and no Part
//! adopted it. A table that every boundary correctly excludes is a table that
//! silently never crosses, and "the list is complete" read off a list that never
//! contained it is the silent-skip shape rule 11 exists to stop. §9 carries the
//! row; §7's list now names this family.
//!
//! # The family — grepped, not recalled
//!
//! §3.2's rule is that a table name in this plan is measured. A tree-wide sweep
//! of every `CREATE TABLE` in `apps/`, `crates/` and `modules/` whose name
//! matches AVL / vendor / screening / approval returns exactly **one** table:
//!
//! | table | columns | the numbers in it |
//! |---|---|---|
//! | `avl_vendors` | 12 | **none — every column is `VARCHAR`** |
//!
//! Built by one `CREATE TABLE IF NOT EXISTS` in one module
//! (`apps/aberp/src/avl_vendors.rs:222–237`).
//!
//! **There is no second table**, and each absence has a reason worth stating so
//! the next session does not go looking:
//!
//! * **no vendor-event / screening-history table.** Vendor events are appended
//!   to the **audit ledger**, not to a table of this family's own:
//!   `append_vendor_event` (`avl_vendors.rs:577`) routes through the shared
//!   `Handle`'s writer into `aberp_audit_ledger::append_in_tx`. The ledger
//!   crossed in **Step 4** and is not re-carried here.
//! * **no overdue / reminder-state table.** `fire_overdue_screening_reminders`
//!   (`:609`) keeps no persistent state: it re-scans on every boot and re-fires,
//!   which its own docs call the natural reminder cadence.
//! * **the four AVL *overlay* columns are on `partners`, not here.**
//!   `dpas_rating`, `eccn`, `export_screening_status` and `export_screened_at`
//!   are added to `partners` by S361's `ALTER TABLE` ladder
//!   (`partners.rs:730–739`). They are **partners** columns and **Part B already
//!   carries all four** — measured, not assumed. A session that read "AVL" and
//!   reached for them here would carry them twice.
//!
//! # This family has NO number, and that is measured rather than assumed
//!
//! Every one of the twelve columns is `VARCHAR` on DuckDB and crosses as `TEXT`
//! (§3.2 H's mechanical rename). `DECIMAL`, `NUMERIC`, `DOUBLE`, `REAL`,
//! `FLOAT`, `BIGINT`, `INTEGER`, `BOOLEAN` and `BLOB` all return **zero** hits
//! against this table's DDL.
//!
//! So **R1, R2, R3, `canonical_decimal`, `canonical_decimal_from_f64`,
//! [`finite_measurement`](crate::migrate_quality::finite_measurement) and
//! `fold_i64` all have nothing to guard here** — a stronger form of the position
//! Parts E, F and H were in, because those at least carried counts.
//!
//! ⚠ **There is no vendor price, no rating and no score column, and the
//! `DpasRating` in this subsystem's name is not a number.**
//! `aberp_compliance::avl::DpasRating` is the DPAS **priority vocabulary**
//! (`DO`/`DX` + a program code), it is parsed to a closed enum, and it is stored
//! on **`partners`** as a `VARCHAR` — not here. Saying this plainly is the
//! point: "rating" is the one word in this family that would make a later
//! session reach for a float or an R2 money column, and there is no column here
//! for it to reach for. The family test pins the DDL against every numeric type
//! so that adding one is a red rather than an undeclared representation choice.
//!
//! # The refusal, and the honest limit of it
//!
//! [`unique_natural_keys`](crate::migrate_dispatch::unique_natural_keys) is
//! reused verbatim rather than re-implemented, keyed on `tenant_id#id` — the
//! composite every read in `avl_vendors.rs` is scoped by (`WHERE tenant_id = ?
//! AND id = ?`). The gate keys its per-row `BTreeMap` on that composite, so a
//! duplicate would collapse two rows onto one entry and leave the row count, the
//! `typeof` sweep and the per-row arm all green while one row is compared twice
//! and the other never.
//!
//! ⚠ **Like Part H and unlike Parts F and G, this table DOES carry a `PRIMARY
//! KEY`** (`id VARCHAR NOT NULL PRIMARY KEY`) — and it is on the **bare `id`**,
//! not on the composite. So the product's own schema already stops a duplicate
//! `id`, and the refusal cannot fire against a source the product wrote. The
//! guard is here for the shape a `PRIMARY KEY` does not cover — a table
//! hand-recreated by a repair, a restore or an older schema — and the family
//! test reaches it by building exactly that source rather than by pretending the
//! product can produce one. One function, four uses, not four mechanisms.
//!
//! # The duplicate this family CAN produce, and why it is carried not refused
//!
//! ⚠ **`(tenant_id, partner_id)` is NOT unique, deliberately.**
//! `get_vendor_by_partner` (`:347`) — the PO gate's lookup, and the call
//! `purchasing.rs:605`'s `resolve_avl` makes — reads `ORDER BY created_at DESC`
//! and takes the **first** row, which is the module's way of saying a partner
//! may hold several AVL entries over time and the newest wins. That is a
//! property of the source's data model, not a defect, so the carry preserves it
//! and the refusal is keyed on `tenant_id#id` rather than on the partner. Keying
//! the guard on the partner would refuse to migrate a database the product
//! considers correct.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5 and 7B–7H, and Ervin's same constraint: **the migrator
//! half only.** DuckDB remains the source of truth.
//! `apps/aberp/src/avl_vendors.rs` is untouched — every one of its queries still
//! runs against a DuckDB `Connection`, and its `ensure_schema` still builds the
//! DuckDB schema.
//!
//! ⚠ **This does NOT cut purchasing over, and it is not sufficient on its own to
//! let it.** §9's Part-G row sequences purchasing's Rust half behind *both*
//! `avl_vendors` **and** QA/QC, because `create_po` calls
//! `avl_vendors::get_vendor_by_partner(&guard, …)` and `receive_delivery` calls
//! `quality::create_ncr(&mut guard, …)` on the **same writer guard**, and a
//! guard is one engine's connection (rule 14's boundary shape 2). Both are now
//! migrator halves; **neither has a Rust half**, and a migrator half is blind to
//! a shared guard because a migrator never holds the product's. What this Part
//! removes is the "has crossed in NO Part" blocker, not the fusion.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** The module
//! contains **no SQL arithmetic at all** — `SUM`, `AVG`, `COUNT`, `+` and `*`
//! return **zero** hits — and the one place arithmetic could hide is the overdue
//! scan, which filters in **Rust** (`vendor_is_overdue`, `:628`) over rows a
//! plain `SELECT` returned. The T-8 pending-fold register is **unchanged by this
//! commit**: nothing joined it and nothing left it.
//!
//! **Nor does this family carry an M11-shaped hazard**: `LOWER(` and `LIKE`
//! both return **zero** hits across the module. Its closed vocabularies
//! (`ApprovedStatus`, `ApprovalCategory`) are parsed in Rust before a value is
//! ever bound, so no ASCII-`LOWER()` fold crosses the seam here and this
//! family's cutover owes no T-12-shaped fix.
//!
//! **There is no `ensure_columns` ladder**, measured: `ALTER TABLE avl_vendors`
//! returns **zero** hits across `apps/`, `crates/` and `modules/`. The table is
//! created whole and has never been widened, so M8's post-condition has nothing
//! to exercise — the same position Parts E, F and H were in. (The S361 ladder
//! that *is* nearby widens `partners`, and Part B owns it.)

use std::collections::BTreeMap;

use anyhow::Result;

use aberp_db::engine::{Connection as SqliteConn, ToSql};

use crate::migrate_dispatch::unique_natural_keys;

/// The one table in this family.
const TABLE: &str = "avl_vendors";

// ---------------------------------------------------------------------------
// The STRICT DDL — column-for-column, in declaration order, with §3.2's renames
// ---------------------------------------------------------------------------

/// `avl_vendors` as `avl_vendors.rs`'s `SCHEMA_SQL` creates it, `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H) on **all twelve** columns. There is no other
/// rename to make, because there is no other type in the source table.
///
/// **The `PRIMARY KEY` on `id` is preserved verbatim**, because the DuckDB
/// schema declares one — the same call Part H made on `outbound_email_queue`,
/// and the opposite of Parts F and G, where adding a key would have moved an
/// invariant into the DDL on one engine only. Dropping it here would *remove*
/// one the source has.
///
/// **No index.** The source declares none, and `avl_vendors.rs:239–240` states
/// why in as many words: the table is small master data, scanned in full.
///
/// `[[no-sql-specific]]` is otherwise preserved verbatim: **no `CHECK` and no
/// `DEFAULT`**. The closed `approved_status` and `approval_categories`
/// vocabularies live in `ApprovedStatus::from_storage_str` /
/// `ApprovalCategory::from_storage_str`; `STRICT` constrains storage classes,
/// not vocabularies, and porting the vocabulary into the DDL would fork the two
/// schemas.
const SQLITE_AVL_VENDORS_DDL: &str = "
CREATE TABLE IF NOT EXISTS avl_vendors (
    id                  TEXT NOT NULL PRIMARY KEY,
    tenant_id           TEXT NOT NULL,
    partner_id          TEXT NOT NULL,
    approved_status     TEXT NOT NULL,
    approval_categories TEXT NOT NULL,
    approved_until_utc  TEXT,
    screening_notes     TEXT NOT NULL,
    reviewer_login      TEXT NOT NULL,
    reviewed_at_utc     TEXT,
    revoked_reason      TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
) STRICT;
";

/// Build the family's SQLite schema.
pub fn ensure_avl_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_AVL_VENDORS_DDL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// Every column on `avl_vendors` — **all twelve are `TEXT`** — with the two the
/// natural key is built from leading, in the order the carry reads, writes and
/// compares them.
///
/// One list drives the `SELECT`, the `INSERT`, the per-row equality and the
/// `typeof()` sweep, so the four cannot drift into different column orders.
///
/// `tenant_id` leads because every read in `avl_vendors.rs` is tenant-scoped —
/// the same call Part G's four tables made — and `id` follows it because the
/// composite is the gate's key.
///
/// The gate's per-row arm covers **all twelve** rather than a chosen subset, and
/// on this table that is load-bearing: `approved_status` decides whether a
/// purchase order may be raised against the vendor at all, `approval_categories`
/// decides for *what*, and `approved_until_utc` decides *until when*. There is
/// no fold and no storage class that can see a drift in any of them — this
/// family has no number for a sum to catch.
const AVL_TEXT_COLUMNS: &[&str] = &[
    "tenant_id",
    "id",
    "partner_id",
    "approved_status",
    "approval_categories",
    "approved_until_utc",
    "screening_notes",
    "reviewer_login",
    "reviewed_at_utc",
    "revoked_reason",
    "created_at",
    "updated_at",
];

/// One `avl_vendors` row: the 12 `TEXT` columns in [`AVL_TEXT_COLUMNS`] order.
///
/// `Eq` holds: **this family carries no float at all**, so there is no value in
/// it that compares unequal to itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AvlRow {
    text: Vec<Option<String>>,
}

impl AvlRow {
    /// `tenant_id#id` — `AVL_TEXT_COLUMNS[0]` and `[1]`, both `NOT NULL` on both
    /// engines.
    fn key(&self) -> String {
        format!(
            "{}#{}",
            self.text[0].as_deref().unwrap_or(""),
            self.text[1].as_deref().unwrap_or("")
        )
    }
}

/// What the AVL carry did. As everywhere else in this migrator these are **not**
/// the numbers the gate compares (B4) — the gate re-derives every one of them
/// from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AvlCarry {
    pub avl_vendors: u64,
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// Is the AVL family present in this DuckDB database?
///
/// A database in which no vendor has ever been approved is a legitimate shape
/// — the table is created lazily, on the first `ensure_schema` call of the first
/// AVL reader or writer — so the migrator has to be able to answer. The answer
/// is then held symmetrically by the gate: present on one side and absent on the
/// other is a hard stop, never a skip.
///
/// Presence is per **family** rather than per table here, and as in Part H that
/// is not a simplification — the family *is* one table, so the two are the same
/// question.
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = ?",
        [TABLE],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Is the AVL family present in this SQLite database?
pub fn family_present_sqlite(conn: &SqliteConn) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [TABLE],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// The carry
// ---------------------------------------------------------------------------

fn insert_sql() -> String {
    format!(
        "INSERT INTO {TABLE} ({}) VALUES ({})",
        AVL_TEXT_COLUMNS.join(", "),
        vec!["?"; AVL_TEXT_COLUMNS.len()].join(", ")
    )
}

/// Carry the family from an already-open **read-only** DuckDB connection into an
/// already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// The caller owns both connections, the writer lock and the preconditions; this
/// function does the typed transformation and nothing else.
pub fn carry_avl(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<AvlCarry> {
    if !family_present_duckdb(src)? {
        return Ok(AvlCarry::default());
    }
    ensure_avl_schema(dst)?;
    let rows = read_duckdb(src)?;

    let sql = insert_sql();
    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the AVL carry transaction: {e}"))?;
    for row in &rows {
        let binds: Vec<&dyn ToSql> = row.text.iter().map(|v| v as &dyn ToSql).collect();
        tx.execute(&sql, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(AvlCarry {
        avl_vendors: rows.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// The `SELECT` both engines run.
///
/// **There is no `CAST(… AS VARCHAR)` anywhere in this module**, and that is a
/// statement about the family rather than an omission: no column in it is
/// `DECIMAL` or `DOUBLE` on DuckDB, so no value in it needs an R2 projection.
/// Ordered by the natural key's two columns — so both engines hand back the same
/// order and a diff between two runs is a real difference rather than a row
/// order.
///
/// ⚠ **Deliberately NOT ordered by `created_at DESC`**, which is what
/// `get_vendor_by_partner` orders by. That ordering picks the newest of a
/// partner's several AVL entries; this one has to be a **total** order over the
/// carried set, and `created_at` is neither unique nor the key.
fn select_sql() -> String {
    format!(
        "SELECT {} FROM {TABLE} ORDER BY tenant_id ASC, id ASC",
        AVL_TEXT_COLUMNS.join(", ")
    )
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// each read is written once per engine.
///
/// **The DuckDB side validates; the SQLite side does not.** A duplicate key is a
/// property of the source, and [`unique_natural_keys`] must name the source's
/// defect rather than the copy's symptom.
fn read_duckdb(conn: &duckdb::Connection) -> Result<Vec<AvlRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(AVL_TEXT_COLUMNS.len());
        for i in 0..AVL_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(AvlRow { text })
    })?;
    let rows: Vec<AvlRow> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    unique_natural_keys(&rows.iter().map(|r| r.key()).collect::<Vec<_>>(), TABLE)?;
    Ok(rows)
}

fn read_sqlite(conn: &SqliteConn) -> Result<Vec<AvlRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(AVL_TEXT_COLUMNS.len());
        for i in 0..AVL_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(AvlRow { text })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per row, over **every** column. Every number here is
/// re-derived from disk by the gate (B4).
///
/// **There is no sum arm**, and unlike Parts E–H that is not a choice about
/// which columns to fold: this family has no numeric column to fold. The
/// per-row arm over all twelve `TEXT` columns is therefore the *whole* of the
/// value comparison, which is why it covers the full column set rather than a
/// subset.
pub fn reconcile_avl(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    // --- 1. presence, symmetrically ---
    match (family_present_duckdb(src)?, family_present_sqlite(dst)?) {
        (false, false) => {
            checks.push(
                "✓ AVL family absent on BOTH sides — nothing to reconcile (a database in which no \
                 vendor has ever been approved is a legitimate shape; an asymmetry is not)"
                    .to_string(),
            );
            return Ok(());
        }
        (true, false) => {
            hard_stops.push(format!(
                "{TABLE} exists in DuckDB but NOT in SQLite — the family was not carried. This is \
                 the silent-skip shape: every check below would have been skipped and the gate \
                 would have reported PASS"
            ));
            return Ok(());
        }
        (false, true) => {
            hard_stops.push(format!(
                "{TABLE} exists in SQLite but NOT in DuckDB — the copy has a table the source does \
                 not. There is no source of truth to reconcile against"
            ));
            return Ok(());
        }
        (true, true) => {}
    }

    // --- 2. row count ---
    let sql = format!("SELECT count(*) FROM {TABLE}");
    let d: i64 = src.query_row(&sql, [], |r| r.get(0))?;
    let l: i64 = dst.query_row(&sql, [], |r| r.get(0))?;
    if d == l {
        checks.push(format!("✓ {TABLE} row count = {d}"));
    } else {
        hard_stops.push(format!("{TABLE} row count: DuckDB {d}, SQLite {l}"));
    }

    // --- 3. per-row equality over EVERY column, keyed on the natural key ---
    let duck = read_duckdb(src)?;
    let lite: BTreeMap<String, AvlRow> = read_sqlite(dst)?
        .into_iter()
        .map(|r| (r.key(), r))
        .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = row.key();
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("AVL vendor {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        for (i, name) in AVL_TEXT_COLUMNS.iter().enumerate() {
            if row.text[i] != got.text[i] {
                hard_stops.push(format!(
                    "AVL vendor {key}: {name} differs — DuckDB {:?}, SQLite {:?}",
                    row.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {TABLE} column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            AVL_TEXT_COLUMNS.len()
        ));
    }

    // --- 4. typeof() over every row of every column (T-2 / M1) ---
    for col in AVL_TEXT_COLUMNS {
        typeof_sweep(dst, col, "text", checks, hard_stops)?;
    }

    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// NULL is permitted wherever the column is nullable; what must never appear is
/// the **wrong** non-null storage class. On this table the sweep is the pin that
/// every one of the twelve columns really did land as `TEXT`: in SQLite `TEXT`,
/// `INTEGER` and `BLOB` are distinct storage classes that never compare equal,
/// so a `approved_until_utc` that arrived as an `INTEGER` would make every date
/// comparison the overdue scan performs read the wrong answer.
fn typeof_sweep(
    dst: &SqliteConn,
    col: &str,
    want: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let bad: i64 = dst.query_row(
        &format!("SELECT count(*) FROM {TABLE} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
        [want],
        |r| r.get(0),
    )?;
    if bad == 0 {
        checks.push(format!("✓ typeof({TABLE}.{col}) = '{want}' on every row"));
    } else {
        hard_stops.push(format!(
            "{TABLE}.{col}: {bad} row(s) are not '{want}' — in SQLite TEXT, INTEGER and BLOB are \
             distinct storage classes that never compare equal, so a wrong one here silently \
             breaks lookups"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::TEXT;

    /// The carry leads with the two columns its key is built from, and the
    /// `SELECT` orders on the same two.
    #[test]
    fn the_carry_leads_with_the_natural_key() {
        assert_eq!(AVL_TEXT_COLUMNS[0], "tenant_id");
        assert_eq!(AVL_TEXT_COLUMNS[1], "id");
        assert!(
            select_sql().ends_with("ORDER BY tenant_id ASC, id ASC"),
            "{}",
            select_sql()
        );
    }

    /// **The family carries no number of any kind** — the DDL is what proves it
    /// rather than the module docs.
    ///
    /// This is the pin that fails if a later session adds a vendor price, a
    /// score, a numeric rating or a count without deciding its representation —
    /// which is the moment R1, R2, `finite_measurement` and the §3.4 fold rules
    /// start applying to a family that today is exempt from all of them.
    /// `DpasRating` is a vocabulary and lives on `partners`; nothing here is a
    /// number, and this test is what keeps that true.
    #[test]
    fn no_column_in_this_family_is_a_number_or_a_blob() {
        for banned in [
            "DECIMAL", "NUMERIC", "REAL", "DOUBLE", "FLOAT", "BLOB", "BOOLEAN", "INTEGER", "INT",
        ] {
            assert!(
                !SQLITE_AVL_VENDORS_DDL.contains(banned),
                "the AVL family has no numeric column at all — every one of its twelve columns is \
                 VARCHAR on DuckDB. {banned:?} appearing here means that stopped being true, and \
                 the new column needs an ADR-0108 §3.2 classification before it \
                 crosses:\n{SQLITE_AVL_VENDORS_DDL}"
            );
        }
    }

    /// Is `col` declared with `ty` in the DDL? Matched on the line whose first
    /// token is the column name, so the pins below do not depend on the DDL's
    /// column alignment.
    ///
    /// ⚠ **The trailing comma is stripped, and that is load-bearing.** A
    /// **nullable** column carries no `NOT NULL`, so its type token is the last
    /// on the line and reads as `TEXT,` — a bare `nth(1) == Some("TEXT")`
    /// silently reports "not declared TEXT" for exactly the three nullable
    /// columns in this table. That is a false RED here, but the same helper
    /// spelled the same way would be a false GREEN in a `!declared_as(…)`
    /// assertion, which is the shape a later Part is likelier to write.
    fn declared_as(col: &str, ty: &str) -> bool {
        SQLITE_AVL_VENDORS_DDL
            .lines()
            .filter(|l| l.split_whitespace().next() == Some(col))
            .any(|l| l.split_whitespace().nth(1).map(|t| t.trim_end_matches(',')) == Some(ty))
    }

    /// [`declared_as`] sees a **nullable** column's type, not just a
    /// `NOT NULL` one.
    ///
    /// Pinned directly because the bug it guards is invisible: the helper
    /// returns `false` rather than erroring, so a suite that only ever asked it
    /// about `NOT NULL` columns would stay green while the helper was blind to
    /// a third of the table.
    #[test]
    fn declared_as_sees_nullable_columns_too() {
        // The three nullable columns — no `NOT NULL`, so the type token ends in
        // a comma.
        for col in ["approved_until_utc", "reviewed_at_utc", "revoked_reason"] {
            assert!(declared_as(col, "TEXT"), "{col} is nullable TEXT");
        }
        // And it still discriminates rather than answering `true` to anything.
        assert!(!declared_as("approved_until_utc", "INTEGER"));
        assert!(!declared_as("nonexistent_column", "TEXT"));
    }

    /// The declared types this module uses are the vocabulary constants, spelled
    /// the same way, the table is `STRICT`, and **every** column is `TEXT`.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        assert!(!SQLITE_AVL_VENDORS_DDL.contains("VARCHAR"));
        assert!(SQLITE_AVL_VENDORS_DDL.contains("STRICT"));
        for col in AVL_TEXT_COLUMNS {
            assert!(
                declared_as(col, TEXT),
                "{TABLE}.{col} must be declared TEXT (§3.2 H)"
            );
        }
    }

    /// **The `PRIMARY KEY` is preserved and no index is created**, matching the
    /// DuckDB schema.
    ///
    /// Both halves matter. Dropping the key would remove an invariant the source
    /// has; creating an index would fork the two schemas against a table
    /// `avl_vendors.rs:239-240` documents as small master data scanned in full.
    ///
    /// ⚠ The key is on the **bare `id`**, not on `tenant_id#id` — so it does
    /// **not** enforce the composite the gate keys on. That is exactly why
    /// [`unique_natural_keys`] still runs.
    #[test]
    fn the_primary_key_is_kept_on_the_bare_id_and_no_index_is_created() {
        assert!(
            SQLITE_AVL_VENDORS_DDL
                .lines()
                .filter(|l| l.split_whitespace().next() == Some("id"))
                .any(|l| l.contains("PRIMARY KEY")),
            "the DuckDB schema declares `id … PRIMARY KEY`; dropping it here would remove an \
             invariant the source has"
        );
        assert!(
            !SQLITE_AVL_VENDORS_DDL
                .lines()
                .filter(|l| l.split_whitespace().next() == Some("tenant_id"))
                .any(|l| l.contains("PRIMARY KEY")),
            "the source's PRIMARY KEY is on the bare `id`; widening it to the composite here \
             would add an invariant the source does not have"
        );
        assert!(!SQLITE_AVL_VENDORS_DDL.contains("CREATE INDEX"));
        for banned in ["CHECK", "DEFAULT", "UNIQUE"] {
            assert!(
                !SQLITE_AVL_VENDORS_DDL.contains(banned),
                "avl_vendors.rs:239-240's [[no-sql-specific]] posture declares no {banned}"
            );
        }
    }

    /// The generated SQL names every column of the table exactly once, in the
    /// same order, in both statements — which is what makes the positional
    /// decode in [`read_duckdb`] / [`read_sqlite`] safe.
    #[test]
    fn the_select_and_the_insert_agree_on_column_order() {
        assert_eq!(AVL_TEXT_COLUMNS.len(), 12, "the table has twelve columns");
        let joined = AVL_TEXT_COLUMNS.join(", ");
        assert!(
            select_sql().starts_with(&format!("SELECT {joined} FROM {TABLE}")),
            "{}",
            select_sql()
        );
        assert!(
            insert_sql().starts_with(&format!("INSERT INTO {TABLE} ({joined})")),
            "{}",
            insert_sql()
        );
        // And the column set is exactly the DDL's, no more and no fewer.
        let declared = SQLITE_AVL_VENDORS_DDL
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|w| AVL_TEXT_COLUMNS.contains(w))
            .count();
        assert_eq!(
            declared,
            AVL_TEXT_COLUMNS.len(),
            "the carry names {} columns but the DDL declares {declared} of them",
            AVL_TEXT_COLUMNS.len()
        );
    }

    /// The key is the composite, not the bare `id` — and it distinguishes two
    /// rows that differ only in `tenant_id`.
    ///
    /// Pinned because keying on the bare `id` would look correct (the source has
    /// a `PRIMARY KEY` on it) while silently collapsing two tenants' rows in the
    /// gate's `BTreeMap` on any database where an id was reused across tenants.
    #[test]
    fn the_key_is_the_composite_and_separates_tenants() {
        let row = |tenant: &str, id: &str| AvlRow {
            text: (0..AVL_TEXT_COLUMNS.len())
                .map(|i| match i {
                    0 => Some(tenant.to_string()),
                    1 => Some(id.to_string()),
                    _ => None,
                })
                .collect(),
        };
        assert_eq!(row("acme", "avl_1").key(), "acme#avl_1");
        assert_ne!(row("acme", "avl_1").key(), row("globex", "avl_1").key());
        // And the refusal fires on a genuine duplicate of the composite.
        assert!(unique_natural_keys(
            &[row("acme", "avl_1").key(), row("acme", "avl_1").key()],
            TABLE
        )
        .is_err());
        assert!(unique_natural_keys(
            &[row("acme", "avl_1").key(), row("globex", "avl_1").key()],
            TABLE
        )
        .is_ok());
    }
}
