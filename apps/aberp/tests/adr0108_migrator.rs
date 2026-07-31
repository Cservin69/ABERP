//! ADR-0108 Step 4 — the migrator's refusals and the reconciliation gate's
//! hard stops.
//!
//! **T-18 is the reason this file exists.** ADR-0108's blocker B1 is that a
//! signature-stripped ledger passes `verify_chain`, passes
//! `verify_chain_signed`, passes `PRAGMA integrity_check`, matches on head
//! hash, and returns `fully_anchored: true` — four green checks and a
//! reassuring flag on the most thoroughly gutted input. Only two `COUNT(*)`
//! equalities can see it. So this file runs the migrator in the **rejected**
//! mirror-shape mode and asserts the gate goes red on exactly those two, while
//! asserting the other checks stay green. A gate against B1 that has never been
//! shown to catch B1 is not a gate.
//!
//! Everything is `sqlite-engine`-gated: the migrator links both engines (it
//! reads DuckDB and writes SQLite), which is the one binary in the plan where
//! that is intended.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_to_sqlite::{migrate_ledger, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-migrator-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A DEV tenant DB with a **real, verifying** audit chain, plus the
/// tamper-evidence layer the gate turns on.
///
/// The entries are appended through the ledger's own API, so `entry_hash` /
/// `prev_hash` are genuine and `verify_chain` passes. The three session columns
/// are then stamped by direct UPDATE — which is legitimate precisely *because*
/// `compute_entry_hash` excludes them, and that exclusion is the whole reason
/// B1's strip is invisible to every hash check. Seeding it this way makes the
/// test's premise the same fact the gate defends against.
fn seed(dir: &Path, entries: u64, anchors: u64) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([9u8; 32]),
        )
        .unwrap();
        for i in 0..entries {
            ledger
                .append(
                    EventKind::DbAutoRecovered,
                    format!(r#"{{"n":{i}}}"#).into_bytes(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
        }
        // The mirror is the cross-check arm; the gate refuses without one.
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(&db))
            .unwrap();
    }
    // Stamp the session columns + the anchors. A fresh connection is fine here:
    // the ledger above is closed, and this is test setup, not a runtime path.
    let conn = duckdb::Connection::open(&db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger
            SET session_id = 'sess-1',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    for i in 1..=anchors {
        conn.execute(
            "INSERT INTO audit_ledger_anchors
               (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
                timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
             VALUES (?, ?, 'sess-1', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                     '2026-07-31T00:00:00Z')",
            duckdb::params![format!("anc-{i}"), TENANT, vec![7u8; 8]],
        )
        .unwrap();
    }
    conn.close().unwrap();
    db
}

fn sha256(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(std::fs::read(p).unwrap());
    hex::encode(h.finalize())
}
// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

/// The ledger crosses with its tamper-evidence intact, and the gate — a
/// separate invocation that re-derives every number from disk — passes.
#[test]
fn the_ledger_crosses_and_the_reconciliation_gate_passes() {
    let dir = scratch("happy");
    let db = seed(&dir, 6, 2);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let out = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.entries_carried, 6);
    assert_eq!(out.anchors_carried, 2);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "unexpected hard stops: {:?}",
        r.hard_stops
    );
    // The checks that actually matter are present, not merely "some checks ran".
    for want in [
        "non-NULL event_sig",
        "audit_ledger_anchors row count",
        "head entry_hash",
        "verify_chain",
        "structural link walk",
        "typeof(audit_ledger.entry_hash)",
        "typeof(audit_ledger.event_sig)",
        "typeof(audit_ledger_anchors.chain_head_hash_at_anchor)",
        "integrity_check",
        "mirror tail",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(want)),
            "the gate never ran a `{want}` check; it has {:?}",
            r.checks
        );
    }
}

// ---------------------------------------------------------------------------
// T-18 — the gate catches its own blocker
// ---------------------------------------------------------------------------

/// **T-18 (B1).** Run the migrator in the REJECTED mirror-shape mode and assert
/// the gate goes red on the two `COUNT(*)` equalities — **while** asserting
/// that the head hash, the row count, the structural link walk and
/// `integrity_check` all still pass.
///
/// The point of the test is that four green checks and one red one is the true
/// picture. Remove the two count comparisons from `reconcile` and this whole
/// gutted carry reports PASS.
///
/// The rejected mode is simulated from the table (nulling the three session
/// columns, carrying no anchors) rather than decoded from the mirror file,
/// because the mirror's decode path (`MirrorEntry::to_entry`) is `pub(crate)`.
/// That makes the test *stronger*, not weaker: it isolates the strip itself
/// from any question about the mirror's on-disk format. The mirror's own
/// divergence arm is pinned separately below.
#[test]
fn t18_the_gate_hard_stops_on_a_signature_stripped_carry() {
    let dir = scratch("t18");
    let db = seed(&dir, 5, 3);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let out = migrate_ledger(
        &db,
        &lite,
        TENANT,
        &snap,
        LedgerSource::RejectedMirrorReplay,
    )
    .expect("the rejected mode still produces a file — that is the hazard");
    assert_eq!(out.entries_carried, 5, "every entry crossed");
    assert_eq!(out.anchors_carried, 0, "the mirror never held the anchors");

    let r = reconcile(&db, &lite, TENANT).expect("the gate runs");

    // The two hard stops, named.
    assert!(
        r.hard_stops.iter().any(|s| s.contains("event_sig")),
        "the non-NULL event_sig equality must hard-stop: {:?}",
        r.hard_stops
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("audit_ledger_anchors")),
        "the anchor-count equality must hard-stop: {:?}",
        r.hard_stops
    );

    // And everything else is GREEN. This half is the test.
    for still_green in [
        "audit_ledger row count",
        "head entry_hash",
        "head seq",
        "verify_chain",
        "structural link walk",
        "integrity_check",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(still_green)),
            "`{still_green}` should still PASS on a gutted ledger — that is exactly why the \
             counts are the gate. checks: {:?}",
            r.checks
        );
        assert!(
            !r.hard_stops.iter().any(|s| s.contains(still_green)),
            "`{still_green}` was expected green: {:?}",
            r.hard_stops
        );
    }
    // S449: FOUR, not two. The mirror-replay strip nulls `session_id` and
    // `session_pubkey` alongside `event_sig`, and those two now carry their own
    // equalities — see `s449_the_gate_hard_stops_when_only_the_session_pubkey_
    // is_stripped` for why an `event_sig`-only gate was not enough.
    assert_eq!(
        r.hard_stops.len(),
        4,
        "exactly the four tamper-evidence counts should have fired: {:?}",
        r.hard_stops
    );
}

/// B1's non-zero precondition, at the gate. An equality between two zeros is
/// not a check: a DuckDB side with no signatures and no anchors must hard-stop
/// rather than report a green `0 == 0` forever.
///
/// **S449 — this test was vacuous and is rewritten.** As landed it stripped the
/// signatures BEFORE taking the snapshot and then wrapped its only assertion in
/// `if lite.exists()`. The migrator refused (the snapshot no longer verified
/// against its own baseline), so `aberp.sqlite` was never created, so the
/// assertion never ran — measured, not inferred: deleting BOTH `duck_signed ==
/// 0` / `duck_anchors == 0` guards from `reconcile` left all 11 tests in this
/// file GREEN. B1's headline "an equality between two zeros is not a check" was
/// itself unchecked (CLAUDE.md rule 9).
///
/// The rewrite migrates a HEALTHY ledger first — so the carry succeeds and
/// there is a real SQLite side to compare — and only then strips both sides to
/// zero, which is the state the guard exists for: every equality matches
/// (`0 == 0` four times over), `verify_chain` is happy, the link walk is happy,
/// `integrity_check` is `ok`. Nothing but the non-zero preconditions can see
/// it. The assertions are unconditional.
#[test]
fn the_gate_hard_stops_when_the_duckdb_side_has_no_tamper_evidence_at_all() {
    let dir = scratch("zero");
    let db = seed(&dir, 4, 2);
    let snap_dir = dir.join("snap");
    run_snapshot(&db, TENANT, Some(&snap_dir)).expect("snapshot of a healthy ledger");
    let lite = dir.join("aberp.sqlite");
    migrate_ledger(&db, &lite, TENANT, &snap_dir, LedgerSource::Table)
        .expect("the healthy carry must succeed — otherwise there is nothing to gate");

    // Now gut BOTH sides equally. This is the "green zero" shape: the counts
    // agree, so every equality passes; only the preconditions can object.
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(
            "UPDATE audit_ledger
                SET event_sig = NULL, session_id = NULL, session_pubkey = NULL;
             DELETE FROM audit_ledger_anchors;",
        )
        .unwrap();
        conn.close().unwrap();
    }
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE audit_ledger
                SET event_sig = NULL, session_id = NULL, session_pubkey = NULL;
             DELETE FROM audit_ledger_anchors;",
        )
        .unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).expect("the gate itself must still run");

    // The premise: every equality really does pass, so the hard stops below can
    // only have come from the non-zero preconditions.
    assert!(
        !r.hard_stops
            .iter()
            .any(|s| s.contains("DuckDB 0, SQLite 0")),
        "no equality should have fired — the two sides are identical: {:?}",
        r.hard_stops
    );
    for want in [
        "0 audit_ledger rows with a non-NULL event_sig",
        "0 audit_ledger_anchors rows",
        "0 audit_ledger rows with a non-NULL session_id",
        "0 audit_ledger rows with a non-NULL session_pubkey",
    ] {
        assert!(
            r.hard_stops.iter().any(|s| s.contains(want)),
            "a zero-coverage DuckDB side must hard-stop on `{want}`: {:?}",
            r.hard_stops
        );
    }
}

/// **S449 (B4) — the gate must not compare against a STALE DuckDB extraction.**
///
/// `run_snapshot`, `verify_against_manifest_locked` and `migrate_ledger` all
/// refuse on a non-empty `aberp.duckdb.wal`. `reconcile` did not, and it is the
/// one that matters most: the writer lock stops a CONCURRENT writer, never a
/// previously CRASHED one, so a DuckDB build that wrote rows and died between
/// the carry and the gate leaves committed data a read-only open cannot replay.
/// Every count on the DuckDB side is then short. If the hidden rows are ones
/// the migrator already carried, all four B1 equalities match, the head hashes
/// match, and the gate reports PASS over a source that is ahead of the copy.
///
/// Mutation-verify: delete the `unfolded_wal_len` block from `reconcile` and
/// this goes red.
#[test]
fn s449_the_gate_refuses_when_an_unfolded_duckdb_wal_is_present() {
    let dir = scratch("gate-wal");
    let db = seed(&dir, 5, 2);
    let snap_dir = dir.join("snap");
    run_snapshot(&db, TENANT, Some(&snap_dir)).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_ledger(&db, &lite, TENANT, &snap_dir, LedgerSource::Table).unwrap();

    // The gate is green on the clean state — otherwise the refusal below could
    // be coming from anywhere.
    assert!(
        reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
        "the carry must reconcile cleanly before the WAL is planted"
    );

    // A crashed writer's leftovers. The bytes need not be a valid WAL: the
    // point is that a NON-EMPTY one is unreplayable by a read-only open, so
    // refusing on its presence is the only sound reading (same probe the other
    // three entry points use).
    std::fs::write(
        aberp_db::readonly::wal_path_for(&db),
        b"committed-but-unfolded",
    )
    .unwrap();

    let err = reconcile(&db, &lite, TENANT)
        .expect_err("a gate that reads a short extraction must refuse, not compare");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unfolded WAL"),
        "the refusal must name the WAL so the operator knows what to fold: {msg}"
    );
}

/// **S449 (C-II) — the gate's TARGET path was unguarded.**
///
/// `reconcile` checked `ensure_dev_only` on the DuckDB path only, and
/// `open_hardened` CREATES the file it is given — so a typo'd `--sqlite` under
/// `~/.aberp/` wrote a fresh database into the production root before the first
/// query failed. `migrate_ledger` guards both paths; the gate is the command an
/// operator runs by hand, so it needed the same.
#[test]
fn s449_the_gate_refuses_a_sqlite_target_under_the_production_root() {
    let dir = scratch("gate-target");
    let db = seed(&dir, 3, 1);
    let home = std::env::var("HOME").expect("HOME");
    let prod_target = PathBuf::from(&home).join(".aberp/prod/s449-must-never-appear.sqlite");

    let err = reconcile(&db, &prod_target, TENANT)
        .expect_err("a target under the production root must be refused");
    assert!(
        format!("{err:#}").contains("DEV-only"),
        "expected the C-II refusal, got: {err:#}"
    );
    assert!(
        !prod_target.exists(),
        "the refusal must come BEFORE the open — `open_hardened` would have created {}",
        prod_target.display()
    );
}

/// **S449 — the strip shape the two landed equalities could not see.**
///
/// PR #51's gate counted `event_sig IS NOT NULL` and the anchors, and nothing
/// else. But a signature is only checkable against the key that made it: a
/// carry that keeps every `event_sig` and drops `session_pubkey` produces a
/// ledger whose tamper-evidence can never be verified again — and it passes
/// both landed equalities, the row count, both head checks, `verify_chain` on
/// the DuckDB side, the structural link walk, `PRAGMA integrity_check`, and the
/// `typeof` sweep (which only inspects non-NULL values). Six green checks on a
/// ledger with no usable signatures. That is B1 restated one column over.
///
/// It is unreachable through today's two `LedgerSource` arms — `Table` copies
/// all three columns and `RejectedMirrorReplay` nulls all three — which is
/// precisely why it needed a test rather than an argument: Steps 5-9 add paths
/// into this file, and the gate has to hold for the ones not written yet.
///
/// Mutation-verify: delete the `session_pubkey` arm from `reconcile`'s
/// `extra` loop and this goes red.
#[test]
fn s449_the_gate_hard_stops_when_only_the_session_pubkey_is_stripped() {
    let dir = scratch("pubkey-strip");
    let db = seed(&dir, 6, 2);
    let snap_dir = dir.join("snap");
    run_snapshot(&db, TENANT, Some(&snap_dir)).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_ledger(&db, &lite, TENANT, &snap_dir, LedgerSource::Table).unwrap();

    // The strip: SQLite side only, one column, signatures left intact.
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE audit_ledger SET session_pubkey = NULL;")
            .unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).unwrap();
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("non-NULL session_pubkey")),
        "a pubkey-stripped carry must hard-stop: {:?}",
        r.hard_stops
    );
    // And the reason this needed its own arm: everything else is still green.
    for still_green in [
        "non-NULL event_sig",
        "audit_ledger_anchors row count",
        "audit_ledger row count",
        "head entry_hash",
        "verify_chain",
        "structural link walk",
        "integrity_check",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(still_green)),
            "`{still_green}` should still PASS — that is why the pubkey needed its own              equality. checks: {:?}",
            r.checks
        );
    }
    assert_eq!(
        r.hard_stops.len(),
        1,
        "exactly the session_pubkey count should have fired: {:?}",
        r.hard_stops
    );
}

// ---------------------------------------------------------------------------
// T-19 arm 2 — C-I, measured across a full migrator run
// ---------------------------------------------------------------------------

/// **T-19, arm 2.** The DuckDB file is byte-unchanged across a full migrator
/// run. This is the single mechanism behind C-I — "rollback is stop the SQLite
/// binary, rebuild default, start" is only true because nothing wrote to the
/// DuckDB file.
///
/// Arm 1 (a read-only connection refuses every write) is pinned in
/// `aberp_db::readonly`.
#[test]
fn t19_the_duckdb_file_is_byte_unchanged_across_a_full_migrator_run() {
    let dir = scratch("t19");
    let db = seed(&dir, 8, 2);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let before = sha256(&db);
    let before_mtime = std::fs::metadata(&db).unwrap().modified().unwrap();

    let lite = dir.join("aberp.sqlite");
    let out = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();
    reconcile(&db, &lite, TENANT).unwrap();

    assert_eq!(
        sha256(&db),
        before,
        "the DuckDB file changed (C-I violated)"
    );
    assert_eq!(out.duckdb_sha256_after, before);
    assert_eq!(
        std::fs::metadata(&db).unwrap().modified().unwrap(),
        before_mtime,
        "the DuckDB file was touched"
    );
    // And no WAL was created beside it — a read-only open cannot make one.
    assert!(!aberp_db::readonly::wal_path_for(&db).exists());
}

// ---------------------------------------------------------------------------
// The four preconditions (B4). All refusals; none waits.
// ---------------------------------------------------------------------------

/// Precondition 1 — rule 13. A fresh opener reads Handle-WAL-resident DuckDB
/// **stale**, so a migrator that runs while `serve` is live silently migrates a
/// short ledger. Refuse; never wait, never force.
#[test]
fn the_migrator_refuses_while_another_writer_holds_the_tenant_lock() {
    let dir = scratch("lock");
    let db = seed(&dir, 3, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let held = aberp::db_writer_lock::acquire_or_refuse(&db, TENANT, "test serve").unwrap();
    let err = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a held writer lock must refuse the migrator");
    assert!(err.to_string().contains("single-writer"), "{err}");
    assert!(!lite.exists(), "nothing may be produced on a refusal");
    drop(held);
}

/// Precondition 2 — B3. A read-only open cannot replay a WAL, so an unfolded
/// one is committed data the migrator would silently not see. Holding the lock
/// does not make this redundant: the lock stops a concurrent writer, this stops
/// a previously crashed one.
#[test]
fn the_migrator_refuses_on_an_unfolded_wal() {
    let dir = scratch("wal");
    let db = seed(&dir, 3, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    std::fs::write(aberp_db::readonly::wal_path_for(&db), b"unfolded").unwrap();

    let lite = dir.join("aberp.sqlite");
    let err = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("an unfolded WAL must refuse");
    assert!(err.to_string().contains("unfolded WAL"), "{err}");
    assert!(!lite.exists());
}

/// Precondition 4 — there is no rollback target without a verifying snapshot,
/// and a snapshot that no longer matches the database is worse than none: it
/// would report PASS on a restore that put back the wrong bytes.
#[test]
fn the_migrator_refuses_without_a_snapshot_and_when_the_snapshot_no_longer_verifies() {
    let dir = scratch("snap");
    let db = seed(&dir, 3, 1);
    let lite = dir.join("aberp.sqlite");

    // (a) no snapshot at all
    let err = migrate_ledger(&db, &lite, TENANT, &dir.join("nope"), LedgerSource::Table)
        .expect_err("no manifest must refuse");
    assert!(
        err.to_string().contains("no pre-migration manifest"),
        "{err}"
    );

    // (b) a snapshot that has gone stale
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch("UPDATE audit_ledger SET event_sig = NULL WHERE seq = 1;")
            .unwrap();
        conn.close().unwrap();
    }
    let err = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a snapshot that no longer verifies must refuse");
    assert!(err.to_string().contains("does NOT verify"), "{err}");
    assert!(!lite.exists());
}

/// C-II, and the C-I extension agreement in both directions.
#[test]
fn the_migrator_refuses_a_production_path_and_a_mismatched_extension() {
    let dir = scratch("guards");
    let db = seed(&dir, 3, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();

    let home = std::env::var("HOME").unwrap();
    let prod = PathBuf::from(&home).join(".aberp/prod/aberp.sqlite");
    let err = migrate_ledger(&db, &prod, TENANT, &snap, LedgerSource::Table)
        .expect_err("a target under ~/.aberp must be refused");
    assert!(err.to_string().contains("DEV-only violation"), "{err}");

    // The target must be a `.sqlite`, not anything else.
    let err = migrate_ledger(
        &db,
        &dir.join("aberp.db"),
        TENANT,
        &snap,
        LedgerSource::Table,
    )
    .expect_err("a non-.sqlite target must be refused");
    assert!(err.to_string().contains("engine/DB-path mismatch"), "{err}");
}

/// The migrator never overwrites. If a `.sqlite` is already there, the operator
/// is mid-exercise and the right move is a rollback, not a silent clobber.
#[test]
fn the_migrator_refuses_to_overwrite_an_existing_sqlite_file() {
    let dir = scratch("overwrite");
    let db = seed(&dir, 3, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    std::fs::write(&lite, b"someone else's work").unwrap();

    let err = migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("an existing target must be refused");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(std::fs::read(&lite).unwrap(), b"someone else's work");
}

// ---------------------------------------------------------------------------
// The mirror's three-arm classification (Q7)
// ---------------------------------------------------------------------------

/// The 2026-07-19 shape: the mirror is AHEAD of the table. The gate must stop
/// and route to the heal path — **a migration is not a repair tool** — rather
/// than either failing flat with no route forward or quietly proceeding.
#[test]
fn the_gate_stops_and_routes_to_heal_when_the_mirror_is_ahead() {
    let dir = scratch("mirror-ahead");
    let db = seed(&dir, 4, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();

    // Append one more mirror line than the table has, by hand: the mirror's
    // last record, re-emitted with the next seq.
    let mirror = aberp_audit_ledger::mirror_path_for(&db);
    let text = std::fs::read_to_string(&mirror).unwrap();
    let last = text.lines().last().unwrap().to_string();
    let bumped = last.replace(r#""seq":4"#, r#""seq":5"#);
    std::fs::write(&mirror, format!("{text}{bumped}\n")).unwrap();

    let r = reconcile(&db, &lite, TENANT).unwrap();
    assert!(
        r.hard_stops.iter().any(|s| s.contains("mirror is AHEAD")),
        "{:?}",
        r.hard_stops
    );
    assert!(
        r.hard_stops.iter().any(|s| s.contains("not a repair tool")),
        "the operator must be told the route, not just the fault: {:?}",
        r.hard_stops
    );
}

/// The other direction: the table is ahead. The fsync'd mirror missed a
/// committed append — a durability failure in the artefact the whole scheme
/// leans on. **Hard stop, no heal.**
#[test]
fn the_gate_hard_stops_with_no_heal_when_the_table_is_ahead_of_the_mirror() {
    let dir = scratch("table-ahead");
    let db = seed(&dir, 4, 1);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_ledger(&db, &lite, TENANT, &snap, LedgerSource::Table).unwrap();

    // Drop the mirror's last line.
    let mirror = aberp_audit_ledger::mirror_path_for(&db);
    let text = std::fs::read_to_string(&mirror).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.pop();
    std::fs::write(&mirror, format!("{}\n", lines.join("\n"))).unwrap();

    let r = reconcile(&db, &lite, TENANT).unwrap();
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("AHEAD of the mirror") && s.contains("no heal")),
        "{:?}",
        r.hard_stops
    );
}
