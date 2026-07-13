//! ADR-0099 H3 Addendum 3 — SERVE_HANDLE_LIVE runtime tripwire.
//!
//! Proves the tripwire has TEETH: while a serve `Handle` is registered on a tenant
//! DB, an INDEPENDENT open of that file (`Ledger::open` / `DuckDbBillingStore::open`
//! — the audit-ledger fork primitive and the invoice store-shape) panics loudly;
//! and proves it does NOT over-fire — nothing registered, a different path, a
//! dropped guard, and the shared Handle's OWN reads/writes all pass clean.
//!
//! Debug/test only by construction (`assert_no_serve_handle` is `debug_assertions`-
//! gated); `cargo test` builds set `debug_assertions`, so these run.

use aberp_audit_ledger::serve_tripwire::{is_serve_handle_live, register_serve_handle};
use aberp_audit_ledger::{
    append_in_tx, ensure_schema, Actor, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId,
};
use aberp_billing::DuckDbBillingStore;
use std::path::PathBuf;
use ulid::Ulid;

fn tid() -> TenantId {
    TenantId::new("tripwire".to_string()).unwrap()
}
const BH: BinaryHash = BinaryHash::from_bytes([7u8; 32]);

/// A unique temp DB path per test — the registry is process-global, so distinct
/// paths keep parallel tests from cross-contaminating.
fn fresh_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aberp-tripwire-{tag}-{}.duckdb", Ulid::new()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
#[should_panic(expected = "SERVE_HANDLE_LIVE tripwire")]
fn ledger_open_trips_while_serve_handle_registered() {
    let db = fresh_db("ledger-trip");
    // Create the file first so this is a genuine independent RE-open, not a create.
    drop(Ledger::open(&db, tid(), BH).expect("seed ledger"));
    let _guard = register_serve_handle(&db);
    // The fresh Ledger::open must trip the tripwire BEFORE it forks the ledger.
    let _ = Ledger::open(&db, tid(), BH);
}

#[test]
#[should_panic(expected = "SERVE_HANDLE_LIVE tripwire")]
fn billing_store_open_trips_while_serve_handle_registered() {
    let db = fresh_db("store-trip");
    drop(DuckDbBillingStore::open(&db).expect("seed store"));
    let _guard = register_serve_handle(&db);
    // The invoice store-shape opener must trip while serve holds the Handle.
    let _ = DuckDbBillingStore::open(&db);
}

#[test]
fn ledger_open_does_not_trip_when_unregistered() {
    let db = fresh_db("ledger-clean");
    assert!(!is_serve_handle_live(&db));
    // No serve Handle registered → a fresh open is fine (the CLI-one-shot posture).
    let _l = Ledger::open(&db, tid(), BH).expect("open must succeed when unregistered");
}

#[test]
fn ledger_open_does_not_trip_on_a_different_path() {
    let live = fresh_db("live-path");
    let other = fresh_db("other-path");
    let _guard = register_serve_handle(&live);
    assert!(is_serve_handle_live(&live));
    assert!(!is_serve_handle_live(&other));
    // A different tenant DB is not the registered one — no fork, no trip.
    let _l = Ledger::open(&other, tid(), BH).expect("open on a different path must succeed");
}

#[test]
fn tripwire_clears_after_guard_drop() {
    let db = fresh_db("drop-clears");
    drop(Ledger::open(&db, tid(), BH).expect("seed ledger"));
    {
        let _guard = register_serve_handle(&db);
        assert!(is_serve_handle_live(&db));
    }
    // Guard dropped → serve is no longer live on the path → open is fine again.
    assert!(!is_serve_handle_live(&db));
    let _l = Ledger::open(&db, tid(), BH).expect("open must succeed after the guard drops");
}

#[test]
fn nested_registration_is_refcounted() {
    let db = fresh_db("refcount");
    let g1 = register_serve_handle(&db);
    let g2 = register_serve_handle(&db);
    assert!(is_serve_handle_live(&db));
    drop(g1);
    // Still live — g2 holds a second reference.
    assert!(is_serve_handle_live(&db));
    drop(g2);
    assert!(!is_serve_handle_live(&db));
}

#[test]
fn shared_handle_access_does_not_trip() {
    // The tripwire targets INDEPENDENT opens, not access through the shared Handle.
    // Open a real Handle (as serve does), register its path, then exercise the
    // Handle's own read + write+append — none of which call Ledger::open /
    // DuckDbBillingStore::open, so none trip.
    let db = fresh_db("handle-ok");
    let handle = aberp_db::Handle::open_default(&db, tid()).expect("open handle");
    let _guard = register_serve_handle(&db);
    assert!(is_serve_handle_live(&db));

    // A Handle read (rides the shared instance via try_clone, not Connection::open).
    let read_conn = handle.read().expect("handle read");
    drop(read_conn);

    // A Handle write + audit append on the SAME live instance — the migrated shape.
    let mut guard = handle.write().expect("handle write");
    ensure_schema(&guard).expect("ensure audit schema on the handle");
    let tx = guard.transaction().expect("begin tx on the handle");
    let meta = LedgerMeta::new(tid(), BH);
    append_in_tx(
        &tx,
        &meta,
        EventKind::DaemonShutdownCompleted,
        b"{}".to_vec(),
        Actor::from_local_cli(Ulid::new().to_string(), "tripwire-test"),
        None,
    )
    .expect("append through the handle must NOT trip the tripwire");
    tx.commit().expect("commit");
}

#[test]
fn handle_background_paths_do_not_trip_while_serve_handle_registered() {
    // ADR-0099 H3 — Task 1 (arm-tripwire prerequisite). `shared_handle_access_does_
    // not_trip` above proves read / write / append_in_tx are clean, but it does NOT
    // exercise the Handle's BACKGROUND maintenance paths: the lockstep
    // `WriteGuard::drop -> sync_mirror` and the debounced durable checkpoint
    // (`checkpoint_on_idle` + the guard-drop checkpoint branch). If EITHER fresh-
    // opened the registered live path (via Ledger::open / DuckDbBillingStore::open),
    // arming the tripwire by default (Task 4) would trip on serve's OWN machinery.
    // This proves it by EXERCISING those paths under an armed tripwire, not by
    // reading the source.
    //
    // The H3 default is `checkpoint_enabled=false`, which would make the checkpoint
    // branch a dead no-op we could not observe. We deliberately force it TRUE (and a
    // large coalescing window) so the checkpoint code path actually RUNS — reaching
    // `run_durable_checkpoint_locked`, the exact site the tripwire would fire on if
    // it fresh-opened. In H3 that fn is a fold-nothing stub (it opens nothing and
    // logs one error line), so forcing the flag on is safe for a test.
    let db = fresh_db("bg-paths");
    let cfg = aberp_db::HandleConfig {
        // Large window: the FIRST guard-drop checkpoints (last_checkpoint==None), the
        // second does not, leaving the file dirty so the later `checkpoint_on_idle`
        // reaches `run_durable_checkpoint_locked` too — both checkpoint entry points
        // exercised under the armed tripwire.
        min_checkpoint_interval: std::time::Duration::from_secs(3600),
        checkpoint_enabled: true,
        disable_implicit_close_checkpoint: true,
    };
    let handle = aberp_db::Handle::open(&db, tid(), cfg).expect("open handle");
    let _guard = register_serve_handle(&db);
    assert!(is_serve_handle_live(&db));

    let meta = LedgerMeta::new(tid(), BH);

    // Write #1: seed schema + one committed audit row. Guard drops at scope end ->
    // WriteGuard::drop runs sync_mirror (DB now has 1 row -> real mirror append) AND
    // the checkpoint branch (first drop -> run_durable_checkpoint_locked). Neither
    // may trip.
    {
        let mut w = handle.write().expect("handle write 1");
        ensure_schema(&w).expect("ensure audit schema");
        let tx = w.transaction().expect("begin tx1");
        append_in_tx(
            &tx,
            &meta,
            EventKind::DaemonShutdownCompleted,
            b"{}".to_vec(),
            Actor::from_local_cli(Ulid::new().to_string(), "bg-paths-test"),
            None,
        )
        .expect("append 1");
        tx.commit().expect("commit 1");
    }

    // Write #2: a second committed row. This guard-drop's sync_mirror now sees a
    // NON-empty mirror head -> exercises the head-hash verification branch of
    // sync_mirror (still no fresh open). This drop does NOT checkpoint (inside the
    // 1h window), so the file stays dirty for the idle hook below.
    {
        let mut w = handle.write().expect("handle write 2");
        let tx = w.transaction().expect("begin tx2");
        append_in_tx(
            &tx,
            &meta,
            EventKind::DaemonShutdownCompleted,
            b"{}".to_vec(),
            Actor::from_local_cli(Ulid::new().to_string(), "bg-paths-test"),
            None,
        )
        .expect("append 2");
        tx.commit().expect("commit 2");
    }

    // Idle-checkpoint hook while the tripwire is armed and the file is dirty ->
    // reaches run_durable_checkpoint_locked via the SECOND checkpoint entry point.
    handle.checkpoint_on_idle();

    // Reached here without a `SERVE_HANDLE_LIVE tripwire` panic: sync_mirror and the
    // debounced checkpoint both route through the shared instance / the mirror file,
    // never a fresh open of the registered live path. Arming the tripwire is safe.
    assert!(is_serve_handle_live(&db));
}
