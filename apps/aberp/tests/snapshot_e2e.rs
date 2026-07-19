//! S426 / ADR-0082 — app-level snapshot integration tests.
//!
//! The `aberp-snapshot` crate's own suite covers export/import/validate/
//! retention math. This suite covers what only the app layer can: the
//! **audit-event emission** for each operation and the full operator
//! journey create → list → restore → validate ([[customer-journey-e2e-gate]],
//! here operator-internal but high-stakes).

use std::path::{Path, PathBuf};

use aberp::snapshot::{restore_and_emit, retention_and_emit, take_and_emit};
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use aberp_db::{Handle, HandleArc};
use aberp_snapshot::{list_snapshots, RetentionPolicy};
use duckdb::Connection;

// ── scaffolding ────────────────────────────────────────────────────────

struct ScopedTempDir(PathBuf);
impl ScopedTempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = C.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "aberp-s426-e2e-{label}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const TENANT: &str = "prod";

fn tid() -> TenantId {
    TenantId::new(TENANT.to_string()).unwrap()
}
fn bh() -> BinaryHash {
    BinaryHash::from_bytes([2u8; 32])
}
fn actor() -> Actor {
    Actor::test_only()
}

/// Seed `db` with an invoice table (+rows) and `n_audit` valid audit
/// entries (a well-formed chain).
fn seed(db: &Path, n_invoice: usize, n_audit: usize) {
    {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS invoice (id BIGINT, amount DOUBLE);")
            .unwrap();
        for i in 0..n_invoice {
            conn.execute(
                "INSERT INTO invoice VALUES (?, ?)",
                duckdb::params![i as i64, (i as f64) * 5.0],
            )
            .unwrap();
        }
    }
    let mut l = Ledger::open(db, tid(), bh()).unwrap();
    for i in 0..n_audit {
        l.append(
            EventKind::Test,
            format!("{{\"i\":{i}}}").into_bytes(),
            actor(),
            None,
        )
        .unwrap();
    }
}

/// Open the ONE shared Handle over a seeded DB — the same primitive `serve`
/// hands the snapshot daemon (ADR-0099 H3).
fn handle(db: &Path) -> HandleArc {
    Handle::open_default(db, tid()).expect("open shared handle")
}

/// All event kinds currently in the ledger, in seq order, read THROUGH the
/// shared Handle.
///
/// Deliberately NOT a fresh `Ledger::open`. A second opener alongside the live
/// Handle folds the Handle's WAL when it closes (the incident mechanism — see
/// `snapshot_does_not_fold_the_handles_wal`), so a test that read that way
/// would be checkpointing the DB between its own assertions and could not
/// observe the very property the WAL-fold test pins. `Handle::read()` is a
/// `try_clone` of the one instance: no second open, nothing to fold.
fn ledger_kinds(db: &HandleArc) -> Vec<EventKind> {
    let conn = db.read().expect("shared read connection");
    let l = Ledger::from_connection(conn, tid(), bh());
    l.entries()
        .unwrap()
        .iter()
        .map(|e| e.kind.clone())
        .collect()
}

fn count_kind(db: &HandleArc, kind: EventKind) -> usize {
    ledger_kinds(db).into_iter().filter(|k| *k == kind).count()
}

// ── tests ──────────────────────────────────────────────────────────────

#[test]
fn create_list_restore_journey_emits_events() {
    let dir = ScopedTempDir::new("journey");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 4, 3);
    let h = handle(&db);
    let store = dir.path().join("store");

    // CREATE — emits SnapshotCreated against the live ledger.
    let before = ledger_kinds(&h).len();
    let rec = take_and_emit(&h, &store, &tid(), bh(), actor()).expect("take");
    assert!(rec.meta.valid, "fresh snapshot valid: {:?}", rec.meta);
    assert_eq!(rec.meta.audit_count, 3);
    assert_eq!(rec.meta.invoice_count, 4);
    assert_eq!(count_kind(&h, EventKind::SnapshotCreated), 1);
    assert_eq!(
        ledger_kinds(&h).last().cloned(),
        Some(EventKind::SnapshotCreated),
        "SnapshotCreated is the newest ledger entry"
    );
    assert!(ledger_kinds(&h).len() > before);

    // LIST — the snapshot is discoverable.
    let listed = list_snapshots(&store).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].meta.seq, rec.meta.seq);

    // RESTORE — to a side path; emits SnapshotRestored. The restored DB
    // carries the same invoice rows (validate the round-trip end-to-end).
    let target = dir.path().join("recovery").join("aberp.duckdb");
    let selector = rec.meta.seq.to_string();
    restore_and_emit(&h, &store, &selector, &target, &tid(), bh(), actor()).expect("restore");
    assert!(target.exists());
    assert_eq!(count_kind(&h, EventKind::SnapshotRestored), 1);

    let conn = Connection::open(&target).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM invoice", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "restored DB has the original invoice rows");
}

#[test]
fn validation_failure_emits_validation_failed_event() {
    let dir = ScopedTempDir::new("valfail");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 1, 3);
    // Tamper the chain so validation must fail.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("UPDATE audit_ledger SET payload = 'x'::BLOB WHERE seq = 1;")
            .unwrap();
    }

    let h = handle(&db);
    let store = dir.path().join("store");
    let rec = take_and_emit(&h, &store, &tid(), bh(), actor()).expect("take produces a record");
    assert!(!rec.meta.valid, "tampered chain must fail validation");
    assert_eq!(count_kind(&h, EventKind::SnapshotValidationFailed), 1);
    assert_eq!(count_kind(&h, EventKind::SnapshotCreated), 0);
}

#[test]
fn retention_emits_pruned_event_and_removes_dirs() {
    let dir = ScopedTempDir::new("retain");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 1, 1);
    let h = handle(&db);
    let store = dir.path().join("store");

    // Take three snapshots.
    for _ in 0..3 {
        take_and_emit(&h, &store, &tid(), bh(), actor()).unwrap();
    }
    assert_eq!(list_snapshots(&store).unwrap().len(), 3);

    // Retain only the newest valid (keep_last=1, no day/week windows).
    let policy = RetentionPolicy {
        keep_last: 1,
        daily_days: 0,
        weekly_weeks: 0,
    };
    let removed = retention_and_emit(&h, &store, &tid(), bh(), actor(), &policy).unwrap();
    assert_eq!(removed.len(), 2, "two older snapshots pruned");
    assert_eq!(list_snapshots(&store).unwrap().len(), 1);
    assert_eq!(count_kind(&h, EventKind::SnapshotPruned), 1);
}

// ── ADR-0099 regression pins (2026-07-19 prod boot refusal) ────────────
//
// Incident: the snapshot daemon was the one audit writer never migrated to
// the shared Handle. Audit mirror seq 8060 > DB seq 8058 with a hash fork at
// the boundary; prod refused to boot. Two independent defects, one test each.

/// Append `n` audit events the way every MIGRATED writer does — through the
/// shared Handle. This is the "other writer" the snapshot daemon raced.
fn handle_append(db: &HandleArc, n: usize) {
    for i in 0..n {
        let mut guard = db.write().expect("shared writer");
        let tx = guard.transaction().expect("tx");
        aberp_audit_ledger::append_in_tx(
            &tx,
            &aberp_audit_ledger::LedgerMeta::new(tid(), bh()),
            EventKind::Test,
            format!("{{\"concurrent\":{i}}}").into_bytes(),
            actor(),
            None,
        )
        .expect("append_in_tx");
        tx.commit().expect("commit");
    }
}

/// All seqs in the ledger, read through the shared Handle.
fn ledger_seqs(db: &HandleArc) -> Vec<u64> {
    let conn = db.read().expect("shared read connection");
    let mut stmt = conn
        .prepare("SELECT seq FROM audit_ledger ORDER BY seq")
        .expect("prepare");
    stmt.query_map([], |r| r.get::<_, i64>(0))
        .expect("query")
        .map(|r| r.unwrap() as u64)
        .collect()
}

/// DEFECT 2 — the checkpoint tear: the snapshot must not FOLD the Handle's WAL.
///
/// The Handle deliberately runs `checkpoint_enabled:false` +
/// `PRAGMA disable_checkpoint_on_shutdown` (crates/aberp-db/src/lib.rs), a
/// pragma set there and NOWHERE else, so its commits stay WAL-resident. The
/// daemon's own `Connection::open` did not carry those settings, so when it
/// closed, DuckDB folded the WAL into the main file underneath the Handle —
/// while `sync_mirror` had already durably appended those rows to the audit
/// mirror. That is the mirror-ahead-of-DB half of the incident (seq 8060 > 8058).
///
/// The observable invariant is therefore the WAL itself: taking a snapshot
/// must leave it intact. Asserting on exported row counts does NOT work as a
/// pin — DuckDB shares one instance per path per process, so an in-process
/// fresh `Connection::open` still reads the WAL and the counts come out
/// identical either way (verified by mutation). The FOLD is what differs.
#[test]
fn snapshot_does_not_fold_the_handles_wal() {
    let dir = ScopedTempDir::new("walfold");
    let db = dir.path().join("aberp.duckdb");
    let wal = {
        let mut os = db.as_os_str().to_owned();
        os.push(".wal");
        PathBuf::from(os)
    };
    seed(&db, 1, 2);
    let h = handle(&db);
    let store = dir.path().join("store");

    // Handle writes stay in the WAL (no checkpoint while the Handle is live).
    handle_append(&h, 5);
    let wal_before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_before > 0,
        "precondition: Handle commits must be WAL-resident (checkpoint_enabled:false)"
    );

    let rec = take_and_emit(&h, &store, &tid(), bh(), actor()).expect("take");
    assert!(rec.meta.valid, "snapshot must validate: {:?}", rec.meta);

    let wal_after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_after >= wal_before,
        "CHECKPOINT TEAR — taking a snapshot folded the Handle's WAL ({wal_before} -> {wal_after} \
         bytes). A snapshot must never checkpoint the live DB underneath the shared Handle; \
         that is what put the audit mirror ahead of the DB (seq 8060 > 8058)."
    );
}

/// DEFECT 1 — the duplicate-seq half, the direct cause of the boot refusal.
///
/// `Ledger::append` serializes on AUDIT_APPEND_LOCK; Handle writers use
/// `append_in_tx`, which does not take that lock. Two DISJOINT mutexes, and no
/// `UNIQUE(seq)` since S341 (dropped for DuckDB ART corruption) — so a snapshot
/// append and a concurrent Handle append could both read head N-1 and both
/// write seq N. That is precisely what produced seq 8056 twice.
///
/// Now that the snapshot path also goes through `db.write()`, both writers
/// contend for the SAME writer mutex and the seqs must come out unique and
/// gapless. Nothing else in the schema enforces this, so this assertion is the
/// only thing standing between a regression and another forked prod ledger.
#[test]
fn snapshot_audit_and_concurrent_handle_writer_cannot_share_a_seq() {
    let dir = ScopedTempDir::new("seqrace");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 1, 1);
    let h = handle(&db);
    let store = dir.path().join("store");

    let before = ledger_seqs(&h).len();

    // Snapshot audit appends racing a steady stream of Handle appends.
    let writer = {
        let h2 = h.clone();
        std::thread::spawn(move || handle_append(&h2, 12))
    };
    for _ in 0..3 {
        take_and_emit(&h, &store, &tid(), bh(), actor()).expect("take");
    }
    writer.join().expect("concurrent writer thread");

    let seqs = ledger_seqs(&h);
    assert_eq!(
        seqs.len(),
        before + 12 + 3,
        "every append must land exactly once (1 seeded + 12 Handle + 3 SnapshotCreated)"
    );

    let mut sorted = seqs.clone();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        seqs.len(),
        "DUPLICATE SEQ — the snapshot audit path forked the ledger again (incident seq 8056): {seqs:?}"
    );
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(
        seqs, expected,
        "seqs must be unique and gapless under contention: {seqs:?}"
    );
    assert_eq!(count_kind(&h, EventKind::SnapshotCreated), 3);
}

// ── F-E whole-DB writer flock: the snapshot CLI one-shots are REAL writers ──

/// ADR-0099 F-E coherence guarantee, pinned permanently — the mirror of
/// `export_invoice_bundle_smoke.rs::run_refuses_while_the_whole_db_writer_lock_is_held`
/// for `aberp snapshot now`.
///
/// `snapshot.rs::open_cli_handle` documents that the one-shots are "mutually
/// excluded by the F-E whole-DB writer flock". That claim is only true because
/// `run_now` ACQUIRES the flock; nothing else enforces it. While another writer
/// (stand-in for a running `aberp serve`) holds the lock, `snapshot now` must
/// REFUSE — otherwise it opens a second whole-DB writer on a live tenant DB and
/// appends `SnapshotCreated` into a forked ledger.
#[test]
fn snapshot_now_refuses_while_the_whole_db_writer_lock_is_held() {
    let dir = ScopedTempDir::new("now-flock");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 2, 1);
    let store = dir.path().join("store");

    // Stand-in for a running `serve`: hold the whole-DB writer lock for `db`.
    let _held = aberp::db_writer_lock::try_acquire(&db, TENANT)
        .expect("acquire ok")
        .expect("stand-in serve must get the lock");

    let args = aberp::cli::SnapshotNowArgs {
        db: db.clone(),
        tenant: TENANT.to_string(),
        store: Some(store.clone()),
    };
    let err = aberp::snapshot::run_now(&args)
        .expect_err("`snapshot now` MUST refuse while the whole-DB writer lock is held");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("single-writer") || msg.contains("already running"),
        "refusal must cite the single-writer rule: {msg}"
    );
    assert!(
        !store.exists(),
        "no snapshot may be written when `snapshot now` refuses"
    );
}

/// Same pin for `aberp snapshot restore` — the acute one: this is the
/// documented prod-repair path, and it appends `SnapshotRestored` to the LIVE
/// tenant ledger. A real, restorable snapshot is staged first, so without the
/// flock this restore would SUCCEED — that is the mutation tooth.
#[test]
fn snapshot_restore_refuses_while_the_whole_db_writer_lock_is_held() {
    let dir = ScopedTempDir::new("restore-flock");
    let db = dir.path().join("aberp.duckdb");
    seed(&db, 4, 2);
    let store = dir.path().join("store");

    // Stage a genuine snapshot so the ONLY thing that can stop the restore is
    // the flock (selector resolves, store is valid, --to is legal).
    let rec = {
        let h = handle(&db);
        take_and_emit(&h, &store, &tid(), bh(), actor()).expect("stage a snapshot")
    };
    assert!(
        rec.meta.valid,
        "staged snapshot must be valid: {:?}",
        rec.meta
    );

    let target = dir.path().join("recovery").join("aberp.duckdb");

    // Stand-in for a running (or crash-looping) `serve`.
    let _held = aberp::db_writer_lock::try_acquire(&db, TENANT)
        .expect("acquire ok")
        .expect("stand-in serve must get the lock");

    let args = aberp::cli::SnapshotRestoreArgs {
        selector: rec.meta.seq.to_string(),
        to: target.clone(),
        confirm: true,
        tenant: TENANT.to_string(),
        db: db.clone(),
        store: Some(store.clone()),
    };
    let err = aberp::snapshot::run_restore(&args)
        .expect_err("`snapshot restore` MUST refuse while the whole-DB writer lock is held");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("single-writer") || msg.contains("already running"),
        "refusal must cite the single-writer rule: {msg}"
    );
    assert!(
        !target.exists(),
        "no database may be written at --to when the restore refuses"
    );

    // And the refusal is the LOCK, not a broken fixture: once the stand-in
    // writer releases, the very same restore succeeds. Without this the test
    // could pass against an unrelated failure.
    drop(_held);
    aberp::snapshot::run_restore(&args).expect("restore succeeds once the lock is free");
    assert!(
        target.exists(),
        "restore writes the target DB when unblocked"
    );
}
