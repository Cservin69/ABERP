//! **ADR-0110 D7 — the WAL fence: does `durable_ack` notice when its WAL was
//! truncated out from under it?**
//!
//! # The defect
//!
//! A foreign `duckdb::Connection::open` on the tenant DB — the GROUP-A shape
//! the ADR-0099 read-fork census tracks — carries DuckDB's DEFAULT pragmas: no
//! `disable_checkpoint_on_shutdown`, no raised `wal_autocheckpoint`. On close
//! it FOLDS and TRUNCATES the live `Handle`'s WAL. Past that point the Handle's
//! `commit()` keeps returning `Ok` while the bytes reach no file.
//!
//! D3's [`Handle::durable_ack`] was blind to it **by construction**, because it
//! `fsync`s PATHS. After the truncation `<db>.wal` is absent or empty, so
//!
//! ```ignore
//! if self.wal_path.exists() {          // <-- FALSE. Skipped.
//!     self.fsync_and_record(&self.wal_path)?;
//! }
//! ```
//!
//! skipped it, the main-file `fsync` succeeded, and `Ok(())` came back. A green
//! durability light with nothing behind it — worse than no light, because it is
//! believed. That is incident 00012, and every D3 gate stayed green through it.
//!
//! # What this file pins
//!
//! [`the_group_a_shape_must_fail_the_ack`] is the RED-first pin: it reproduces
//! the exact prod shape and requires [`DbError::WalTruncatedUnderWriter`]. On
//! the pre-D7 `durable_ack` body it goes RED (the ack returns `Ok`); with the
//! fence it goes green. This is the test that would have caught 00012.
//!
//! Everything after it guards the **other** direction, which is the harder
//! half. A fence that fires on a healthy tenant is not a safety feature, it is
//! an alarm the operator learns to dismiss — and then it is worse than nothing
//! the day it is right. So the four shapes that legitimately move a WAL around
//! each get a pin saying the fence stays SILENT:
//!
//! * [`a_boot_onto_a_pre_existing_wal_must_not_fire`]
//! * [`the_first_ack_after_a_boot_fold_must_not_fire`]
//! * [`concurrent_daemon_writes_must_not_fire`]
//! * [`a_legitimate_reopen_must_not_fire`]
//! * [`a_healthy_tenant_never_fires_across_many_acks`]
//!
//! and [`swapping_the_inode_between_commit_and_ack_must_fail_the_ack`] pins the
//! A2 residual: `durable_ack` opens BY PATH, so it could `fsync` a file that is
//! not the one it wrote to and report success.
//!
//! # Mutation verification
//!
//! A pin that cannot go red is not a pin. Each direction was verified before
//! landing — see the per-test notes.
//!
//! # Scope
//!
//! `$TMPDIR` only. Nothing here touches `~/.aberp/**` or any tenant database.

use std::path::{Path, PathBuf};

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, Actor, BinaryHash, EventKind, LedgerMeta, TenantId,
};
use aberp_db::{DbError, Handle, HandleConfig, WalBreach};
use duckdb::Connection;

const TENANT: &str = "tenant-adr0110-d7-fence";

// ── scaffolding ─────────────────────────────────────────────────────────────

struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "aberp-adr0110-d7-{label}-{}-{nanos}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&p).expect("mkdir scratch tenant dir");
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
    TenantId::new(TENANT.to_string()).expect("test tenant id is valid")
}

fn wal_of(db: &Path) -> PathBuf {
    let mut os = db.as_os_str().to_owned();
    os.push(".wal");
    PathBuf::from(os)
}

fn wal_len(db: &Path) -> u64 {
    std::fs::metadata(wal_of(db)).map(|m| m.len()).unwrap_or(0)
}

/// Seed an empty tenant DB with the audit schema and fold it, so the Handle
/// opens onto a clean checkpointed file with no WAL — the boot state.
fn seed(db: &Path) {
    let conn = Connection::open(db).expect("seed open");
    ensure_schema(&conn).expect("seed schema");
    conn.execute_batch("CHECKPOINT;").expect("seed fold");
}

/// A Handle with the D7 fence **ARMED**.
///
/// As of ADR-0110 D7.6 (2026-08-13) this is what `HandleConfig::default()` gives
/// too, so these tests now describe the production posture rather than a
/// hypothetical one. It stays an explicit config all the same: what every test
/// below asserts is a property of the ARMED fence, and a test that says so out
/// loud cannot be quietly re-aimed by a future change to the default.
/// [`the_fence_ships_armed_by_default`] is where the default itself is pinned;
/// [`with_the_fence_disarmed_the_group_a_shape_does_not_fail_the_ack`] and
/// [`a_disarmed_fence_never_touches_the_watermark`] pin the off-state, which is
/// kept because it is bit-for-bit the pre-D7 D3 ack.
fn handle(db: &Path) -> std::sync::Arc<Handle> {
    Handle::open(db, tenant(), armed_config()).expect("open shared Handle (fence ARMED)")
}

fn armed_config() -> HandleConfig {
    HandleConfig {
        wal_fence_enabled: true,
        ..Default::default()
    }
}

/// The fence explicitly OFF. Since D7.6 armed the shipping default this is no
/// longer `HandleConfig::default()`, and the off-state pins say so rather than
/// leaning on the default — the D5-N3 lesson, applied before it could bite.
fn disarmed_config() -> HandleConfig {
    HandleConfig {
        wal_fence_enabled: false,
        ..Default::default()
    }
}

/// One committed audit row through the shared Handle — the shape every money
/// path takes. Returns after the guard has dropped, so the lockstep mirror sync
/// AND the D7 watermark sample have both run.
fn commit_one(h: &Handle, label: &str) {
    let meta = LedgerMeta::new(tenant(), BinaryHash::from_bytes([7u8; 32]));
    let mut guard = h.write().expect("acquire the shared writer");
    let tx = guard.conn().transaction().expect("begin");
    let actor = Actor::from_local_cli(format!("ulid-{label}"), "tester");
    append_in_tx(
        &tx,
        &meta,
        // A neutral probe kind: never collides with the fence's own
        // `db.durability_loss_detected` forensic row.
        EventKind::Test,
        format!("{{\"probe\":\"{label}\"}}").into_bytes(),
        actor,
        None,
    )
    .expect("append");
    tx.commit().expect("commit");
    drop(guard);
}

/// One committed `db.durability_loss_detected` row — the LEGACY on-chain
/// diagnostic, as a prod tenant recovered from incident 00012 may still hold it.
///
/// Nothing in this tree writes this kind any more: ADR-0110 §15.3 routed D7's
/// fence to the non-chained marker, as D5 was already routed. The
/// `restore_durability_alert` readers for it are kept for backward
/// compatibility, and this is what lets them stay under test now that no
/// production path produces one.
fn commit_legacy_loss_row(h: &Handle) {
    let meta = LedgerMeta::new(tenant(), BinaryHash::from_bytes([7u8; 32]));
    let mut guard = h.write().expect("acquire the shared writer");
    let tx = guard.conn().transaction().expect("begin");
    let actor = Actor::from_local_cli("ulid-legacy-loss".to_string(), "system:aberp-db");
    append_in_tx(
        &tx,
        &meta,
        EventKind::DbDurabilityLossDetected,
        br#"{"trigger":"wal_truncated_under_writer","breach":"wal_vanished"}"#.to_vec(),
        actor,
        None,
    )
    .expect("append the legacy loss row");
    tx.commit().expect("commit");
    drop(guard);
}

/// **The defect primitive.** A foreign DuckDB instance on the same path,
/// opened and closed with DEFAULT pragmas. Its close folds and truncates the
/// live Handle's WAL. This was `serve::read_invoice_total_gross_minor` before
/// PR #1 and `calibration_overview_request` / `resolve_recipient_email` /
/// `handle_quote_pipeline_status` before ADR-0110 D8 — all now Handle-routed.
/// It remains the shape of every CLI one-shot (GROUP B), which is a separate OS
/// process and so cannot be migrated the same way — as of ADR-0110 D9 those are
/// instead FENCED: each takes the F-E whole-DB writer flock before opening, so
/// this shape can no longer occur *against a live serve*. The shape itself stays
/// exactly as written here, because that is what the fence must keep catching if
/// a future opener slips the fencing.
fn foreign_open_and_close(db: &Path) {
    let c = Connection::open(db).expect("foreign open");
    c.execute_batch("SELECT 1;").expect("foreign read");
    // `c` drops here. No `disable_checkpoint_on_shutdown`, so DuckDB folds.
}

#[track_caller]
fn expect_breach(err: DbError, want: WalBreach) {
    match err {
        DbError::WalTruncatedUnderWriter { breach, .. } => assert_eq!(
            breach, want,
            "the fence fired, but classified the breach as {breach:?} rather than {want:?}"
        ),
        other => panic!("expected DbError::WalTruncatedUnderWriter, got {other:?}"),
    }
}

// ── THE RED-FIRST PIN ───────────────────────────────────────────────────────

/// **The test that would have caught 00012.**
///
/// Handle opens, commits, a foreign `Connection::open` + close lands on the same
/// path (the GROUP-A shape verbatim), the Handle commits again — and the ack
/// must REFUSE. Before D7 this returned `Ok(())`: the truncation removed the
/// WAL, `if wal_path.exists()` skipped it, and the main-file `fsync` reported
/// success on a file that did not have the rows.
///
/// Mutation-verified: revert `durable_ack` to the D3 three-liner and this goes
/// RED while `durable_ack_fault_injection.rs` and the D6b power-loss tiers all
/// stay green — which is exactly the blind spot it exists to cover.
#[test]
fn the_group_a_shape_must_fail_the_ack() {
    let tmp = Tmp::new("group-a");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    // (1) A committed money-path write. Its rows are WAL-resident: under H3 the
    //     runtime checkpoint is disabled, which is what makes the fold below
    //     destructive rather than merely wasteful.
    commit_one(&h, "before");
    let before = wal_len(&db);
    assert!(
        before > 0,
        "precondition: a Handle commit must be WAL-resident (checkpoint_enabled=false); \
         got a {before}-byte WAL"
    );
    h.durable_ack()
        .expect("precondition: the ack must succeed on a healthy tenant");

    // (2) THE DEFECT. A foreign opener with DEFAULT pragmas folds + truncates
    //     the live Handle's WAL on close.
    foreign_open_and_close(&db);
    let after = wal_len(&db);
    assert!(
        after < before,
        "precondition: the foreign close must have folded the WAL ({before} -> {after} bytes). \
         If DuckDB stopped doing this, the fence still holds but this pin no longer reproduces \
         the prod mechanism and must be re-derived rather than deleted."
    );

    // (3) The Handle commits again — `commit()` returns Ok, as it did in prod.
    commit_one(&h, "after");

    // (4) THE FENCE. This is the whole PR.
    let err = h.durable_ack().expect_err(
        "ADR-0110 D7 REGRESSION: durable_ack returned Ok after a foreign DuckDB opener folded \
         and truncated this Handle's WAL. That is incident 00012 exactly: the ack fsyncs PATHS, \
         the truncated-away WAL reads as 'absent', the existence check SKIPS it, the main-file \
         fsync succeeds, and the operator is told a write is durable that reached no file.",
    );
    match err {
        DbError::WalTruncatedUnderWriter { breach, .. } => assert!(
            matches!(
                breach,
                WalBreach::WalVanished | WalBreach::WalShrank | WalBreach::WalReplaced
            ),
            "the fence fired but classified a WAL truncation as {breach:?}"
        ),
        other => panic!("expected DbError::WalTruncatedUnderWriter, got {other:?}"),
    }

    // (5) The operator signal is set, and it is STICKY.
    let alert = h
        .durability_alert()
        .expect("a fired fence must raise the sticky operator alert");
    assert!(
        alert.message.contains("Durability loss detected"),
        "the alert must read as an alarm an operator can act on; got {:?}",
        alert.message
    );

    // (6) KEEP SERVING (Ervin, 2026-08-12). The fence is not a sticky write
    //     refusal: the very next healthy ack succeeds. A fence that bricked the
    //     app would turn one truncation into a total outage, and the operator
    //     would learn to route around it.
    commit_one(&h, "recovered");
    h.durable_ack().expect(
        "KEEP-SERVING REGRESSION: the ack after a detected breach must succeed on a tenant that \
         is healthy again. A latched refusal is a hard stop, which was explicitly ruled out.",
    );

    // ...and the alert SURVIVES that recovery. "It stopped happening" is not
    // "it did not happen"; the rows that went missing do not come back.
    assert!(
        h.durability_alert().is_some(),
        "the durability alert must be sticky until explicitly cleared — a healthy ack must not \
         silently take the operator's banner down"
    );
    // ADR-0110 D5: clearing now records the acknowledgement in the non-chained
    // durability-alert marker FIRST and only then clears the flag, so it is
    // fallible — and a failure has to leave the banner up.
    h.clear_durability_alert()
        .expect("the acknowledgement must record");
    assert!(
        h.durability_alert().is_none(),
        "clear_durability_alert is the one thing that takes it down"
    );
}

// ── FALSE-POSITIVE GUARDS (the crux) ────────────────────────────────────────

/// A Handle that boots onto a WAL somebody else left behind must not fire.
///
/// This is the shape after any unclean shutdown: `<db>.wal` exists and is large
/// before we ever open. There is no honest "before" to compare it against, so
/// the first observation may only baseline.
///
/// Mutation-verified: drop the `mark.wal_high_water > 0` guard from
/// `detect_breach`'s WalVanished arm and the sibling boot test goes RED. The
/// per-combination table is pinned deterministically in `fence_tests`.
#[test]
fn a_boot_onto_a_pre_existing_wal_must_not_fire() {
    let tmp = Tmp::new("preexisting-wal");
    let db = tmp.db();
    seed(&db);

    // Leave a real, unfolded WAL behind: write through one Handle and drop it
    // without folding (the F-A pragmas guarantee the drop does not fold).
    {
        let h = handle(&db);
        commit_one(&h, "previous-boot");
        assert!(wal_len(&db) > 0, "precondition: leave a WAL behind");
    }

    // A fresh boot onto it.
    let h = handle(&db);
    h.durable_ack()
        .expect("the first ack of a boot must never fire the fence — there is no prior sample");
    commit_one(&h, "fresh-boot");
    h.durable_ack()
        .expect("nor the ack after the first commit of that boot");
    assert!(h.durability_alert().is_none(), "no alert on a healthy boot");
}

/// The first ack after a boot fold must not fire.
///
/// A boot legitimately folds the WAL down to nothing (that is how the WAL gets
/// bounded at all, given H4 was never built). A fence that counted the boot
/// fold as a truncation would fire on every single startup.
#[test]
fn the_first_ack_after_a_boot_fold_must_not_fire() {
    let tmp = Tmp::new("boot-fold");
    let db = tmp.db();
    seed(&db);

    // Boot #1 leaves a WAL.
    {
        let h = handle(&db);
        commit_one(&h, "b1");
    }
    let carried = wal_len(&db);
    assert!(carried > 0, "precondition: boot #1 leaves a WAL");

    // The boot fold: a plain opener replays and folds it, exactly as serve's
    // boot chokepoint does before the Handle is constructed.
    foreign_open_and_close(&db);

    // Boot #2 opens onto the folded file. Its FIRST ack must be clean — the
    // fold happened before this Handle existed and is none of its business.
    let h = handle(&db);
    h.durable_ack()
        .expect("the first ack after a boot fold must not fire — the fold predates this Handle");
    commit_one(&h, "b2");
    h.durable_ack().expect("nor the one after the first commit");
    assert!(
        h.durability_alert().is_none(),
        "no alert across a boot fold"
    );
}

/// Concurrent writers must not fire it.
///
/// `durable_ack` deliberately takes no writer lock, so daemons commit while a
/// money path is mid-ack and the WAL GROWS underneath it. A last-seen-length
/// check would read that as drift on every busy minute. The high-water reads it
/// as what it is.
///
/// Mutation-verified — and the result is worth stating precisely, because two
/// plausible-sounding claims about this test are FALSE:
///
/// * Making the comparison intolerant of growth (`!=` instead of `<`) turns
///   this RED, along with every other guard here. Growth-tolerance IS
///   load-bearing and IS pinned.
/// * Replacing the monotone `max` with a last-seen assignment does NOT turn it
///   red, and neither does moving the `stat` outside the watermark lock (tried
///   with a 2 ms window). Because `observe_durable_set` holds the lock across
///   stat-compare-update, observations are totally ordered and a last-seen
///   value is already equal to the high-water on a healthy tenant — the `max`
///   is the invariant written down, not the thing doing the work. Do not read
///   this test as a pin on it.
#[test]
fn concurrent_daemon_writes_must_not_fire() {
    use std::sync::Arc;

    let tmp = Tmp::new("concurrent");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);
    commit_one(&h, "seed");

    // Two daemons hammering the WAL while a third thread acks in a tight loop —
    // the ack's `stat` and the writers' samples interleave freely.
    let mut threads = Vec::new();
    for t in 0..2 {
        let h: Arc<Handle> = Arc::clone(&h);
        threads.push(std::thread::spawn(move || {
            for i in 0..15 {
                commit_one(&h, &format!("daemon-{t}-{i}"));
            }
        }));
    }
    {
        let h: Arc<Handle> = Arc::clone(&h);
        threads.push(std::thread::spawn(move || {
            for _ in 0..40 {
                h.durable_ack().expect(
                    "FALSE POSITIVE: a concurrent daemon write GREW the WAL and the fence read \
                     the growth as a truncation. The high-water must be monotone, and the stat \
                     must happen inside the watermark lock so observations are totally ordered.",
                );
            }
        }));
    }
    for t in threads {
        t.join().expect("worker thread panicked");
    }
    assert!(
        h.durability_alert().is_none(),
        "FALSE POSITIVE: concurrent healthy writes raised a durability alert"
    );
}

/// A reopen the Handle performs ITSELF must not fire.
///
/// Post-poison recovery drops the shared connection and reopens it; that
/// destroys the DuckDB `Database`, and the reopen replays the WAL and may fold
/// it. It is the one sanctioned shrink, and `note_self_fold` is how the fence
/// is told.
///
/// Mutation-verified, with an honest negative result: deleting
/// `self.note_self_fold()` from `recover_from_poison` does NOT turn this red.
/// Measured on the pinned libduckdb 1.5.3 (2026-08-12): the recovery's
/// drop-and-reopen does not fold at all — the WAL GREW, 1270 → 2118 bytes,
/// because the F-A pragmas are on the connection being closed and the reopen's
/// replay does not truncate. So this test pins the ABSENCE of a false positive
/// across a real poison recovery, which is what an operator cares about; the
/// `note_self_fold` line is insurance against an engine behaviour we do not
/// control, and the escape hatch itself is pinned deterministically by
/// `fence_tests::a_declared_self_fold_re_baselines_instead_of_firing` in the
/// crate's own unit tests.
#[test]
fn a_legitimate_reopen_must_not_fire() {
    use std::sync::Arc;

    let tmp = Tmp::new("reopen");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);
    commit_one(&h, "pre-poison");
    h.durable_ack().expect("healthy before the poison");
    assert!(wal_len(&db) > 0, "precondition: a WAL to lose");

    // Poison the writer mutex: panic while holding the guard. The next
    // acquire runs `recover_from_poison`, which drops and reopens.
    {
        let h: Arc<Handle> = Arc::clone(&h);
        let _ = std::thread::spawn(move || {
            let _guard = h.write().expect("acquire before the panic");
            panic!("deliberate poisoning panic");
        })
        .join();
    }

    // The recovery happens on this next acquire.
    commit_one(&h, "post-poison");
    h.durable_ack().expect(
        "FALSE POSITIVE: the Handle's OWN drop-and-reopen (post-poison recovery) folded the WAL \
         and the fence read our own recovery as a foreign truncation. `note_self_fold` must \
         declare it.",
    );
    assert!(
        h.durability_alert().is_none(),
        "FALSE POSITIVE: post-poison recovery raised a durability alert"
    );
}

/// The plain everyday case: a healthy tenant, many acks, no alarm. Cheap, and
/// it is the one that fails first if the fence is wired backwards.
#[test]
fn a_healthy_tenant_never_fires_across_many_acks() {
    let tmp = Tmp::new("healthy");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    for i in 0..25 {
        commit_one(&h, &format!("row-{i}"));
        h.durable_ack()
            .unwrap_or_else(|e| panic!("FALSE POSITIVE on a healthy tenant at ack {i}: {e}"));
    }
    assert!(
        h.durability_alert().is_none(),
        "FALSE POSITIVE: a healthy tenant raised a durability alert"
    );
}

// ── THE A2 RESIDUAL — fsync by path, on the wrong inode ─────────────────────

/// `durable_ack` opens BY PATH. Swap the inode behind the name between the
/// commit and the ack and the pre-A2 code would happily `fsync` the impostor
/// and journal it as durable.
///
/// The swap here is a rename-over, which is how a restore or a fold-and-install
/// does it — the Handle's own open `fd` keeps pointing at the original inode,
/// so the process notices nothing while every later commit goes somewhere the
/// name no longer reaches.
///
/// Mutation-verified: delete the `fstat`-and-compare block from
/// `fsync_and_record` and this goes RED while every other test in this file
/// stays green.
#[test]
fn swapping_the_inode_between_commit_and_ack_must_fail_the_ack() {
    let tmp = Tmp::new("inode-swap");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    commit_one(&h, "original");
    h.durable_ack()
        .expect("precondition: the ack is healthy before the swap");

    // Swap the main DB file for a different inode under the same name.
    let decoy = tmp.0.join("decoy.duckdb");
    seed(&decoy);
    std::fs::rename(&decoy, &db).expect("rename a different inode over the live path");

    let err = h.durable_ack().expect_err(
        "ADR-0110 D7 / A2 REGRESSION: durable_ack returned Ok after the main DB file's inode was \
         swapped under it. It opened the path, fsync'd whatever was behind the name, and \
         journalled it as durable — certifying a file this Handle never wrote a byte to.",
    );
    expect_breach(err, WalBreach::MainReplaced);
    assert!(
        h.durability_alert().is_some(),
        "an inode swap is durability loss and must raise the operator alert too"
    );
}

// ── the fence must not disturb the D3 contract ──────────────────────────────

/// D3's journal still records what was actually `fsync`'d on the healthy path.
/// The D6b power-loss spec DERIVES its durable set from this, so a fence that
/// quietly stopped the recording would turn that spec green-by-vacuum.
#[test]
fn the_healthy_path_still_journals_what_it_synced() {
    let tmp = Tmp::new("journal");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);
    commit_one(&h, "row");
    h.durable_ack().expect("healthy ack");

    let synced = h.fsynced_paths();
    assert!(
        synced.iter().any(|p| p == &db),
        "the main DB file must still be journalled; got {synced:?}"
    );
    assert!(
        synced.iter().any(|p| p == &wal_of(&db)),
        "the WAL must still be journalled on a tenant that has one; got {synced:?}"
    );
}

// ── D7.6 — THE FENCE SHIPS ARMED ────────────────────────────────────────────

/// **The shipping default is ON, as of ADR-0110 D7.6 (2026-08-13).**
///
/// This pin is the inverse of the one it replaces, and the reason it is still a
/// pin is that arming was gated on three preconditions that each have to STAY
/// closed. If any regresses, the fence stops being an alarm and becomes a
/// money-path outage: armed over a surviving fold, the next issue-invoice or
/// mark-paid fails its `durable_ack`, and that failure PROPAGATES via `?` (the
/// D3-C cut-gate enforces exactly that propagation) — a committed invoice
/// reported as failed, NAV handoff skipped.
///
/// The three, and what keeps each closed:
///
/// 1. **In-serve openers — D8.** GROUP A in
///    `tools/adr0099_read_fork_structural_baseline.txt` is empty; held by
///    `tools/cut_gate_read_fork.sh` + `cut_gate_opener_census.sh`.
/// 2. **CLI-against-live openers — D9.** Every DB-mutating one-shot takes the
///    F-E whole-DB writer flock before it opens the tenant DB, so it REFUSES
///    against a live serve rather than folding its WAL; held by
///    `apps/aberp/tests/adr0110_d9_flock_shape.rs` and
///    `aberp-inventory/tests/rebuild_stock_cache_flock.rs`.
/// 3. **The fence's own diagnostic must not fork the audit chain — §15.3.**
///    A truncation regresses the DB audit head below the mirror's, so the
///    `db.durability_loss_detected` ledger append the fence used to make landed
///    at a seq the mirror held a different entry for; the next boot's gated
///    auto-heal then refused and `serve` exited non-zero. An armed fence would
///    have bricked the tenant with its own alarm (D5-B1). The record now goes to
///    the non-chained marker; held by
///    [`the_d5_b1_scenario_driven_through_the_armed_fence_must_boot_cleanly`].
///
/// The disarmed body is deliberately KEPT and still pinned
/// ([`with_the_fence_disarmed_the_group_a_shape_does_not_fail_the_ack`],
/// [`a_disarmed_fence_never_touches_the_watermark`]): it is bit-for-bit the D3
/// ack, and it is what a bisect through the two months the fence shipped dark
/// lands on.
#[test]
fn the_fence_ships_armed_by_default() {
    assert!(
        HandleConfig::default().wal_fence_enabled,
        "ADR-0110 D7.6 REGRESSION: the WAL fence must default ARMED. Disarming it again \
         re-opens incident 00012's blind spot in full: a foreign opener folds this Handle's \
         WAL, `durable_ack` fsyncs a path that no longer holds the rows, and every commit \
         after it returns Ok while the bytes reach no file. That is a GREEN durability light \
         with nothing behind it, which is worse than no light because it is believed. If a \
         real fold has been found and the fence must go dark to keep the money paths up, that \
         is an incident decision — record it in ADR-0110 D7.6 with the fold that forced it, \
         do not just flip the constant."
    );
}

/// The disarmed half of the RED-first pin: the exact GROUP-A shape that
/// [`the_group_a_shape_must_fail_the_ack`] proves the armed fence catches must,
/// with the fence off, behave exactly as it did before D7 — the ack succeeds.
///
/// Pinning BOTH states is the point. A flag whose off-state is untested is a
/// flag nobody can trust to be off.
#[test]
fn with_the_fence_disarmed_the_group_a_shape_does_not_fail_the_ack() {
    let tmp = Tmp::new("group-a-disarmed");
    let db = tmp.db();
    seed(&db);
    // An EXPLICITLY disarmed config. This used to be `open_default`, which was
    // the same thing until D7.6 armed the default (2026-08-13). The property
    // here is about the FLAG's off-state, so it is now stated as such —
    // re-pointing a flag test at whatever the default happens to be is how
    // D5-N3's vacuous pin happened.
    let h = Handle::open(&db, tenant(), disarmed_config())
        .expect("open shared Handle (fence DISARMED)");

    commit_one(&h, "before");
    assert!(wal_len(&db) > 0, "precondition: the commit is WAL-resident");
    h.durable_ack().expect("healthy ack");

    // The defect fires — and with the fence off, nothing notices. That is
    // today's behaviour, deliberately preserved.
    foreign_open_and_close(&db);
    commit_one(&h, "after");

    h.durable_ack().expect(
        "REGRESSION: with `wal_fence_enabled: false` the ack must behave exactly as it did \
         under D3 and SUCCEED. The off-state is what a bisect through the two months the fence \
         shipped dark runs on, and what an incident would fall back to if a real fold were \
         found — it has to keep working even though the default is now ARMED (D7.6).",
    );
    assert!(
        h.durability_alert().is_none(),
        "REGRESSION: a disarmed fence must raise no operator alert either. Detection and the \
         banner are one decision, so an off-state that still raised would be the worst of both: \
         no protection on the money path and an alarm the operator cannot act on."
    );
}

/// A disarmed fence must not even LOOK. The guard drop is the hot path (every
/// committed write in the process), so "off" has to mean no `stat` and no
/// watermark mutex, not merely a suppressed verdict.
#[test]
fn a_disarmed_fence_never_touches_the_watermark() {
    let tmp = Tmp::new("disarmed-quiet");
    let db = tmp.db();
    seed(&db);
    let h = Handle::open(&db, tenant(), disarmed_config()).expect("open handle");

    // Drive the shape that WOULD arm a breach, then prove none was latched:
    // with the fence off, `durable_ack` never consults the watermark, so a
    // latched-but-unreported breach would be a landmine waiting for the day
    // someone flips the flag on a long-running process.
    commit_one(&h, "one");
    foreign_open_and_close(&db);
    commit_one(&h, "two");
    h.durable_ack().expect("disarmed ack succeeds");
    assert!(
        h.durability_alert().is_none(),
        "a disarmed fence must leave no alert"
    );
}

// ── N2 — "I could not look" is not "it is gone" ─────────────────────────────

/// A `stat` that fails for a reason OTHER than ENOENT must resolve to
/// "not checked", never to `WalVanished` — the most severe verdict the fence
/// can reach.
///
/// The real-world shape is a NAS or removable mount hiccuping: ESTALE, EIO,
/// ETIMEDOUT. Those are not reproducible in a unit test, so this uses the one
/// non-ENOENT `stat` failure that IS deterministic — EACCES from an
/// unsearchable parent directory. The code path is identical: `metadata()`
/// returns an `Err` whose kind is not `NotFound`.
///
/// Mutation-verified: restore the original `std::fs::metadata(..).ok()` and
/// this goes RED with a `WalTruncatedUnderWriter{ breach: WalVanished }` — a
/// flaky mount reported as durability loss.
#[cfg(unix)]
#[test]
fn an_unreadable_wal_is_not_a_vanished_wal() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = Tmp::new("unreadable");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);
    commit_one(&h, "before");
    assert!(
        wal_len(&db) > 0,
        "precondition: a real WAL to lose sight of"
    );
    h.durable_ack().expect("healthy before the mount hiccup");

    // Make the tenant directory unsearchable: `stat` on anything inside now
    // fails EACCES rather than ENOENT.
    let dir_perms = std::fs::metadata(&tmp.0)
        .expect("stat tenant dir")
        .permissions();
    std::fs::set_permissions(&tmp.0, std::fs::Permissions::from_mode(0o000))
        .expect("chmod the tenant dir unsearchable");

    let stat_now_fails = std::fs::metadata(wal_of(&db))
        .err()
        .filter(|e| e.kind() != std::io::ErrorKind::NotFound)
        .is_some();

    let outcome = h.durable_ack();

    // Restore before asserting, so a failure does not also leave an
    // undeletable temp dir behind.
    std::fs::set_permissions(&tmp.0, dir_perms).expect("restore tenant dir permissions");

    if !stat_now_fails {
        // Running as root (CI containers sometimes do): permissions do not
        // bite, so there is nothing to assert. Say so rather than passing
        // vacuously.
        eprintln!(
            "SKIPPED an_unreadable_wal_is_not_a_vanished_wal: this process can stat through a              0o000 directory (running as root?), so the EACCES path was never reached."
        );
        return;
    }

    match outcome {
        // The honest outcome: we could not reach the files, and the `fsync`
        // says so in its own words.
        Err(DbError::DurableAck { .. }) => {}
        Err(DbError::WalTruncatedUnderWriter { breach, .. }) => panic!(
            "N2 REGRESSION: an unreadable WAL (EACCES, not ENOENT) was reported as {breach:?}.              A NAS or removable mount that hiccups for one syscall would raise a DURABILITY              LOSS alarm and a sticky red banner on a tenant that never lost a byte.              'I could not look' must resolve to NOT CHECKED."
        ),
        other => panic!("expected DbError::DurableAck for an unreadable tenant dir, got {other:?}"),
    }
    assert!(
        h.durability_alert().is_none(),
        "N2 REGRESSION: an unreadable WAL raised the sticky durability banner"
    );

    // And the watermark survived intact: the next readable observation still
    // compares against the last thing we actually saw, so a hiccup does not
    // blind the fence afterwards.
    commit_one(&h, "after");
    h.durable_ack()
        .expect("once the mount is back, acks are healthy again");
    assert!(
        h.durability_alert().is_none(),
        "N2 REGRESSION: the observation after a stat hiccup fired the fence — the unreadable          observation must leave the watermark untouched, not reset it"
    );
}

/// A Handle configured exactly as production configures it is the one under
/// test everywhere above; assert that explicitly, because the fence's premise
/// ("our WAL only ever grows") is a consequence of the F-A pragmas and of
/// nothing else.
#[test]
fn the_fence_is_pinned_to_the_production_posture() {
    let cfg = HandleConfig::default();
    assert!(
        cfg.disable_implicit_close_checkpoint,
        "the D7 fence presupposes the F-A pragmas: they are what make the WAL append-only from \
         this Handle's point of view. Without them DuckDB folds legitimately and the fence \
         would be reporting the engine's own bookkeeping as a durability loss."
    );
    assert!(
        !cfg.checkpoint_enabled,
        "H3 posture: the runtime checkpoint is disabled, which is what makes committed rows \
         WAL-resident and therefore what makes a foreign fold destructive"
    );
}

// ── R3-N1 — the DB side must PARSE then max, never SQL-MAX a VARCHAR ────────
//
// `time_wall` is a VARCHAR column. `SELECT MAX(time_wall)` is therefore a
// LEXICOGRAPHIC max — precisely the comparison the `time` dependency and its
// Cargo.toml note exist to avoid. `time`'s Rfc3339 trims trailing zeros, so on
// same-second stamps the two orders disagree often, not rarely.
//
// Both proofs below drive `Handle::restore_durability_alert_from_mirror`
// through hand-written audit rows, which is the only way to control the exact
// stamps. They observe the outcome through the public
// `Handle::durability_alert`.

use aberp_audit_ledger::mirror_path_for;

/// Write one audit row DIRECTLY, with an exact `time_wall`, bypassing
/// `append_in_tx` (which would stamp `now`). Chain fields are not exercised by
/// the re-derivation — it reads `kind` and `time_wall` only — so they are
/// filled with fixed placeholders rather than a real chain.
fn insert_audit_row(db: &Path, kind: &str, time_wall: &str, seq: u64) {
    let conn = Connection::open(db).expect("open to insert an audit row");
    conn.execute(
        "INSERT INTO audit_ledger \
         (id, seq, prev_hash, time_wall, time_mono, actor, binary_hash, tenant_id, kind, \
          payload, idempotency_key, entry_hash, session_id, session_pubkey, event_sig) \
         VALUES (?, ?, ?, ?, 0, ?, ?, ?, ?, ?, NULL, ?, NULL, NULL, NULL)",
        duckdb::params![
            format!("id-{seq}"),
            seq as i64,
            vec![0u8; 32],
            time_wall,
            "{\"kind\":\"local_cli\",\"session_id\":\"t\",\"user_id\":\"t\"}",
            vec![0u8; 32],
            TENANT,
            kind,
            b"{}".to_vec(),
            vec![0u8; 32],
        ],
    )
    .expect("insert the audit row");
}

/// A tenant seeded with the audit schema and NO mirror, so the re-derivation's
/// verdict comes from the DB half alone.
fn db_only_tenant(tag: &str) -> (Tmp, PathBuf) {
    let tmp = Tmp::new(tag);
    let db = tmp.db();
    seed(&db);
    let mirror = mirror_path_for(&db);
    let _ = std::fs::remove_file(&mirror);
    (tmp, db)
}

fn alert_after_reopen(db: &Path) -> bool {
    // `Handle::open` runs the re-derivation as part of construction.
    let h = Handle::open_default(db, tenant()).expect("reopen");
    h.durability_alert().is_some()
}

/// **The fail-toward-DOWN case.** A loss at `10:00:00.9Z` and an ack at
/// `10:00:00.5Z` — the ack is EARLIER, so the banner must stay UP.
///
/// Under `SELECT MAX(time_wall)` the loss row loses to a bare-second sibling:
/// `"10:00:00Z"` sorts ABOVE `"10:00:00.9Z"` because `'Z'` (0x5A) > `'.'`
/// (0x2E). MAX therefore returns the wrong row for the loss, the ack at `.5Z`
/// out-ranks it, and the banner drops on a tenant that never acknowledged
/// anything.
#[test]
fn max_time_wall_is_a_lexicographic_compare_on_a_varchar() {
    let (_tmp, db) = db_only_tenant("lex-max");
    // Two loss rows in the same second. The NEWEST is `.9Z`; the bare-second
    // one is older but sorts higher as a string.
    insert_audit_row(
        &db,
        "db.durability_loss_detected",
        "2026-08-12T10:00:00Z",
        1,
    );
    insert_audit_row(
        &db,
        "db.durability_loss_detected",
        "2026-08-12T10:00:00.9Z",
        2,
    );
    // The operator acknowledged BEFORE the newest loss — so it does not cover it.
    insert_audit_row(
        &db,
        "db.durability_alert_acknowledged",
        "2026-08-12T10:00:00.5Z",
        3,
    );

    assert!(
        alert_after_reopen(&db),
        "R3-N1 REGRESSION: the banner is DOWN. The newest loss is at .9Z and the only \
         acknowledgement is at .5Z — EARLIER — so it cannot cover it. This is `SELECT \
         MAX(time_wall)` doing a LEXICOGRAPHIC compare on a VARCHAR: \"10:00:00Z\" sorts ABOVE \
         \"10:00:00.9Z\" ('Z' 0x5A > '.' 0x2E), so MAX hands back the OLDER loss and the ack \
         out-ranks it. That is the exact comparison the `time` dependency was added to avoid, \
         and it fails toward BANNER DOWN. Parse the rows and `.max()` on OffsetDateTime, the \
         way `mirror_audit_times` already does."
    );
}

/// **The malformed-stamp case.** One unparseable `time_wall` must cost us that
/// ROW, not the whole DB-side verdict.
///
/// `SELECT MAX(...)` collapses the column to a single value BEFORE any parsing,
/// so if the lexicographic winner happens to be malformed the parse drops it —
/// and takes every good row with it. Here a perfectly good loss row sits right
/// beside it and the banner still goes down.
#[test]
fn a_malformed_time_wall_on_the_loss_row_fails_toward_banner_down() {
    let (_tmp, db) = db_only_tenant("malformed-max");
    insert_audit_row(
        &db,
        "db.durability_loss_detected",
        "2026-08-12T10:00:00Z",
        1,
    );
    // Malformed, and lexicographically the highest ('~' 0x7E tops every digit).
    insert_audit_row(&db, "db.durability_loss_detected", "~not-a-timestamp", 2);

    assert!(
        alert_after_reopen(&db),
        "R3-N1 REGRESSION: the banner is DOWN even though a well-formed, unacknowledged \
         `db.durability_loss_detected` row is present. `SELECT MAX(time_wall)` collapses the \
         column BEFORE anything is parsed, so a single malformed stamp that wins the \
         lexicographic compare is selected, fails to parse, and takes the entire DB-side loss \
         verdict with it. One bad row must cost that ROW only. Select the rows, parse each, and \
         `.max()` the ones that parsed."
    );
}

/// **R2-B1's both-store rule, pinned directly** — mirror holds the loss, DB
/// holds the acknowledgement, and the banner must be DOWN.
///
/// This exists because R3-N2 showed the route-level test that used to cover it
/// had gone vacuous. That test staged a "permanently frozen mirror", which
/// serve never actually reaches: the boot reconcile
/// (`ensure_consistent_with_db`) attempts a gated auto-heal on EVERY boot and,
/// on success, replays the DB up to the mirror head — un-freezing `sync_mirror`
/// so the acknowledgement reaches the mirror after all. With the real boot
/// order in the path, deleting the DB half no longer turned it red.
///
/// So the DB half is **defence in depth**, not the common path, and this pins
/// it as exactly that: the split-stores state is constructed directly rather
/// than by pretending production sits in it. The window it covers is real but
/// narrow — a mid-process `sync_mirror` divergence, or any moment between the
/// acknowledgement and the next boot's reconcile, where the DB is the only
/// store holding the ack.
///
/// # D7.6 (2026-08-13) — this is now a LEGACY-ROW pin, and it had to be
///
/// It used to stage the split by firing the fence for real, on the strength of
/// the fence writing a `db.durability_loss_detected` ledger row. §15.3 moved
/// that writer to the non-chained marker, so no code path in this tree appends
/// that kind any more and the old staging died at its own precondition
/// (`expect("precondition: the loss row reached the mirror")`) — the failure
/// that proves the routing landed.
///
/// The PROPERTY is untouched and still load-bearing, which is why the pin
/// stayed rather than being deleted with the writer. `restore_durability_alert`
/// keeps both ledger readers for backward compatibility: a prod tenant
/// recovered from incident 00012 may already hold such a row, and retiring the
/// reader would silently stop re-raising it. A reader nothing writes to is
/// exactly the kind that rots unnoticed, so the row is now staged DIRECTLY —
/// appended through the Handle so the guard's drop mirrors it, then deleted
/// from the DB the way a truncation deletes it. That is a more faithful model
/// of the case anyway: what re-derivation meets on such a tenant is a persisted
/// row, not one this process just wrote.
///
/// Note the marker is deliberately NOT involved. If the fence fired here its
/// marker record would hold the banner up on its own and the ledger halves
/// would never be consulted — the test would pass without testing anything.
///
/// Mutation-verified: replace `self.db_audit_times()` with
/// `DurabilityAuditTimes::default()` and this goes RED; drop the mirror half
/// instead and it goes red at its own precondition.
#[test]
fn an_ack_that_reached_only_the_db_still_clears_a_loss_that_reached_only_the_mirror() {
    let tmp = Tmp::new("split-stores");
    let db = tmp.db();
    seed(&db);

    // Stage a LEGACY on-chain loss row into BOTH stores: appended through the
    // Handle, so the guard's drop runs the lockstep mirror sync that carries it
    // to the mirror. This is the shape a tenant recovered from incident 00012
    // arrives with.
    {
        let h = handle(&db);
        commit_one(&h, "before");
        commit_legacy_loss_row(&h);
        h.durable_ack()
            .expect("healthy ack — no fold happened here");
        assert!(
            h.durability_alert().is_none(),
            "precondition: nothing in this process fired a detector; the row is staged, not \
             detected, so the marker must be untouched"
        );
    }
    let mirror = mirror_path_for(&db);
    let mirrored = aberp_audit_ledger::read_mirror_entries(&mirror).expect("read mirror");
    let loss = mirrored
        .iter()
        .find(|e| e.kind == "db.durability_loss_detected")
        .expect("precondition: the loss row reached the mirror");
    let loss_time = loss.time_wall.clone();

    // Now split the stores: delete the loss from the DB (what a truncation
    // does) and write the acknowledgement to the DB ONLY (what happens while
    // the mirror is refusing appends). The mirror is left untouched.
    {
        let conn = Connection::open(&db).expect("open to split the stores");
        conn.execute(
            "DELETE FROM audit_ledger WHERE kind = 'db.durability_loss_detected'",
            [],
        )
        .expect("drop the loss row from the DB");
    }
    // An acknowledgement strictly AFTER the loss.
    let ack_time = {
        let t: String = loss_time.clone();
        // Same instant plus a second, formatted the way the ledger formats.
        let parsed =
            time::OffsetDateTime::parse(&t, &time::format_description::well_known::Rfc3339)
                .expect("the mirror's time_wall parses");
        (parsed + time::Duration::seconds(1))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format")
    };
    insert_audit_row(&db, "db.durability_alert_acknowledged", &ack_time, 9_000);

    let h = Handle::open_default(&db, tenant()).expect("reopen");
    assert!(
        h.durability_alert().is_none(),
        "R2-B1 REGRESSION: the loss is in the MIRROR only and the acknowledgement is in the DB \
         only, and the banner is still up. Re-derivation must consult BOTH stores for what each \
         is authoritative about — the mirror survives a truncation and holds the loss, the DB \
         is what still accepts writes while the mirror is refusing appends and holds the ack. \
         Reading the mirror alone makes the Acknowledge button inert in exactly that window."
    );
}

// ── D7.6 — THE DIAGNOSTIC GOES OFF-CHAIN, AND THE FENCE IS ARMED ────────────
//
// ADR-0110 §15.3 named routing D7's durability row to the non-chained marker a
// PRECONDITION for arming the fence, not an optional tidy-up. The mechanism is
// D5-B1 with a different trigger: a WAL truncation is exactly what regresses
// the DB's audit head below the append-only mirror's, so an `audit_ledger`
// append at that moment consumes a seq the mirror already holds a DIFFERENT
// entry for. The chains fork; the next boot's gated auto-heal proves benignness
// by matching head `entry_hash`es and REFUSES; `ensure_consistent_with_db`
// answers `MirrorAheadOfDb` and `serve` exits non-zero.
//
// Armed, that turns the alarm into the brick. These pins are what say it is
// dead.

/// Every line of the durability-alert marker, or `[]` if there is none.
fn marker_lines(h: &Handle) -> Vec<String> {
    match std::fs::read_to_string(h.durability_marker_path()) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("read the durability-alert marker: {e}"),
    }
}

/// Count of `db.durability_loss_detected` rows in the DB, read on a fresh
/// connection so it sees what a later boot would see.
fn ledger_loss_rows(db: &Path) -> u64 {
    let conn = Connection::open(db).expect("open to count loss rows");
    conn.query_row(
        "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'db.durability_loss_detected'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v as u64)
    .expect("count the loss rows")
}

/// Serve's boot mirror reconcile, run exactly where `serve::run` runs it: on
/// its own boot-phase connection, BEFORE any Handle exists.
fn boot_reconcile(db: &Path) -> Result<(), String> {
    let conn = Connection::open(db).expect("boot-phase open");
    ensure_schema(&conn).expect("boot schema");
    let out = aberp_audit_ledger::ensure_consistent_with_db(&conn, &mirror_path_for(db))
        .map(|_| ())
        .map_err(|e| e.to_string());
    drop(conn);
    out
}

/// **A fired fence records the MARKER and consumes no ledger seq.**
///
/// The positive half of the routing change: the alarm still reaches the
/// operator on exactly the surface D7 built (the sticky alert, hence
/// `GET /health durability_alert`, hence the banner), the episode is durable,
/// and the hash-chained ledger is untouched in BOTH stores.
///
/// The trigger column is asserted explicitly. It is the only thing that tells a
/// forensic reader whether an episode came from the WAL fence or from D5's
/// mirror freeze, and both write into the same file.
///
/// Mutation-verified: restore the `emit_durability_loss_audit` call in
/// `raise_durability_alert` and the `ledger_loss_rows` assertion goes RED; drop
/// the `record_loss` call and the marker assertions go red while the ack still
/// fails — which is the split that matters, because the ack failing is what a
/// casual reading would take as proof the alarm landed.
#[test]
fn an_armed_fence_records_the_marker_and_consumes_no_ledger_seq() {
    let tmp = Tmp::new("fence-marker");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    commit_one(&h, "before");
    h.durable_ack().expect("healthy ack");
    assert!(marker_lines(&h).is_empty(), "precondition: a quiet marker");

    foreign_open_and_close(&db);
    commit_one(&h, "after");
    expect_breach(
        h.durable_ack().expect_err("the armed fence fires"),
        WalBreach::WalVanished,
    );

    let alert = h.durability_alert().expect("the sticky alert must be up");
    assert_eq!(alert.breach, WalBreach::WalVanished);
    assert!(
        alert.message.contains("Stop and recover"),
        "the banner text must carry the operator instruction, got: {}",
        alert.message
    );

    let lines = marker_lines(&h);
    assert_eq!(
        lines.len(),
        1,
        "exactly one marker record for the episode, got {lines:?}"
    );
    assert!(
        lines[0].starts_with("v1\tloss\t"),
        "the marker record must be a versioned loss line, got: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\twal_truncated_under_writer\t"),
        "the record must name D7's trigger so forensics can tell it from D5's \
         audit_mirror_sync_refused, got: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\twal_vanished\t"),
        "the record must carry the machine breach code that was DETECTED, got: {}",
        lines[0]
    );

    assert_eq!(
        ledger_loss_rows(&db),
        0,
        "D5-B1 APPLIED TO D7: the fence's diagnostic must NOT consume a ledger seq. A truncation \
         is exactly what regresses the DB head below the mirror's, so an append here lands at a \
         seq the mirror holds a different entry for — the chains fork and the next boot's gated \
         auto-heal refuses. The alarm that says 'stop and recover' must not be what stops the \
         recovery."
    );
    let mirrored =
        aberp_audit_ledger::read_mirror_entries(&mirror_path_for(&db)).expect("read the mirror");
    assert!(
        !mirrored
            .iter()
            .any(|e| e.kind == "db.durability_loss_detected"),
        "and not into the mirror either — the ledger is one chain across two stores"
    );
}

/// **THE BRICK IS DEAD — an armed fence must not flip the tenant into the
/// equal-length divergence that refuses boot.**
///
/// This is the pin ADR-0110 §15.3 asked for by name: D5-B1's mechanism, driven
/// through the WAL-truncation fence, with the fence ARMED.
///
/// # Where the durable fork actually lives — and why the first cut of this pin
/// was VACUOUS
///
/// The obvious construction — fire the fence, then look for the diagnostic row
/// in the DB — proves nothing, and only the mutation showed it. **The DB copy
/// of that row cannot survive.** The fold that fires the fence severs the live
/// instance from its WAL, so every commit after it returns `Ok` with `wal_len`
/// pinned at 0 and the bytes reach no file. That is incident 00012, reproduced
/// in miniature by the very mutation meant to test for it: with the on-chain
/// append restored, the first version of this test stayed GREEN.
///
/// The fork is durable in the OTHER store. `emit_durability_loss_audit` relied
/// on `WriteGuard::drop`'s lockstep `sync_mirror` to carry the row to the
/// `fsync`'d mirror, and said so as a FEATURE — "the durable copy lands in the
/// mirror even when the DB copy does not". That is exactly what leaves the
/// append-only store one entry ahead of a DB that will never hold that seq.
///
/// # Why that bricks
///
/// A tenant does not sit still after an incident. The next thing to touch it —
/// a flock-fenced CLI one-shot, which runs no `ensure_consistent_with_db` of
/// its own — commits one business row, and it lands at exactly the seq the
/// mirror is holding the diagnostic at. The two stores are now the SAME LENGTH
/// with DIFFERENT content at the head, so `ensure_consistent_with_db`'s
/// equal-length arm compares head `entry_hash`es and REFUSES. `serve` exits
/// non-zero and does not boot. **The alarm that says "stop and recover" is what
/// stopped the operator recovering** — and arming the fence is what would have
/// made that reachable in production.
///
/// Without the diagnostic the same sequence is benign: nothing consumed that
/// seq, the CLI's row lands in both stores, and the tenant boots. The only
/// difference between the two runs is whether a machine-spawned diagnostic took
/// a ledger seq — which is Ervin's rule (2026-08-13) stated as a boot outcome.
///
/// # One thing deliberately NOT done
///
/// No business write between the fold and the ack. One would land in the mirror
/// and leave it ahead on its own, which is the pre-existing reconciler window
/// R5-N3 describes (ADR-0110 §15.5) — a confound that bricks the CLEAN run too
/// and would make this pin prove nothing about the diagnostic.
///
/// # Mutation verification, stated as it actually behaves
///
/// Restore the `emit_durability_loss_audit` call in `raise_durability_alert`
/// and this goes RED — at the **mirror assertion**, which fires first because
/// it is the earlier and more specific control.
///
/// The boot refusal downstream of it was verified separately rather than
/// assumed, by suppressing that assertion and letting the mutated run reach
/// `boot_reconcile`. It answers, verbatim:
///
/// ```text
/// audit-ledger mirror is unrecoverable (mirror head entry_hash diverges
/// from the DB at equal length (seq=4)); the original was preserved to
/// …/aberp.duckdb.audit.log.corrupt-….bak
/// ```
///
/// Note WHICH refuse arm that is: the **equal-length** one, not
/// `MirrorAheadOfDb`. The CLI's business row brought the DB level with the
/// mirror, so the reconciler compares head `entry_hash`es and finds them
/// different — the arm ADR-0110 §15.5 (R5-N3) describes. Recorded precisely
/// because a docstring that named the wrong arm would send the next reader
/// looking at the wrong code.
///
/// Both CONTROLS matter — a run where the fence never fired boots cleanly too,
/// and would have been just as green before the change.
#[test]
fn the_d5_b1_scenario_driven_through_the_armed_fence_must_boot_cleanly() {
    let tmp = Tmp::new("b1-fence-boot");
    let db = tmp.db();
    seed(&db);

    // ── The incident: a live tenant in lockstep meets a foreign fold ────────
    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
        commit_one(&h, "three");
        h.durable_ack().expect("healthy ack in lockstep");

        // The GROUP-A shape verbatim. Its close folds and truncates the WAL,
        // which arms the breach — and from here on is what stops this
        // instance's writes reaching a file at all.
        foreign_open_and_close(&db);

        expect_breach(
            h.durable_ack().expect_err(
                "CONTROL: the armed fence must fire — otherwise every assertion below is about \
                 a tenant that never had an incident",
            ),
            WalBreach::WalVanished,
        );
        assert!(
            marker_lines(&h)
                .iter()
                .any(|l| l.contains("\twal_truncated_under_writer\t")),
            "CONTROL: the episode must actually have been recorded off-chain"
        );
    }

    // The alarm consumed nothing, in EITHER store. The mirror half is the
    // load-bearing one — see the docs.
    assert_eq!(
        ledger_loss_rows(&db),
        0,
        "the fence's diagnostic must not consume a ledger seq in the DB"
    );
    let mirrored =
        aberp_audit_ledger::read_mirror_entries(&mirror_path_for(&db)).expect("read the mirror");
    assert!(
        !mirrored
            .iter()
            .any(|e| e.kind == "db.durability_loss_detected"),
        "D5-B1 APPLIED TO D7: the diagnostic reached the MIRROR. That is where the old on-chain \
         path's row actually survived — the DB copy is lost to the same truncation that fired \
         the fence, but the mirror is fsync'd, so the alarm leaves the append-only store one \
         entry ahead of a DB that will never hold that seq. The next write to touch this tenant \
         then collides with it."
    );

    // ── What happens next: a flock-fenced CLI one-shot touches the tenant ───
    // It runs no boot reconcile of its own, so it writes on top of whatever the
    // incident left behind. One business row.
    {
        let h = Handle::open_default(&db, tenant()).expect("a CLI one-shot opens the tenant");
        commit_one(&h, "the-next-thing-that-touches-the-tenant");
    }

    // ── And then the tenant is booted ───────────────────────────────────────
    boot_reconcile(&db).expect(
        "D5-B1 REGRESSION VIA D7: the tenant no longer boots. The armed fence's diagnostic \
         consumed a ledger seq and reached the fsync'd mirror; the DB, severed from its WAL by \
         the same fold, never got it. The next write landed at that seq with different content, \
         so the two stores are the same length with different head hashes and \
         `ensure_consistent_with_db`'s equal-length arm REFUSES — serve exits non-zero. The \
         alarm that says 'stop and recover' must not be the thing that stops the recovery. \
         Routing the diagnostic to the non-chained marker is the precondition ADR-0110 §15.3 \
         made for arming this fence.",
    );
}

/// **The alert survives a restart, carrying the breach it was DETECTED as.**
///
/// Deliberately driven with an INODE swap rather than a vanished WAL, so the
/// expected code is `MainReplaced`. `WalVanished` is the ledger-sourced path's
/// hard-coded default (D5-N2), so a `WalVanished` scenario would pass whether
/// or not the marker's stored code is consulted at all — vacuous exactly where
/// the value is. A recovery turns on this distinction: "the WAL was truncated"
/// and "the main DB file was swapped underneath us" are different incidents.
///
/// Mutation-verified: drop the `record_loss` call from `raise_durability_alert`
/// and the alert does not survive at all; hard-code `WalVanished` in
/// `restore_durability_alert` and the breach half alone goes red.
#[test]
fn the_fence_alert_survives_a_restart_with_the_breach_it_was_detected_as() {
    let tmp = Tmp::new("fence-restart");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "before");
        h.durable_ack().expect("healthy ack");

        // Swap the main DB file for a different inode with the same contents —
        // the A2 shape `swapping_the_inode_between_commit_and_ack_must_fail_the_ack`
        // uses.
        let copy = {
            let mut os = db.as_os_str().to_owned();
            os.push(".swapped");
            PathBuf::from(os)
        };
        std::fs::copy(&db, &copy).expect("copy the main file");
        std::fs::rename(&copy, &db).expect("swap the inode");

        expect_breach(
            h.durable_ack()
                .expect_err("the armed fence fires on the swap"),
            WalBreach::MainReplaced,
        );
    }

    let reopened = Handle::open_default(&db, tenant()).expect("reopen");
    let alert = reopened.durability_alert().expect(
        "D7.4b REGRESSION: the alert did not survive the restart. A restart is not an \
                 acknowledgement — the banner tells the operator to stop and recover, so the \
                 restart it asks for must not be the mute button.",
    );
    assert_eq!(
        alert.breach,
        WalBreach::MainReplaced,
        "D5-N2 REGRESSION: the restored alert reports the wrong breach. The marker stores the \
         code that was DETECTED; reporting the hard-coded WalVanished default instead loses the \
         one distinction a recovery turns on — a swapped main file is not a truncated WAL."
    );
}

/// **An acknowledged fence alert stays down across a restart.**
///
/// The other half of the sticky contract: the alarm must be clearable, and the
/// clear must be durable. It goes through the same `clear_durability_alert` D5
/// uses — one acknowledge path for both triggers, which is the whole reason the
/// routing change reuses the marker rather than building a second store.
///
/// Mutation-verified: skip the `record_ack` call in `clear_durability_alert`
/// and the banner comes back on the reopen.
#[test]
fn an_acknowledged_fence_alert_stays_down_across_a_restart() {
    let tmp = Tmp::new("fence-ack");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "before");
        h.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db);
        commit_one(&h, "after");
        let _ = h.durable_ack().expect_err("the armed fence fires");
        assert!(
            h.durability_alert().is_some(),
            "precondition: the banner is up"
        );
        h.clear_durability_alert()
            .expect("the operator acknowledges");
        assert!(
            h.durability_alert().is_none(),
            "and it goes down in-process"
        );
    }

    let reopened = Handle::open_default(&db, tenant()).expect("reopen");
    assert!(
        reopened.durability_alert().is_none(),
        "the acknowledgement must be DURABLE. If the banner is back, the operator cleared it, \
         restarted, and found it up again with nothing left to do about it — which is the alarm \
         they learn to route around."
    );
}

/// **Both marker triggers coexist: one file, one re-derivation, one
/// acknowledge — and each episode keeps its own breach code.**
///
/// D5 (`audit_mirror_sync_refused` / `audit_mirror_frozen`) and D7
/// (`wal_truncated_under_writer` / a WAL breach) now write into the SAME
/// non-chained marker. That is deliberate — a second store would mean a second
/// reader, a second acknowledge path and a second way to get the ordering wrong
/// — so what has to hold is that they do not tread on each other: both records
/// survive, the NEWEST one decides which breach `/health` and the banner report,
/// and one acknowledgement out-ranks both.
///
/// The ordering assertion is the load-bearing one. `restore_durability_alert`
/// picks the breach by matching the marker's newest loss instant against the
/// overall newest loss, so a reader that took the FIRST record, or the last
/// line of the file regardless of stamp, would report the wrong incident to the
/// operator while looking entirely healthy under a single-trigger test.
///
/// Mutation-verified: change `note_loss` to keep the FIRST loss rather than the
/// newest and the breach assertion goes RED; drop the marker's `ack` half and
/// the final assertion goes red.
#[test]
fn both_marker_triggers_coexist_and_the_newest_decides_the_breach() {
    let tmp = Tmp::new("both-triggers");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
        h.durable_ack().expect("healthy ack");

        // D5 first: regress the DB head below the mirror's THROUGH the Handle,
        // so the guard's drop meets `MirrorDivergent` and raises the freeze.
        {
            let guard = h.write().expect("acquire the shared writer");
            guard
                .execute_batch("DELETE FROM audit_ledger WHERE seq > 1;")
                .expect("regress the DB audit head");
        }
        assert_eq!(
            h.durability_alert().map(|a| a.breach),
            Some(WalBreach::AuditMirrorFrozen),
            "precondition: D5 fired first, so the sticky alert holds ITS breach"
        );

        // Then D7, strictly later: the fold arms the fence and the next ack
        // fires it. The sticky alert is `get_or_insert` — the operator needs to
        // know when the tenant STARTED losing writes — so in-process it keeps
        // D5's. The marker holds both.
        foreign_open_and_close(&db);
        let _ = h.durable_ack().expect_err("the armed fence fires too");

        let lines = marker_lines(&h);
        assert_eq!(
            lines.len(),
            2,
            "both episodes must be recorded, got {lines:?}"
        );
        assert!(
            lines[0].contains("\taudit_mirror_sync_refused\t")
                && lines[0].contains("\taudit_mirror_frozen\t"),
            "the first record is D5's, with D5's breach: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("\twal_truncated_under_writer\t")
                && lines[1].contains("\twal_vanished\t"),
            "the second is D7's, with D7's breach — the two must not collapse into one \
             vocabulary: {}",
            lines[1]
        );
        assert_eq!(
            ledger_loss_rows(&db),
            0,
            "and NEITHER trigger may consume a ledger seq"
        );
    }

    // Re-derivation: the NEWEST loss decides the reported breach, which here is
    // D7's.
    let reopened = Handle::open_default(&db, tenant()).expect("reopen");
    assert_eq!(
        reopened.durability_alert().map(|a| a.breach),
        Some(WalBreach::WalVanished),
        "the restored alert must report the NEWEST episode's breach. Reporting the older one \
         sends the operator after the wrong incident — and a single-trigger test cannot see the \
         difference, which is why this pin exists."
    );

    // And ONE acknowledgement covers both, because there is one alert.
    reopened
        .clear_durability_alert()
        .expect("the operator acknowledges");
    let again = Handle::open_default(&db, tenant()).expect("reopen after the ack");
    assert!(
        again.durability_alert().is_none(),
        "one acknowledgement must out-rank every loss older than it, whichever trigger raised \
         them — two triggers must not mean two banners the operator has to clear separately"
    );
}

// ── D7.6 — NO FALSE POSITIVE ON A HEALTHY *ARMED* BOX ───────────────────────
//
// The pins above this section already cover boot, boot onto a pre-existing WAL,
// the first ack after a boot fold, concurrent daemon writes, a legitimate
// reopen and a long healthy run — all against `armed_config()`, so arming the
// default did not change what they prove. These two close the shapes that were
// only ever exercised elsewhere, and that a reader of this file would otherwise
// have to take on trust.

/// **DuckDB's own auto-checkpoint must not fire the fence.**
///
/// The fence's premise is "our WAL only ever grows", and that is a consequence
/// of the F-A pragmas — `disable_checkpoint_on_shutdown` plus the
/// `wal_autocheckpoint` raise — and of nothing else. This drives enough
/// committed bytes through the Handle to pass DuckDB's stock 16 MiB
/// auto-checkpoint threshold several times over and requires silence.
///
/// If a future change loses those pragmas on the Handle's own connections, the
/// engine folds its own WAL legitimately and an armed fence reports the
/// engine's bookkeeping as durability loss — on a perfectly healthy box, under
/// load, i.e. exactly when it does most damage. `the_fence_is_pinned_to_the_
/// production_posture` asserts the pragmas are configured; this asserts the
/// behaviour they are configured for.
#[test]
fn sustained_writes_past_the_auto_checkpoint_threshold_must_not_fire() {
    let tmp = Tmp::new("autocheckpoint");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    {
        let g = h.write().expect("writer");
        g.execute_batch("CREATE TABLE IF NOT EXISTS bulk(x INTEGER, pad VARCHAR);")
            .expect("bulk table");
    }
    // ~64 MiB of WAL-resident payload — four times DuckDB's stock 16 MiB
    // wal_autocheckpoint, so a Handle without the raise would fold repeatedly.
    for round in 0..8 {
        {
            let g = h.write().expect("writer");
            g.execute_batch(
                "INSERT INTO bulk SELECT i, repeat('x', 1024) FROM range(0, 8192) t(i);",
            )
            .expect("bulk insert");
        }
        h.durable_ack().unwrap_or_else(|e| {
            panic!(
                "round {round}: the armed fence fired on a HEALTHY box \
                 under sustained load. DuckDB's stock 16 MiB wal_autocheckpoint folded the WAL \
                 because this Handle's connections lost the F-A pragmas — and an armed fence \
                 reads the engine's own bookkeeping as durability loss. Error: {e}"
            )
        });
    }
    assert!(
        h.durability_alert().is_none(),
        "and no banner: sustained committed load on a healthy tenant must be silent"
    );
    assert!(marker_lines(&h).is_empty(), "and nothing recorded");
}

/// **The `.creating-*` staging sweep must not fire the fence.**
///
/// `aberp_snapshot::crash_safe::cleanup_stale_staging` deletes `<db>.creating-*`
/// siblings at boot (ADR-0095 §2), and it runs while a Handle may be live. It
/// selects by filename prefix, and `<db>.wal` does not carry the `.creating-`
/// infix — but "the sweep is prefix-scoped" is a property of one `starts_with`,
/// and an armed fence turns a widened prefix into a money-path outage rather
/// than a stray deletion. Modelled here as the sweep's own primitive (write
/// staging litter beside a live tenant, remove everything matching) so the pin
/// does not depend on `aberp-db` gaining a dependency on `aberp-snapshot`.
#[test]
fn the_stale_staging_sweep_must_not_fire() {
    let tmp = Tmp::new("staging-sweep");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);
    commit_one(&h, "before");
    h.durable_ack().expect("healthy ack");
    assert!(wal_len(&db) > 0, "precondition: the commit is WAL-resident");

    // Litter from a crashed prior provision, beside the live DB.
    let stem = db
        .file_name()
        .and_then(|n| n.to_str())
        .expect("db filename");
    let parent = db.parent().expect("tenant dir");
    for tag in ["a", "b"] {
        std::fs::write(
            parent.join(format!("{stem}.creating-{tag}.duckdb")),
            b"litter",
        )
        .expect("write staging litter");
    }

    // The sweep, verbatim in shape: remove every sibling whose name starts with
    // `<stem>.creating-`.
    let prefix = format!("{stem}.creating-");
    for entry in std::fs::read_dir(parent)
        .expect("read the tenant dir")
        .flatten()
    {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    commit_one(&h, "after");
    h.durable_ack().expect(
        "the staging sweep fired the armed fence. It must select `<db>.creating-*` siblings \
         ONLY — a prefix that also catches `<db>.wal` deletes the live WAL under a running \
         Handle, and armed that is the next invoice issuance failing its durable_ack.",
    );
    assert!(h.durability_alert().is_none(), "and no banner");
}

/// **A flock-refused CLI must not fire the fence.**
///
/// D9's answer to the CLI-against-live class is mutual exclusion, not
/// migration: `serve` holds the F-E whole-DB writer flock for its process
/// lifetime, and every DB-mutating one-shot takes it before opening the tenant
/// DB — so it REFUSES rather than opening a default-pragma connection whose
/// close folds the live WAL. `adr0110_d9_flock_shape.rs` pins that every command
/// has the acquire in the right place; this pins the consequence the arming
/// decision actually rests on, which is that the refusal leaves the fence
/// silent.
///
/// The contrast is the point: `foreign_open_and_close` — the same CLI shape
/// WITHOUT the flock — is what `the_group_a_shape_must_fail_the_ack` proves
/// fires. So this is not "nothing happened", it is "the flock is what stopped
/// it happening".
#[test]
fn a_flock_refused_cli_must_not_fire() {
    use aberp_db::db_writer_lock;

    let tmp = Tmp::new("flock-refused");
    let db = tmp.db();
    seed(&db);

    // Serve's posture: the whole-DB writer flock, held for the process.
    let _serve_lock = db_writer_lock::acquire_or_refuse(&db, TENANT, "serve")
        .expect("serve takes the whole-DB writer flock");

    let h = handle(&db);
    commit_one(&h, "before");
    h.durable_ack().expect("healthy ack");
    let wal_before = wal_len(&db);
    assert!(wal_before > 0, "precondition: the commit is WAL-resident");

    // The one-shot arrives and is REFUSED before it can open the DB. This is
    // the whole of D9: no second connection exists, so no close can fold.
    assert!(
        db_writer_lock::acquire_or_refuse(&db, TENANT, "rebuild-stock-cache").is_err(),
        "D9 REGRESSION: a DB-mutating one-shot acquired the whole-DB writer flock while serve \
         holds it. It would then open a default-pragma connection on the live tenant DB, and \
         its close would fold and truncate the Handle's WAL — the exact GROUP-B hazard arming \
         the fence rests on being closed."
    );

    assert_eq!(
        wal_len(&db),
        wal_before,
        "the refusal must not have touched the WAL at all"
    );
    commit_one(&h, "after");
    h.durable_ack()
        .expect("and the armed fence stays silent: the refused one-shot never opened the DB");
    assert!(h.durability_alert().is_none(), "no banner on a refused CLI");
    assert!(marker_lines(&h).is_empty(), "and nothing recorded");
}
