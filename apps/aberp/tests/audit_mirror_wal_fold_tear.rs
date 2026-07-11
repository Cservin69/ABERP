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
//! WHAT IT PROVES NOW (ADR-audit-armor). The raw DuckDB WAL-fold hazard is real
//! and unchanged — a residual fresh-open writer that snapshots before a Handle
//! write and commits its own write still drops the mirrored tail from the DB
//! (scenario G). What changed is the boot oracle: rather than migrate every
//! opener and refuse on a tear, the audit ledger is ARMORED — the gated auto-heal
//! (`ensure_consistent_with_db`) replays the mirror's provable-loss tail back
//! into the DB, so the tear self-heals at the next boot. Scenarios A/C pin what
//! folds safely; G now pins that the tear HEALS (was: refuses) with db == mirror.

use aberp_audit_ledger::{
    append_in_tx, ensure_consistent_with_db, ensure_schema, mirror_path_for, read_mirror_entries,
    Actor, BinaryHash, EventKind, LedgerMeta, RecoveryAction, TenantId,
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

// ── THE HAZARD — now a SELF-HEALING non-event (ADR-audit-armor) ──────────────

#[test]
fn g_spanning_residual_writer_heals_the_ledger() {
    // FLIPPED by ADR-audit-armor (was: assert tear/refuse). The raw DuckDB
    // WAL-fold hazard is unchanged — a residual fresh `Connection::open` whose
    // snapshot PREDATES a Handle write, that then COMMITS ITS OWN write, still
    // folds the WAL back and drops the Handle's mirrored tail from the DB, so
    // the mirror ends AHEAD. What changed is the boot oracle: instead of
    // refusing (`MirrorAheadOfDb`), the gated auto-heal PROVES this a benign
    // loss (boundary agrees, tail verifies, in-tx full-genesis re-verify passes)
    // and REPLAYS the lost tail into the DB, so boot CONTINUES with db == mirror.
    let db = tmp_db();
    {
        let c = Connection::open(&db).expect("boot-ensure conn");
        ensure_schema(&c).expect("boot-ensure schema");
    }
    let handle = aberp_db::Handle::open_default(&db, tid()).expect("open shared Handle");
    handle_append(&handle, "handle-write"); // the mirrored anchor (seq 1)
    {
        let residual = Connection::open(&db).expect("residual open (snapshot pre-write)");
        handle_append(&handle, "victim"); // mirrored (seq 2); still only in the WAL
        residual
            .execute_batch("CREATE TABLE IF NOT EXISTS junk(x INT); INSERT INTO junk VALUES(1);")
            .expect("residual commit truncates the WAL");
        drop(residual);
    }
    drop(handle); // clean shutdown — checkpoint disabled, WAL preserved

    let mirror = mirror_path_for(&db);
    // Pre-boot the raw hazard is intact: the mirror kept the lost seq-2 tail.
    assert_eq!(
        read_mirror_entries(&mirror).unwrap().len(),
        2,
        "mirror still holds the WAL-folded tail (the raw hazard is unchanged)"
    );

    // Boot reconcile — must HEAL, not refuse.
    let c = Connection::open(&db).expect("reboot conn");
    let action = ensure_consistent_with_db(&c, &mirror).expect(
        "REGRESSION: the WAL-fold tear must now AUTO-HEAL, not refuse. If the heal was \
         removed or gated off, this reverts to MirrorAheadOfDb — restore the armor.",
    );
    assert!(
        matches!(
            action,
            RecoveryAction::Healed {
                entries_replayed: 1
            }
        ),
        "expected Healed{{entries_replayed:1}} (the lost seq-2 row replayed), got {action:?}"
    );
    drop(c);

    // db == mirror proof: a SECOND boot reconcile is Unchanged — which by its own
    // definition requires equal length + matching head hash — and it does not
    // loop into a re-heal.
    let c2 = Connection::open(&db).expect("second reboot conn");
    let again =
        ensure_consistent_with_db(&c2, &mirror).expect("second boot after a heal must succeed");
    assert_eq!(
        again,
        RecoveryAction::Unchanged,
        "the healed state is stable (db == mirror) — no re-heal loop"
    );
    drop(c2);

    // The healed mirror = [seq1, seq2 replayed, db.auto_recovered forensic row].
    let healed = read_mirror_entries(&mirror).unwrap();
    assert_eq!(healed.len(), 3, "seq1 + replayed seq2 + forensic row");
    assert_eq!(
        healed[2].kind, "db.auto_recovered",
        "the heal emits a db.auto_recovered forensic row"
    );

    let _ = std::fs::remove_dir_all(db.parent().unwrap());
}
