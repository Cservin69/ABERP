//! ADR-0099 F-E / ADR-0110 D9 — `rebuild-stock-cache` must REFUSE while the
//! whole-DB writer flock is held.
//!
//! The mirror of
//! `apps/aberp/tests/export_invoice_bundle_smoke.rs::run_refuses_while_the_whole_db_writer_lock_is_held`
//! and `snapshot_e2e.rs::snapshot_now_refuses_…`, for the one DB-mutating
//! one-shot that lives OUTSIDE `apps/aberp`.
//!
//! Why this one is the acute case. `rebuild-stock-cache` is a **documented**
//! recovery path — ADR-0061 §3 tells the operator, in those words, that when the
//! cache and the ledger disagree "the recovery is `cargo run --
//! rebuild-stock-cache`". So it is run on a live shop, with `aberp serve` up
//! holding the tenant's `aberp_db::Handle` and its WAL. It opened the tenant DB
//! with a bare `duckdb::Connection::open` — DuckDB DEFAULT pragmas — and a
//! default-pragma close CHECKPOINTS and TRUNCATES that WAL. Every commit serve
//! made since the last checkpoint (invoices, movements, audit-chain appends)
//! would be folded away while `commit()` kept returning `Ok`: the ADR-0110 D7
//! write-loss primitive, armed by the recovery tool.
//!
//! It is driven here as the REAL BINARY in a SEPARATE process (the flock is a
//! cross-process primitive; an in-process call could not prove it), against a
//! DB whose cache is deliberately corrupt.
//!
//! # The mutation tooth
//!
//! One corruption, two runs, opposite outcomes. Delete the `acquire_or_refuse`
//! from `src/bin/rebuild_stock_cache.rs` and the LOCKED arm's process exits 0
//! and rewrites the cache → red on both its assertions. Weaken the lock the
//! other way (leave the flock but break the rebuild) and the FREE arm goes red.
//! Neither half can pass by accident, and the test cannot go green by vacuum:
//! the free run proves the refusal is the lock talking and not a broken binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use duckdb::Connection;

const TENANT: &str = "ten_test_rebuild_flock";
const CORRUPT_QTY: &str = "999999.999999";
/// `SUM(qty_delta)` of the movements seeded below — what a correct rebuild
/// must write over `CORRUPT_QTY`.
const TRUE_QTY: &str = "7.000000";

/// Mirror of `apps/aberp/src/products.rs::PRODUCTS_SCHEMA_SQL`, as
/// `repository_round_trip.rs` carries it — the inventory migration ALTERs this
/// table, so it has to exist first.
const PRODUCTS_SCHEMA_FOR_TESTS: &str = "
CREATE TABLE IF NOT EXISTS products (
    id               VARCHAR NOT NULL PRIMARY KEY,
    tenant_id        VARCHAR NOT NULL,
    name             VARCHAR NOT NULL,
    unit_kind        VARCHAR NOT NULL,
    unit_value       VARCHAR NOT NULL,
    currency         VARCHAR NOT NULL,
    unit_price_minor BIGINT  NOT NULL,
    created_at       VARCHAR NOT NULL,
    updated_at       VARCHAR NOT NULL,
    deleted_at       VARCHAR
);
";

fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-rebuild-flock-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tenant DB with one product whose ledger says 7 but whose cache says
/// 999999.999999 — the exact "cache drifted from the ledger" state ADR-0061 §3
/// sends the operator to this binary for.
fn seed_drifted_db(db: &Path) {
    let conn = Connection::open(db).expect("open seed connection");
    conn.execute_batch(PRODUCTS_SCHEMA_FOR_TESTS)
        .expect("products schema");
    aberp_inventory::ensure_schema(&conn).expect("inventory schema");
    conn.execute(
        "INSERT INTO products (id, tenant_id, name, unit_kind, unit_value, currency,
                               unit_price_minor, created_at, updated_at, deleted_at,
                               stock_qty, min_stock)
         VALUES ('prd_flock', ?, 'Flock probe', 'Piece', '1', 'HUF',
                 100, '2026-08-13T00:00:00Z', '2026-08-13T00:00:00Z', NULL,
                 CAST(? AS DECIMAL(18,6)), 0);",
        duckdb::params![TENANT, CORRUPT_QTY],
    )
    .expect("insert drifted product");
    for (id, delta, at) in [
        ("mvt_a", "10", "2026-08-13T01:00:00Z"),
        ("mvt_b", "-3", "2026-08-13T02:00:00Z"),
    ] {
        conn.execute(
            "INSERT INTO stock_movements (movement_id, tenant_id, product_id, qty_delta,
                                          reason, ref_kind, ref_id, at_iso8601, operator,
                                          idempotency_key, notes)
             VALUES (?, ?, 'prd_flock', CAST(? AS DECIMAL(18,6)), 'Adjustment', 'Manual',
                     NULL, ?, 'tester', ?, NULL);",
            duckdb::params![id, TENANT, delta, at, id],
        )
        .expect("insert movement");
    }
    // Close before the binary runs — this seeder is a plain writer, not the
    // stand-in for serve; the flock, not this connection, is what the LOCKED
    // arm contends against.
    drop(conn);
}

fn cached_qty(db: &Path) -> String {
    let conn = Connection::open(db).expect("open reader");
    conn.query_row(
        "SELECT CAST(stock_qty AS VARCHAR) FROM products WHERE id = 'prd_flock';",
        [],
        |r| r.get::<_, String>(0),
    )
    .expect("read cached stock_qty")
}

/// Run the real `rebuild-stock-cache` binary against `db` as a separate OS
/// process. Returns `(success, stderr)`.
fn run_binary(db: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rebuild-stock-cache"))
        .args(["--tenant", TENANT, "--db"])
        .arg(db)
        .output()
        .expect("spawn rebuild-stock-cache");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn rebuild_stock_cache_refuses_while_the_whole_db_writer_lock_is_held() {
    let dir = scratch_dir("refuse");
    let db = dir.join("tenant.duckdb");
    seed_drifted_db(&db);
    assert_eq!(
        cached_qty(&db),
        CORRUPT_QTY,
        "precondition: the seeded cache is drifted"
    );

    // ── LOCKED arm — stand-in for a running `aberp serve` holding the tenant.
    {
        let _held = aberp_db::db_writer_lock::try_acquire(&db, TENANT)
            .expect("acquire ok")
            .expect("stand-in serve must get the lock");

        let (ok, stderr) = run_binary(&db);
        assert!(
            !ok,
            "rebuild-stock-cache MUST refuse while the whole-DB writer lock is held — \
             it opens the tenant DB with DuckDB DEFAULT pragmas, so its close folds and \
             truncates the live serve Handle's WAL (ADR-0110 D7's write-loss primitive). \
             stderr: {stderr}"
        );
        assert!(
            stderr.contains("single-writer") || stderr.contains("already running"),
            "the refusal must cite the single-writer rule so the operator knows to stop \
             serve rather than retry blindly: {stderr}"
        );
        assert_eq!(
            cached_qty(&db),
            CORRUPT_QTY,
            "a refused rebuild must not have touched the cache — refusing AFTER opening \
             the DB would already have armed the fold on close"
        );
    }

    // ── FREE arm — the same command, the same DB, lock released. This is what
    // stops the assertions above from passing on a binary that is simply
    // broken: with no contender it must do its ADR-0061 §3 job.
    let (ok, stderr) = run_binary(&db);
    assert!(
        ok,
        "with the lock free, rebuild-stock-cache must still perform its recovery: {stderr}"
    );
    assert_eq!(
        cached_qty(&db),
        TRUE_QTY,
        "the free run must re-derive stock_qty from SUM(qty_delta) (10 + -3)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
