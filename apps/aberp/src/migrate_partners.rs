//! ADR-0108 Step 7 Part B — **the partners family crosses.** `partners` joins
//! `audit_ledger`, `audit_ledger_anchors` and the invoice family on the SQLite
//! side.
//!
//! It is §7's prescribed first Step-7 family, and it is the smallest one in the
//! plan: **one table, 24 columns, and not a single monetary value.** The only
//! non-string column is `issued_invoice_count` (§3.2 F — a count, `BIGINT` →
//! `INTEGER`, unchanged). §3's R1/R2 money rules therefore have nothing to bite
//! on here, which is exactly why this family goes first: the machinery Step 5
//! built gets exercised once more on a family where a mistake cannot move a
//! filed figure.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Step 5, and for the same reason: **DuckDB remains the source of
//! truth.** What lands here is the *migrator half* — the STRICT DDL, the typed
//! row-by-row carry, and the reconciliation gate's arm. `partners.rs`'s own 12
//! DDL sites and its queries still run against DuckDB and are untouched.
//!
//! **M11 and T-12 do NOT land here, and that is deliberate.** §5's M11 is
//! "escape the `LIKE` metacharacters and replace SQL `LOWER()` with Rust
//! `to_lowercase()` on both sides", covering `partners.rs:1001–1005` (the
//! duplicate-partner guard) and `:1049` (the typeahead filter). Those are
//! DuckDB `Connection` queries today; nothing in this commit makes them run
//! against SQLite, so a T-12 written now could only assert that DuckDB's
//! `LOWER()` still folds `Á` — which it does, and which is not the property
//! M11 exists to protect. §7's own ordering rule applies: *a refusal whose test
//! cannot be written yet is not landed yet.* M11/T-12 move to the family's
//! Rust-side crossing, and §9 carries the row.
//!
//! # Two carry decisions, both conservative, both flagged
//!
//! 1. **The base `CREATE` omits all seven ladder columns**, so PR-97, S361 and
//!    S428 actually run through [`ensure_columns`] and M8's post-condition is
//!    exercised rather than no-op'd over a table that already had everything —
//!    the same call the Step-5 module makes and for the same F-1c reason.
//! 2. **`customer_vat_status` and `issued_invoice_count` are carried verbatim,
//!    NULL included, and their SQLite columns are nullable** even though the
//!    DuckDB *base* `CREATE` declares both `NOT NULL DEFAULT`. A partners table
//!    built by the PR-97 ladder rather than by the base `CREATE` has them
//!    nullable on DuckDB too (the ladder adds them unconstrained and backfills
//!    in a follow-on `UPDATE`, because DuckDB rejects a constraint on
//!    `ADD COLUMN`), so nullable is the shape that carries **both** histories.
//!    Re-deriving the backfill here would be the "verify the extraction against
//!    itself" shape B4 forbids: the gate could then no longer compare the two
//!    sides row-for-row without modelling the transformation.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use aberp_db::engine::{Connection as SqliteConn, ToSql};
use aberp_db::schema::{ensure_columns, COUNT_INT, TEXT};

// ---------------------------------------------------------------------------
// The STRICT schema
// ---------------------------------------------------------------------------

/// The pre-PR-97 shape of `partners`, `STRICT`.
///
/// `VARCHAR` → `TEXT` throughout (§3.2 H). The two indexes are carried across
/// unchanged; both key on columns present in this base `CREATE`, so neither
/// has to wait for a ladder.
///
/// S410 / `[[no-sql-specific]]` is preserved verbatim: there is **no** DB-level
/// `CHECK` on `kind`, `customer_vat_status` or `customer_type`. All three are
/// closed vocabularies enforced in Rust (`PartnerKind::from_db_str`,
/// `CustomerType::from_db_str`), and `STRICT` does not change that — it
/// constrains storage classes, not vocabularies.
const SQLITE_PARTNERS_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS partners (
    id                  TEXT NOT NULL PRIMARY KEY,
    tenant_id           TEXT NOT NULL,
    display_name        TEXT NOT NULL,
    legal_name          TEXT NOT NULL,
    kind                TEXT NOT NULL,
    tax_number          TEXT,
    eu_vat_number       TEXT,
    address_street      TEXT,
    address_postal_code TEXT,
    address_city        TEXT,
    address_country     TEXT,
    bank_account        TEXT,
    contact_email       TEXT,
    contact_phone       TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    deleted_at          TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS partners_tenant_deleted_idx
    ON partners (tenant_id, deleted_at);
CREATE INDEX IF NOT EXISTS partners_tenant_display_idx
    ON partners (tenant_id, display_name);
";

/// PR-97 / ADR-0048 — the customer VAT posture and the issued-invoice counter.
///
/// `issued_invoice_count` is the family's only non-string column: §3.2 F, a
/// count, `BIGINT` → `INTEGER`. It is **not** money and no §3 money rule
/// applies to it.
const PARTNERS_PR97: &[(&str, &str)] = &[
    ("customer_vat_status", TEXT),
    ("issued_invoice_count", COUNT_INT),
];

/// S361 / PR-48 (ADR-0078) — the Approved-Vendor-List overlay. All four are
/// nullable with no SQL `DEFAULT`, exactly as on DuckDB: NULL is the "not yet
/// captured" sentinel and the app layer interprets it.
const PARTNERS_S361_AVL: &[(&str, &str)] = &[
    ("dpas_rating", TEXT),
    ("eccn", TEXT),
    ("export_screening_status", TEXT),
    ("export_screened_at", TEXT),
];

/// S428 — the `customer_type` column.
const PARTNERS_S428: &[(&str, &str)] = &[("customer_type", TEXT)];

/// Build the family's SQLite schema: the base `CREATE` + its two indexes, then
/// the three ladders through [`ensure_columns`].
///
/// The ladder order is the DuckDB build's order (`partners::ensure_schema`), so
/// a reader comparing the two schemas can line them up statement for statement.
/// The difference is that `ensure_columns` **proves** afterwards that the
/// columns landed; `ADD COLUMN IF NOT EXISTS` could only ever report that a
/// statement ran.
pub fn ensure_partners_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_PARTNERS_BASE_DDL)
        .context("create the partners STRICT base table")?;

    ensure_columns(conn, "partners", PARTNERS_PR97)?;
    ensure_columns(conn, "partners", PARTNERS_S361_AVL)?;
    ensure_columns(conn, "partners", PARTNERS_S428)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// Every `TEXT` column on `partners`, in the order the carry reads, writes and
/// compares them.
///
/// One list drives the `SELECT`, the `INSERT`, the per-row equality and the
/// `typeof()` sweep. That is what makes the gate's row comparison cover **all**
/// 24 columns instead of a hand-picked subset: on a table with no money, the
/// regulatory payload is spread across `tax_number` / `eu_vat_number` (the
/// buyer identifiers on the NAV filing) and the five identity columns the
/// duplicate-partner guard keys on, and picking "the important ones" here would
/// be picking wrong.
///
/// `&'static str` throughout — the same identifier discipline `ensure_columns`
/// enforces by type, applied to the one place in this module that builds SQL
/// with `format!`.
const PARTNER_TEXT_COLUMNS: &[&str] = &[
    "id",
    "tenant_id",
    "display_name",
    "legal_name",
    "kind",
    "tax_number",
    "eu_vat_number",
    "address_street",
    "address_postal_code",
    "address_city",
    "address_country",
    "bank_account",
    "contact_email",
    "contact_phone",
    "customer_vat_status",
    "created_at",
    "updated_at",
    "deleted_at",
    "dpas_rating",
    "eccn",
    "export_screening_status",
    "export_screened_at",
    "customer_type",
];

/// One `partners` row: the 23 `TEXT` columns in [`PARTNER_TEXT_COLUMNS`] order,
/// plus the single count.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PartnerRow {
    text: Vec<Option<String>>,
    /// §3.2 F. `Option` because the PR-97 ladder adds it unconstrained.
    issued_invoice_count: Option<i64>,
}

impl PartnerRow {
    /// `id` is `PARTNER_TEXT_COLUMNS[0]` and is `NOT NULL` on both engines.
    fn id(&self) -> &str {
        self.text[0].as_deref().unwrap_or("")
    }
}

/// What the partners carry did. As everywhere else in this migrator, these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one
/// of them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PartnersCarry {
    pub partners: u64,
}

/// Is the partners family present in this DuckDB database?
///
/// A ledger-only or invoice-only fixture is a legitimate shape, so the migrator
/// has to be able to answer the question. The answer is then held to
/// symmetrically by the gate: present on one side and absent on the other is a
/// hard stop, never a skip.
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = 'partners'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Is the partners family present in this SQLite database?
pub fn family_present_sqlite(conn: &SqliteConn) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'partners'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Carry the family from an already-open **read-only** DuckDB connection into
/// an already-open SQLite one, inside a single `BEGIN IMMEDIATE` transaction.
///
/// The caller owns both connections, the writer lock and the preconditions;
/// this function does the typed transformation and nothing else.
pub fn carry_partners(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<PartnersCarry> {
    let rows = read_partners_duckdb(src)?;

    let insert = format!(
        "INSERT INTO partners ({}, issued_invoice_count) VALUES ({})",
        PARTNER_TEXT_COLUMNS.join(", "),
        vec!["?"; PARTNER_TEXT_COLUMNS.len() + 1].join(", ")
    );

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the partners carry transaction: {e}"))?;
    for p in &rows {
        let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(PARTNER_TEXT_COLUMNS.len() + 1);
        for v in &p.text {
            binds.push(v);
        }
        binds.push(&p.issued_invoice_count);
        tx.execute(&insert, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(PartnersCarry {
        partners: rows.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The two read sides
// ---------------------------------------------------------------------------

/// The `SELECT` both sides run. Built from [`PARTNER_TEXT_COLUMNS`], so the two
/// readers below cannot drift into two different column orders — which is the
/// only way the per-row equality in the gate could compare the wrong pairs and
/// still look green. Ordered by `id` so a diff between two runs is a real
/// difference rather than a row order.
fn select_sql() -> String {
    format!(
        "SELECT {}, issued_invoice_count FROM partners ORDER BY id ASC",
        PARTNER_TEXT_COLUMNS.join(", ")
    )
}

/// Read `partners` from DuckDB.
fn read_partners_duckdb(conn: &duckdb::Connection) -> Result<Vec<PartnerRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(PARTNER_TEXT_COLUMNS.len());
        for i in 0..PARTNER_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(PartnerRow {
            text,
            issued_invoice_count: r.get(PARTNER_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Read `partners` back from SQLite through the ordinary typed path — no cast,
/// no string parse on the count.
fn read_partners_sqlite(conn: &SqliteConn) -> Result<Vec<PartnerRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(PARTNER_TEXT_COLUMNS.len());
        for i in 0..PARTNER_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        Ok(PartnerRow {
            text,
            issued_invoice_count: r.get(PARTNER_TEXT_COLUMNS.len())?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides. Every number here is re-derived from disk by the gate
/// (B4); none of them was produced by the migrator.
pub fn reconcile_partners(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    // --- 1. presence, symmetrically ---
    match (family_present_duckdb(src)?, family_present_sqlite(dst)?) {
        (false, false) => {
            checks.push(
                "✓ partners family absent on BOTH sides — nothing to reconcile (a database \
                 without partners is a legitimate shape; an asymmetry is not)"
                    .to_string(),
            );
            return Ok(());
        }
        (true, false) => {
            hard_stops.push(
                "the partners family exists in DuckDB but NOT in SQLite — the family was not \
                 carried. This is the silent-skip shape: every check below would have been \
                 skipped and the gate would have reported PASS"
                    .to_string(),
            );
            return Ok(());
        }
        (false, true) => {
            hard_stops.push(
                "the partners family exists in SQLite but NOT in DuckDB — the copy has a table \
                 the source does not. There is no source of truth to reconcile against"
                    .to_string(),
            );
            return Ok(());
        }
        (true, true) => {}
    }

    // --- 2. row count ---
    let d: i64 = src.query_row("SELECT count(*) FROM partners", [], |r| r.get(0))?;
    let l: i64 = dst.query_row("SELECT count(*) FROM partners", [], |r| r.get(0))?;
    if d == l {
        checks.push(format!("✓ partners row count = {d}"));
    } else {
        hard_stops.push(format!("partners row count: DuckDB {d}, SQLite {l}"));
    }

    // --- 3. per-row equality over EVERY column, keyed by the primary key ---
    let duck = read_partners_duckdb(src)?;
    let lite: BTreeMap<String, PartnerRow> = read_partners_sqlite(dst)?
        .into_iter()
        .map(|p| (p.id().to_string(), p))
        .collect();
    let mut drift = 0usize;
    for p in &duck {
        let Some(got) = lite.get(p.id()) else {
            hard_stops.push(format!("partner {} is missing on the SQLite side", p.id()));
            drift += 1;
            continue;
        };
        for (i, name) in PARTNER_TEXT_COLUMNS.iter().enumerate() {
            if got.text[i] != p.text[i] {
                hard_stops.push(format!(
                    "partner {}: {name} DuckDB {:?}, SQLite {:?}",
                    p.id(),
                    p.text[i],
                    got.text[i]
                ));
                drift += 1;
            }
        }
        if got.issued_invoice_count != p.issued_invoice_count {
            hard_stops.push(format!(
                "partner {}: issued_invoice_count DuckDB {:?}, SQLite {:?}",
                p.id(),
                p.issued_invoice_count,
                got.issued_invoice_count
            ));
            drift += 1;
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every partners column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            PARTNER_TEXT_COLUMNS.len() + 1
        ));
    }

    // --- 4. the exact sum, folded in RUST on both sides (never SQL SUM) ---
    // Not money, but the same discipline: SQL `SUM` is where a wrap or a float
    // coercion would enter, and §3.4's rule is about the operation, not about
    // the column's business meaning.
    let duck_count = crate::migrate_billing::fold_i64(
        duck.iter().filter_map(|p| p.issued_invoice_count),
        "issued_invoice_count",
    )?;
    let lite_count = crate::migrate_billing::fold_i64(
        lite.values().filter_map(|p| p.issued_invoice_count),
        "issued_invoice_count",
    )?;
    if duck_count == lite_count {
        checks.push(format!(
            "✓ Σ partners.issued_invoice_count = {duck_count} (folded in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ partners.issued_invoice_count: DuckDB {duck_count}, SQLite {lite_count}"
        ));
    }

    // --- 5. typeof() over every row of every column (T-2) ---
    for col in PARTNER_TEXT_COLUMNS {
        typeof_sweep(dst, col, "text", checks, hard_stops)?;
    }
    typeof_sweep(dst, "issued_invoice_count", "integer", checks, hard_stops)?;

    Ok(())
}

/// `SELECT typeof(col)` over every non-NULL value of one column.
///
/// NULL is permitted wherever the column is nullable; what must never appear is
/// the **wrong** non-null storage class. Under `STRICT` that should have been
/// impossible at `INSERT`, so a hit here means the DDL is not what this gate
/// believes it is.
fn typeof_sweep(
    dst: &SqliteConn,
    col: &'static str,
    want: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let bad: i64 = dst.query_row(
        &format!("SELECT count(*) FROM partners WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
        [want],
        |r| r.get(0),
    )?;
    if bad == 0 {
        checks.push(format!("✓ typeof(partners.{col}) = '{want}' on every row"));
    } else {
        hard_stops.push(format!(
            "partners.{col}: {bad} row(s) are not '{want}' — in SQLite TEXT and INTEGER never \
             compare equal, so a wrong storage class here silently breaks lookups"
        ));
    }
    Ok(())
}
