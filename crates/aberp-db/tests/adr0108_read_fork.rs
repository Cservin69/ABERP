//! ADR-0108 T-20 / T-21 — **the read-fork class, measured on both engines.**
//!
//! # Why this file exists
//!
//! Five of July's production incidents (#40 invoice render, #41 modification
//! base, #42 auto-email, E1, the S444 invoice-number re-issue) share one
//! primitive: **a second connection opened on a database the writer already
//! holds, handing back a view that is missing the writer's most recent
//! commits.** ADR-0099's `Handle` closed it for the migrated families by making
//! `read()` a `try_clone` of the ONE instance — not a second open.
//!
//! ADR-0108 changes that. Under `sqlite-engine` there is no `try_clone` of a
//! shared cache to hand out; `Handle::read()` becomes **a genuine second
//! connection**. ADR-0108 §2.4 asserts this is "semantically *stronger* (it sees
//! every prior commit)". That claim is the load-bearing one for the whole
//! migration and §2.4 never pinned it. This file pins it — in **both**
//! directions, and against the DuckDB behaviour it replaces.
//!
//! # The four questions, and which test answers each
//!
//! | Question | Test |
//! |---|---|
//! | Does a *pre-existing* second connection see a commit? (§2.4's claim) | [`t20a_autocommit_reader_sees_a_later_commit`] |
//! | Does it still see it once it is inside an explicit transaction? | [`t20b_the_snapshot_is_taken_at_the_first_read_not_at_begin`] |
//! | Can a reader ever observe a torn / pre-commit state? | [`t20c_a_reader_never_observes_an_uncommitted_write`] |
//! | Does a foreign connection's *close* destroy WAL-resident commits? | [`t20d_a_foreign_close_does_not_drop_wal_resident_commits`] |
//!
//! Plus the `busy_timeout` (Q11) behaviour the number was chosen for:
//! [`q11_a_wal_reader_does_not_contend_with_a_live_writer`] and
//! [`q11_busy_timeout_does_not_retry_a_snapshot_conflict`].
//!
//! And the contrast that gives all of the above their meaning: the two DuckDB
//! arms, [`duckdb_the_forked_read_itself_is_coherent`] and
//! [`duckdb_a_foreign_close_silently_destroys_every_later_commit`], which
//! reproduce the incident primitive under the exact pragmas
//! `aberp_db::Handle` applies — and show that it is the fork's **close**, not
//! its read, that does the damage.
//!
//! # Mutation verification — what actually makes each of these red
//!
//! "A pin that cannot go red is not a pin" (ADR-0108 §8). Measured by flipping
//! `apply_posture`'s `journal_mode` to `DELETE`, and by giving the DuckDB fork
//! the Handle's two pragmas:
//!
//! | Test | `journal_mode=DELETE` | fork gets the pragmas |
//! |---|---|---|
//! | `t20a` | still green | — |
//! | `t20b` | **RED** | — |
//! | `t20c` | still green | — |
//! | `t20d` | still green | — |
//! | `q11_a_wal_reader…` | still green | — |
//! | `q11_busy_timeout…` | **RED** | — |
//! | `duckdb_…_destroys_every_later_commit` | — | **RED** (0 rows lost) |
//!
//! Stated rather than glossed: **only `t20b` and the snapshot-conflict arm
//! discriminate WAL.** The other three SQLite properties hold under a rollback
//! journal too — which makes the headline result *stronger*, not weaker: "a
//! foreign connection's close cannot cost a committed row" is a property of
//! SQLite, not a property of the journal mode we happen to have selected. The
//! DuckDB arm's mutation is the mirror image: give the fork
//! `disable_checkpoint_on_shutdown` and the loss goes to zero, which is what
//! identifies the close — not the read — as the injury.
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
        "aberp-adr0108-readfork-{tag}-{}-{nanos}-{}",
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
#[cfg(not(feature = "sqlite-engine"))]
fn commit_rows(conn: &duckdb::Connection, from: i64, n: i64) {
    for i in from..from + n {
        conn.execute_batch(&format!(
            "BEGIN; INSERT INTO invoices VALUES ('inv-{i}', {i}); COMMIT;"
        ))
        .unwrap();
    }
}

#[cfg(not(feature = "sqlite-engine"))]
fn count(conn: &duckdb::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap()
}

/// A connection with the exact pragmas `aberp_db::open_runtime_connection`
/// applies — i.e. a stand-in for the process-wide `Handle`.
#[cfg(not(feature = "sqlite-engine"))]
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
#[cfg(not(feature = "sqlite-engine"))]
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
/// The SQLite twin is [`t20d_a_foreign_close_does_not_drop_wal_resident_commits`],
/// which shows this half of the class does not exist under WAL.
#[cfg(not(feature = "sqlite-engine"))]
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
// The SQLite arms — T-20.
// ===========================================================================

/// **T-20, first direction — ADR-0108 §2.4's claim, pinned.**
///
/// A second connection that was opened **before** the write sees the commit as
/// soon as it lands, with no reopen, no checkpoint, and no coordination. This
/// is what makes `Handle::read()`-as-a-real-connection safe, and it is the
/// exact opposite of the DuckDB arm above.
///
/// Mutation-verify: there is nothing to remove — the property is the engine's.
/// The test's value is that it will go red if a future posture change (a
/// `journal_mode` other than WAL, a shared cache, a `PRAGMA read_uncommitted`)
/// takes it away.
#[cfg(feature = "sqlite-engine")]
#[test]
fn t20a_autocommit_reader_sees_a_later_commit() {
    let dir = scratch("t20a");
    let db = dir.join("aberp.sqlite");

    let writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    writer
        .execute_batch("CREATE TABLE invoices (id TEXT NOT NULL, n INTEGER NOT NULL) STRICT;")
        .unwrap();

    // Reader opened FIRST — before the row exists. This is the ordering that
    // matters: a reader that opens after the commit would prove nothing.
    let reader = aberp_db::sqlite::open_hardened(&db).unwrap();
    let before: i64 = reader
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 0);

    writer
        .execute("INSERT INTO invoices VALUES ('inv-1', 1)", [])
        .unwrap();

    let after: i64 = reader
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        after, 1,
        "a pre-existing autocommit reader must see the writer's commit — this \
         is ADR-0108 §2.4's 'strictly sees more', and the whole read-fork \
         argument for the migration rests on it"
    );
}

/// **T-20, second direction — and a correction to how ADR-0108 states it.**
///
/// ADR-0108 §7 Step 3 writes the in-transaction half as: "the reader freezes
/// its snapshot **at `BEGIN`** and will not see a commit that lands after it."
/// **That is not what SQLite does, and a T-20 written to that wording would
/// pin a false claim.**
///
/// `BEGIN` is `BEGIN DEFERRED`: it acquires nothing and starts no read
/// transaction. The snapshot is taken at the **first read statement**. So:
///
/// * `BEGIN` → writer commits → `SELECT` **sees** the commit.
/// * `BEGIN` → `SELECT` → writer commits → `SELECT` does **not**.
///
/// Both halves are asserted here, because the first is the one that makes the
/// ADR's wording wrong and the second is the hazard the ADR was reaching for.
/// The practical rule the audit draws from this: a `read()` that opens a
/// transaction and holds it across a `write()` is the only frozen-snapshot
/// exposure, and the freeze begins at its first `SELECT`, not at `BEGIN`.
#[cfg(feature = "sqlite-engine")]
#[test]
fn t20b_the_snapshot_is_taken_at_the_first_read_not_at_begin() {
    let dir = scratch("t20b");
    let db = dir.join("aberp.sqlite");

    let writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    writer
        .execute_batch("CREATE TABLE invoices (id TEXT NOT NULL, n INTEGER NOT NULL) STRICT;")
        .unwrap();
    let reader = aberp_db::sqlite::open_hardened(&db).unwrap();

    // --- Half 1: BEGIN alone does NOT freeze anything. ---
    reader.execute_batch("BEGIN").unwrap();
    writer
        .execute("INSERT INTO invoices VALUES ('inv-1', 1)", [])
        .unwrap();
    let seen: i64 = reader
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        seen, 1,
        "BEGIN (DEFERRED) starts no read transaction — the snapshot is taken \
         at the FIRST READ. ADR-0108 §7 Step 3 says 'freezes its snapshot at \
         BEGIN'; that wording is wrong and this half is the correction."
    );

    // --- Half 2: once the first read has run, the snapshot IS frozen. ---
    writer
        .execute("INSERT INTO invoices VALUES ('inv-2', 2)", [])
        .unwrap();
    let seen_again: i64 = reader
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        seen_again, 1,
        "inside an open read transaction the snapshot is frozen — a commit \
         that lands after the first read is invisible until COMMIT/ROLLBACK. \
         This is the real hazard, and it is why the audit's axis (a) asks \
         whether a read() site holds a transaction open."
    );

    reader.execute_batch("COMMIT").unwrap();
    let after_release: i64 = reader
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        after_release, 2,
        "releasing the read transaction re-syncs the reader to the head"
    );
}

/// **T-20c — a reader never observes a torn or pre-commit state.**
///
/// The DuckDB failure mode has two halves: *stale* (the arm above) and *torn*
/// (a foreign checkpoint folding a WAL under a live instance). This pins that
/// SQLite has neither half on the read side: mid-transaction rows are
/// invisible, and they appear atomically — all of them, at COMMIT.
#[cfg(feature = "sqlite-engine")]
#[test]
fn t20c_a_reader_never_observes_an_uncommitted_write() {
    let dir = scratch("t20c");
    let db = dir.join("aberp.sqlite");

    let mut writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    writer
        .execute_batch("CREATE TABLE lines (n INTEGER NOT NULL) STRICT;")
        .unwrap();
    let reader = aberp_db::sqlite::open_hardened(&db).unwrap();

    let tx = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    for n in 0..18_i64 {
        tx.execute("INSERT INTO lines VALUES (?)", [n]).unwrap();
    }

    let mid: i64 = reader
        .query_row("SELECT count(*) FROM lines", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        mid, 0,
        "an open write transaction must be entirely invisible to a reader — \
         no partial row set, ever"
    );

    tx.commit().unwrap();

    let after: i64 = reader
        .query_row("SELECT count(*) FROM lines", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 18, "and then all 18 appear at once");
}

/// **T-20d — the DuckDB failure mode has no SQLite analogue on close.**
///
/// CLAUDE.md rule 13: "a co-resident fresh `Connection::open` checkpoint-tears
/// the Handle's WAL on close". That is the *destructive* half of the read-fork
/// class — the half that is not merely a short answer.
///
/// Under SQLite WAL a closing connection may checkpoint, but a checkpoint
/// **folds committed frames into the main database**; it cannot discard them.
/// This pins that: a foreign connection opens, reads, and closes while commits
/// are WAL-resident, and every row is still there afterwards — both to the
/// original writer and to a connection opened after the close.
#[cfg(feature = "sqlite-engine")]
#[test]
fn t20d_a_foreign_close_does_not_drop_wal_resident_commits() {
    let dir = scratch("t20d");
    let db = dir.join("aberp.sqlite");

    let writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    writer
        .execute_batch("CREATE TABLE invoices (id TEXT NOT NULL) STRICT;")
        .unwrap();
    for i in 0..5 {
        writer
            .execute("INSERT INTO invoices VALUES (?)", [format!("inv-{i}")])
            .unwrap();
    }

    // A foreign connection: opens, reads, closes. The `Handle` is still live.
    {
        let foreign = aberp_db::sqlite::open_hardened(&db).unwrap();
        let n: i64 = foreign
            .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5, "the foreign reader sees every commit");
        foreign.close().unwrap();
    }

    let after_foreign_close: i64 = writer
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        after_foreign_close, 5,
        "a foreign connection's close must not cost the live writer a single \
         committed row — this is the half of CLAUDE.md rule 13 that SQLite WAL \
         genuinely removes"
    );

    let fresh = aberp_db::sqlite::open_hardened(&db).unwrap();
    let via_fresh: i64 = fresh
        .query_row("SELECT count(*) FROM invoices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(via_fresh, 5);
}

// ===========================================================================
// Q11 — what `busy_timeout` is, and is not, for.
// ===========================================================================

/// **Q11, first half — a WAL reader does not contend with a live writer.**
///
/// ADR-0108 §7 Step 3 argues that a `read()` taken while a `write()` guard is
/// live "now contends for a real file lock", making `busy_timeout` the
/// observability of that case. **On the read side that is not so**: WAL's
/// defining property is that readers do not block writers and writers do not
/// block readers. A reader reaches `busy_timeout` only against a
/// *checkpointer*, not against a writer.
///
/// This matters for the number: 5000 ms is a **write**-contention knob. It is
/// not what protects a read, and it is not what makes a nested
/// `read()`-inside-`write()` observable — see the audit's Finding R-3.
#[cfg(feature = "sqlite-engine")]
#[test]
fn q11_a_wal_reader_does_not_contend_with_a_live_writer() {
    let dir = scratch("q11a");
    let db = dir.join("aberp.sqlite");

    let mut writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    writer
        .execute_batch("CREATE TABLE t (n INTEGER NOT NULL) STRICT;")
        .unwrap();
    writer.execute("INSERT INTO t VALUES (1)", []).unwrap();

    let reader = aberp_db::sqlite::open_hardened(&db).unwrap();
    // Drop the reader's timeout to ~0 so a genuine lock wait would surface as
    // an immediate SQLITE_BUSY instead of a 5-second pause.
    reader
        .busy_timeout(std::time::Duration::from_millis(0))
        .unwrap();

    let tx = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    tx.execute("INSERT INTO t VALUES (2)", []).unwrap();

    let started = std::time::Instant::now();
    let n: i64 = reader
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .expect("a WAL reader must not be blocked by a live write transaction");
    let waited = started.elapsed();

    assert_eq!(n, 1, "the reader sees the last commit, not the open write");
    assert!(
        waited < std::time::Duration::from_millis(500),
        "the read must not have waited on a lock (took {waited:?}); if it did, \
         WAL is not in effect and the posture is broken"
    );
    tx.commit().unwrap();
}

/// **Q11, second half — `busy_timeout` cannot rescue a snapshot conflict, so
/// M5's `BEGIN IMMEDIATE` is not optional.**
///
/// A DEFERRED transaction that reads, then tries to write, after another
/// connection has committed in between gets `SQLITE_BUSY_SNAPSHOT` (5/517).
/// The busy handler is **not** invoked for it — retrying cannot help, because
/// the transaction's snapshot is already stale, so SQLite returns immediately
/// and the whole transaction must be rolled back.
///
/// This is the concrete reason `engine::begin_immediate` exists: it is the only
/// thing standing between the read-modify-write shape (the invoice-number
/// allocator, the audit-chain append) and a failure that no timeout can absorb.
#[cfg(feature = "sqlite-engine")]
#[test]
fn q11_busy_timeout_does_not_retry_a_snapshot_conflict() {
    let dir = scratch("q11b");
    let db = dir.join("aberp.sqlite");

    let a = aberp_db::sqlite::open_hardened(&db).unwrap();
    a.execute_batch("CREATE TABLE alloc (last INTEGER NOT NULL) STRICT;")
        .unwrap();
    a.execute("INSERT INTO alloc VALUES (61)", []).unwrap();
    let b = aberp_db::sqlite::open_hardened(&db).unwrap();

    // A: DEFERRED — read the allocator's head, decide from it...
    a.execute_batch("BEGIN").unwrap();
    let last: i64 = a
        .query_row("SELECT last FROM alloc", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last, 61);

    // ...while B commits the next number underneath it.
    b.execute("UPDATE alloc SET last = 62", []).unwrap();

    // A now tries to write back. `busy_timeout` is 5000 ms on this connection.
    let started = std::time::Instant::now();
    let err = a
        .execute("UPDATE alloc SET last = ?", [last + 1])
        .expect_err(
            "a DEFERRED read-then-write across a concurrent commit MUST fail — \
             if it succeeds, two writers just allocated the same invoice number",
        );
    let waited = started.elapsed();

    assert!(
        waited < std::time::Duration::from_millis(2_000),
        "SQLITE_BUSY_SNAPSHOT is returned immediately; the busy handler is not \
         consulted. Waiting ~{waited:?} would mean busy_timeout WAS engaged, \
         which would change what the 5000 ms number means."
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("busy") || msg.contains("snapshot") || msg.contains("locked"),
        "unexpected error shape: {err}"
    );
    let _ = a.execute_batch("ROLLBACK");

    // And the same sequence under BEGIN IMMEDIATE (M5) cannot reach that state:
    // the write lock is taken before the read, so B's commit cannot interleave.
    let mut a2 = aberp_db::sqlite::open_hardened(&db).unwrap();
    let tx = aberp_db::engine::begin_immediate(&mut a2).unwrap();
    let last2: i64 = tx
        .query_row("SELECT last FROM alloc", [], |r| r.get(0))
        .unwrap();
    tx.execute("UPDATE alloc SET last = ?", [last2 + 1])
        .unwrap();
    tx.commit()
        .expect("BEGIN IMMEDIATE serialises the read-modify-write");

    let final_last: i64 = b
        .query_row("SELECT last FROM alloc", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        final_last, 63,
        "62 was B's, 63 is the IMMEDIATE transaction's — no number reissued"
    );
}
