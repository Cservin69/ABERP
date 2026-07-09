//! ADR-0099 H3 — the load-bearing acceptance proof for the shared `Handle`.
//!
//! These open **real DuckDB** files (concurrent separate-instance writers,
//! audit appends, poison recovery), so they run under `cargo test -p aberp-db`
//! on the **Mac / CI gate** — the bundled libduckdb 1.5.3 amalgamation. The
//! PURE D2 debounce logic is unit-tested in `src/debounce.rs` and runs anywhere.
//!
//! Backported from the production-proven editions suite
//! (`ABERP-Editions` 1e6097d, `tests/handle_concurrency_e2e.rs`), restricted to
//! the **checkpoint-DISABLED** subset — H3 disables the runtime validated
//! durable checkpoint (`checkpoint_enabled = false`); the checkpoint-fold tests
//! (`live_durable_checkpoint` / `checkpoint_is_current`) land with H4. A NEW
//! `poisoned_writer_is_recovered_in_place_not_bricked` proves Bug 5.
//!
//! The crossing-the-finish-line test is
//! [`concurrent_separate_opens_tear_the_file_but_shared_handle_never_does`]:
//! separate `Connection::open` instances tear the file; both writers sharing one
//! `aberp_db::Handle` never do.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, mirror_path_for, read_mirror_entries, recent_entries, Actor,
    BinaryHash, EventKind, LedgerMeta, TenantId,
};
use aberp_db::{Handle, HandleConfig};
use duckdb::Connection;

const TENANT: &str = "prod";

struct Tmp(PathBuf);
impl Tmp {
    fn new(label: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p =
            std::env::temp_dir().join(format!("aberp-db-it-{label}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn db(&self) -> PathBuf {
        self.0.join("aberp.duckdb")
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tenant() -> TenantId {
    TenantId::new(TENANT.to_string()).unwrap()
}

/// Seed an empty tenant DB with the audit schema (so a fresh open is valid).
fn seed(db: &Path) {
    let conn = Connection::open(db).unwrap();
    ensure_schema(&conn).unwrap();
    conn.execute_batch("CHECKPOINT;").unwrap();
}

/// Append one audit row on a connection (the shape a daemon write takes).
fn append_one(conn: &mut Connection, seq_label: &str) {
    let meta = LedgerMeta::new(tenant(), BinaryHash::from_bytes([7u8; 32]));
    let tx = conn.transaction().unwrap();
    let actor = Actor::from_local_cli(format!("ulid-{seq_label}"), "tester");
    // A neutral probe kind so it never collides with the poison-recovery
    // path's own `db.auto_recovered` forensic row.
    append_in_tx(
        &tx,
        &meta,
        EventKind::Test,
        format!("{{\"probe\":\"{seq_label}\"}}").into_bytes(),
        actor,
        None,
    )
    .unwrap();
    tx.commit().unwrap();
}

/// Does a *fresh* open of `db` succeed (i.e. the on-disk checkpoint is not
/// torn)? The tear signature is a fresh open failing in `LoadCheckpoint` with
/// "metadata pointer (id 0, idx 0, ptr 0)".
fn fresh_open_ok(db: &Path) -> bool {
    match Connection::open(db) {
        Ok(c) => c.execute_batch("SELECT 1;").is_ok(),
        Err(_) => false,
    }
}

/// **THE acceptance test.** Two independent writers hammer the same single-file
/// DuckDB through ONE shared `aberp_db::Handle`: the file must NEVER tear across
/// all iterations, and every committed row is present (seq coherence). The
/// pre-fix separate-`Connection::open` arm (which tears) runs only under
/// `ABERP_REPRO_1702_TEAR`.
#[test]
fn concurrent_separate_opens_tear_the_file_but_shared_handle_never_does() {
    let tmp = Tmp::new("shared");
    let db = tmp.db();
    seed(&db);
    // Isolate the single-instance property: checkpoint disabled (H3 posture).
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        ..Default::default()
    };
    let handle: Arc<Handle> = Handle::open(&db, tenant(), cfg).unwrap();

    let iterations = 200usize;
    let mut workers = Vec::new();
    // Writer 1 — pricing-style enqueue cadence.
    {
        let h = handle.clone();
        workers.push(thread::spawn(move || {
            for i in 0..iterations {
                let mut g = h.write().unwrap();
                append_one(&mut g, &format!("w1-{i}"));
            }
        }));
    }
    // Writer 2 — email-relay-style claim cadence (tightened for the test).
    {
        let h = handle.clone();
        workers.push(thread::spawn(move || {
            for i in 0..iterations {
                {
                    let mut g = h.write().unwrap();
                    append_one(&mut g, &format!("w2-{i}"));
                }
                thread::sleep(Duration::from_micros(50));
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    // The whole point: a fresh open still succeeds — the file never tore.
    assert!(
        fresh_open_ok(&db),
        "shared-handle path tore the single-file DB — single-instance regression"
    );
    // And every committed row is present (2 writers * iterations).
    let conn = handle.read().unwrap();
    let entries = recent_entries(&conn, u32::MAX).unwrap();
    assert_eq!(
        entries.len(),
        iterations * 2,
        "shared handle lost/duplicated audit rows (seq coherence)"
    );

    // ---- Pre-fix repro (opt-in): separate instances, EXPECTED to tear. ----
    if std::env::var("ABERP_REPRO_1702_TEAR").is_ok() {
        let tmp2 = Tmp::new("separate");
        let db2 = tmp2.db();
        seed(&db2);
        let mut tore = false;
        let mut ws = Vec::new();
        for w in 0..2 {
            let dbp = db2.clone();
            ws.push(thread::spawn(move || {
                for i in 0..iterations {
                    if let Ok(mut c) = Connection::open(&dbp) {
                        let _ = ensure_schema(&c);
                        append_one(&mut c, &format!("sep-{w}-{i}"));
                    }
                }
            }));
        }
        for w in ws {
            let _ = w.join();
        }
        if !fresh_open_ok(&db2) {
            tore = true;
        }
        assert!(
            tore,
            "pre-fix separate-instance arm did NOT reproduce the tear"
        );
    }
}

/// Lockstep mirror (closes the mirror-lag gap at the source): after a handle
/// write drops, the mirror head == the DB head — the mirror tracks the DB with
/// no lag.
#[test]
fn daemon_write_appends_to_mirror_in_lockstep() {
    let tmp = Tmp::new("lockstep");
    let db = tmp.db();
    seed(&db);
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        ..Default::default()
    };
    let handle = Handle::open(&db, tenant(), cfg).unwrap();

    for i in 0..5 {
        let mut g = handle.write().unwrap();
        append_one(&mut g, &format!("ls-{i}"));
        // guard drop here -> post-commit hook runs sync_mirror
    }

    let conn = handle.read().unwrap();
    let db_entries = recent_entries(&conn, u32::MAX).unwrap();
    let db_head = db_entries.len() as u64;

    let mirror = mirror_path_for(&db);
    let mirror_entries = read_mirror_entries(&mirror).unwrap();
    let mirror_head = mirror_entries.last().map(|e| e.seq).unwrap_or(0);

    assert_eq!(
        mirror_head, db_head,
        "mirror head ({mirror_head}) lags DB head ({db_head}) — lockstep broken"
    );
}

/// F-C / S335 — **read-after-write coherence via `try_clone`.** `Handle::read()`
/// hands out a `try_clone` of the SHARED instance (one buffer cache, no second
/// OS open), so a read observes every committed write immediately. Pins that
/// coherence while the read-write instance stays continuously open (checkpoint
/// off) and multiple read clones coexist in-process.
#[test]
fn c2_read_clone_is_coherent_while_rw_instance_stays_open() {
    let tmp = Tmp::new("clone-coherent");
    let db = tmp.db();
    seed(&db);
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        ..Default::default()
    };
    let handle = Handle::open(&db, tenant(), cfg).unwrap();

    {
        let mut g = handle.write().unwrap();
        append_one(&mut g, "co-1");
    }

    // (1) A read clone taken while the RW instance is live observes the commit.
    let r1 = handle.read().unwrap();
    assert_eq!(
        recent_entries(&r1, u32::MAX).unwrap().len(),
        1,
        "read() try_clone did not observe the committed write (coherence broken)"
    );

    // (2) Hold r1 open, commit a second write; a fresh clone sees BOTH, and the
    //     still-held r1 (on a NEW query) also sees the latest committed state.
    {
        let mut g = handle.write().unwrap();
        append_one(&mut g, "co-2");
    }
    let r2 = handle.read().unwrap();
    assert_eq!(
        recent_entries(&r2, u32::MAX).unwrap().len(),
        2,
        "fresh read() try_clone did not see the second committed write"
    );
    assert_eq!(
        recent_entries(&r1, u32::MAX).unwrap().len(),
        2,
        "held read() try_clone did not observe the later commit (single-instance coherence)"
    );

    drop(r1);
    drop(r2);
    assert!(
        fresh_open_ok(&db),
        "file tore under read-clone + read-write same-process coexistence"
    );
}

/// F-C / S335 reinforcement: after EACH of many commits, a fresh read (concurrent
/// with the continuously-open read-write instance) observes exactly the committed
/// count so far. Deterministic (single thread) to avoid CI timing flakiness.
#[test]
fn c2_assertion1_readonly_open_sees_each_commit_while_rw_instance_stays_open() {
    let tmp = Tmp::new("ro-seq");
    let db = tmp.db();
    seed(&db);
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        ..Default::default()
    };
    let handle = Handle::open(&db, tenant(), cfg).unwrap();

    let n = 50usize;
    for i in 0..n {
        {
            let mut g = handle.write().unwrap();
            append_one(&mut g, &format!("seq-{i}"));
        }
        let ro = handle
            .read()
            .expect("read open rejected while the read-write instance is live");
        assert_eq!(
            recent_entries(&ro, u32::MAX).unwrap().len(),
            i + 1,
            "read open did not see all commits up to iteration {i}"
        );
    }
    assert!(
        fresh_open_ok(&db),
        "file tore under repeated read + read-write coexistence"
    );
}

/// ADR-0099 H3 / Bug 5 — **a poisoned writer is RECOVERED in place, not bricked.**
///
/// Before the shared Handle, a daemon panic hurt only itself. The shared Handle
/// makes a panic while holding the [`aberp_db::WriteGuard`] poison the ONE
/// process-wide writer mutex — which, un-handled, bricks every write path for the
/// whole process until restart. This drives that exact sequence: a thread panics
/// while holding the write guard (poisoning the mutex), then asserts a subsequent
/// `write()` SUCCEEDS (clear_poison + post-poison integrity re-verify PASS) and
/// that the recovery emitted a `db.auto_recovered` forensic audit row.
#[test]
fn poisoned_writer_is_recovered_in_place_not_bricked() {
    let tmp = Tmp::new("poison");
    let db = tmp.db();
    seed(&db);
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        ..Default::default()
    };
    let handle = Handle::open(&db, tenant(), cfg).unwrap();

    // A valid committed head so the post-poison chain re-verify has something to
    // verify (genesis→head).
    {
        let mut g = handle.write().unwrap();
        append_one(&mut g, "pre");
    }

    // Poison the writer mutex: panic while holding the WriteGuard. The unwind
    // drops the guard's MutexGuard mid-panic, which std marks as poisoned.
    let h = handle.clone();
    let joined = thread::spawn(move || {
        let _g = h.write().unwrap();
        panic!("intentional panic to poison the shared writer mutex");
    })
    .join();
    assert!(joined.is_err(), "the panicking writer thread must have unwound");

    // The mutex is now poisoned. A subsequent write() MUST recover in place and
    // succeed — NOT return an error and NOT brick the process.
    {
        let mut g = handle
            .write()
            .expect("poisoned writer must be recovered in place, not bricked (Bug 5)");
        append_one(&mut g, "post");
    }

    // The chain is now: pre (test) → db.auto_recovered (recovery) → post (test).
    let conn = handle.read().unwrap();
    let recovered: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'db.auto_recovered'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        recovered, 1,
        "poison recovery must emit exactly one db.auto_recovered forensic audit row"
    );
    // The recovery row's payload carries the trigger (BLOB → cast to text).
    let trigger_ok: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'db.auto_recovered' \
             AND CAST(payload AS VARCHAR) LIKE '%writer_poison_recovered%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(trigger_ok, 1, "recovery row must carry trigger=writer_poison_recovered");
    // Both business rows survived (pre + post) alongside the one recovery row.
    let total = recent_entries(&conn, u32::MAX).unwrap().len();
    assert_eq!(total, 3, "expected pre + auto_recovered + post = 3 rows, got {total}");
    assert!(fresh_open_ok(&db), "DB must stay openable after poison recovery");
}

// ─────────────────────────────────────────────────────────────────────────
// Boot-audit coherence — the separate-boot-opener fork the shared Handle closes.
//
// A separate un-pragma'd boot opener that folds the Handle's pending-WAL row
// DOUBLE-APPLIES the tail row; because `audit_ledger` carries no UNIQUE on `seq`
// the fork is INSERTED rather than rejected — a DB-only, mirror-absent duplicate,
// once per launch. Routing boot accesses through the shared Handle
// (`read()`/`write()`) means exactly ONE instance, no double-apply.

fn wal_path(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(".wal");
    PathBuf::from(s)
}

const PY_KIND: &str = "quote.pipeline_python_resolved";

/// Append one `quote.pipeline_python_resolved` audit row through the shared
/// Handle. The WriteGuard drop runs the lockstep `sync_mirror`, so the row lands
/// in DB **and** mirror.
fn append_python_resolved(handle: &Handle, launch: &str) {
    let mut g = handle.write().unwrap();
    let meta = LedgerMeta::new(tenant(), BinaryHash::from_bytes([7u8; 32]));
    let tx = g.transaction().unwrap();
    let actor = Actor::from_local_cli(format!("proc-{launch}"), "system");
    append_in_tx(
        &tx,
        &meta,
        EventKind::PipelinePythonResolved,
        br#"{"resolution_kind":"project_venv","module_importable":true}"#.to_vec(),
        actor,
        Some("quote_pipeline_python_resolved:prod:project_venv:/opt/venv".to_string()),
    )
    .unwrap();
    tx.commit().unwrap();
    // g drops here -> lockstep sync_mirror.
}

fn py_rows_in_db(db: &Path) -> i64 {
    let c = Connection::open(db).unwrap();
    c.query_row(
        "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'quote.pipeline_python_resolved'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn py_rows_in_mirror(db: &Path) -> usize {
    read_mirror_entries(&mirror_path_for(db))
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == PY_KIND)
        .count()
}

fn db_total_vs_distinct_seq(db: &Path) -> (i64, i64) {
    let c = Connection::open(db).unwrap();
    let total: i64 = c
        .query_row("SELECT COUNT(*) FROM audit_ledger", [], |r| r.get(0))
        .unwrap();
    let distinct: i64 = c
        .query_row("SELECT COUNT(DISTINCT seq) FROM audit_ledger", [], |r| {
            r.get(0)
        })
        .unwrap();
    (total, distinct)
}

/// PRE-FIX repro: a separate un-pragma'd boot opener folds the Handle's pending
/// WAL row; with no UNIQUE on `seq` the double-apply FORKS a DB-only,
/// mirror-absent duplicate. Deterministic model of the two coexisting instances
/// via a WAL snapshot/restore.
#[test]
fn separate_boot_opener_forks_a_pending_wal_python_resolved_row() {
    let tmp = Tmp::new("pyfork");
    let db = tmp.db();
    seed(&db);
    // Handle keeps a PENDING WAL: no debounced checkpoint, no close-fold.
    let cfg = HandleConfig {
        checkpoint_enabled: false,
        disable_implicit_close_checkpoint: true,
        ..Default::default()
    };
    let handle = Handle::open(&db, tenant(), cfg).unwrap();
    append_python_resolved(&handle, "launch-1");
    assert_eq!(py_rows_in_mirror(&db), 1, "mirror got the one coherent row");
    let wal = wal_path(&db);
    let snap = std::fs::read(&wal).expect("row must sit in a pending WAL, not yet folded");
    assert!(!snap.is_empty(), "pending WAL must be non-empty");
    drop(handle);
    // Boot opener #1 (a separate, un-pragma'd instance) folds the WAL on close.
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch("SELECT 1;").unwrap();
    }
    // Restore the co-existing instance's WAL view and let a second separate
    // opener fold it too -> double-apply.
    std::fs::write(&wal, &snap).unwrap();
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CHECKPOINT;").unwrap();
    }
    let db_ct = py_rows_in_db(&db);
    let mir_ct = py_rows_in_mirror(&db) as i64;
    let (total, distinct) = db_total_vs_distinct_seq(&db);
    assert!(
        db_ct >= 2,
        "pre-fix: the double-apply must FORK the python-resolved row in the DB (got {db_ct})"
    );
    assert!(
        db_ct > mir_ct,
        "pre-fix: the fork is DB-present but mirror-absent: db={db_ct} mirror={mir_ct}"
    );
    assert!(
        total > distinct,
        "pre-fix: a duplicate/forked seq exists (total={total} distinct={distinct})"
    );
}

/// POST-FIX invariant: when the boot DB accesses are routed through the shared
/// Handle (`read()`/`write()`), there is ONE instance, so no separate opener can
/// double-apply the pending WAL. Across two launches the audit chain stays
/// coherent: every DB row is mirrored, the mirror head tracks the DB head, and no
/// `seq` is forked.
#[test]
fn boot_access_through_shared_handle_keeps_python_resolved_db_mirror_coherent() {
    let tmp = Tmp::new("pycoherent");
    let db = tmp.db();
    seed(&db);
    for launch in ["launch-1", "launch-2"] {
        // H3 default: checkpoint disabled (still lockstep-mirrors on drop).
        let handle = Handle::open(&db, tenant(), HandleConfig::default()).unwrap();
        append_python_resolved(&handle, launch);
        // Boot row-count via the ONE shared instance (models count_jobs).
        {
            let c = handle.read().unwrap();
            let _boot_count: i64 = c
                .query_row("SELECT COUNT(*) FROM audit_ledger", [], |r| r.get(0))
                .unwrap();
        }
        // Boot index-migration DDL via the ONE shared instance (models the S288
        // migrate). No separate un-pragma'd opener exists to fold the WAL.
        {
            let g = handle.write().unwrap();
            g.execute_batch(
                "CREATE TABLE IF NOT EXISTS boot_probe(x INTEGER); DROP TABLE boot_probe;",
            )
            .unwrap();
        }
        drop(handle);
    }
    let db_ct = py_rows_in_db(&db);
    let mir_ct = py_rows_in_mirror(&db) as i64;
    assert_eq!(
        db_ct, mir_ct,
        "no DB-only orphan: python-resolved DB rows ({db_ct}) must equal mirror rows ({mir_ct})"
    );
    let (total, distinct) = db_total_vs_distinct_seq(&db);
    assert_eq!(
        total, distinct,
        "no forked seq across relaunch (total={total} distinct={distinct})"
    );
    let db_max: i64 = {
        let c = Connection::open(&db).unwrap();
        c.query_row("SELECT COALESCE(MAX(seq), 0) FROM audit_ledger", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let mir_max = read_mirror_entries(&mirror_path_for(&db))
        .unwrap()
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    assert_eq!(
        mir_max as i64, db_max,
        "mirror head ({mir_max}) must equal DB head ({db_max}) — no DB-only rows"
    );
}
