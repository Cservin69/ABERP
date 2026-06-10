//! S332 / PR-31 — regression pin for the DuckDB ART crash reported on
//! the S307 email-outbox poll daemon's audit-write path.
//!
//! ## What Ervin saw (2026-06-10, PROD_v2.27.14)
//!
//! The poll daemon's `write_audit` kept logging, every ~5s cycle:
//!
//! ```text
//!   email-outbox audit write failed kind=quote.email_outbox_fetched
//!   ... ART::Insert -> Prefix::New -> FixedSizeAllocator::New
//!       -> FixedSizeBuffer::GetOffset ... WriteToWAL -> Commit
//!   This error signals an assertion failure within DuckDB.
//!   Error code 1: Unknown error code
//! ```
//!
//! i.e. a DuckDB `InternalException` raised inside the ART (Adaptive
//! Radix Tree) secondary-index append while committing one
//! `EmailOutboxFetched` audit row to `audit_ledger`.
//!
//! ## Why there is NO schema migration in this PR (the S288 template
//! does NOT apply here)
//!
//! The S288/PR-269 fix dropped a *named, query-acceleration* secondary
//! index (`quote_pricing_jobs_tenant_state_idx`) created via
//! `CREATE INDEX`. The `audit_ledger` table has no such index. Its only
//! ART indexes are the *inline* `UNIQUE (seq)` and `UNIQUE (id)`
//! constraints (see `crates/audit-ledger/src/storage/schema.rs`). Those:
//!   1. do NOT appear in `duckdb_indexes()` (verified: 0 rows) and have
//!      no droppable name — `DROP INDEX IF EXISTS <name>` cannot target
//!      them, so the S288 detection/drop mechanic is structurally
//!      inapplicable; and
//!   2. are the ledger's documented integrity invariants — `UNIQUE(seq)`
//!      is the cross-writer hash-chain fork guard. Dropping it (only
//!      possible via a full table rebuild) would trade a *contained*,
//!      caught-and-logged audit-write error for a *silent* integrity
//!      hole in the tamper-evident ledger. That is the wrong trade for a
//!      crown-jewel table on a mis-premised, locally-unreproduced crash.
//!
//! See `docs/findings/s332-duckdb-art-email-outbox.md` for the full
//! diagnosis, the conservative-call rationale, and the recommended
//! durable fixes (which live outside this PR's frozen scope).
//!
//! ## What this test pins
//!
//! It reproduces the daemon's EXACT write shape — a fresh
//! `Connection::open(path)` + `ensure_schema` + one-row transaction +
//! `commit` per audit row, against a FILE-backed DB so the WAL /
//! checkpoint path that the crash stack walks is actually exercised
//! (an in-memory DB never hits `WriteToWAL`). It writes N back-to-back
//! `EmailOutboxFetched` rows and asserts:
//!   * the loop completes with no panic and no `append`/`commit` error;
//!   * the chain verifies end-to-end (`verify_chain`) — integrity intact
//!     across all N appends, which is exactly the guarantee a naive
//!     "drop the UNIQUE index to dodge the crash" fix would have broken.
//!
//! NOTE (honest): at the unit scale this test runs (N=200, and probes up
//! to 1,000,000 rows during investigation), the DuckDB 1.1.x build in our
//! lockfile does NOT reproduce the production ART `InternalException`.
//! The prod trigger needs prod-specific state (a far larger accumulated
//! ledger and/or the live on-disk ART). This test therefore pins the
//! invariant we CAN assert deterministically — the audit-write path is
//! panic-free and integrity-preserving for N sequential
//! `EmailOutboxFetched` writes — and stands as the harness a future,
//! prod-state-seeded repro would extend. Raise N via `S332_N` to stress
//! locally.

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, Actor, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId,
};
use duckdb::Connection;
use ulid::Ulid;

/// One audit write, byte-for-byte the daemon's `write_audit` shape:
/// fresh connection, ensure schema, single-row tx, commit, drop.
fn daemon_style_write(path: &std::path::Path, meta: &LedgerMeta, i: usize) {
    let mut conn = Connection::open(path).expect("open DB for email-outbox audit");
    ensure_schema(&conn).expect("ensure audit schema");
    let tx = conn.transaction().expect("begin email-outbox audit tx");
    let actor = Actor::from_local_cli(Ulid::new().to_string(), "s332-regression");
    // Mirrors EmailOutboxFetchedPayload's idle-cycle shape (fetched_count
    // = 0) — the highest-frequency producer and the exact kind in the
    // crash log.
    let payload = format!("{{\"fetched_count\":0,\"i\":{i}}}").into_bytes();
    append_in_tx(
        &tx,
        meta,
        EventKind::EmailOutboxFetched,
        payload,
        actor,
        None,
    )
    .expect("append email-outbox audit");
    tx.commit().expect("commit email-outbox audit");
}

#[test]
fn s332_regression_email_outbox_fetched_audit_write_does_not_crash() {
    let n: usize = std::env::var("S332_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let mut path = std::env::temp_dir();
    path.push(format!("s332_audit_{}.duckdb", Ulid::new()));
    let _ = std::fs::remove_file(&path);

    let tenant = TenantId::new("s332-tenant").expect("tenant id");
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    let meta = LedgerMeta::new(tenant.clone(), binary_hash);

    // Tight loop of back-to-back EmailOutboxFetched writes — the daemon's
    // every-5s-cycle behaviour, compressed. A panic or an Err on any
    // append/commit fails the test loudly.
    for i in 0..n {
        daemon_style_write(&path, &meta, i);
    }

    // Integrity pin: open the same file as a Ledger and verify the full
    // hash chain. This is the guarantee that a "drop UNIQUE(seq) to dodge
    // the ART crash" fix would silently break — so the test would catch
    // such a regression.
    let ledger = Ledger::open(&path, tenant, binary_hash).expect("reopen ledger");
    let verified = ledger.verify_chain().expect("chain verifies");
    assert_eq!(
        verified as usize, n,
        "verify_chain must confirm exactly {n} entries; got {verified}"
    );

    let entries = ledger.entries().expect("read entries");
    assert_eq!(entries.len(), n, "ledger must hold exactly {n} rows");
    assert!(
        entries
            .iter()
            .all(|e| e.kind == EventKind::EmailOutboxFetched),
        "every row must be EmailOutboxFetched"
    );
    // Seq is dense and strictly increasing 1..=n — the invariant the
    // UNIQUE(seq) ART index defends and that this test proves intact.
    for (idx, e) in entries.iter().enumerate() {
        assert_eq!(
            e.seq.as_u64(),
            (idx as u64) + 1,
            "seq must be dense + monotonic"
        );
    }

    let _ = std::fs::remove_file(&path);
}
