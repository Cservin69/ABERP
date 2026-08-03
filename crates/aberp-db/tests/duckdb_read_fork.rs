//! **The read-fork class, measured on DuckDB.**
//!
//! # Why this file exists
//!
//! Five of July 2026's production incidents (#40 invoice render, #41
//! modification base, #42 auto-email, E1, the S444 invoice-number re-issue)
//! share one primitive: **a second connection opened on a database the writer
//! already holds.** ADR-0099's `Handle` closed it for the migrated families by
//! making `read()` a `try_clone` of the ONE instance — not a second open.
//!
//! These two arms reproduce the primitive under the exact pragmas
//! `aberp_db::Handle` applies, and localise the damage:
//!
//! | Question | Test |
//! |---|---|
//! | Is the forked *read* itself stale? | [`duckdb_the_forked_read_itself_is_coherent`] |
//! | Does the fork's *close* destroy WAL-resident commits? | [`duckdb_a_foreign_close_silently_destroys_every_later_commit`] |
//!
//! The answer is the non-obvious half of the class, and the reason the naive
//! "stale read" model of these incidents is wrong: the forked read is
//! **coherent**. It is the fork's **CLOSE** that truncates the Handle's WAL, so
//! that every later `COMMIT` on the original connection returns `Ok` and writes
//! nowhere. See `docs/findings/read-fork-audit-sqlite-20260731.md`.
//!
//! # Mutation verification
//!
//! A pin that cannot go red is not a pin. Give the fork the Handle's
//! `disable_checkpoint_on_shutdown` pragma and the loss goes to zero —
//! `duckdb_a_foreign_close_silently_destroys_every_later_commit` goes RED.
//! That is what identifies the close, and not the read, as the injury.
//!
//! # Scope
//!
//! Temp files only. Nothing here touches `~/.aberp/**` or any tenant database.

#![allow(clippy::items_after_test_module)]

use std::path::PathBuf;

/// A scratch database path under the OS temp dir, unique per process + call.
fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-readfork-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ===========================================================================
// The DuckDB arm — the incident primitive, reproduced.
// ===========================================================================

/// Commit `n` rows through `conn`, one transaction each.
fn commit_rows(conn: &duckdb::Connection, from: i64, n: i64) {
    for i in from..from + n {
        conn.execute_batch(&format!(
            "BEGIN; INSERT INTO invoices VALUES ('inv-{i}', {i}); COMMIT;"
        ))
        .unwrap();
    }
}

fn count(conn: &duckdb::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap()
}

/// A connection with the exact pragmas `aberp_db::open_runtime_connection`
/// applies — i.e. a stand-in for the process-wide `Handle`.
fn open_like_the_handle(db: &std::path::Path) -> duckdb::Connection {
    let conn = duckdb::Connection::open(db).unwrap();
    conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown;")
        .unwrap();
    conn.execute_batch("PRAGMA wal_autocheckpoint='1TB';")
        .unwrap();
    conn
}

/// **ADR-0107 §1.3 F1, answered — and the answer is not the one the frozen
/// baseline assumes.**
///
/// `tools/adr0099_read_fork_structural_baseline.txt` GROUP A states the
/// mechanism as: "anything the Handle has written is WAL-resident and these
/// read the last-checkpointed SUBSET". **Measured, that is false.** A
/// co-resident second DuckDB instance *replays the WAL on open* and sees every
/// committed row.
///
/// This matters because it means the forked *read* is not where the damage is —
/// see [`duckdb_a_foreign_close_silently_destroys_every_later_commit`], which
/// is. Recorded as its own test so the two halves cannot be conflated again.
#[test]
fn duckdb_the_forked_read_itself_is_coherent() {
    let dir = scratch("duck-forkread");
    let db = dir.join("aberp.duckdb");

    let writer = open_like_the_handle(&db);
    writer
        .execute_batch("CREATE TABLE invoices (id VARCHAR NOT NULL, n BIGINT NOT NULL);")
        .unwrap();
    commit_rows(&writer, 0, 5);
    assert_eq!(count(&writer), 5);

    let forked = duckdb::Connection::open(&db).unwrap();
    assert_eq!(
        count(&forked),
        5,
        "a co-resident second instance REPLAYS the WAL — the read is coherent. \
         The 'reads the last-checkpointed subset' rationale in the frozen \
         baseline describes a symptom of a PRIOR fork's close, not this read."
    );
}

/// **The read-fork primitive, measured: a foreign connection's CLOSE silently
/// destroys the durability of every commit the writer makes afterwards.**
///
/// This is the mechanism behind the July incident cluster, and it is strictly
/// worse than the stale read it was assumed to be.
///
/// 1. The `Handle` holds `disable_checkpoint_on_shutdown` +
///    `wal_autocheckpoint='1TB'`, so its commits stay WAL-resident by design.
/// 2. A second connection — any of the 13 live in-serve entries in the frozen
///    baseline — opens, reads, and **closes**. It carries neither pragma, so
///    its close checkpoints: the WAL is folded into the main file and
///    **truncated to zero**.
/// 3. From that moment the writer's WAL is gone. Every subsequent `COMMIT`
///    returns `Ok`, is visible to the writer's own connection, and is written
///    **nowhere durable**. The WAL stays at 0 bytes.
/// 4. Any other reader now sees only the pre-fold state — *this* is the stale
///    read (#40's "no InvoiceDraftCreated audit entry found").
/// 5. On process exit every post-fold commit is gone.
///
/// The control arm is inside this test on purpose: with no fork at all and the
/// same two pragmas, all 15 rows survive. So the loss cannot be attributed to
/// the pragmas — only to the foreign close.
///
/// which shows this half of the class does not exist under WAL.
#[test]
fn duckdb_a_foreign_close_silently_destroys_every_later_commit() {
    let dir = scratch("duck-forkclose");
    let db = dir.join("aberp.duckdb");
    let wal = dir.join("aberp.duckdb.wal");
    let wal_len = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);

    // ---- The control: same pragmas, no fork. Nothing is lost. ----
    {
        let ctl_dir = scratch("duck-control");
        let ctl = ctl_dir.join("aberp.duckdb");
        let w = open_like_the_handle(&ctl);
        w.execute_batch("CREATE TABLE invoices (id VARCHAR NOT NULL, n BIGINT NOT NULL);")
            .unwrap();
        commit_rows(&w, 0, 15);
        drop(w);
        let reopened = duckdb::Connection::open(&ctl).unwrap();
        assert_eq!(
            count(&reopened),
            15,
            "CONTROL: with no foreign connection, the Handle's pragmas lose \
             nothing. If this ever fails, the fault is the pragmas and the \
             main arm below is measuring the wrong thing."
        );
    }

    // ---- The measurement. ----
    let writer = open_like_the_handle(&db);
    writer
        .execute_batch("CREATE TABLE invoices (id VARCHAR NOT NULL, n BIGINT NOT NULL);")
        .unwrap();
    commit_rows(&writer, 0, 5);
    assert!(wal_len() > 0, "the 5 commits are WAL-resident by design");

    // The fork: open, read, close. Exactly what a GROUP-A route does per request.
    {
        let forked = duckdb::Connection::open(&db).unwrap();
        let _ = count(&forked);
    }
    assert_eq!(
        wal_len(),
        0,
        "the foreign close folded and TRUNCATED the live writer's WAL"
    );

    // The writer keeps working, and keeps reporting success.
    commit_rows(&writer, 5, 10);
    assert_eq!(
        count(&writer),
        15,
        "the writer's own instance reports all 15 — every COMMIT returned Ok"
    );
    assert_eq!(
        wal_len(),
        0,
        "...but nothing reached the WAL. The writer's durability is gone and \
         nothing anywhere has said so (CLAUDE.md rule 11's worst class)."
    );

    drop(writer);
    let reopened = duckdb::Connection::open(&db).unwrap();
    assert_eq!(
        count(&reopened),
        5,
        "TEN COMMITTED ROWS ARE PERMANENTLY LOST. This is the read-fork class's \
         real damage: not a short read, a silent write-off of everything after \
         the first foreign close. It is why the mirror can run AHEAD of the DB \
         (2026-07-19), why an invoice-number floor kept in a business table \
         rewound (S444), and why an audit row that was written could not be \
         found (#40)."
    );
}

// ===========================================================================
// Q11 — what `busy_timeout` is, and is not, for.
// ===========================================================================
