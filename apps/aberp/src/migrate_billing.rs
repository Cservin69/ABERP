//! ADR-0108 Step 5 — **the fused transactional core crosses**: the invoice
//! family (`invoice`, `invoice_line`, `invoice_series`,
//! `invoice_sequence_state`, `invoice_sequence_reservation`) joins
//! `audit_ledger` + `audit_ledger_anchors` on the SQLite side.
//!
//! This is the first family whose *storage representation* changes, and it is
//! the legally-binding one (ADR-0009, 8-year retention). Three things happen
//! here that did not happen in Step 4:
//!
//! # 1. `ensure_columns` becomes load-bearing, not decorative
//!
//! The DuckDB schema is built as a fresh `CREATE TABLE` **plus** six additive
//! `ALTER TABLE … ADD COLUMN IF NOT EXISTS` ladders (PR-44γ, PR-73, PR-82,
//! PR-84, PR-203, ADR-0101 — 16 columns in all). SQLite has no
//! `IF NOT EXISTS` on `ADD COLUMN`, so ADR-0108 §4.1 routes every one of them
//! through [`aberp_db::schema::ensure_columns`], whose `cols` parameter is
//! `&'static [(&'static str, &'static str)]` — the identifier rule enforced by
//! the type system rather than by a grep.
//!
//! **The base `CREATE` here deliberately omits the 16 ladder columns**, so the
//! ladders actually run and M8's post-condition (re-read `pragma_table_info`,
//! assert every requested column is present, `Err` otherwise) is exercised on
//! the real family instead of being a no-op over a table that already had
//! everything. A ladder that silently added nothing is exactly PR #49 F-1c's
//! fail-open: a column absent → a later read `unwrap_or_default()`s → an ÁFA
//! guard passes vacuously.
//!
//! # 2. §3's representation rules bite for the first time
//!
//! | column | DuckDB today | SQLite (STRICT) | rule |
//! |---|---|---|---|
//! | `invoice.huf_equivalent_total` | `DECIMAL(18,0)` | **`INTEGER`** | R1 |
//! | `invoice_line.unit_price` | `BIGINT` | `INTEGER` | R1 |
//! | `invoice.exchange_rate` | `DECIMAL(18,6)` | `TEXT` | R2 |
//! | `invoice_line.quantity` | `DECIMAL(18,6)` | `TEXT` | R2 |
//! | `exchange_rate_date` / `payment_deadline` / `delivery_date` | `DATE` | `TEXT` | §3.2 H |
//!
//! `huf_equivalent_total` is the single highest-value line in §3: it is the HUF
//! figure that feeds the NAV filing and the printed invoice, and PR #49 F-6a
//! names it as the one that silently becomes a float. It is carried by
//! **parsing the canonical decimal string and refusing anything non-integral**
//! rather than by `CAST(… AS BIGINT)` — a cast cannot tell you it rounded.
//!
//! The R2 columns cross as the **canonical string verbatim**. DuckDB renders
//! `DECIMAL(18,6)` `1.5` as `"1.500000"`; a fresh SQLite write would emit
//! `"1.5"`. Both `Decimal::from_str` to the same value and both `.normalize()`
//! to the same emitted bytes — which is why the `.normalize()` calls in
//! `invoice-pdf/src/format.rs` and `nav_xml.rs` must not be removed.
//!
//! # 3. What is NOT backfilled, and why that is the conservative call
//!
//! The DuckDB ladders end with two `UPDATE` backfills (`currency = 'HUF'`,
//! `vat_rate_kind = 'Percent'`) because they must be idempotent across every
//! boot. **The migrator carries both columns verbatim, NULL included.** The
//! read paths already resolve a NULL `currency` to HUF and a NULL
//! `vat_rate_kind` to `Percent`, so no emitted byte moves — and carrying
//! verbatim is what lets the reconciliation gate compare the two sides
//! **row by row for equality** instead of having to model a transformation.
//! A backfill would have to be re-derived on the DuckDB side by the gate,
//! which is the "verify the extraction against itself" shape B4 forbids.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;

use aberp_db::engine::Connection as SqliteConn;
use aberp_db::schema::{ensure_columns, EXACT_DECIMAL, MONEY_MINOR, TEXT};

// ---------------------------------------------------------------------------
// The STRICT DDL — base tables, then the six ladders
// ---------------------------------------------------------------------------

/// The pre-ladder shape of the five tables, `STRICT`.
///
/// `VARCHAR` → `TEXT`, `BIGINT` → `INTEGER` (SQLite's INTEGER is 64-bit),
/// `DECIMAL(18,6) quantity` → `TEXT` (R2), `BIGINT unit_price` → `INTEGER`
/// (R1). The `PRIMARY KEY` / `UNIQUE` declarations are carried across
/// unchanged: they are the same structural constraints DuckDB already
/// enforces, not new SQL-specific invariants — `[[no-sql-specific]]` is about
/// `CHECK` constraints and closed vocabularies, both of which stay in Rust
/// here exactly as `duckdb_store.rs` documents.
///
/// **`invoice_line.quantity` is created `TEXT` outright**, which is what makes
/// the S157 widen ladder (`MIGRATE_S157_SQL`'s add → backfill → `DROP COLUMN`
/// → `RENAME`) unreachable rather than merely unused: SQLite refuses
/// `DROP COLUMN` on a PK/indexed column, so a ladder that *did* fire would be
/// a hard boot abort. [`ensure_billing_schema`] asserts the ladder's scratch
/// column never exists (ADR-0108 §3.2 C).
const SQLITE_BILLING_BASE_DDL: &str = "
CREATE TABLE IF NOT EXISTS invoice_series (
    id           TEXT    NOT NULL PRIMARY KEY,
    code         TEXT    NOT NULL UNIQUE,
    reset_policy TEXT    NOT NULL,
    fiscal_year  INTEGER,
    created_at   TEXT    NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS invoice_sequence_state (
    series_id   TEXT    NOT NULL,
    fiscal_year INTEGER NOT NULL,
    next_number INTEGER NOT NULL,
    updated_at  TEXT    NOT NULL,
    PRIMARY KEY (series_id, fiscal_year)
) STRICT;

CREATE TABLE IF NOT EXISTS invoice_sequence_reservation (
    id          TEXT    NOT NULL PRIMARY KEY,
    series_id   TEXT    NOT NULL,
    fiscal_year INTEGER NOT NULL,
    number      INTEGER NOT NULL,
    invoice_id  TEXT    NOT NULL UNIQUE,
    status      TEXT    NOT NULL,
    void_reason TEXT,
    reserved_at TEXT    NOT NULL,
    used_at     TEXT,
    voided_at   TEXT,
    UNIQUE (series_id, fiscal_year, number)
) STRICT;

CREATE TABLE IF NOT EXISTS invoice (
    id              TEXT    NOT NULL PRIMARY KEY,
    series_id       TEXT    NOT NULL,
    customer_id     TEXT    NOT NULL,
    issue_date      TEXT    NOT NULL,
    sequence_number INTEGER NOT NULL,
    fiscal_year     INTEGER NOT NULL,
    idempotency_key TEXT    NOT NULL UNIQUE,
    UNIQUE (series_id, fiscal_year, sequence_number)
) STRICT;

CREATE TABLE IF NOT EXISTS invoice_line (
    invoice_id            TEXT    NOT NULL,
    ordinal               INTEGER NOT NULL,
    description           TEXT    NOT NULL,
    quantity              TEXT    NOT NULL,
    unit_price            INTEGER NOT NULL,
    vat_rate_basis_points INTEGER NOT NULL,
    PRIMARY KEY (invoice_id, ordinal)
) STRICT;
";

/// PR-44γ / ADR-0037 — currency + the four exchange-rate columns.
///
/// `exchange_rate` is R2 (`TEXT` decimal); `huf_equivalent_total` is **R1**
/// (`INTEGER` minor units) and is the one column in this family whose Rust-side
/// bind changes, from `i64::to_string()` to a bare `i64`.
const INVOICE_PR_44C: &[(&str, &str)] = &[
    ("currency", TEXT),
    ("exchange_rate", EXACT_DECIMAL),
    ("exchange_rate_source", TEXT),
    ("exchange_rate_date", TEXT),
    ("huf_equivalent_total", MONEY_MINOR),
];

/// PR-73 / ADR-0040 §addendum — the denormalized per-invoice bank snapshot.
const INVOICE_PR_73: &[(&str, &str)] = &[
    ("bank_account_id", TEXT),
    ("bank_account_currency", TEXT),
    ("bank_account_number", TEXT),
    ("bank_account_bank_name", TEXT),
    ("bank_account_swift_bic", TEXT),
];

/// PR-82 — the buyer-facing invoice-level note. Never reaches the NAV XML
/// (`adr/0042-invoice-notes-never-in-nav-xml.md`).
const INVOICE_PR_82: &[(&str, &str)] = &[("invoice_note", TEXT)];

/// PR-82 — the buyer-facing per-line note. Same regulatory boundary.
const INVOICE_LINE_PR_82: &[(&str, &str)] = &[("note", TEXT)];

/// PR-84 — the two operator-chosen calendar dates. `DATE` → `TEXT` (§3.2 H):
/// they are already canonical ISO-8601 `YYYY-MM-DD` on the wire and already
/// read through `CAST(… AS VARCHAR)`.
const INVOICE_PR_84: &[(&str, &str)] = &[("payment_deadline", TEXT), ("delivery_date", TEXT)];

/// PR-203 / S203 — the per-invoice email recipient override.
const INVOICE_PR_203: &[(&str, &str)] = &[("email_recipient_override", TEXT)];

/// ADR-0101 — the per-line VAT rate-KIND. Carried verbatim (NULL included);
/// the read path resolves NULL to `Percent`, so no emitted byte moves.
const INVOICE_LINE_ADR0101: &[(&str, &str)] = &[("vat_rate_kind", TEXT)];

/// Build the family's SQLite schema: the base `CREATE`s, then the six ladders
/// through [`ensure_columns`].
///
/// Every ladder is the same call shape the DuckDB build makes, in the same
/// order, so a reader comparing the two schemas can line them up. The
/// difference is that `ensure_columns` **proves** afterwards that the columns
/// landed; `ADD COLUMN IF NOT EXISTS` could only ever report that a statement
/// ran.
pub fn ensure_billing_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_BILLING_BASE_DDL)
        .context("create the invoice family's STRICT base tables")?;

    ensure_columns(conn, "invoice", INVOICE_PR_44C)?;
    ensure_columns(conn, "invoice", INVOICE_PR_73)?;
    ensure_columns(conn, "invoice", INVOICE_PR_82)?;
    ensure_columns(conn, "invoice_line", INVOICE_LINE_PR_82)?;
    ensure_columns(conn, "invoice", INVOICE_PR_84)?;
    ensure_columns(conn, "invoice", INVOICE_PR_203)?;
    ensure_columns(conn, "invoice_line", INVOICE_LINE_ADR0101)?;

    // §3.2 C — the S157 widen ladder must be UNREACHABLE, not merely unused.
    // Its trigger on DuckDB is `quantity_column_is_integer`, which queries
    // `information_schema.columns` and `.ok()`s the error to `None` → `false`;
    // on SQLite that query is a hard parse error, so the guard would silently
    // report "not integer" and the ladder would never fire. That happens to be
    // the correct outcome and the wrong mechanism for reaching it — so assert
    // the outcome directly instead of relying on a swallowed error.
    let scratch: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('invoice_line') WHERE name = 'quantity_dec'",
        [],
        |r| r.get(0),
    )?;
    if scratch != 0 {
        bail!(
            "invoice_line.quantity_dec exists on the SQLite side. That is the S157 widen \
             ladder's scratch column, and the ladder is unreachable here by construction \
             (quantity is created TEXT outright). Its presence means something ran the \
             DuckDB ladder against SQLite, where the DROP COLUMN step cannot succeed on a \
             PK-indexed column."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One `invoice` row. A struct rather than a 21-wide tuple: the money columns
/// have to be named at the point they are validated, and a positional `r.11`
/// on the HUF total is exactly the sort of thing that gets mis-indexed once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceRow {
    pub id: String,
    pub series_id: String,
    pub customer_id: String,
    pub issue_date: String,
    pub sequence_number: i64,
    pub fiscal_year: i32,
    pub idempotency_key: String,
    pub currency: Option<String>,
    /// R2 — the canonical decimal string, carried verbatim.
    pub exchange_rate: Option<String>,
    pub exchange_rate_source: Option<String>,
    pub exchange_rate_date: Option<String>,
    /// **R1** — minor units. Parsed from the canonical decimal string and
    /// refused if non-integral; never cast.
    pub huf_equivalent_total: Option<i64>,
    pub bank_account_id: Option<String>,
    pub bank_account_currency: Option<String>,
    pub bank_account_number: Option<String>,
    pub bank_account_bank_name: Option<String>,
    pub bank_account_swift_bic: Option<String>,
    pub invoice_note: Option<String>,
    pub payment_deadline: Option<String>,
    pub delivery_date: Option<String>,
    pub email_recipient_override: Option<String>,
}

/// One `invoice_line` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceLineRow {
    pub invoice_id: String,
    pub ordinal: i32,
    pub description: String,
    /// R2 — the canonical decimal string, carried verbatim.
    pub quantity: String,
    /// R1 — minor units, already `i64` on both sides.
    pub unit_price: i64,
    pub vat_rate_basis_points: i32,
    pub note: Option<String>,
    pub vat_rate_kind: Option<String>,
}

/// What the billing carry did. As with [`crate::migrate_to_sqlite`], these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one
/// of them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BillingCarry {
    pub series: u64,
    pub sequence_state: u64,
    pub reservations: u64,
    pub invoices: u64,
    pub invoice_lines: u64,
}

/// Is the invoice family present in this DuckDB database?
///
/// A ledger-only fixture is a legitimate shape (Step 4's tests seed exactly
/// that), so the migrator has to be able to answer the question. It is
/// answered on the **`invoice` table only** and the answer is then held to
/// symmetrically by the gate: present on one side and absent on the other is a
/// hard stop, never a skip.
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = 'invoice'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Is the invoice family present in this SQLite database?
pub fn family_present_sqlite(conn: &SqliteConn) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'invoice'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Carry the whole family from an already-open **read-only** DuckDB connection
/// into an already-open SQLite one, inside a single `BEGIN IMMEDIATE`
/// transaction.
///
/// The caller owns both connections, the writer lock and the preconditions;
/// this function does the typed transformation and nothing else. Rule 15's
/// shape: all five tables land in one transaction, so a failure part-way
/// through leaves no half-carried family.
pub fn carry_billing(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<BillingCarry> {
    let series = read_series(src)?;
    let state = read_sequence_state(src)?;
    let reservations = read_reservations(src)?;
    let invoices = read_invoices(src)?;
    let lines = read_invoice_lines(src)?;

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the billing carry transaction: {e}"))?;

    for s in &series {
        tx.execute(
            "INSERT INTO invoice_series (id, code, reset_policy, fiscal_year, created_at)
             VALUES (?, ?, ?, ?, ?)",
            aberp_db::engine::params![s.0, s.1, s.2, s.3, s.4],
        )?;
    }
    for s in &state {
        tx.execute(
            "INSERT INTO invoice_sequence_state (series_id, fiscal_year, next_number, updated_at)
             VALUES (?, ?, ?, ?)",
            aberp_db::engine::params![s.0, s.1, s.2, s.3],
        )?;
    }
    for r in &reservations {
        tx.execute(
            "INSERT INTO invoice_sequence_reservation
               (id, series_id, fiscal_year, number, invoice_id, status, void_reason,
                reserved_at, used_at, voided_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            aberp_db::engine::params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9],
        )?;
    }
    for i in &invoices {
        tx.execute(
            "INSERT INTO invoice
               (id, series_id, customer_id, issue_date, sequence_number, fiscal_year,
                idempotency_key, currency, exchange_rate, exchange_rate_source,
                exchange_rate_date, huf_equivalent_total, bank_account_id,
                bank_account_currency, bank_account_number, bank_account_bank_name,
                bank_account_swift_bic, invoice_note, payment_deadline, delivery_date,
                email_recipient_override)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            aberp_db::engine::params![
                i.id,
                i.series_id,
                i.customer_id,
                i.issue_date,
                i.sequence_number,
                i.fiscal_year,
                i.idempotency_key,
                i.currency,
                i.exchange_rate,
                i.exchange_rate_source,
                i.exchange_rate_date,
                i.huf_equivalent_total,
                i.bank_account_id,
                i.bank_account_currency,
                i.bank_account_number,
                i.bank_account_bank_name,
                i.bank_account_swift_bic,
                i.invoice_note,
                i.payment_deadline,
                i.delivery_date,
                i.email_recipient_override,
            ],
        )?;
    }
    for l in &lines {
        tx.execute(
            "INSERT INTO invoice_line
               (invoice_id, ordinal, description, quantity, unit_price,
                vat_rate_basis_points, note, vat_rate_kind)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            aberp_db::engine::params![
                l.invoice_id,
                l.ordinal,
                l.description,
                l.quantity,
                l.unit_price,
                l.vat_rate_basis_points,
                l.note,
                l.vat_rate_kind,
            ],
        )?;
    }
    tx.commit()?;

    Ok(BillingCarry {
        series: series.len() as u64,
        sequence_state: state.len() as u64,
        reservations: reservations.len() as u64,
        invoices: invoices.len() as u64,
        invoice_lines: lines.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The DuckDB read side
// ---------------------------------------------------------------------------

type SeriesRow = (String, String, String, Option<i32>, String);
type SequenceStateRow = (String, i32, i64, String);
type ReservationRow = (
    String,
    String,
    i32,
    i64,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

fn read_series(conn: &duckdb::Connection) -> Result<Vec<SeriesRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, code, reset_policy, fiscal_year, created_at
             FROM invoice_series ORDER BY id ASC",
        )
        .context("prepare the invoice_series read")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_sequence_state(conn: &duckdb::Connection) -> Result<Vec<SequenceStateRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT series_id, fiscal_year, next_number, updated_at
             FROM invoice_sequence_state ORDER BY series_id, fiscal_year ASC",
        )
        .context("prepare the invoice_sequence_state read")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_reservations(conn: &duckdb::Connection) -> Result<Vec<ReservationRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, series_id, fiscal_year, number, invoice_id, status, void_reason,
                    reserved_at, used_at, voided_at
             FROM invoice_sequence_reservation ORDER BY id ASC",
        )
        .context("prepare the invoice_sequence_reservation read")?;
    let rows = stmt.query_map([], |r| {
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
            r.get(9)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Read `invoice`, applying §3's representation rules **with a refusal on
/// anything that does not round-trip**.
///
/// The two `DECIMAL` columns are read as `CAST(… AS VARCHAR)`, which is what
/// the tree's own read paths already do (`duckdb_store.rs`'s Decimal-as-string
/// bind, `invoice_currency_metadata.rs`). `huf_equivalent_total` is then
/// *parsed* rather than cast: `CAST(… AS BIGINT)` cannot tell you it rounded,
/// and this is the column that feeds the NAV filing.
pub fn read_invoices(conn: &duckdb::Connection) -> Result<Vec<InvoiceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, series_id, customer_id, issue_date, sequence_number, fiscal_year,
                    idempotency_key, currency,
                    CAST(exchange_rate AS VARCHAR), exchange_rate_source,
                    CAST(exchange_rate_date AS VARCHAR),
                    CAST(huf_equivalent_total AS VARCHAR),
                    bank_account_id, bank_account_currency, bank_account_number,
                    bank_account_bank_name, bank_account_swift_bic, invoice_note,
                    CAST(payment_deadline AS VARCHAR), CAST(delivery_date AS VARCHAR),
                    email_recipient_override
             FROM invoice ORDER BY id ASC",
        )
        .context(
            "prepare the invoice read — every column of the six ADR-0108 §4.2 ladders must \
             exist on the DuckDB side; a missing one means `ensure_schema` never ran against \
             this database",
        )?;
    #[allow(clippy::type_complexity)]
    let raw: Vec<(
        String,
        String,
        String,
        String,
        i64,
        i32,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
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
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
                r.get(14)?,
                r.get(15)?,
                r.get(16)?,
                r.get(17)?,
                r.get(18)?,
                r.get(19)?,
                r.get(20)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    raw.into_iter()
        .map(|r| {
            let id = r.0;
            let exchange_rate =
                r.8.map(|s| canonical_decimal(&s, &id, "invoice.exchange_rate"))
                    .transpose()?;
            let huf_equivalent_total =
                r.11.map(|s| money_minor_units(&s, &id, "invoice.huf_equivalent_total"))
                    .transpose()?;
            Ok(InvoiceRow {
                id,
                series_id: r.1,
                customer_id: r.2,
                issue_date: r.3,
                sequence_number: r.4,
                fiscal_year: r.5,
                idempotency_key: r.6,
                currency: r.7,
                exchange_rate,
                exchange_rate_source: r.9,
                exchange_rate_date: r.10,
                huf_equivalent_total,
                bank_account_id: r.12,
                bank_account_currency: r.13,
                bank_account_number: r.14,
                bank_account_bank_name: r.15,
                bank_account_swift_bic: r.16,
                invoice_note: r.17,
                payment_deadline: r.18,
                delivery_date: r.19,
                email_recipient_override: r.20,
            })
        })
        .collect()
}

/// Read `invoice_line`. `quantity` crosses as its canonical string; the parse
/// here is a **validation**, not a transformation — the stored bytes are the
/// ones DuckDB rendered.
pub fn read_invoice_lines(conn: &duckdb::Connection) -> Result<Vec<InvoiceLineRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT invoice_id, ordinal, description, CAST(quantity AS VARCHAR), unit_price,
                    vat_rate_basis_points, note, vat_rate_kind
             FROM invoice_line ORDER BY invoice_id, ordinal ASC",
        )
        .context("prepare the invoice_line read")?;
    #[allow(clippy::type_complexity)]
    let raw: Vec<(
        String,
        i32,
        String,
        String,
        i64,
        i32,
        Option<String>,
        Option<String>,
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
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    raw.into_iter()
        .map(|r| {
            let key = format!("{}#{}", r.0, r.1);
            let quantity = canonical_decimal(&r.3, &key, "invoice_line.quantity")?;
            Ok(InvoiceLineRow {
                invoice_id: r.0,
                ordinal: r.1,
                description: r.2,
                quantity,
                unit_price: r.4,
                vat_rate_basis_points: r.5,
                note: r.6,
                vat_rate_kind: r.7,
            })
        })
        .collect()
}

/// **R2's validation.** The string crosses verbatim; this proves it is a value
/// `rust_decimal` can read back, so a later `Decimal::from_str` on the SQLite
/// side cannot be the first thing to discover it is not.
///
/// Returns the *original* string, never a re-rendered one: `"1.500000"` must
/// stay `"1.500000"`, because the formatters `.normalize()` and re-rendering
/// here would make the migration's trailing-zero behaviour depend on when a
/// row happened to be written.
pub(crate) fn canonical_decimal(raw: &str, key: &str, column: &str) -> Result<String> {
    Decimal::from_str(raw).map_err(|e| {
        anyhow::anyhow!(
            "{column} on {key} is {raw:?}, which rust_decimal cannot parse ({e}). Refusing to \
             carry a value the SQLite read path could not restore — ADR-0108 R2 stores the \
             canonical decimal string and nothing else."
        )
    })?;
    Ok(raw.to_string())
}

/// **R1's validation.** `DECIMAL(18,0)` is scale-0 by declaration, so a
/// fractional value here means the column's own type was not what it claimed.
/// Refuse rather than round: this is the HUF figure on the NAV wire.
fn money_minor_units(raw: &str, key: &str, column: &str) -> Result<i64> {
    let d = Decimal::from_str(raw).map_err(|e| {
        anyhow::anyhow!("{column} on {key} is {raw:?}, which rust_decimal cannot parse ({e})")
    })?;
    if d.fract() != Decimal::ZERO {
        bail!(
            "{column} on {key} is {raw:?}, which is not a whole number of minor units. \
             ADR-0108 R1 stores money as INTEGER minor units and this value cannot cross \
             without losing precision — refusing rather than rounding the figure that goes \
             on the NAV filing."
        );
    }
    i64::try_from(d).map_err(|e| {
        anyhow::anyhow!("{column} on {key} is {raw:?}, which does not fit in an i64 ({e})")
    })
}

// ---------------------------------------------------------------------------
// The reconciliation gate's billing arm
// ---------------------------------------------------------------------------

/// The per-family reconciliation, appended to the Step-4 report.
///
/// Every number here is re-derived from disk by the gate's own connections
/// (B4): nothing the migrator computed is admissible. The checks, in the order
/// ADR-0108 §6.3 lists them:
///
/// 1. **Symmetry** — present on one side and absent on the other is a hard
///    stop. A ledger-only source is a legitimate shape; a source that has the
///    family while the copy does not is a silently unmigrated family.
/// 2. **Per-table row counts.**
/// 3. **Per-row equality on the §3.2 money / decimal columns**, keyed by
///    primary key. Strictly stronger than the sum check below — a sum can hide
///    two compensating errors.
/// 4. **Per-money-column exact sums, folded in Rust on both sides**, never with
///    SQL `SUM` (§3.4: `TEXT * INTEGER` coerces to `REAL`, and `SUM` over
///    INTEGER raises on i64 overflow). This is the check that catches an
///    overflow the per-row pass cannot see.
/// 5. **`typeof()` over every row** of every §3.2 column in the family (T-2).
/// 6. **The sequence tables agree**, bucket by bucket — the S444 surface.
pub fn reconcile_billing(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let in_duck = family_present_duckdb(src)?;
    let in_lite = family_present_sqlite(dst)?;
    match (in_duck, in_lite) {
        (false, false) => {
            checks.push(
                "✓ invoice family absent on BOTH sides — nothing to reconcile (a ledger-only \
                 database is a legitimate shape; an asymmetry is not)"
                    .to_string(),
            );
            return Ok(());
        }
        (true, false) => {
            hard_stops.push(
                "the invoice family exists in DuckDB but NOT in SQLite — the family was not \
                 carried. This is the silent-skip shape: every check below would have been \
                 skipped and the gate would have reported PASS"
                    .to_string(),
            );
            return Ok(());
        }
        (false, true) => {
            hard_stops.push(
                "the invoice family exists in SQLite but NOT in DuckDB — the copy has tables \
                 the source does not. There is no source of truth to reconcile against"
                    .to_string(),
            );
            return Ok(());
        }
        (true, true) => {}
    }

    // --- 2. row counts ---
    for table in [
        "invoice_series",
        "invoice_sequence_state",
        "invoice_sequence_reservation",
        "invoice",
        "invoice_line",
    ] {
        let d: i64 = src.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        let l: i64 = dst.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if d == l {
            checks.push(format!("✓ {table} row count = {d}"));
        } else {
            hard_stops.push(format!("{table} row count: DuckDB {d}, SQLite {l}"));
        }
    }

    // --- 3. per-row money / decimal equality, keyed by primary key ---
    let duck_invoices = read_invoices(src)?;
    let lite_invoices = read_sqlite_invoice_money(dst)?;
    let mut drift = 0usize;
    for i in &duck_invoices {
        match lite_invoices.get(&i.id) {
            None => {
                hard_stops.push(format!("invoice {} is missing on the SQLite side", i.id));
                drift += 1;
            }
            Some((rate, huf)) => {
                if rate != &i.exchange_rate {
                    hard_stops.push(format!(
                        "invoice {}: exchange_rate DuckDB {:?}, SQLite {:?}",
                        i.id, i.exchange_rate, rate
                    ));
                    drift += 1;
                }
                if huf != &i.huf_equivalent_total {
                    hard_stops.push(format!(
                        "invoice {}: huf_equivalent_total DuckDB {:?}, SQLite {:?} — this is \
                         the HUF figure on the NAV filing",
                        i.id, i.huf_equivalent_total, huf
                    ));
                    drift += 1;
                }
            }
        }
    }
    let duck_lines = read_invoice_lines(src)?;
    let lite_lines = read_sqlite_line_money(dst)?;
    for l in &duck_lines {
        let key = (l.invoice_id.clone(), l.ordinal);
        match lite_lines.get(&key) {
            None => {
                hard_stops.push(format!(
                    "invoice_line {}#{} is missing on the SQLite side",
                    l.invoice_id, l.ordinal
                ));
                drift += 1;
            }
            Some((qty, price)) => {
                if qty != &l.quantity {
                    hard_stops.push(format!(
                        "invoice_line {}#{}: quantity DuckDB {:?}, SQLite {:?}",
                        l.invoice_id, l.ordinal, l.quantity, qty
                    ));
                    drift += 1;
                }
                if *price != l.unit_price {
                    hard_stops.push(format!(
                        "invoice_line {}#{}: unit_price DuckDB {}, SQLite {}",
                        l.invoice_id, l.ordinal, l.unit_price, price
                    ));
                    drift += 1;
                }
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every monetary value round-trips with ZERO drift ({} invoices × \
             {{exchange_rate, huf_equivalent_total}}, {} lines × {{quantity, unit_price}})",
            duck_invoices.len(),
            duck_lines.len()
        ));
    }

    // --- 4. exact sums, folded in RUST on both sides (never SQL SUM) ---
    let duck_unit_price = fold_i64(duck_lines.iter().map(|l| l.unit_price), "unit_price")?;
    let lite_unit_price = fold_i64(lite_lines.values().map(|(_, p)| *p), "unit_price")?;
    if duck_unit_price == lite_unit_price {
        checks.push(format!(
            "✓ Σ invoice_line.unit_price = {duck_unit_price} (folded in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ invoice_line.unit_price: DuckDB {duck_unit_price}, SQLite {lite_unit_price}"
        ));
    }
    let duck_huf = fold_i64(
        duck_invoices.iter().filter_map(|i| i.huf_equivalent_total),
        "huf_equivalent_total",
    )?;
    let lite_huf = fold_i64(
        lite_invoices.values().filter_map(|(_, h)| *h),
        "huf_equivalent_total",
    )?;
    if duck_huf == lite_huf {
        checks.push(format!(
            "✓ Σ invoice.huf_equivalent_total = {duck_huf} (folded in Rust on both sides)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ invoice.huf_equivalent_total: DuckDB {duck_huf}, SQLite {lite_huf}"
        ));
    }
    let duck_qty = fold_decimal(duck_lines.iter().map(|l| l.quantity.as_str()))?;
    let lite_qty = fold_decimal(lite_lines.values().map(|(q, _)| q.as_str()))?;
    if duck_qty == lite_qty {
        checks.push(format!(
            "✓ Σ invoice_line.quantity = {duck_qty} (Decimal fold in Rust; SQL SUM over a \
             TEXT column coerces to REAL)"
        ));
    } else {
        hard_stops.push(format!(
            "Σ invoice_line.quantity: DuckDB {duck_qty}, SQLite {lite_qty}"
        ));
    }

    // --- 5. typeof() over every row (T-2) ---
    let sweep: &[(&str, &str, &str)] = &[
        ("invoice", "exchange_rate", "text"),
        ("invoice", "huf_equivalent_total", "integer"),
        ("invoice", "sequence_number", "integer"),
        ("invoice", "fiscal_year", "integer"),
        ("invoice", "exchange_rate_date", "text"),
        ("invoice", "payment_deadline", "text"),
        ("invoice", "delivery_date", "text"),
        ("invoice_line", "quantity", "text"),
        ("invoice_line", "unit_price", "integer"),
        ("invoice_line", "vat_rate_basis_points", "integer"),
        ("invoice_sequence_state", "next_number", "integer"),
        ("invoice_sequence_reservation", "number", "integer"),
    ];
    for (table, col, want) in sweep {
        let bad: i64 = dst.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
            [want],
            |r| r.get(0),
        )?;
        if bad == 0 {
            checks.push(format!("✓ typeof({table}.{col}) = '{want}' on every row"));
        } else {
            hard_stops.push(format!(
                "{table}.{col}: {bad} row(s) are not '{want}' — under STRICT that should have \
                 been impossible at INSERT, so a hit here means the DDL is not what this gate \
                 believes it is"
            ));
        }
    }

    // --- 6. the sequence tables, bucket by bucket (the S444 surface) ---
    let duck_state = read_sequence_state(src)?;
    let mut lite_state_stmt = dst.prepare(
        "SELECT series_id, fiscal_year, next_number FROM invoice_sequence_state
         ORDER BY series_id, fiscal_year ASC",
    )?;
    let lite_state: BTreeMap<(String, i32), i64> = lite_state_stmt
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get::<_, i64>(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut bucket_drift = 0usize;
    for (series_id, fiscal_year, next_number, _) in &duck_state {
        match lite_state.get(&(series_id.clone(), *fiscal_year)) {
            Some(n) if n == next_number => {}
            other => {
                hard_stops.push(format!(
                    "invoice_sequence_state[{series_id}, {fiscal_year}]: DuckDB next_number \
                     {next_number}, SQLite {other:?} — the invoice-number allocator's stored \
                     floor must cross exactly or the copy can re-issue a szamla number"
                ));
                bucket_drift += 1;
            }
        }
    }
    if bucket_drift == 0 {
        checks.push(format!(
            "✓ invoice_sequence_state agrees bucket-by-bucket ({} bucket(s))",
            duck_state.len()
        ));
    }

    Ok(())
}

/// The SQLite side's money columns for `invoice`, read through the ordinary
/// typed path: `huf_equivalent_total` comes back as a bare `i64` — no cast, no
/// string parse — which is the whole point of R1.
fn read_sqlite_invoice_money(
    conn: &SqliteConn,
) -> Result<BTreeMap<String, (Option<String>, Option<i64>)>> {
    let mut stmt =
        conn.prepare("SELECT id, exchange_rate, huf_equivalent_total FROM invoice ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?),
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The SQLite side's money columns for `invoice_line`.
fn read_sqlite_line_money(conn: &SqliteConn) -> Result<BTreeMap<(String, i32), (String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT invoice_id, ordinal, quantity, unit_price FROM invoice_line
         ORDER BY invoice_id, ordinal",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, i32>(1)?),
            (r.get::<_, String>(2)?, r.get::<_, i64>(3)?),
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// §3.4 — the fold that replaces SQL `SUM` on a money column. `checked_add`,
/// because `SUM` over INTEGER raises on i64 overflow and a silent wrap here
/// would be a wrong total on an ÁFA report.
pub(crate) fn fold_i64(values: impl Iterator<Item = i64>, what: &str) -> Result<i64> {
    let mut acc: i64 = 0;
    for v in values {
        acc = acc.checked_add(v).ok_or_else(|| {
            anyhow::anyhow!(
                "Σ {what} overflowed i64 while folding in Rust. SQL SUM would have raised too; \
                 what must never happen is a silent wrap"
            )
        })?;
    }
    Ok(acc)
}

/// The `Decimal` fold for an R2 column. `SUM` on a `TEXT` column coerces to
/// `REAL` in SQLite, which is the float-money class this migration exists to
/// remove — so the sum is never asked of the engine.
pub(crate) fn fold_decimal<'a>(values: impl Iterator<Item = &'a str>) -> Result<Decimal> {
    let mut acc = Decimal::ZERO;
    for v in values {
        let d = Decimal::from_str(v)
            .map_err(|e| anyhow::anyhow!("cannot fold {v:?} as a Decimal: {e}"))?;
        acc = acc
            .checked_add(d)
            .ok_or_else(|| anyhow::anyhow!("Σ quantity overflowed Decimal while folding"))?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **R1's refusal, both arms.** A whole number crosses; a fractional one
    /// is refused rather than rounded.
    ///
    /// Mutation-verify: replace the `d.fract() != Decimal::ZERO` bail with a
    /// `d.round()` and this goes red — which is the point, because a rounded
    /// HUF total is a wrong NAV filing that no later check can see.
    #[test]
    fn r1_refuses_a_non_integral_money_value_instead_of_rounding_it() {
        assert_eq!(
            money_minor_units("5065", "inv_x", "invoice.huf_equivalent_total").unwrap(),
            5_065
        );
        assert_eq!(
            money_minor_units("5065.0", "inv_x", "invoice.huf_equivalent_total").unwrap(),
            5_065
        );
        assert_eq!(
            money_minor_units("-40523", "inv_x", "invoice.huf_equivalent_total").unwrap(),
            -40_523
        );

        let err = money_minor_units("5065.37", "inv_x", "invoice.huf_equivalent_total")
            .expect_err("a fractional minor-unit value must be refused");
        let msg = err.to_string();
        assert!(msg.contains("inv_x"), "must name the row: {msg}");
        assert!(
            msg.contains("NAV") || msg.contains("rounding"),
            "must say why refusing beats rounding: {msg}"
        );
    }

    /// **R2 carries the string VERBATIM.** `"1.500000"` must not be
    /// re-rendered to `"1.5"` — the migration's trailing-zero behaviour must
    /// not depend on when a row happened to be written, because the
    /// `.normalize()` in the formatters is what makes both forms emit the same
    /// bytes.
    #[test]
    fn r2_carries_the_canonical_string_verbatim() {
        for s in ["1.500000", "1.5", "0.000001", "-2.250000", "310.550000"] {
            assert_eq!(
                canonical_decimal(s, "k", "invoice_line.quantity").unwrap(),
                s,
                "the stored bytes must be the DuckDB-rendered ones"
            );
        }
        let err = canonical_decimal("not-a-number", "k", "invoice_line.quantity")
            .expect_err("an unparseable decimal must be refused, not carried");
        assert!(err.to_string().contains("rust_decimal"), "{err}");
    }

    /// The Rust folds are exact where the SQL ones are not, and loud where SQL
    /// would wrap.
    #[test]
    fn the_rust_folds_are_exact_and_loud() {
        assert_eq!(fold_i64([1_i64, 2, 3].into_iter(), "x").unwrap(), 6);
        assert!(fold_i64([i64::MAX, 1].into_iter(), "x").is_err());

        // 0.1 + 0.2 == 0.3 exactly over Decimal. Over f64 — which is what
        // `SUM` on a TEXT column gives you — it is not.
        let d = fold_decimal(["0.1", "0.2"].into_iter()).unwrap();
        assert_eq!(d, Decimal::from_str("0.3").unwrap());
        assert_eq!(d.to_string(), "0.3");
    }
}
