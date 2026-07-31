//! ADR-0108 Step 3 — **the type seam**. The only place in the tree that names
//! either engine crate.
//!
//! # Why a re-export module and not a trait (Decision D2, §2.3)
//!
//! `duckdb-rs` is a fork of `rusqlite`. The surfaces this tree uses are
//! name-identical: `Connection`, `Transaction`, `Statement`, `Row`, `Result`,
//! `params!`, `prepare`, `query_row`, `query_map`, `execute`, `execute_batch`,
//! `transaction`, `try_clone`, `ToSql`, `types::Type::Text`,
//! `Error::QueryReturnedNoRows`, `Error::FromSqlConversionFailure`.
//!
//! So the migration of 449 `params!` sites and 120 `duckdb::Connection`
//! signatures is a mechanical `use duckdb::X` → `use aberp_db::engine::X`
//! rewrite, and a trait abstraction over a surface that is *already identical*
//! is a wrapper around nothing — CLAUDE.md rule 12's "should this exist at
//! all?" says no.
//!
//! **This module does not perform that rewrite.** Step 3 lands the seam and its
//! divergence handling; the families cross from Step 5, one fused family at a
//! time (rule 14).
//!
//! # The measured divergences — the complete list
//!
//! | Divergence | Sites | Handling |
//! |---|---:|---|
//! | `duckdb::Error::DuckDBFailure` → `rusqlite::Error::SqliteFailure` | 3 | [`is_engine_failure`] |
//! | `DbError::Duck(#[from] duckdb::Error)` | 1 | stays; `DbError::Sqlite` is its twin |
//! | `Connection::open_in_memory()` | 2 + tests | same name in both |
//! | `Appender` | 0 | not used |
//! | `savepoint` | 0 | not used |
//! | `MappedRows` | 1 | same name in both |
//!
//! And one divergence the ADR's table does not list, found building this:
//! **`duckdb-rs` has no `TransactionBehavior` and no `transaction_with_behavior`
//! at all** (`duckdb/src/transaction.rs` exposes only `transaction()`). So M5's
//! `BEGIN IMMEDIATE` cannot be a re-export — it is [`begin_immediate`], whose
//! two arms are genuinely different code. See its docs.

use crate::DbError;

#[cfg(not(feature = "sqlite-engine"))]
pub use duckdb::{
    params, types, Connection, Error, MappedRows, Params, Result, Row, Statement, ToSql,
    Transaction,
};

#[cfg(feature = "sqlite-engine")]
pub use rusqlite::{
    params, types, Connection, Error, MappedRows, Params, Result, Row, Statement, ToSql,
    Transaction,
};

/// Is this error the engine's own "the C library failed" variant?
///
/// The one error variant with no same-named twin: `duckdb::Error::DuckDBFailure`
/// vs `rusqlite::Error::SqliteFailure`. Three call sites in the tree match on
/// it; they call this instead of naming either variant, so the match compiles
/// under both engines.
pub fn is_engine_failure(e: &Error) -> bool {
    #[cfg(not(feature = "sqlite-engine"))]
    {
        matches!(e, Error::DuckDBFailure(..))
    }
    #[cfg(feature = "sqlite-engine")]
    {
        matches!(e, Error::SqliteFailure(..))
    }
}

/// **M5 — start a transaction that takes its write lock at `BEGIN`, not at the
/// first write.**
///
/// Every read-modify-write in this tree must go through here: the audit-chain
/// append, the invoice-number allocator, every upsert, the stock-cache rebuild.
/// The shape they all share is *read a value, decide from it, write back*, and
/// that shape is only safe if no other writer can slip between the read and the
/// write.
///
/// # Why the two arms are different code, not a re-export
///
/// * **SQLite.** A plain `BEGIN` is DEFERRED: the transaction takes no lock
///   until its first write. Two transactions can therefore both read the chain
///   head, and the second one to write gets `SQLITE_BUSY` **at commit** — by
///   which time the application has already decided what to write from a stale
///   read. `BEGIN IMMEDIATE` takes the write lock up front, so the second
///   transaction blocks (or fails) *before* it reads. This is the difference
///   between "two links off one `prev_hash`" and "one of them waits".
/// * **DuckDB.** There is no `IMMEDIATE`. DuckDB is MVCC with optimistic
///   concurrency and no `transaction_with_behavior` API at all, and the
///   in-process single-writer `Mutex` in [`crate::Handle`] is what serialises
///   writers today. So the DuckDB arm is `conn.transaction()` — unchanged
///   behaviour, same call site.
///
/// The point of routing both through one function is that a call site written
/// today cannot silently become a DEFERRED transaction when the feature flips.
pub fn begin_immediate(conn: &mut Connection) -> Result<Transaction<'_>, DbError> {
    #[cfg(not(feature = "sqlite-engine"))]
    {
        Ok(conn.transaction()?)
    }
    #[cfg(feature = "sqlite-engine")]
    {
        Ok(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam compiles and round-trips a value under whichever engine is
    /// linked. Thin by design — its job is to fail the build if a re-export
    /// name diverges, which is the only way this module can be wrong.
    #[test]
    fn the_seam_round_trips_a_value_under_the_linked_engine() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a VARCHAR NOT NULL, n BIGINT NOT NULL);")
            .unwrap();
        conn.execute("INSERT INTO t VALUES (?, ?)", params!["x", 7_i64])
            .unwrap();
        let (a, n): (String, i64) = conn
            .query_row("SELECT a, n FROM t", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((a.as_str(), n), ("x", 7));
    }

    /// **M5 — [`begin_immediate`] commits and rolls back like an ordinary
    /// transaction.** The *contention* behaviour it exists for needs two
    /// connections and is pinned by `t6_two_writers_cannot_fork_the_chain` in
    /// `crates/aberp-db/tests/adr0108_seam.rs`; this asserts the boring half so
    /// a broken arm shows up as a unit failure rather than a race.
    #[test]
    fn begin_immediate_commits_and_rolls_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (n BIGINT NOT NULL);")
            .unwrap();

        let tx = begin_immediate(&mut conn).unwrap();
        tx.execute("INSERT INTO t VALUES (1)", []).unwrap();
        tx.commit().unwrap();

        let tx = begin_immediate(&mut conn).unwrap();
        tx.execute("INSERT INTO t VALUES (2)", []).unwrap();
        drop(tx); // rollback

        let n: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the committed row survives; the dropped one does not");
    }

    /// [`is_engine_failure`] must be `false` for an ordinary logical error, or
    /// the three call sites that use it would treat every miss as a C-library
    /// failure.
    #[test]
    fn is_engine_failure_is_false_for_an_ordinary_error() {
        let conn = Connection::open_in_memory().unwrap();
        let err = conn
            .query_row("SELECT 1 WHERE 1 = 0", [], |r| r.get::<_, i64>(0))
            .expect_err("no rows");
        assert!(matches!(err, Error::QueryReturnedNoRows));
        assert!(!is_engine_failure(&err));
    }
}
