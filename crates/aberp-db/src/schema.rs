//! ADR-0108 Step 3 — `ensure_columns`, the **one** way a column is added, and
//! the column-type vocabulary the migration's representation rules are written
//! in.
//!
//! # Why this exists: 114 sites, and D2a's shape hiding in every one
//!
//! ADR-0108 §1.2 counts **114 executable `ALTER TABLE … ADD COLUMN IF NOT
//! EXISTS`** sites in src (105 in 12 `.rs` files, 8 in 3 `.sql` files, 1 already
//! const-driven). SQLite has no `IF NOT EXISTS` on `ADD COLUMN`, so every one
//! of them becomes read-the-columns-then-decide — and PR #49 F-1c identifies
//! that rewrite as reproducing **D2a's exact fail-open shape**: a column
//! silently not added → a later read `.unwrap_or_default()`s → a guard passes
//! vacuously → an exempt ÁFA base re-files to NAV at 0%.
//!
//! The declarative `IF NOT EXISTS` form could never express "silently skip".
//! The imperative rewrite can, and will, unless the post-condition below is
//! there. **[`ensure_columns`] step 4 is the whole reason this helper exists.**
//!
//! # The contract, in order
//!
//! 1. Read the existing column set once.
//! 2. **Table absent → `Err`.** Not `Ok(())`. A missing table at
//!    `ensure_schema` time is a broken boot.
//! 3. For each missing `(name, type)`: `ALTER TABLE t ADD COLUMN n T`.
//! 4. **Re-read, and assert every requested column is now present. Any absent
//!    → `Err`.** This is M8, and it is the step a hand-written rewrite omits.
//! 5. Every error names the table, the column, and the declared type (rule 11).
//!
//! # The identifier rule, enforced by the type system rather than a grep
//!
//! `cols` is `&'static [(&'static str, &'static str)]`. A runtime `String` will
//! not compile. That is stronger than any gate: there is no code path that
//! reaches step 3's `format!` with data.

use crate::engine::Connection;
use crate::DbError;

// ---------------------------------------------------------------------------
// The column-type vocabulary (ADR-0108 §3 R1 / R2 / R3)
// ---------------------------------------------------------------------------

/// **R1 — money is `INTEGER` minor units.** An `i64` count of the currency's
/// minor unit (HUF: whole forints, per `Huf(i64)`; EUR: cents, per `Eur(i64)`).
///
/// Never `REAL`, never `TEXT`, and never `NUMERIC`/`DECIMAL` — which `STRICT`
/// forbids outright, which is the point: it forces the decision at DDL time
/// instead of allowing deferral.
pub const MONEY_MINOR: &str = "INTEGER";

/// **R2 — exact non-integer values are `TEXT` holding the canonical
/// `rust_decimal::Decimal` string.** Quantities, exchange rates, tolerances.
///
/// This is already what the code does today (`duckdb_store.rs`'s
/// "Decimal-as-string bind", and the `CAST(quantity AS VARCHAR)` →
/// `Decimal::from_str` read), which is why R2 is the smallest-diff option and
/// not merely the safest.
///
/// **The rule that makes R2 hold at runtime: no arithmetic and no comparison
/// on an R2 column in SQL.** `TEXT * INTEGER` coerces to `REAL`, and `TEXT <
/// TEXT` is *lexicographic* — `'9' < '10'` is FALSE. Fold in Rust over
/// `Decimal`. See [`crate::schema`]'s sibling work in Step 7.
pub const EXACT_DECIMAL: &str = "TEXT";

/// **R3 — hashes and canonical payloads are `BLOB`, bound as `&[u8]`.**
///
/// In SQLite `BLOB` and `TEXT` are distinct storage classes that **never
/// compare equal**. A single `&str` bind where `&[u8]` belongs makes a
/// chain-link lookup return "not found" — the symptom that already cost PR #40
/// once, on DuckDB, for a different reason.
pub const HASH_BLOB: &str = "BLOB";

/// Counts, sequence numbers, basis points, timestamps-as-millis, booleans.
///
/// `vat_rate_basis_points INTEGER` is why VAT never touches a float **in
/// storage**: 27% is `2700`, not `0.27`.
pub const COUNT_INT: &str = "INTEGER";

/// Identifiers, enum strings, ISO-8601 dates. ULIDs minted in Rust (ADR-0019 —
/// no engine-minted identity).
pub const TEXT: &str = "TEXT";

/// **Non-money** floats only — measurements, ratios, percentages that are not
/// money and not quantities (ADR-0108 §3.2 category E).
///
/// Using this for anything on a money or quantity path is the regression PR #49
/// F-6a names. It is a separate constant from the others precisely so that
/// choosing it is a visible act.
pub const NON_MONEY_FLOAT: &str = "REAL";

/// **M1 — the table-definition suffix.** Every table created by the SQLite
/// build carries it.
///
/// `STRICT` is not an invariant moving into the DDL (which ADR-0019 and
/// `[[no-sql-specific]]` forbid). It is a *type* declaration that makes a
/// storage-class violation loud — the same job the DuckDB `DECIMAL` declaration
/// was doing. Without it SQLite's dynamic typing accepts a float into an
/// INTEGER money column silently, which is F-6a.
pub const STRICT: &str = " STRICT";

/// Every declared type this tree is allowed to use. `STRICT` accepts exactly
/// `INT`, `INTEGER`, `REAL`, `TEXT`, `BLOB` and `ANY`; ABERP narrows that to
/// four and excludes `ANY`, because `ANY` reinstates dynamic typing on the one
/// column somebody reaches for it.
const ALLOWED_TYPES: &[&str] = &["INTEGER", "TEXT", "BLOB", "REAL"];

// ---------------------------------------------------------------------------
// ensure_columns
// ---------------------------------------------------------------------------

/// Add every column in `cols` to `table` that is not already there, and
/// **prove** afterwards that they are.
///
/// Identifiers come from `&'static str` arguments — never from a value, never
/// from a `format!` of runtime data. See the module docs for why the signature
/// is shaped this way rather than taking `&[(String, String)]`.
///
/// Portability: `pragma_table_info` is a table-valued function on **both**
/// engines, so one implementation serves the DuckDB build and the SQLite one.
pub fn ensure_columns(
    conn: &Connection,
    table: &'static str,
    cols: &'static [(&'static str, &'static str)],
) -> Result<(), DbError> {
    for (name, ty) in cols {
        if !ALLOWED_TYPES.contains(ty) {
            return Err(DbError::Schema(format!(
                "{table}.{name}: declared type {ty:?} is not one of {ALLOWED_TYPES:?} — \
                 ADR-0108 M1 restricts STRICT tables to these four (no DECIMAL, no NUMERIC, \
                 no ANY)"
            )));
        }
    }

    // --- 1/2. read the existing set; a missing TABLE is an Err, not Ok(()) ---
    if !table_exists(conn, table)? {
        return Err(DbError::Schema(format!(
            "ensure_columns({table}): the table does not exist. A missing table at \
             ensure_schema time is a broken boot, not something to skip — the declarative \
             `ADD COLUMN IF NOT EXISTS` form this replaces could never express 'silently \
             skip' either"
        )));
    }
    let existing = column_names(conn, table)?;

    // --- 3. add what is missing ---
    for (name, ty) in cols {
        if existing.iter().any(|c| c == name) {
            continue;
        }
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty};"))
            .map_err(|e| {
                DbError::Schema(format!(
                    "ensure_columns({table}): ADD COLUMN {name} {ty} failed: {e}"
                ))
            })?;
    }

    // --- 4. M8's post-condition. The reason this helper exists. ---
    let after = column_names(conn, table)?;
    let missing: Vec<&str> = cols
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !after.iter().any(|c| c == n))
        .collect();
    if !missing.is_empty() {
        return Err(DbError::Schema(format!(
            "ensure_columns({table}): POST-CONDITION FAILED — {missing:?} still absent after \
             the ALTERs reported success. Continuing would leave a later read to \
             `unwrap_or_default()` a column that is not there, which is how an exempt ÁFA base \
             re-files to NAV at 0% (PR #49 F-1c / D2a)"
        )));
    }
    Ok(())
}

/// The column names of `table`, or an empty vector when it does not exist.
///
/// **`pragma_table_info` with the table name BOUND**, never interpolated: it is
/// a table-valued function whose argument is an ordinary value position, so the
/// identifier crosses as a parameter. That also means it works identically on
/// DuckDB and SQLite, which is what lets one helper serve both engines.
///
/// It replaces the four `information_schema.columns` / `information_schema.
/// tables` query sites (ADR-0108 §1.1 G-3), which SQLite does not have at all.
/// One of those — `duckdb_store.rs:427`'s S157 guard — `.ok()`s its error into
/// `None` → `false`, so on SQLite it would silently report "not integer" and
/// the quantity-widen ladder would never run. This function returns a `Result`
/// so that call site can be made loud when it crosses in Step 5.
pub fn column_names(conn: &Connection, table: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?)")?;
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Does `table` exist?
///
/// **The one place the two engines genuinely disagree, normalised here rather
/// than at 4 call sites.** Given an unknown table name,
/// `pragma_table_info(?)` returns **zero rows** on SQLite and raises a
/// **catalog error** on DuckDB (`Catalog Error: Table with name … does not
/// exist!`). Both mean "absent"; a caller that handled only one shape would
/// work on one engine and fail on the other — the exact half-migrated hazard
/// CLAUDE.md rule 14 is about.
///
/// **Fail-closed:** only an error recognised as "no such relation" becomes
/// `Ok(false)`. Anything else propagates, so a genuine engine failure is never
/// silently reported as "the table isn't there" — which is how a boot guard
/// ends up skipping a schema it should have refused over.
///
/// It replaces the two `information_schema.tables` sites (`print_invoice.rs:926,
/// 986`), which SQLite does not have (ADR-0108 §1.1 G-3).
pub fn table_exists(conn: &Connection, table: &str) -> Result<bool, DbError> {
    match column_names(conn, table) {
        Ok(cols) => Ok(!cols.is_empty()),
        Err(e) if is_missing_relation(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Is this error the engine's "that relation does not exist"?
///
/// Message matching is not lovely, but there is no portable typed signal:
/// DuckDB reports a generic `DuckDBFailure` with the detail in its message, and
/// SQLite does not error at all. It is written as an allowlist of the two
/// engines' exact phrasings so an unrecognised error propagates (fail-closed)
/// rather than being read as absence.
fn is_missing_relation(e: &DbError) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("does not exist") || msg.contains("no such table")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    /// **T-9, arm 1.** The ordinary path: missing columns are added, present
    /// ones are left alone, and a second run is a no-op (these helpers run on
    /// every boot).
    #[test]
    fn t9_ensure_columns_adds_what_is_missing_and_is_idempotent() {
        let c = conn();
        c.execute_batch("CREATE TABLE invoice (id TEXT NOT NULL);")
            .unwrap();

        const COLS: &[(&str, &str)] = &[
            ("huf_equivalent_total", MONEY_MINOR),
            ("exchange_rate", EXACT_DECIMAL),
            ("entry_hash", HASH_BLOB),
        ];
        ensure_columns(&c, "invoice", COLS).unwrap();
        let names = column_names(&c, "invoice").unwrap();
        for (n, _) in COLS {
            assert!(names.iter().any(|x| x == n), "{n} was not added: {names:?}");
        }

        // Idempotent — and the second run must not re-ALTER an existing column.
        ensure_columns(&c, "invoice", COLS).unwrap();
        assert_eq!(column_names(&c, "invoice").unwrap().len(), names.len());
    }

    /// **T-9, arm 2 — the arm that matters.** A missing TABLE is an `Err`, not
    /// `Ok(())`.
    ///
    /// Mutation-verify: change the `existing.is_empty()` branch to
    /// `return Ok(())` and this goes red. That mutation is exactly the
    /// fail-open a hand-written `information_schema` → `pragma_table_info`
    /// rewrite produces by accident.
    #[test]
    fn t9_a_missing_table_is_an_error_not_a_silent_skip() {
        let c = conn();
        const COLS: &[(&str, &str)] = &[("x", TEXT)];
        let err = ensure_columns(&c, "no_such_table", COLS)
            .expect_err("a missing table at ensure_schema time is a broken boot");
        let msg = err.to_string();
        assert!(msg.contains("no_such_table"), "must name the table: {msg}");
        assert!(
            msg.contains("does not exist"),
            "must say WHY, not just that something failed: {msg}"
        );
    }

    /// **T-9, arm 3 — M8's post-condition fires.**
    ///
    /// Driven by making an `ALTER` impossible: a column name that already
    /// exists with different case cannot be added twice, and a reserved-shape
    /// name fails to parse. Either way the helper must report the column as
    /// still absent rather than returning `Ok` on an incomplete schema.
    #[test]
    fn t9_a_column_that_cannot_be_added_is_an_error() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id TEXT NOT NULL);")
            .unwrap();
        // `select` is a keyword; the unquoted ALTER cannot parse it. The helper
        // must surface that rather than swallow it.
        const COLS: &[(&str, &str)] = &[("select", TEXT)];
        let err = ensure_columns(&c, "t", COLS).expect_err("an unaddable column must be an error");
        let msg = err.to_string();
        assert!(msg.contains("t"), "{msg}");
        assert!(
            msg.contains("ADD COLUMN") || msg.contains("POST-CONDITION"),
            "the error must name the failing operation: {msg}"
        );
        // And nothing partially landed under a name the caller would then read.
        assert!(!column_names(&c, "t").unwrap().iter().any(|n| n == "select"));
    }

    /// **M1 at the helper boundary.** A `DECIMAL`/`NUMERIC`/`ANY` declared type
    /// is refused before any DDL runs — `STRICT` would reject `DECIMAL` at
    /// `CREATE TABLE` anyway, but only for tables this build creates. Catching
    /// it here means a bad type in a `const` list is a loud error on the DuckDB
    /// build too, one step before it could reach SQLite.
    #[test]
    fn a_non_strict_declared_type_is_refused_before_any_ddl_runs() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id TEXT NOT NULL);")
            .unwrap();
        const BAD: &[&[(&str, &str)]] = &[
            &[("c", "DECIMAL(18,6)")],
            &[("c", "NUMERIC")],
            &[("c", "ANY")],
            &[("c", "VARCHAR")],
            &[("c", "DOUBLE")],
            &[("c", "BIGINT")],
        ];
        for cols in BAD {
            let err = ensure_columns(&c, "t", cols)
                .expect_err("a non-STRICT declared type must be refused");
            assert!(
                err.to_string().contains("is not one of"),
                "the refusal must name the allowed set: {err}"
            );
        }
        // And the refusal happened BEFORE any DDL: the column is not there.
        assert!(!column_names(&c, "t").unwrap().iter().any(|n| n == "c"));
    }

    /// The vocabulary itself: every constant the migration is written in is one
    /// of the four `STRICT` types. A future edit that "helpfully" changes
    /// [`MONEY_MINOR`] to `"REAL"` is caught here rather than at the first
    /// invoice.
    #[test]
    fn every_column_type_constant_is_a_legal_strict_type() {
        for t in [
            MONEY_MINOR,
            EXACT_DECIMAL,
            HASH_BLOB,
            COUNT_INT,
            TEXT,
            NON_MONEY_FLOAT,
        ] {
            assert!(ALLOWED_TYPES.contains(&t), "{t} is not a legal STRICT type");
        }
        assert_eq!(MONEY_MINOR, "INTEGER", "R1: money is INTEGER minor units");
        assert_eq!(
            EXACT_DECIMAL, "TEXT",
            "R2: exact non-integers are TEXT decimal"
        );
        assert_eq!(HASH_BLOB, "BLOB", "R3: hashes are BLOB");
    }

    /// [`column_names`] binds the table identifier rather than interpolating
    /// it, so a hostile name is a miss, never a statement.
    #[test]
    fn column_names_binds_the_table_name() {
        let c = conn();
        c.execute_batch("CREATE TABLE t (id TEXT NOT NULL);")
            .unwrap();
        assert_eq!(column_names(&c, "t").unwrap(), vec!["id".to_string()]);
        // The injected text is consumed as a table NAME, not as SQL: the engine
        // looks for a relation literally called `t; DROP TABLE t` and does not
        // find one. (DuckDB says so with a catalog error, SQLite with zero rows
        // — `table_exists` normalises both to `false`.)
        assert!(!table_exists(&c, "t; DROP TABLE t").unwrap());
        // And the real table is still there.
        assert!(table_exists(&c, "t").unwrap());
        assert_eq!(column_names(&c, "t").unwrap(), vec!["id".to_string()]);
    }
}
