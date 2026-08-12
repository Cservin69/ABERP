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

/// A Handle with the D7 fence **ARMED**. Every fence test uses this, because
/// `HandleConfig::default()` ships it DISARMED (B1) and always will until the
/// GROUP-A opener sweep lands — see [`the_fence_ships_disarmed_by_default`] and
/// [`with_the_fence_disarmed_the_group_a_shape_does_not_fail_the_ack`], which
/// pin that other state.
fn handle(db: &Path) -> std::sync::Arc<Handle> {
    Handle::open(db, tenant(), armed_config()).expect("open shared Handle (fence ARMED)")
}

fn armed_config() -> HandleConfig {
    HandleConfig {
        wal_fence_enabled: true,
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

/// **The defect primitive.** A foreign DuckDB instance on the same path,
/// opened and closed with DEFAULT pragmas. Its close folds and truncates the
/// live Handle's WAL. This is `serve::read_invoice_total_gross_minor` before
/// PR #1, and `calibration_overview_request` / `resolve_recipient_email` /
/// `handle_quote_pipeline_status` today.
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
    h.clear_durability_alert();
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

// ── B1 — THE FENCE SHIPS DISARMED ───────────────────────────────────────────

/// **The shipping default is OFF, and that is the whole point of B1.**
///
/// Three foreign GROUP-A openers are still live in-serve on this head
/// (`calibration_overview_request`, `resolve_recipient_email`,
/// `handle_quote_pipeline_status`). Each truncates this Handle's WAL on close.
/// With the fence ARMED before those are swept, an operator opening the
/// quote-calibration overview arms a breach, and the NEXT invoice issuance
/// fails `durable_ack` — a failure that PROPAGATES via `?`, because the D3-C
/// cut-gate enforces exactly that. A committed invoice would report as failed
/// and skip its NAV handoff.
///
/// So this default is load-bearing safety, not a toggle someone should tidy
/// away. Flip it only after the sweep.
#[test]
fn the_fence_ships_disarmed_by_default() {
    assert!(
        !HandleConfig::default().wal_fence_enabled,
        "ADR-0110 D7 / B1 REGRESSION: the WAL fence must default DISARMED until the GROUP-A          opener sweep lands. Armed on this head it converts a silent durability bug into          routine LOUD money-path failures: calibration-overview / invoice-email /          pricing-jobs each truncate the WAL, and the next issue-invoice or mark-paid then          fails its durable_ack and propagates — a committed invoice reported as failed, NAV          handoff skipped. That is worse than the bug it detects."
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
    // `open_default` — the production constructor, verbatim.
    let h = Handle::open_default(&db, tenant()).expect("open shared Handle (fence DISARMED)");

    commit_one(&h, "before");
    assert!(wal_len(&db) > 0, "precondition: the commit is WAL-resident");
    h.durable_ack().expect("healthy ack");

    // The defect fires — and with the fence off, nothing notices. That is
    // today's behaviour, deliberately preserved.
    foreign_open_and_close(&db);
    commit_one(&h, "after");

    h.durable_ack().expect(
        "B1 REGRESSION: with `wal_fence_enabled: false` the ack must behave exactly as it did          under D3 and SUCCEED. If this fails, the fence is armed on the default path and every          GROUP-A opener in serve becomes a money-path outage.",
    );
    assert!(
        h.durability_alert().is_none(),
        "B1 REGRESSION: a disarmed fence must raise no operator alert either — the banner would          be up on every box while the openers still ship"
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
    let h = Handle::open_default(&db, tenant()).expect("open handle");

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
