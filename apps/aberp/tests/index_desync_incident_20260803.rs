//! Prod incident 2026-08-03 — "mark AP invoice paid" fatally invalidated the
//! live DuckDB instance.
//!
//! The fixture is a distilled copy of the real incident database
//! (`~/.aberp/prod/aberp.duckdb`, sha256 `f59c1883…`): every table except
//! `ap_invoice` dropped, the 96 healthy rows deleted, leaving the 10 rows whose
//! entries are MISSING from the two non-constraint ART indexes
//! (`ap_invoice_tenant_status_idx`, `ap_invoice_tenant_issue_idx`). It is
//! stored zstd-compressed because `*.duckdb` is gitignored — and because a
//! poisoned ART cannot be reconstructed by any sequence of SQL, so it has to be
//! carried as bytes.
//!
//! What the two arms pin:
//!
//!   * `fixture_is_still_poisoned` — RED-if-the-fixture-goes-stale. The exact
//!     prod statement must still fail on the untouched fixture. If this ever
//!     passes, the fixture has lost its teeth and the green arm below proves
//!     nothing.
//!   * `rebuild_secondary_indexes_cures_the_poisoned_database` — the fix. After
//!     `aberp_db::index_integrity::rebuild_secondary_indexes`, the same
//!     statement succeeds and the row is `Paid`, with no row lost.
//!
//! There is deliberately no "boot probe detects it" arm: measured on this
//! fixture, a rolled-back `UPDATE`/`DELETE` reports CLEAN on a poisoned DB
//! (the fault fires inside `Commit`, which a rollback never reaches), so such a
//! probe would be a test that cannot fail. See `aberp_db::index_integrity`.

use std::path::{Path, PathBuf};

use duckdb::Connection;

/// The row Ervin tried to mark paid when prod went down.
const POISONED_ROW_ID: &str = "apinv_01KYSF5RYMSYJZ6Q56QCVR8P5P";

/// The literal statement that invalidated the prod instance.
const THE_FATAL_STATEMENT: &str =
    "UPDATE ap_invoice SET local_status='Paid' WHERE id='apinv_01KYSF5RYMSYJZ6Q56QCVR8P5P';";

#[test]
fn fixture_is_still_poisoned() {
    let db = unpack_fixture("poisoned");
    let conn = Connection::open(&db).expect("open the incident fixture");

    let err = conn
        .execute_batch(THE_FATAL_STATEMENT)
        .expect_err("the incident fixture must STILL reproduce the prod failure");
    let msg = err.to_string();
    assert!(
        msg.contains("delete all rows from index") || msg.contains("duplicate key"),
        "expected the ART index-maintenance failure, got: {msg}"
    );
}

#[test]
fn rebuild_secondary_indexes_cures_the_poisoned_database() {
    let db = unpack_fixture("cured");
    let conn = Connection::open(&db).expect("open the incident fixture");

    let before: i64 = conn
        .query_row("SELECT count(*) FROM ap_invoice", [], |r| r.get(0))
        .expect("count rows before");

    let rebuilt = aberp_db::index_integrity::rebuild_secondary_indexes(&conn)
        .expect("rebuild the secondary ART indexes");
    let mut names = rebuilt.clone();
    names.sort();
    assert_eq!(
        names,
        vec![
            "ap_invoice_tenant_issue_idx".to_string(),
            "ap_invoice_tenant_status_idx".to_string(),
        ],
        "both ap_invoice secondaries must be rebuilt"
    );

    // The statement that killed prod now lands.
    conn.execute_batch(THE_FATAL_STATEMENT)
        .expect("after the rebuild the prod statement must succeed");

    let status: String = conn
        .query_row(
            "SELECT local_status FROM ap_invoice WHERE id = ?",
            [POISONED_ROW_ID],
            |r| r.get(0),
        )
        .expect("read the row back");
    assert_eq!(status, "Paid");

    let after: i64 = conn
        .query_row("SELECT count(*) FROM ap_invoice", [], |r| r.get(0))
        .expect("count rows after");
    assert_eq!(after, before, "the rebuild must not lose a row");

    // And a DELETE — the other half of the index-maintenance path, and the
    // shape that reproduced the original error verbatim — is writable too.
    conn.execute_batch(&format!(
        "DELETE FROM ap_invoice WHERE id='{POISONED_ROW_ID}';"
    ))
    .expect("after the rebuild an indexed DELETE must succeed");
}

/// The `serve.rs` source, embedded at test-compile time (a change to it
/// recompiles this pin). Same structural-pin technique as
/// `boot_order_switch_hint_before_flock_and_handle.rs`: `run()` binds a
/// loopback TLS listener, reads the OS keychain and spawns daemons, so its boot
/// order cannot be exercised end-to-end in a test.
const SERVE_RS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs"));

/// The repair is only worth anything if it actually runs at boot, in the right
/// place: AFTER the mirror reconcile/heal (so a healed DB is covered too) and
/// BEFORE the shared Handle opens (so the app never touches a poisoned ART).
/// Deleting the call, or moving it past `open_tenant_handle`, turns this red.
#[test]
fn boot_rebuilds_the_indexes_after_the_heal_and_before_the_handle_opens() {
    let src = SERVE_RS;
    let run_start = src
        .find("pub fn run(args: &ServeArgs) -> Result<()> {")
        .expect("locate the run() boot fn in serve.rs");
    let find_after = |needle: &str| {
        src[run_start..]
            .find(needle)
            .map(|i| run_start + i)
            .unwrap_or_else(|| panic!("run() must contain {needle:?}"))
    };

    let reconcile = find_after("ensure_consistent_with_db(");
    let rebuild = find_after("index_integrity::rebuild_secondary_indexes(");
    let handle = find_after("open_tenant_handle(");

    assert!(
        reconcile < rebuild,
        "the index rebuild must run AFTER the audit-mirror reconcile/heal — a heal that \
         lands rows must be covered by the rebuild that follows it"
    );
    assert!(
        rebuild < handle,
        "BOOT-ORDER REGRESSION: the secondary-index rebuild MUST run BEFORE the shared \
         aberp_db::Handle opens. Past that point the app can issue the UPDATE that \
         invalidated prod on 2026-08-03."
    );
}

/// Decompress the fixture into a fresh per-test path. Each test gets its OWN
/// copy: the poisoned arm leaves DuckDB's in-process instance for that path
/// invalidated, and DuckDB shares one instance per path per process.
fn unpack_fixture(label: &str) -> PathBuf {
    let dir = tempdir_for_test(label);
    let dest = dir.join("aberp.duckdb");
    let packed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ap_invoice_index_desync_20260803.duckdb.zst");
    let bytes =
        std::fs::read(&packed).unwrap_or_else(|e| panic!("read fixture {}: {e}", packed.display()));
    let plain = zstd::decode_all(bytes.as_slice()).expect("zstd-decode the incident fixture");
    std::fs::write(&dest, plain).expect("write the unpacked fixture");
    dest
}

/// Per-test tempdir under the system temp root, matching the PID + nanos +
/// counter scheme the sibling tests under `apps/aberp/tests/` use.
fn tempdir_for_test(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aberp_index_desync_{label}_{}_{}_{}",
        std::process::id(),
        nanos,
        seq,
    ));
    std::fs::create_dir_all(&path).expect("create tempdir");
    path
}
