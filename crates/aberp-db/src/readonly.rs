//! ADR-0108 — the **read-only DuckDB open**, the single mechanism behind C-I.
//!
//! # Why this is new capability and not an existing one
//!
//! ADR-0108 §6.3 / B4 measured it: a sweep for `access_mode` / `read_only` /
//! `READ_ONLY` across `apps/`, `crates/` and `modules/` returns **zero**
//! non-test hits. Every DuckDB open in the tree today is read-write. So
//! "the migrator opens the DuckDB file read-only" was a capability to be
//! *built*, not one to be assumed — and it is the single mechanism behind
//! constraint C-I ("the DuckDB file is byte-unmodified for the whole
//! exercise"), so it gets its own pin rather than an assertion (T-19).
//!
//! # Why it lands in Step 1 rather than Step 4 (deliberate deviation)
//!
//! ADR-0108 §7 assigns this to Step 4 with the migrator. It is here one step
//! earlier because **Step 1's pre-migration manifest is its first consumer**:
//! §7 Step 1 requires the manifest to record the per-table row counts, the head
//! `seq`/`entry_hash`, the non-NULL `event_sig` count and the
//! `audit_ledger_anchors` count *before any engine code exists* — "recording
//! them before any engine code exists is what makes them a baseline rather
//! than a self-report" (B1). Producing that baseline through an ordinary
//! read-write open would have the manifest tool modify the very file whose
//! digest it is recording. The ADR's own F6 ordering rule applies in the other
//! direction here: machinery belongs in the step where its consumer and its
//! test both exist.
//!
//! T-19 splits accordingly: the **write-refusal** arm is pinned here (Step 1);
//! the **digest-unchanged-across-a-full-migrator-run** arm lands with the
//! migrator in Step 4, because that is where a migrator run exists to measure.
//!
//! # Why it is not a census residual (ADR-0098 / ADR-0099)
//!
//! `crates/aberp-db` is the ONE sanctioned shared-instance seam and is excluded
//! from the opener census by construction (`tools/cut_gate_opener_census.sh`).
//! This opener is also not a *runtime* opener in the census's sense: it is
//! read-only, one-shot, and used by CLI tooling that holds the whole-DB writer
//! lock for its run — it can neither fork the audit ledger nor tear a WAL,
//! which are the two hazards the census exists to freeze.

use std::path::Path;

use duckdb::{AccessMode, Config, Connection};

use crate::DbError;

/// Open `db_path` **read-only**. The connection cannot write, and DuckDB
/// enforces that at the storage layer rather than by convention.
///
/// Two properties the callers depend on, both pinned by the tests below:
///
/// 1. **Any write is an error**, including DDL. This is what makes C-I a
///    checked property: a migrator holding only this connection cannot modify
///    the source of truth even if a future edit tries to.
/// 2. **A read-only open does not replay a WAL.** DuckDB cannot replay
///    `<db>.wal` without write access, so an unfolded WAL is *invisible* to
///    this connection. That is precisely why ADR-0108 §7 Step 4 makes
///    "`aberp.duckdb.wal` absent or empty" a migrator **precondition** (B3):
///    the read-only open is what makes the WAL check load-bearing rather than
///    belt-and-braces. Holding the writer lock does not make it redundant —
///    the lock stops a *concurrent* writer, the WAL check stops a *previously
///    crashed* one.
pub fn open_read_only(db_path: &Path) -> Result<Connection, DbError> {
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    Ok(Connection::open_with_flags(db_path, config)?)
}

/// Is there an unfolded DuckDB WAL beside `db_path`?
///
/// Returns the WAL's byte length when one is present and non-empty. ADR-0108
/// §7 Step 4 precondition 2 refuses on `Some(_)`: a read-only open cannot
/// replay it, so those bytes are committed data the reader would silently not
/// see. Silently short is the failure mode CLAUDE.md rule 11 exists to stop —
/// and in the audit-ledger family "short" means "missing the most recent
/// invoices".
///
/// A zero-length `.wal` is treated as absent: DuckDB leaves an empty WAL behind
/// after a clean checkpoint, and refusing on it would refuse the normal state.
pub fn unfolded_wal_len(db_path: &Path) -> std::io::Result<Option<u64>> {
    let wal = wal_path_for(db_path);
    match std::fs::metadata(&wal) {
        Ok(m) if m.len() > 0 => Ok(Some(m.len())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The DuckDB write-ahead log beside `db_path`: `<db>.wal`.
///
/// It is **part of the database's content**, not a temp file — ADR-0108 §2.5.
/// Every snapshot and every restore treats it as a first-class artefact paired
/// atomically with the main file; restoring the main file beside a
/// foreign-generation WAL does not fail the rollback, it *corrupts* it.
pub fn wal_path_for(db_path: &Path) -> std::path::PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(".wal");
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aberp-adr0108-ro-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("tenant.duckdb")
    }

    /// Seed a real DuckDB file with one table and one row.
    fn seed(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id VARCHAR NOT NULL, n BIGINT NOT NULL);
             INSERT INTO t VALUES ('a', 1);",
        )
        .unwrap();
        // Close cleanly so the WAL is folded — the state the migrator requires.
        conn.close().unwrap();
    }

    /// T-19 arm 1 — **the write refusal**. This is the single mechanism behind
    /// C-I; without it "the DuckDB file is byte-unmodified" is a hope.
    ///
    /// Mutation-verify: change `AccessMode::ReadOnly` to `ReadWrite` in
    /// [`open_read_only`] and every assertion below goes red.
    #[test]
    fn a_read_only_connection_refuses_every_write() {
        let db = scratch("refuse-writes");
        seed(&db);

        let conn = open_read_only(&db).expect("read-only open of an existing DB");

        // Reads work — otherwise the refusal below would prove nothing.
        let n: i64 = conn
            .query_row("SELECT n FROM t WHERE id = 'a'", [], |r| r.get(0))
            .expect("a read-only connection still reads");
        assert_eq!(n, 1);

        // DML, DDL and a bulk write are all refused. Enumerated rather than
        // sampled: "no writes" is the claim, so one statement class is not
        // evidence for the others.
        for sql in [
            "INSERT INTO t VALUES ('b', 2)",
            "UPDATE t SET n = 99 WHERE id = 'a'",
            "DELETE FROM t",
            "CREATE TABLE t2 (x BIGINT)",
            "ALTER TABLE t ADD COLUMN extra VARCHAR",
            "DROP TABLE t",
        ] {
            assert!(
                conn.execute_batch(sql).is_err(),
                "a read-only connection must refuse: {sql}"
            );
        }

        // And the row is still what it was.
        let n: i64 = conn
            .query_row("SELECT n FROM t WHERE id = 'a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "no refused write may have partially landed");
    }

    /// The digest half of C-I, at the granularity Step 1 can measure: opening
    /// read-only and reading through it leaves the file byte-identical.
    /// (Step 4 extends this across a full migrator run — T-19 arm 2.)
    #[test]
    fn a_read_only_open_leaves_the_file_byte_identical() {
        let db = scratch("digest");
        seed(&db);
        let before = std::fs::read(&db).unwrap();

        {
            let conn = open_read_only(&db).expect("read-only open");
            let _: i64 = conn
                .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                .unwrap();
        }

        let after = std::fs::read(&db).unwrap();
        assert_eq!(
            before, after,
            "a read-only open + read must not change one byte of the DuckDB file (C-I)"
        );
    }

    /// [`unfolded_wal_len`] is the B3 precondition's probe. Absent and
    /// zero-length both read as "nothing to replay"; a non-empty WAL reports
    /// its length so the refusal message can name it.
    #[test]
    fn unfolded_wal_is_detected_and_an_empty_one_is_not() {
        let db = scratch("wal");
        seed(&db);
        assert_eq!(
            unfolded_wal_len(&db).unwrap(),
            None,
            "a cleanly-closed DB has no unfolded WAL"
        );

        let wal = wal_path_for(&db);
        std::fs::write(&wal, b"").unwrap();
        assert_eq!(
            unfolded_wal_len(&db).unwrap(),
            None,
            "a zero-length WAL is the normal post-checkpoint state, not a refusal"
        );

        std::fs::write(&wal, b"not empty").unwrap();
        assert_eq!(
            unfolded_wal_len(&db).unwrap(),
            Some(9),
            "a non-empty WAL must be reported so the migrator can refuse and name it"
        );
    }

    /// The WAL sidecar name is `<db>.wal` — appended to the WHOLE path, not
    /// substituted for the extension. ADR-0108 §2.5's snapshot/restore pairing
    /// is written against this exact name; getting it wrong would snapshot
    /// nothing and restore the main file alone, which is the one failure in the
    /// plan with nothing behind it.
    #[test]
    fn wal_path_appends_rather_than_replacing_the_extension() {
        assert_eq!(
            wal_path_for(Path::new("/x/apps/aberp-ui/aberp.duckdb")),
            Path::new("/x/apps/aberp-ui/aberp.duckdb.wal")
        );
    }
}
