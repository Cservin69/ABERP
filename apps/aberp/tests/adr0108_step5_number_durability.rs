//! ADR-0108 **T-14 — crash / invoice-number durability, on SQLite.**
//!
//! S444's defect, re-armed on the new engine: *issue → kill → restart → the
//! next number must not reuse one that was already handed out.*
//!
//! # What this file pins, and what it does not
//!
//! `allocate_in_tx` is `duckdb::`-typed and has **not** crossed the seam — the
//! application still runs on DuckDB, and this migration is DEV-only. So this
//! file cannot drive the real allocator against SQLite, and it does not
//! pretend to. It pins the half that the engine change actually moves: the
//! **storage engine's behaviour under the allocator's transaction shape**,
//! across a real `SIGKILL` of a real process.
//!
//! The other half — that `allocate_in_tx` takes `.max(durable_floor)` at all —
//! is pinned on DuckDB by `s444_torn_tail_number_reuse.rs`, and that test is
//! mutation-verified against reverting the `.max()` term. [`allocate_once`]
//! below is a faithful copy of that arithmetic and says so; it is not a second
//! implementation with its own opinions.
//!
//! # Why the engine's half needed re-pinning at all
//!
//! S444 happened because `invoice_sequence_state.next_number` lived in tables
//! whose committed rows could *vanish*: DuckDB's `Handle` keeps its commits
//! WAL-resident by design, and any co-resident `Connection::open`'s **close**
//! folds and truncates that WAL (PR #52's R-5 — measured, 10 of 15 committed
//! rows lost, every one of them having returned `Ok`). The counter rewound
//! onto numbers already filed with NAV.
//!
//! So the four questions this file answers, by measurement:
//!
//! 1. Does a committed allocation survive `SIGKILL`?
//! 2. Does an *uncommitted* one leave nothing behind — no burned number, no
//!    advanced counter?
//! 3. Across repeated kill cycles, is a number ever re-issued?
//! 4. **Does a second connection's close destroy the writer's commits, the way
//!    it does on DuckDB?** This is the R-5 class, asked of the engine the
//!    migration moves to.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aberp_db::engine::{params, Connection};

const SERIES: &str = "ser-durability";
const FISCAL_YEAR: i64 = 2026;

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-t14-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Wait for the child's signal file and return the number it wrote.
///
/// **Non-empty is the condition, not existence.** `std::fs::write` creates the
/// file before it writes to it, so a parent that only checks `exists()` races
/// the child and reads `""` — which is not a flake to retry past: the parent
/// then panics *before* `child.kill()`, and the child sleeps out its full
/// 60-second bound.
fn wait_for_number(p: &Path, what: &str) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(n) = s.trim().parse::<i64>() {
                return n;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what} at {}", p.display());
}

/// A fresh SQLite invoice family with one series and one open sequence bucket.
fn setup(db: &Path) {
    let conn = aberp_db::sqlite::open_hardened(db).unwrap();
    aberp::migrate_billing::ensure_billing_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO invoice_series (id, code, reset_policy, fiscal_year, created_at)
         VALUES (?, 'T14', 'AnnualOnFiscalYear', NULL, '2026-07-31T00:00:00Z')",
        params![SERIES],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO invoice_sequence_state (series_id, fiscal_year, next_number, updated_at)
         VALUES (?, ?, 1, '2026-07-31T00:00:00Z')",
        params![SERIES, FISCAL_YEAR],
    )
    .unwrap();
}

/// ADR-0009 §3 "Allocate (atomic)", reproduced against SQLite.
///
/// **A faithful copy of `duckdb_store.rs::allocate_in_tx`'s number arithmetic
/// and transaction shape**, narrowed to the sequence tables:
///
/// ```text
/// allocated = next_number .max(start_value) .max(sequence_floor) .max(durable_floor)
/// ```
///
/// with `durable_floor = durable_high_water + 1` (S444) and `start_value` /
/// `sequence_floor` omitted because they are 1 and `None` in every scenario
/// here. `begin_immediate` is `BEGIN IMMEDIATE` on SQLite (M5), which takes
/// the write lock before the read — the difference between "two writers both
/// read the same `next_number`" and "one of them waits".
fn allocate_once(conn: &mut Connection, durable_high_water: Option<i64>, invoice_id: &str) -> i64 {
    let tx = aberp_db::engine::begin_immediate(conn).unwrap();
    let next: i64 = tx
        .query_row(
            "SELECT next_number FROM invoice_sequence_state
             WHERE series_id = ? AND fiscal_year = ?",
            params![SERIES, FISCAL_YEAR],
            |r| r.get(0),
        )
        .unwrap();
    let durable_floor = durable_high_water.map(|h| h + 1).unwrap_or(0);
    let allocated = next.max(durable_floor);

    tx.execute(
        "UPDATE invoice_sequence_state SET next_number = ?, updated_at = ?
         WHERE series_id = ? AND fiscal_year = ?",
        params![allocated + 1, "2026-07-31T00:00:00Z", SERIES, FISCAL_YEAR],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO invoice_sequence_reservation
           (id, series_id, fiscal_year, number, invoice_id, status, void_reason,
            reserved_at, used_at, voided_at)
         VALUES (?, ?, ?, ?, ?, 'reserved', NULL, '2026-07-31T00:00:00Z', NULL, NULL)",
        params![
            format!("res-{invoice_id}"),
            SERIES,
            FISCAL_YEAR,
            allocated,
            invoice_id
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    allocated
}

/// Every number this bucket has ever handed out, from the reservation table —
/// the durable witness, not the counter.
fn reserved_numbers(db: &Path) -> Vec<i64> {
    let conn = aberp_db::sqlite::open_hardened(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT number FROM invoice_sequence_reservation ORDER BY number ASC")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn next_number(db: &Path) -> i64 {
    let conn = aberp_db::sqlite::open_hardened(db).unwrap();
    conn.query_row(
        "SELECT next_number FROM invoice_sequence_state WHERE series_id = ?",
        params![SERIES],
        |r| r.get(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Questions 1 + 2 — SIGKILL, one committed allocation and one in flight
// ---------------------------------------------------------------------------

/// **The headline.** A committed allocation survives `SIGKILL`; an
/// uncommitted one leaves *nothing* — no reservation row, no advanced counter,
/// no half-burned number. On restart the allocator hands out the next number
/// up, and it has never been handed out before.
///
/// The child is a real separate process killed with a real `SIGKILL` (std's
/// `Child::kill`), so no destructor runs, no `Drop` fires, and nothing
/// cooperative cleans up. That is the only way to ask this question honestly:
/// a hand-written `ROLLBACK` would be testing rusqlite's API, not the engine's
/// crash behaviour.
#[test]
fn a_committed_number_survives_sigkill_and_an_uncommitted_one_leaves_nothing() {
    // CHILD ARM: commit allocation #1, then open a SECOND allocation, do all
    // of its writes, and block without committing until killed.
    if std::env::var("ABERP_S5_KILL_CHILD").is_ok() {
        let db = PathBuf::from(std::env::var("ABERP_S5_DB").unwrap());
        let ready = PathBuf::from(std::env::var("ABERP_S5_READY").unwrap());
        let mut conn = aberp_db::sqlite::open_hardened(&db).unwrap();

        let committed = allocate_once(&mut conn, None, "inv-committed");

        // The in-flight one: every write of the allocation sequence, no commit.
        let tx = aberp_db::engine::begin_immediate(&mut conn).unwrap();
        tx.execute(
            "UPDATE invoice_sequence_state SET next_number = ?, updated_at = ?
             WHERE series_id = ? AND fiscal_year = ?",
            params![committed + 2, "in-flight", SERIES, FISCAL_YEAR],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO invoice_sequence_reservation
               (id, series_id, fiscal_year, number, invoice_id, status, void_reason,
                reserved_at, used_at, voided_at)
             VALUES ('res-inflight', ?, ?, ?, 'inv-inflight', 'reserved', NULL,
                     'in-flight', NULL, NULL)",
            params![SERIES, FISCAL_YEAR, committed + 1],
        )
        .unwrap();

        std::fs::write(&ready, committed.to_string()).unwrap();
        // Hold the open transaction until killed. Bounded so a lost parent
        // cannot leave a zombie holding the write lock.
        std::thread::sleep(Duration::from_secs(60));
        drop(tx);
        return;
    }

    let dir = scratch("sigkill");
    let db = dir.join("aberp.sqlite");
    let ready = dir.join("child.ready");
    setup(&db);

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_committed_number_survives_sigkill_and_an_uncommitted_one_leaves_nothing",
        ])
        .env("ABERP_S5_KILL_CHILD", "1")
        .env("ABERP_S5_DB", &db)
        .env("ABERP_S5_READY", &ready)
        .env("RUST_TEST_THREADS", "1")
        .spawn()
        .expect("spawn the allocating child");

    let committed = wait_for_number(&ready, "the child to commit #1 and open #2");
    assert_eq!(committed, 1, "the first allocation in a fresh bucket is 1");

    child.kill().expect("SIGKILL the allocator mid-transaction");
    let _ = child.wait();

    // --- question 1: the committed allocation is still there ---
    let reserved = reserved_numbers(&db);
    assert_eq!(
        reserved,
        vec![committed],
        "the COMMITTED allocation must survive SIGKILL. On DuckDB this is the property a \
         co-resident connection's close destroys (PR #52 R-5); here nothing but the process \
         died"
    );

    // --- question 2: the in-flight one left nothing ---
    assert_eq!(
        next_number(&db),
        committed + 1,
        "the killed transaction must not have advanced the counter — a counter ahead of the \
         reservations is a burned number with no invoice behind it"
    );

    // --- and the next allocation does not reuse ---
    let mut conn = aberp_db::sqlite::open_hardened(&db).unwrap();
    let after = allocate_once(&mut conn, None, "inv-after-restart");
    assert_eq!(after, committed + 1);
    assert!(
        after > committed,
        "re-issued number {after} — it was already handed out before the crash"
    );
    assert_eq!(reserved_numbers(&db), vec![committed, after]);
}

// ---------------------------------------------------------------------------
// Question 3 — repeated kill cycles never re-issue
// ---------------------------------------------------------------------------

/// Issue → kill → restart, three times over, asserting after every cycle that
/// the set of handed-out numbers is strictly increasing and duplicate-free.
///
/// One cycle can pass by luck; the S444 incident was a *repeated* rewind
/// across three DEV sessions, and each repeat was a distinct NAV
/// `INVOICE_NUMBER_NOT_UNIQUE` rejection.
#[test]
fn repeated_crash_cycles_never_reissue_a_number() {
    if std::env::var("ABERP_S5_CYCLE_CHILD").is_ok() {
        let db = PathBuf::from(std::env::var("ABERP_S5_DB").unwrap());
        let ready = PathBuf::from(std::env::var("ABERP_S5_READY").unwrap());
        let tag = std::env::var("ABERP_S5_TAG").unwrap();
        let mut conn = aberp_db::sqlite::open_hardened(&db).unwrap();
        let n = allocate_once(&mut conn, None, &format!("inv-{tag}"));
        std::fs::write(&ready, n.to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }

    let dir = scratch("cycles");
    let db = dir.join("aberp.sqlite");
    setup(&db);

    let mut handed_out: Vec<i64> = Vec::new();
    for cycle in 0..3 {
        let ready = dir.join(format!("cycle-{cycle}.ready"));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "repeated_crash_cycles_never_reissue_a_number"])
            .env("ABERP_S5_CYCLE_CHILD", "1")
            .env("ABERP_S5_DB", &db)
            .env("ABERP_S5_READY", &ready)
            .env("ABERP_S5_TAG", cycle.to_string())
            .env("RUST_TEST_THREADS", "1")
            .spawn()
            .expect("spawn the cycle child");
        let n = wait_for_number(&ready, "the child to commit its allocation");
        child.kill().expect("SIGKILL after the commit");
        let _ = child.wait();

        assert!(
            !handed_out.contains(&n),
            "cycle {cycle} re-issued number {n} (already handed out: {handed_out:?})"
        );
        handed_out.push(n);
        assert_eq!(
            reserved_numbers(&db),
            handed_out,
            "the durable witness must agree with what was handed out, every cycle"
        );
    }
    assert_eq!(handed_out, vec![1, 2, 3]);
}

// ---------------------------------------------------------------------------
// The S444 property itself — a torn tail must not rewind the sequence
// ---------------------------------------------------------------------------

/// **S444, under SQLite.** With the business tail torn — the counter rewound
/// below a number the audit ledger proves was already reserved — the durable
/// floor forces the allocation clear, so no number is re-issued.
///
/// The tear is *induced* here rather than reproduced: SQLite has no mechanism
/// that produces it (that is the finding of the R-5 arm below), so the state
/// is written directly. That is honest and it is the point — the belt stays on
/// even though the braces no longer slip. ADR-0108 §7 Step 5 says S444's
/// durable floor is carried across **unchanged**, and an invariant carried
/// across without a pin on the new engine is an invariant nobody re-checked.
///
/// Mutation-verify: drop the `.max(durable_floor)` term from [`allocate_once`]
/// and this reports `re-issued number 3`.
#[test]
fn a_torn_tail_cannot_rewind_the_sequence_when_the_ledger_knows_better() {
    let dir = scratch("s444");
    let db = dir.join("aberp.sqlite");
    setup(&db);

    let mut conn = aberp_db::sqlite::open_hardened(&db).unwrap();
    for i in 1..=3 {
        assert_eq!(allocate_once(&mut conn, None, &format!("inv-{i}")), i);
    }
    let proven_high_water = 3;

    // The tear: the counter goes back to 3 while 3 has already been filed. On
    // DuckDB this is what a folded WAL produced; here it is written by hand.
    conn.execute(
        "UPDATE invoice_sequence_state SET next_number = 3 WHERE series_id = ?",
        params![SERIES],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM invoice_sequence_reservation WHERE number = 3",
        [],
    )
    .unwrap();
    assert_eq!(next_number(&db), 3, "the torn state is set up as intended");

    // Without the ledger witness the allocator would hand out 3 again.
    // With it, the floor is 4.
    let allocated = allocate_once(&mut conn, Some(proven_high_water), "inv-after-tear");
    assert_eq!(
        allocated, 4,
        "re-issued number {allocated} — the audit ledger proved 3 was already reserved"
    );
    assert!(allocated > proven_high_water);
}

// ---------------------------------------------------------------------------
// Question 4 — the R-5 class, asked of SQLite
// ---------------------------------------------------------------------------

/// **The fork-close class does not exist on SQLite.**
///
/// On DuckDB, a co-resident `Connection::open`'s **close** checkpoints and
/// truncates the `Handle`'s WAL, and from that moment the writer's further
/// commits return `Ok` and are written **nowhere** — measured 3/3 in PR #52,
/// 10 of 15 rows lost. That is the defect this migration is partly a response
/// to, and it is still live in production on DuckDB.
///
/// The same shape is run here against SQLite: a second connection opens the
/// same file, reads, and closes, while the first connection keeps committing.
/// Nothing is lost. Two independent reasons, both worth stating because the
/// test can only measure the outcome: a SQLite checkpoint **copies** WAL
/// frames into the main database rather than discarding them, and it will not
/// truncate past a frame another connection still needs.
///
/// This does not close R-5 — prod stays on DuckDB and R-5 owes its own PR
/// (ADR-0108 §9). What it establishes is that Step 5 has not *reintroduced*
/// the class on the engine it writes to.
#[test]
fn a_second_connections_close_does_not_destroy_the_writers_commits() {
    let dir = scratch("r5");
    let db = dir.join("aberp.sqlite");
    setup(&db);

    let mut writer = aberp_db::sqlite::open_hardened(&db).unwrap();
    for i in 1..=5 {
        assert_eq!(allocate_once(&mut writer, None, &format!("pre-{i}")), i);
    }

    // The fork: a second connection on the same file, opened and closed while
    // the writer stays live. On DuckDB this close is the injury.
    {
        let forked = aberp_db::sqlite::open_hardened(&db).unwrap();
        let seen: i64 = forked
            .query_row(
                "SELECT count(*) FROM invoice_sequence_reservation",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seen, 5, "the forked READ is coherent on both engines");
        drop(forked);
    }

    // Ten further commits on the ORIGINAL connection. This is where DuckDB
    // returns `Ok` and writes nowhere.
    for i in 6..=15 {
        assert_eq!(allocate_once(&mut writer, None, &format!("post-{i}")), i);
    }
    drop(writer);

    let survived = reserved_numbers(&db);
    assert_eq!(
        survived,
        (1..=15).collect::<Vec<i64>>(),
        "every commit before AND after the foreign close must be durable. On DuckDB this \
         assertion fails with 5 of 15 rows present (PR #52, \
         `duckdb_a_foreign_close_silently_destroys_every_later_commit`)"
    );
}
