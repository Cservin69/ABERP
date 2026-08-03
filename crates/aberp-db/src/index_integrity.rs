//! Boot-time rebuild of the non-constraint (`CREATE INDEX`) ART indexes.
//!
//! # The defect this exists for — prod incident 2026-08-03
//!
//! Marking an AP invoice paid killed the live prod instance:
//!
//! ```text
//! FATAL Error: Invalid Input Error: Failed to delete all rows from index.
//!              Only deleted 0 out of 1 rows.
//!   duckdb::RowGroupCollection::RemoveFromIndexes(...)
//!   duckdb::IndexDataRemover::Flush(...)
//!   duckdb::CommitState::CommitDelete(...)
//!   duckdb::UndoBuffer::RevertCommit(...)
//!   INTERNAL Error: Failed to append to PRIMARY_ap_invoice_0: duplicate key
//! ```
//!
//! Measured on a copy of the incident DB — full write-up in
//! `docs/findings/ap-invoice-art-index-desync-2026-08-03.md`, distilled fixture
//! at `apps/aberp/tests/fixtures/ap_invoice_index_desync_20260803.duckdb.zst`:
//!
//!   * The failure is **missing** index entries, not stale row-id mappings —
//!     "deleted 0 out of 1" means the ART simply has no entry for that row.
//!   * It is confined to the two **non-constraint** indexes
//!     (`ap_invoice_tenant_status_idx`, `ap_invoice_tenant_issue_idx`).
//!     Dropping *both* makes the previously fatal `DELETE` succeed; dropping
//!     either one alone does not. The `PRIMARY KEY` / `UNIQUE` ARTs are intact
//!     (a duplicate-key INSERT is still correctly rejected).
//!   * `DROP INDEX` + re-`CREATE INDEX` **cures it**: on the real prod file the
//!     exact statement that invalidated the instance
//!     (`UPDATE ap_invoice SET local_status='Paid' WHERE id=…`) then succeeds,
//!     with zero data loss. 25 indexes rebuilt on the 25 MB prod DB: ~40 ms.
//!   * A rolled-back `UPDATE`/`DELETE` **cannot detect it**. The fault fires in
//!     `CommitState::CommitDelete`, i.e. inside `Commit` — a probe that rolls
//!     back never reaches it and reports a clean bill of health. All three
//!     rolled-back probe shapes returned OK on the poisoned prod file. A
//!     *committed* probe does detect it, but the FATAL invalidates the instance
//!     and surfaces as an uncaught C++ exception in the CLI. **There is no
//!     non-destructive detector for this class in DuckDB 1.5.3.** Do not
//!     re-introduce one; it would be a green light that proves nothing.
//!
//! Hence: no probe. **Repair unconditionally.** A rebuild is deterministic,
//! costs milliseconds at tenant scale, and always yields an ART that agrees
//! with the table — so the poisoned state is simply unreachable past boot.
//!
//! # Scope
//!
//! Only objects `duckdb_indexes()` reports, which is exactly the explicit
//! `CREATE INDEX` set — constraint-backed ARTs (`PRIMARY KEY` / `UNIQUE`) are
//! not listed, cannot be dropped independently of their constraint, and the
//! incident measurement shows they were not the poisoned ones.
//!
//! Statements run in **autocommit, one at a time**. `DROP INDEX` +
//! `CREATE INDEX` inside an explicit `BEGIN`/`COMMIT` crashes DuckDB 1.5.x
//! outright ("Pure virtual function called!"), measured on the same file.
//! Do not wrap this in a transaction.

use duckdb::Connection;

use crate::DbError;

/// One non-constraint ART index, exactly as DuckDB round-trips it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryIndex {
    /// Index name (`duckdb_indexes().index_name`).
    pub name: String,
    /// Table the index is defined on.
    pub table: String,
    /// The `CREATE INDEX …` statement DuckDB reports for this index. Used
    /// verbatim to recreate it, so the rebuild carries no hand-maintained
    /// registry that could drift from the schema (this tree has been bitten
    /// by stale hand-kept censuses repeatedly).
    pub create_sql: String,
}

/// `duckdb_indexes()` lists only explicit `CREATE INDEX` objects; the
/// `NOT is_primary AND NOT is_unique` filter is belt-and-braces so a future
/// DuckDB that starts listing constraint ARTs cannot make us try to drop one.
const LIST_SECONDARY_INDEXES: &str = "SELECT index_name, table_name, sql \
     FROM duckdb_indexes() \
     WHERE NOT is_primary AND NOT is_unique \
     ORDER BY table_name, index_name";

/// Enumerate the non-constraint ART indexes of the connected database.
pub fn list_secondary_indexes(conn: &Connection) -> Result<Vec<SecondaryIndex>, DbError> {
    let mut stmt = conn.prepare(LIST_SECONDARY_INDEXES)?;
    let rows = stmt.query_map([], |row| {
        Ok(SecondaryIndex {
            name: row.get::<_, String>(0)?,
            table: row.get::<_, String>(1)?,
            create_sql: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Rebuild every non-constraint ART index: `DROP INDEX` then re-run the
/// `CREATE INDEX` DuckDB itself reports for it, one autocommit statement at a
/// time, then verify the whole set came back.
///
/// Returns the index names rebuilt, in `(table, index)` order.
///
/// # Failure posture
///
/// Loud. A failed `DROP`/`CREATE`, or a name that does not reappear, is
/// returned as [`DbError::Schema`] — the caller (serve boot) refuses to start.
/// A half-done rebuild leaves at most a *missing* index, which is a query-plan
/// regression and never a correctness one; the next boot's
/// `CREATE INDEX IF NOT EXISTS` schema-ensure re-creates it.
pub fn rebuild_secondary_indexes(conn: &Connection) -> Result<Vec<String>, DbError> {
    let indexes = list_secondary_indexes(conn)?;
    let mut rebuilt = Vec::with_capacity(indexes.len());

    for idx in &indexes {
        if idx.create_sql.trim().is_empty() {
            return Err(DbError::Schema(format!(
                "index {} on {} has no CREATE statement in duckdb_indexes() — \
                 refusing to drop an index that cannot be recreated",
                idx.name, idx.table
            )));
        }
        conn.execute_batch(&format!("DROP INDEX {};", quote_ident(&idx.name)))
            .map_err(|e| {
                DbError::Schema(format!("DROP INDEX {} on {}: {e}", idx.name, idx.table))
            })?;
        conn.execute_batch(&idx.create_sql).map_err(|e| {
            DbError::Schema(format!(
                "recreate index {} on {} via {:?}: {e}",
                idx.name, idx.table, idx.create_sql
            ))
        })?;
        rebuilt.push(idx.name.clone());
    }

    // Post-condition: every index we dropped is back. "The CREATE returned Ok"
    // and "the index is there" are different facts (same posture as
    // `DbError::Schema`'s doc-comment on the ALTER path).
    let after = list_secondary_indexes(conn)?;
    for name in &rebuilt {
        if !after.iter().any(|i| &i.name == name) {
            return Err(DbError::Schema(format!(
                "index {name} did not reappear after rebuild"
            )));
        }
    }

    Ok(rebuilt)
}

/// Double-quote a SQL identifier, doubling any embedded quote. Index names in
/// this tree are plain `snake_case`, but the rebuild interpolates a value read
/// back out of the catalog, so it gets quoted rather than trusted.
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().expect("in-memory duckdb")
    }

    #[test]
    fn quote_ident_doubles_embedded_quotes() {
        assert_eq!(quote_ident("plain_idx"), "\"plain_idx\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn lists_and_rebuilds_only_non_constraint_indexes() {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE t(id VARCHAR NOT NULL PRIMARY KEY, tenant VARCHAR NOT NULL, \
                 st VARCHAR NOT NULL, nav VARCHAR NOT NULL, UNIQUE(tenant, nav));
             CREATE INDEX t_tenant_st_idx ON t(tenant, st);
             INSERT INTO t VALUES ('a','p','Outstanding','n1'),('b','p','Paid','n2');",
        )
        .expect("seed");

        let listed = list_secondary_indexes(&conn).expect("list");
        assert_eq!(
            listed.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["t_tenant_st_idx"],
            "the PRIMARY KEY and UNIQUE ARTs must NOT be listed — dropping one \
             would drop the constraint with it"
        );

        let rebuilt = rebuild_secondary_indexes(&conn).expect("rebuild");
        assert_eq!(rebuilt, vec!["t_tenant_st_idx".to_string()]);

        // The rebuilt index still serves the table, and the constraint ARTs
        // survived the rebuild untouched.
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM t WHERE tenant='p' AND st='Paid'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 1);
        assert!(
            conn.execute_batch("INSERT INTO t VALUES ('a','p','Paid','n3');")
                .is_err(),
            "the PRIMARY KEY must still reject a duplicate after the rebuild"
        );
    }

    #[test]
    fn rebuild_is_a_no_op_on_a_database_with_no_secondary_indexes() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE t(id VARCHAR NOT NULL PRIMARY KEY);")
            .expect("seed");
        assert!(rebuild_secondary_indexes(&conn)
            .expect("rebuild")
            .is_empty());
    }

    #[test]
    fn rebuild_is_idempotent() {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE t(id VARCHAR NOT NULL PRIMARY KEY, a VARCHAR, b VARCHAR);
             CREATE INDEX t_a_idx ON t(a);
             CREATE INDEX t_b_idx ON t(b);
             INSERT INTO t VALUES ('x','1','2');",
        )
        .expect("seed");
        let first = rebuild_secondary_indexes(&conn).expect("rebuild 1");
        let second = rebuild_secondary_indexes(&conn).expect("rebuild 2");
        assert_eq!(first, second);
        assert_eq!(first, vec!["t_a_idx".to_string(), "t_b_idx".to_string()]);
    }
}
