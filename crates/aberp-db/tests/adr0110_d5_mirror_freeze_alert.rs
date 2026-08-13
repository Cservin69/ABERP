//! **ADR-0110 D5 — a frozen audit mirror is a LOUD durability fault, not a
//! `warn!`.**
//!
//! # The defect
//!
//! When the audit mirror diverges from the DB — the mirror holds entries at
//! seqs the DB no longer has, which is exactly the post-WAL-truncation
//! signature — [`aberp_audit_ledger::sync_mirror`] answers `MirrorDivergent`
//! and appends **nothing**. `WriteGuard::drop` only `warn!`d on that, and its
//! warning said the mirror "will reconcile on the next write", which is FALSE
//! for this shape: the next write re-derives the same divergence and refuses
//! again. So the mirror silently FROZE for the rest of the process. Every audit
//! row after that point — including a D7 durability acknowledgement and any
//! later `db.durability_loss_detected` — reached the DB alone and never the
//! `fsync`'d store, and nothing surfaced it. That is the D7.4d/D7.4e residual,
//! and it is why the round-3 adversarial promoted D5 from optional to
//! load-bearing for the D7 alarm.
//!
//! # What this file pins
//!
//! [`a_live_process_mirror_divergence_raises_the_alert_and_audits_it`] is the
//! RED-first pin: the sticky alert goes up, `GET /health` therefore reports it,
//! and a `db.durability_loss_detected` row is appended.
//!
//! The other direction is the harder half, and it is the reason this needed
//! care at all. A mirror that is AHEAD at BOOT is NORMAL: it is what an
//! unclean stop leaves behind (the mirror is `fsync`'d, the DB's WAL tail is
//! not), and `serve` heals it on every boot with
//! [`aberp_audit_ledger::ensure_consistent_with_db`] *before* the shared Handle
//! opens. An alarm that fired on that would be up after half the crashes this
//! system is designed to survive, and an operator learns to ignore a banner
//! like that long before the day it is right. So:
//!
//! * [`a_boot_reconcile_that_heals_a_mirror_ahead_leaves_the_alarm_quiet`]
//! * [`a_divergence_this_handle_never_saw_agree_must_not_raise`]
//!
//! and [`the_freeze_is_audited_once_per_episode`] keeps a continuous fault from
//! becoming one loss row per write.
//!
//! # Mutation verification
//!
//! A pin that cannot go red is not a pin. Each is verified against a specific
//! mutation — see the per-test notes.
//!
//! # Scope
//!
//! `$TMPDIR` only. Nothing here touches `~/.aberp/**` or any tenant database.

use std::path::{Path, PathBuf};

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, mirror_path_for, Actor, BinaryHash, EventKind, LedgerMeta,
    TenantId,
};
use aberp_db::{Handle, HandleConfig, WalBreach};
use duckdb::Connection;

const TENANT: &str = "tenant-adr0110-d5-mirror-freeze";

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
            "aberp-adr0110-d5-{label}-{}-{nanos}-{}",
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

/// Seed an empty tenant DB with the audit schema and fold it — the boot state.
fn seed(db: &Path) {
    let conn = Connection::open(db).expect("seed open");
    ensure_schema(&conn).expect("seed schema");
    conn.execute_batch("CHECKPOINT;").expect("seed fold");
}

/// A Handle in the **production posture**: `HandleConfig::default()`, which
/// ships the D7 WAL fence DISARMED.
///
/// Every test here uses it, and that is the point rather than a convenience:
/// D5 raises an alarm and refuses no write, so unlike the fence it carries no
/// money-path-outage risk and is deliberately NOT gated on
/// `wal_fence_enabled`. Gating it would make it dead code in production, where
/// the flag is `false`. [`the_alarm_is_not_gated_on_the_disarmed_wal_fence`]
/// pins the premise.
fn handle(db: &Path) -> std::sync::Arc<Handle> {
    Handle::open(db, tenant(), HandleConfig::default()).expect("open shared Handle")
}

/// One committed audit row through the shared Handle — the shape every money
/// path takes. Returns after the guard has dropped, so the lockstep mirror sync
/// has run.
fn commit_one(h: &Handle, label: &str) {
    let meta = LedgerMeta::new(tenant(), BinaryHash::from_bytes([7u8; 32]));
    let mut guard = h.write().expect("acquire the shared writer");
    let tx = guard.conn().transaction().expect("begin");
    let actor = Actor::from_local_cli(format!("ulid-{label}"), "tester");
    append_in_tx(
        &tx,
        &meta,
        // A neutral probe kind: never collides with D5's own
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

/// **The defect primitive.** Regress the DB's audit head below the mirror's, by
/// dropping the tail the mirror already holds.
///
/// This is the end state a foreign GROUP-A opener produces when its close folds
/// and truncates the live WAL: the WAL-resident audit tail is gone from the DB,
/// while the append-only, `fsync`'d mirror still has it. Produced here by SQL
/// because the truncation itself is a race; the state it leaves — and therefore
/// what `sync_mirror` sees — is identical, and it is the state, not the
/// mechanism, that D5 responds to.
fn regress_db_head(conn: &Connection, keep_seq: u64) {
    conn.execute_batch(&format!("DELETE FROM audit_ledger WHERE seq > {keep_seq};"))
        .expect("regress the DB audit head");
}

/// The same regression, applied THROUGH the shared Handle so the guard's drop
/// runs the lockstep `sync_mirror` on the regressed DB — i.e. the divergence
/// arises inside a live, already-in-lockstep process.
fn regress_db_head_through_handle(h: &Handle, keep_seq: u64) {
    let guard = h.write().expect("acquire the shared writer");
    regress_db_head(&guard, keep_seq);
    drop(guard);
}

/// Every `db.durability_loss_detected` payload currently in the DB, as UTF-8.
fn loss_payloads(h: &Handle) -> Vec<String> {
    let conn = h.read().expect("read clone");
    let mut stmt = conn
        .prepare("SELECT payload FROM audit_ledger WHERE kind = ? ORDER BY seq")
        .expect("prepare");
    let rows = stmt
        .query_map([EventKind::DbDurabilityLossDetected.as_str()], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .expect("query");
    rows.map(|r| String::from_utf8(r.expect("row")).expect("payload is UTF-8 JSON"))
        .collect()
}

/// Number of entries in the mirror file (one JSON-Lines record per line).
fn mirror_entries(db: &Path) -> u64 {
    aberp_audit_ledger::read_mirror_entries(&mirror_path_for(db))
        .expect("read the mirror")
        .len() as u64
}

/// The DB's audit head seq.
fn db_head_seq(h: &Handle) -> u64 {
    let conn = h.read().expect("read clone");
    conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM audit_ledger", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|v| v as u64)
    .expect("head seq")
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

// ── THE RED-FIRST PIN ───────────────────────────────────────────────────────

/// **The alarm D7.4d said was missing.**
///
/// A Handle in lockstep with its mirror; then the DB head regresses below the
/// mirror's; then the next guard drops. `sync_mirror` refuses the append, and
/// that refusal must reach the operator on the same surface D7 built: the
/// sticky alert (hence `GET /health durability_alert`, hence the red banner)
/// AND a `db.durability_loss_detected` audit row naming the trigger.
///
/// Before D5 every assertion below the divergence failed: the drop `warn!`d and
/// nothing else, so the mirror froze with no alert, no audit row, and a log
/// line claiming it would "reconcile on the next write".
///
/// Mutation-verified: replace the `MirrorDivergent` arm in `WriteGuard::drop`
/// with the old unconditional `warn!` and this goes RED at
/// `expect("the alert must be up")`, while every other test in this file and
/// the whole D7 fence suite stay green — which is exactly the gap it covers.
#[test]
fn a_live_process_mirror_divergence_raises_the_alert_and_audits_it() {
    let tmp = Tmp::new("live-divergence");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    // In lockstep: two committed rows, both mirrored.
    commit_one(&h, "one");
    commit_one(&h, "two");
    assert!(
        h.durability_alert().is_none(),
        "a healthy tenant in lockstep must not raise anything"
    );

    // The DB loses the tail the mirror has. The guard's drop syncs into that.
    regress_db_head_through_handle(&h, 1);

    let alert = h
        .durability_alert()
        .expect("the alert must be up: the mirror REFUSED the append and is now frozen");
    assert_eq!(
        alert.breach,
        WalBreach::AuditMirrorFrozen,
        "the operator must be told WHICH durability fault this is"
    );
    assert!(
        alert.message.contains("Stop and recover"),
        "the banner text must carry the operator instruction, got: {}",
        alert.message
    );

    let payloads = loss_payloads(&h);
    assert_eq!(
        payloads.len(),
        1,
        "exactly one db.durability_loss_detected row, got {payloads:?}"
    );
    assert!(
        payloads[0].contains("\"trigger\":\"audit_mirror_sync_refused\""),
        "the payload must name D5's trigger so forensics can tell it from D7's \
         wal_truncated_under_writer, got: {}",
        payloads[0]
    );
    assert!(
        payloads[0].contains("\"breach\":\"audit_mirror_frozen\""),
        "payload must carry the machine breach code, got: {}",
        payloads[0]
    );
}

/// The alert must survive the restart the banner tells the operator to perform
/// — the D7.4a property, which D5 inherits by reusing D7's `EventKind` rather
/// than minting a sibling.
///
/// The mirror is frozen, so D5's loss row reaches the DB alone. `Handle::open`'s
/// re-derivation reads BOTH stores (D7 R2-B1) and the DB half is what carries
/// it here. A sibling `EventKind` would have had to be taught to both halves to
/// get this, and one that is not taught fails silently in exactly this test's
/// direction.
///
/// Mutation-verified: skip the `append_durability_loss_row` call in
/// `raise_mirror_freeze_alert` (keeping the in-memory alert) and this goes RED
/// while the pin above stays green up to its payload assertions — the in-process
/// banner alone looks identical until you restart.
#[test]
fn the_alert_survives_a_restart() {
    let tmp = Tmp::new("survives-restart");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
        regress_db_head_through_handle(&h, 1);
        assert!(h.durability_alert().is_some(), "precondition: alert raised");
    }

    // A new process, a new Handle, on the same tenant.
    let reopened = handle(&db);
    assert!(
        reopened.durability_alert().is_some(),
        "a restart is not an acknowledgement — the alert must be re-derived from the durable \
         audit record"
    );
}

// ── THE SILENT DIRECTION (the reason this needed care) ──────────────────────

/// **A benign boot-time mirror-ahead must NOT raise the alarm.**
///
/// This is the state `serve` meets on any unclean stop: the mirror is `fsync`'d
/// and kept the tail, the DB's WAL tail did not survive. `ensure_consistent_with_db`
/// runs BEFORE `open_tenant_handle` and heals it by replaying the mirror's tail
/// back into the DB. By the time any `WriteGuard` exists the two stores agree,
/// so the drop never sees a divergence — nothing to suppress, and nothing
/// suppressed. That ordering is what makes D5's discrimination structural in
/// `serve` rather than heuristic; it is itself pinned by
/// `apps/aberp/tests/index_desync_incident_20260803.rs`, which goes red if the
/// reconcile moves past `open_tenant_handle`.
///
/// If this ever goes red, the banner is up after ordinary crashes and the
/// operator stops reading it.
///
/// The lockstep assertion at the end is what makes this a pin on the HEAL and
/// not merely on the silence. Without it the test would pass for the wrong
/// reason — a Handle that never reached lockstep is quiet either way (that is
/// the next test) — and deleting the reconcile would leave it green. With it,
/// deleting the reconcile goes RED: the mirror never receives the post-boot
/// rows because `sync_mirror` is refusing them.
///
/// Mutation-verified in both halves: drop the `boot_reconcile` call and this
/// goes RED on the lockstep assertion; keep the reconcile and remove the
/// `lockstep_seen` guard from `WriteGuard::drop` and it stays GREEN, because
/// after a heal there is no divergence left to misread.
#[test]
fn a_boot_reconcile_that_heals_a_mirror_ahead_leaves_the_alarm_quiet() {
    let tmp = Tmp::new("benign-boot");
    let db = tmp.db();
    seed(&db);

    // A previous process wrote two rows; both are in the fsync'd mirror.
    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
    }
    // The unclean stop: the DB lost its WAL-resident tail, the mirror did not.
    {
        let conn = Connection::open(&db).expect("open to regress");
        regress_db_head(&conn, 1);
    }

    boot_reconcile(&db).expect("the gated auto-heal must resolve a clean mirror-ahead");

    let h = handle(&db);
    commit_one(&h, "after-boot-one");
    commit_one(&h, "after-boot-two");

    assert!(
        h.durability_alert().is_none(),
        "a mirror-ahead the boot reconcile HEALED is the normal post-crash path — raising here \
         would put the banner up after ordinary crashes and make it meaningless"
    );
    assert!(
        loss_payloads(&h).is_empty(),
        "and it must not write a durability-loss row either"
    );
    assert_eq!(
        mirror_entries(&db),
        db_head_seq(&h),
        "the heal must have put the two stores back in LOCKSTEP — a silent alarm over a mirror \
         that is still frozen would be the same bug wearing the benign case's clothes"
    );
}

/// **A divergence this Handle never saw agree must not raise it either.**
///
/// The Handle opened straight onto a diverged tenant — it never observed the
/// two stores in lockstep, so the divergence is a state it INHERITED, not one
/// that arose under its own writing. That is the boot reconciler's business
/// (in `serve` it is unreachable: the reconcile heals it or the boot is
/// refused), and it is the case a one-shot CLI on a tenant awaiting recovery
/// hits. The divergence is still logged loudly on every write; what it does not
/// do is claim THIS process detected a new durability loss.
///
/// This is the whole benign-vs-live discriminator, stated as a test.
///
/// Mutation-verified: delete the `lockstep_seen` guard from the `MirrorDivergent`
/// arm in `WriteGuard::drop` and this goes RED while every other pin here stays
/// green.
#[test]
fn a_divergence_this_handle_never_saw_agree_must_not_raise() {
    let tmp = Tmp::new("inherited-divergence");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
    }
    {
        let conn = Connection::open(&db).expect("open to regress");
        regress_db_head(&conn, 1);
    }

    // No boot reconcile. A fresh Handle, straight onto the diverged tenant.
    let h = handle(&db);
    commit_one(&h, "inherited-one");
    commit_one(&h, "inherited-two");

    assert!(
        h.durability_alert().is_none(),
        "this Handle never saw the two stores agree, so it has not WITNESSED a loss — the \
         inherited state belongs to the boot reconciler, which heals it or refuses the boot"
    );
    assert!(
        loss_payloads(&h).is_empty(),
        "and it must not write a durability-loss row either"
    );
}

/// A continuous fault must not become one audit row per write.
///
/// The divergence does not resolve inside the process, so every later drop sees
/// it again. Without the one-shot latch a busy tenant would bury its own ledger
/// under `db.durability_loss_detected` rows — and each of those is a write into
/// the store that is already the only one still accepting writes.
///
/// Mutation-verified: remove the `!freeze_reported` guard from the
/// `MirrorDivergent` arm and this goes RED with four rows instead of one.
#[test]
fn the_freeze_is_audited_once_per_episode() {
    let tmp = Tmp::new("once-per-episode");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    commit_one(&h, "one");
    commit_one(&h, "two");
    regress_db_head_through_handle(&h, 1);
    assert_eq!(loss_payloads(&h).len(), 1, "precondition: raised once");

    commit_one(&h, "three");
    commit_one(&h, "four");
    commit_one(&h, "five");

    assert_eq!(
        loss_payloads(&h).len(),
        1,
        "the freeze is one episode, not one per write"
    );
    assert!(
        h.durability_alert().is_some(),
        "and the sticky alert stays up throughout"
    );
}

/// The premise of every test above: this alarm runs in the production posture,
/// where the D7 WAL fence is DISARMED.
///
/// D7.6 ships `wal_fence_enabled = false` because the FENCE fails `durable_ack`
/// and a false positive there is a money-path outage. D5 raises an alarm and
/// refuses nothing, so it is ungated — the same reasoning that left the D7 boot
/// re-derivation ungated (D7.4a). If someone later gates it on the flag, the
/// tests above keep passing only if they were written against an armed config;
/// this pin is what stops that being invisible.
#[test]
fn the_alarm_is_not_gated_on_the_disarmed_wal_fence() {
    assert!(
        !HandleConfig::default().wal_fence_enabled,
        "production posture: the D7 fence is disarmed — so the pins above prove D5 fires WITHOUT it"
    );
}
