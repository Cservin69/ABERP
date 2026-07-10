//! ADR-0099 H3 — the WAL-fold ledger tear, reproduced on real primitives.
//!
//! WHY THIS EXISTS. A parallel DEV-rig session reproduced, on real data, that
//! seeding master data and cleanly shutting `aberp serve` down TEARS the audit
//! ledger: the mirror (`<db>.audit.log`) ends AHEAD of the DB, and the next boot
//! REFUSES with `MirrorAheadOfDb`. This test reproduces the *exact* mechanism
//! deterministically with the real [`aberp_db::Handle`], the real lockstep
//! `sync_mirror`, and the real boot oracle
//! [`aberp_audit_ledger::ensure_consistent_with_db`] — so the corruption vector
//! is pinned in CI, not just in a memory.
//!
//! THE MECHANISM (measured, see the scenarios below). The shared `Handle` opens
//! with checkpoint-on-shutdown DISABLED, so its committed audit rows live in the
//! WAL and are mirrored on `WriteGuard::drop`. A residual `duckdb::Connection::
//! open` (any in-serve opener OUTSIDE the Handle — a daemon's per-tick conn, a
//! master-data CRUD handler, a reader) does NOT set that pragma. If such a conn
//! opened a snapshot of the DB BEFORE a Handle write and then COMMITS ITS OWN
//! write, its checkpoint truncates the WAL back to its stale snapshot — dropping
//! the Handle's mirrored tail from the on-disk DB. Mirror ahead → boot refuses.
//!
//! WHAT IT PROVES FOR THE H3 SWEEP. The finish line is NOT census-zero — it is a
//! CLEAN SHUTDOWN AFTER REAL WRITES. As long as ONE in-serve residual fresh-open
//! writer can run concurrently with a Handle write (scenario G), the ledger can
//! tear. Only when every in-serve opener routes through the one shared Handle
//! does the hazard vanish. Scenarios A–F pin what is SAFE; G pins the hazard.

use aberp_audit_ledger::{
    append_in_tx, ensure_consistent_with_db, ensure_schema, mirror_path_for, Actor, BinaryHash,
    EventKind, LedgerMeta, TenantId,
};
use duckdb::Connection;

const BH: BinaryHash = BinaryHash::from_bytes([7u8; 32]);
fn tid() -> TenantId {
    TenantId::new("tear").expect("tid")
}

/// One audit append through the shared Handle. The `WriteGuard` drop runs the
/// lockstep `sync_mirror`, so after this returns the mirror head is one ahead.
fn handle_append(h: &aberp_db::Handle, tag: &str) {
    let mut g = h.write().expect("write guard");
    ensure_schema(&g).expect("ensure schema on guard");
    let tx = g.transaction().expect("tx");
    append_in_tx(
        &tx,
        &LedgerMeta::new(tid(), BH),
        EventKind::Test,
        format!("{{\"t\":\"{tag}\"}}").into_bytes(),
        Actor::from_local_cli("s".into(), "u"),
        None,
    )
    .expect("append");
    tx.commit().expect("commit");
}

fn tmp_db() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aberp-h3-tear-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.join("t.duckdb")
}

/// Boot the shared Handle on a fresh DB (serve-boot posture: ensure the audit
/// schema on a throwaway conn first, exactly as serve.rs does before the Handle
/// opens), run ONE mirrored Handle write, then let `residual` run while the
/// Handle is live, then shut down cleanly and run the real boot reconcile.
/// Returns `Ok(())` if the ledger survived, `Err(reason)` if it tore.
fn run(residual: impl FnOnce(&std::path::Path, &aberp_db::Handle)) -> Result<(), String> {
    let db = tmp_db();
    {
        let c = Connection::open(&db).expect("boot-ensure conn");
        ensure_schema(&c).expect("boot-ensure schema");
    }
    let handle = aberp_db::Handle::open_default(&db, tid()).expect("open shared Handle");
    handle_append(&handle, "handle-write"); // the mirrored victim (seq 1)
    residual(&db, &handle);
    drop(handle); // clean shutdown — checkpoint disabled, WAL preserved

    let c = Connection::open(&db).expect("reboot conn");
    let out = ensure_consistent_with_db(&c, &mirror_path_for(&db))
        .map(|_| ())
        .map_err(|e| format!("{e:?}"));
    let _ = std::fs::remove_dir_all(db.parent().unwrap());
    out
}

// ── SAFE shapes — these must NOT tear ────────────────────────────────────────

#[test]
fn a_no_residual_opener_is_clean() {
    assert!(
        run(|_, _| {}).is_ok(),
        "control: a Handle write + clean shutdown, with NO residual opener, must survive"
    );
}

#[test]
fn c_fresh_open_that_commits_after_the_write_is_clean() {
    // The control contrast to G: the SAME committing fresh open is safe when it
    // snapshots AFTER the mirrored write — its checkpoint folds the mirrored row
    // into main rather than truncating it away. Snapshot timing is the whole
    // difference between safe (C) and torn (G).
    assert!(
        run(|db, _| {
            let c = Connection::open(db).expect("residual open");
            c.execute_batch("CREATE TABLE IF NOT EXISTS junk(x INT); INSERT INTO junk VALUES(1);")
                .expect("residual write");
        })
        .is_ok(),
        "a fresh open that snapshots AFTER the Handle write must not tear"
    );
}

// ── THE HAZARD — this DOES tear on the current tree ──────────────────────────

#[test]
fn g_spanning_residual_writer_tears_the_ledger() {
    // The reproduced corruption: a residual fresh `Connection::open` whose
    // snapshot PREDATES a Handle write, that then COMMITS ITS OWN write, folds
    // the WAL back to its stale snapshot and drops the Handle's mirrored tail.
    let result = run(|db, h| {
        let residual = Connection::open(db).expect("residual open (snapshot pre-write)");
        handle_append(h, "victim"); // mirrored (seq 2); still only in the WAL
        residual
            .execute_batch("CREATE TABLE IF NOT EXISTS junk(x INT); INSERT INTO junk VALUES(1);")
            .expect("residual commit truncates the WAL");
        drop(residual);
    });
    let err = result.expect_err(
        "REGRESSION-OR-FIX: a spanning residual writer no longer tears the ledger. \
         If the Handle machinery changed to make this safe, update this test; if an \
         in-serve opener was migrated, that is progress but this synthetic residual \
         should still demonstrate the raw DuckDB WAL-fold hazard.",
    );
    assert!(
        err.contains("MirrorAheadOfDb"),
        "expected MirrorAheadOfDb (mirror head past DB head), got: {err}"
    );
}
