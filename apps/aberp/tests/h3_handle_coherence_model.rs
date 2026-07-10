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

/// ADR-0099 H3 Addendum 3 — Task 4: the invoice-family migration's UNPROVEN seam.
///
/// The locked recipe replaces `pre_tx_setup`'s fresh `DuckDbBillingStore::open(
/// db_path)` (a fork: Q2-blind AND it trips the SERVE_HANDLE_LIVE tripwire) with
/// `DuckDbBillingStore::from_connection(guard.try_clone()?)`, so the billing
/// pre-tx setup — auto-commit DDL (`ensure_schema`) + DML (`create_series`) — runs
/// on a clone of the SHARED writer connection WHILE the write guard is held. The
/// audit tx then runs on the guard itself, and `allocate_in_tx` READS the series
/// the billing store just created to number the invoice. That read is coherent
/// ONLY IF a committed write on `guard.try_clone()` is visible to the guard.
///
/// This is the deliberate OPPOSITE of Q2: a separate `Connection::open` is BLIND
/// to a post-open commit (proved above), but a `try_clone` SHARES the one DuckDB
/// instance, so its committed writes MUST be visible. The recipe reasoned this
/// coherent (mirroring ap_sync wave-3a's post-cycle verify) but never tested it.
/// Pin it: a DuckDB / aberp-db change that breaks the seam trips HERE, not in the
/// live issuing path — the repo's most dangerous code. Also confirms the migrated
/// shape does NOT trip the re-entrancy tripwire (`try_clone` bypasses
/// `Handle::read/write`) and that `verify_chain` on a second `try_clone` passes
/// (no WAL fold/tear from the interleaved billing clone).
#[test]
fn billing_store_on_guard_try_clone_is_coherent_with_the_guards_own_tx() {
    use aberp_billing::{
        BillingStore, DuckDbBillingStore, InvoiceSeries, ResetPolicy, SeriesCode, SeriesId,
    };

    let dir = std::env::temp_dir().join(format!("aberp-h3-tryclone-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.duckdb");

    let handle = aberp_db::Handle::open_default(&db, tid()).unwrap();
    let mut guard = handle.write().unwrap();

    // ── billing pre-tx setup on a try_clone of the shared writer (the recipe) ──
    let code = SeriesCode::new("INV-TRYCLONE").unwrap();
    {
        let mut billing = DuckDbBillingStore::from_connection(
            guard
                .try_clone()
                .expect("try_clone the shared writer for billing pre-tx setup"),
        );
        // DDL auto-commit on the clone.
        billing
            .ensure_schema()
            .expect("ensure billing schema on the try_clone");
        // DML auto-commit on the clone.
        billing
            .create_series(&InvoiceSeries {
                id: SeriesId::new(),
                code: code.clone(),
                reset_policy: ResetPolicy::Never,
                fiscal_year: None,
                created_at: time::OffsetDateTime::now_utc(),
            })
            .expect("create_series on the try_clone");
        // `billing` (the clone) drops here — its auto-commits are durable on the
        // shared instance the guard also holds.
    }

    // Audit schema ensured on the GUARD (the recipe runs this after billing setup;
    // in-serve it stays on the shared writer, not a fresh open).
    ensure_schema(&guard).expect("ensure audit-ledger schema on the shared writer");

    // ── THE SEAM: the guard's OWN tx must SEE the series the clone committed ──
    // (mirrors run_single_tx → allocate_in_tx reading the series to number it).
    let tx = guard
        .transaction()
        .expect("begin audit tx on the shared writer");
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM invoice_series WHERE code = ?",
            duckdb::params![code.as_str()],
            |r| r.get(0),
        )
        .expect("read invoice_series via the guard's tx");
    assert_eq!(
        n, 1,
        "the guard's tx must SEE the series the billing store committed on \
         guard.try_clone() — a try_clone SHARES the DuckDB instance (UNLIKE a \
         separate Connection::open, which Q2 proves is blind). n=0 would mean the \
         recipe's billing pre-tx setup is still a fork and allocate_in_tx would \
         number the invoice off a phantom-absent series."
    );

    // Append an audit row on the SAME guard tx (as run_single_tx does) and commit.
    let meta = LedgerMeta::new(tid(), BH);
    let actor = Actor::from_local_cli("s".into(), "u");
    append_in_tx(
        &tx,
        &meta,
        EventKind::Test,
        b"{\"t\":\"tryclone\"}".to_vec(),
        actor,
        None,
    )
    .unwrap();
    tx.commit()
        .expect("commit the audit tx on the shared writer");

    // verify_chain on a SECOND try_clone (the recipe's post-commit verify; the
    // ap_sync wave-3a pattern) — proves no WAL fold/tear from the billing-clone
    // interleave.
    Ledger::from_connection(guard.try_clone().unwrap(), tid(), BH)
        .verify_chain()
        .expect("chain verify on a try_clone after the billing-clone interleave");

    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}
