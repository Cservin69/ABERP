//! ADR-0099 H3 — the shared-Handle audit coherence MODEL, pinned as assertions so
//! a future change to aberp-db / DuckDB that shifts it trips a test instead of a
//! production ledger. These three facts drive the ENTIRE migration ordering:
//!
//!   Q1  The Handle SEES a separate connection's write made BEFORE it opened.
//!   Q2  The Handle is BLIND to a separate connection's commit made AFTER it
//!       opened (DuckDB keeps no shared buffer cache across `Connection::open`
//!       instances — see crates/audit-ledger s335 note). This is why a reader
//!       CANNOT migrate to a Handle read before its writers do: it would read a
//!       STALE ledger and silently drop live rows.
//!   Q3  A FRESH `Ledger::open` sees everything committed on disk (pre + the
//!       separate post-open write + the Handle's own write) — WHEN no interleaved
//!       separate open has torn the Handle's WAL (the torn case is the wave-2e
//!       machine_crud hazard, covered by s335_persistent_connection_forks_chain).
//!
//! Corollary (the migration invariant): an audit event FAMILY must be ENTIRELY on
//! the Handle (writers + readers) or ENTIRELY on fresh opens — never mixed. A
//! reader migrates in the SAME atomic commit as its family's writers.

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, Actor, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId,
};
use duckdb::Connection;

fn tid() -> TenantId {
    TenantId::new("coherence".to_string()).unwrap()
}
const BH: BinaryHash = BinaryHash::from_bytes([9u8; 32]);

fn separate_conn_append(path: &std::path::Path, tag: &str) {
    let mut c = Connection::open(path).unwrap();
    ensure_schema(&c).unwrap();
    let tx = c.transaction().unwrap();
    let meta = LedgerMeta::new(tid(), BH);
    let actor = Actor::from_local_cli("s".into(), "u");
    append_in_tx(
        &tx,
        &meta,
        EventKind::Test,
        format!("{{\"t\":\"{tag}\"}}").into_bytes(),
        actor,
        None,
    )
    .unwrap();
    tx.commit().unwrap();
}

fn count_via_handle(h: &aberp_db::Handle) -> usize {
    let c = h.read().unwrap();
    Ledger::from_connection(c, tid(), BH)
        .entries()
        .unwrap()
        .len()
}

#[test]
fn handle_is_blind_to_separate_post_open_commits_but_fresh_open_sees_all() {
    let dir = std::env::temp_dir().join(format!("aberp-h3-coherence-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.duckdb");

    separate_conn_append(&db, "pre"); // written BEFORE the Handle opens
    let handle = aberp_db::Handle::open_default(&db, tid()).unwrap();

    // Q1 — the Handle sees the pre-open separate write.
    assert_eq!(
        count_via_handle(&handle),
        1,
        "Q1: the Handle must see a separate write made before it opened"
    );

    separate_conn_append(&db, "post"); // a separate conn commits AFTER the Handle is open

    // Q2 — the Handle does NOT see it. THE ordering-critical fact.
    assert_eq!(
        count_via_handle(&handle),
        1,
        "Q2: the Handle must be blind to a separate-conn post-open commit \
         (so a reader cannot migrate to a Handle read before its writers do)"
    );

    // The Handle writes its own row.
    {
        let mut g = handle.write().unwrap();
        ensure_schema(&g).unwrap();
        let tx = g.transaction().unwrap();
        let meta = LedgerMeta::new(tid(), BH);
        let actor = Actor::from_local_cli("s".into(), "u");
        append_in_tx(
            &tx,
            &meta,
            EventKind::Test,
            b"{\"t\":\"handle\"}".to_vec(),
            actor,
            None,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Q3 — a fresh open sees ALL THREE on disk; the Handle sees only its own + pre.
    let fresh = Ledger::open(&db, tid(), BH)
        .unwrap()
        .entries()
        .unwrap()
        .len();
    assert_eq!(
        fresh, 3,
        "Q3: a fresh open sees every committed row on disk"
    );
    assert_eq!(
        count_via_handle(&handle),
        2,
        "Q3: the Handle sees pre-open + its own write, still blind to the separate post-open one"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
