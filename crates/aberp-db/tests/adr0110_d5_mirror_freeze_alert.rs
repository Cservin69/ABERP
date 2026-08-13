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
//! later durability diagnostic — reached the DB alone and never the
//! `fsync`'d store, and nothing surfaced it. That is the D7.4d/D7.4e residual,
//! and it is why the round-3 adversarial promoted D5 from optional to
//! load-bearing for the D7 alarm.
//!
//! # Where the alarm is RECORDED, and why it is not the ledger (D5-B1)
//!
//! Ervin, 2026-08-13, route (a): a machine-spawned durability diagnostic must
//! never consume a ledger seq. The freeze is detected exactly when the DB head
//! has regressed below the append-only mirror's, so an append there lands at a
//! seq the mirror holds a *different* entry for; the chains fork, the next
//! boot's gated auto-heal refuses, and `serve` exits non-zero. The alarm that
//! says "stop and recover" would have been what stopped the recovery. The
//! episode therefore goes to `<db>.durability-alert` — append-only, `fsync`'d,
//! chained to nothing — and everything the operator sees is unchanged.
//!
//! # What this file pins
//!
//! [`a_live_process_mirror_divergence_raises_the_alert_and_records_the_marker`]
//! is the RED-first pin: the sticky alert goes up (so `GET /health` reports it
//! and the banner renders), the marker carries the episode, and the ledger is
//! UNTOUCHED. [`the_d5_b1_scenario_must_boot_cleanly_with_the_db_two_rows_behind`]
//! is B1 stated as the boot it used to break, with a control proving the
//! episode really was recorded.
//!
//! The silent direction is the harder half, and it is the reason this needed
//! care at all. A mirror that is AHEAD at BOOT is NORMAL: it is what an
//! unclean stop leaves behind (the mirror is `fsync`'d, the DB's WAL tail is
//! not), and `serve` heals it on every boot with
//! [`aberp_audit_ledger::ensure_consistent_with_db`] *before* the shared Handle
//! opens. A mirror that is ahead because a co-resident CLI wrote to it is not
//! our loss either (D5-B2). An alarm that fired on either would be up after
//! half the crashes this system is designed to survive, and after routine NAV
//! maintenance — and an operator learns to ignore a banner like that long
//! before the day it is right. So:
//!
//! * [`a_boot_reconcile_that_heals_a_mirror_ahead_leaves_the_alarm_quiet`]
//! * [`a_divergence_this_handle_never_saw_agree_must_not_raise`]
//! * [`a_mirror_advanced_by_someone_else_must_not_raise`]
//!
//! and [`the_freeze_is_recorded_once_per_episode`] keeps a continuous fault
//! from becoming one record per write.
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

/// A Handle in the **production posture**: `HandleConfig::default()` — which as
/// of ADR-0110 D7.6 (2026-08-13) ships the D7 WAL fence **ARMED**.
///
/// Every test here uses it, and that is the point rather than a convenience: D5
/// raises an alarm and refuses no write, so unlike the fence it carries no
/// money-path-outage risk and is deliberately NOT gated on `wal_fence_enabled`.
/// These pins therefore have to hold whatever that flag says, and they did not
/// need a single change when it flipped — which is the property, not a
/// coincidence. [`the_alarm_fires_in_both_wal_fence_states`] asserts it on both
/// branches explicitly instead of leaning on whatever the default happens to be.
///
/// Note what the flip DOES add here: with the fence armed, every `WriteGuard`
/// drop in this file now also samples the WAL watermark. That the freeze pins
/// below are unaffected by it is itself worth having under test — a mirror
/// freeze and a WAL truncation are separate detectors that must not bleed into
/// each other.
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

/// Take and drop a write guard WITHOUT writing anything.
///
/// The drop hook runs regardless of whether the body committed a row, so this
/// is how a live process meets a divergence that its own head did not cause —
/// which is the whole of the D5-B2 case. Committing a row instead would move
/// our head and quietly change what is being tested.
fn drop_a_guard(h: &Handle) {
    let guard = h.write().expect("acquire the shared writer");
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

/// **The D5-B2 primitive.** Append one entry to the MIRROR that our DB view
/// does not have — what a co-resident CLI's own `sync_mirror` leaves behind.
///
/// A separate process's rows go into the shared file's WAL, and a different
/// DuckDB instance does not replay another instance's WAL, so from this
/// Handle's connection the mirror simply grows entries the DB "does not have".
/// Reproduced here by copying the mirror's last record with its `seq` bumped:
/// the reader requires contiguous ascending seqs and does not verify the chain,
/// so this is exactly the shape `sync_mirror` meets — and it costs no
/// second process, which a unit test cannot honestly drive anyway.
fn append_foreign_mirror_line(db: &Path) {
    let path = mirror_path_for(db);
    let body = std::fs::read_to_string(&path).expect("read the mirror");
    let last = body
        .lines()
        .last()
        .expect("the mirror has a line")
        .to_string();
    let (head, rest) = last
        .split_once("\"seq\":")
        .expect("a mirror record carries a seq field");
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let seq: u64 = digits.parse().expect("the seq field is a number");
    let bumped = format!("{head}\"seq\":{}{}\n", seq + 1, &rest[digits.len()..]);
    assert_ne!(bumped.trim_end(), last, "the seq bump must have applied");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open the mirror to append");
    f.write_all(bumped.as_bytes()).expect("append");
    f.sync_all().expect("fsync the mirror");
}

/// Every line of the D5 durability-alert marker, or `[]` if there is none.
fn marker_lines(h: &Handle) -> Vec<String> {
    match std::fs::read_to_string(h.durability_marker_path()) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("read the durability-alert marker: {e}"),
    }
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
/// AND a durable record of the episode.
///
/// Before D5 every assertion below the divergence failed: the drop `warn!`d and
/// nothing else, so the mirror froze with no alert, no record, and a log line
/// claiming it would "reconcile on the next write".
///
/// The record goes to the NON-CHAINED marker, and the ledger is asserted to be
/// UNTOUCHED. That second assertion is D5-B1: an append here consumes a seq the
/// mirror already holds a different entry for, which forks the chains and
/// refuses the next boot (see
/// [`the_d5_b1_scenario_must_boot_cleanly_with_the_db_two_rows_behind`]).
///
/// Mutation-verified: replace the `MirrorDivergent` arm in `WriteGuard::drop`
/// with the old unconditional `warn!` and this goes RED at
/// `expect("the alert must be up")`, while every other test in this file and
/// the whole D7 fence suite stay green — which is exactly the gap it covers.
#[test]
fn a_live_process_mirror_divergence_raises_the_alert_and_records_the_marker() {
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
        lines[0].contains("\taudit_mirror_sync_refused\t"),
        "the record must name D5's trigger so forensics can tell it from D7's \
         wal_truncated_under_writer, got: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\taudit_mirror_frozen\t"),
        "the record must carry the machine breach code, got: {}",
        lines[0]
    );

    assert!(
        loss_payloads(&h).is_empty(),
        "D5-B1: the diagnostic must NOT consume a ledger seq. Appending here forks the chains \
         at a seq the mirror already holds and REFUSES the next boot — the alarm that says \
         'stop and recover' would be what stopped the recovery."
    );
}

/// **D5-B1, stated as the boot it used to break.**
///
/// The adversarial scenario verbatim: the DB is exactly TWO rows behind the
/// mirror when the freeze is first detected. With the diagnostic on-chain, D5's
/// append landed at the next DB seq — one the mirror holds a *different* entry
/// for — so the gated auto-heal's boundary check (DB head `entry_hash` vs the
/// mirror's at the same seq) failed, `ensure_consistent_with_db` answered
/// `MirrorAheadOfDb`, and `serve::run` returned `Err` at its boot reconcile
/// step and exited non-zero. A tenant that could have healed itself was bricked
/// BY ITS OWN ALARM.
///
/// With the marker there is no append, so the mirror stays cleanly ahead and
/// the heal arm still applies. Pinned at `ensure_consistent_with_db` rather
/// than by driving `serve::run`: that call IS serve's boot chokepoint for this
/// class (`serve.rs`, the `recover_audit_mirror` step, whose `Err` arms are the
/// only thing that turns this into a non-zero exit), and driving a full boot
/// unattended needs the OS keychain. The modelling is named here rather than
/// implied.
///
/// The CONTROL is the second half: the marker must be present at the moment the
/// boot succeeds. Without it the test would pass for the wrong reason — a run
/// where D5 never fired at all boots cleanly too, and would have been just as
/// green before the fix.
///
/// Mutation-verified: put the append back (have `raise_mirror_freeze_alert`
/// write `EventKind::DbDurabilityLossDetected` through the drop's connection)
/// and this goes RED on the boot assertion, with `MirrorAheadOfDb`.
#[test]
fn the_d5_b1_scenario_must_boot_cleanly_with_the_db_two_rows_behind() {
    let tmp = Tmp::new("b1-boot");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
        commit_one(&h, "three");
        // Exactly two rows behind: mirror head 3, DB head 1.
        regress_db_head_through_handle(&h, 1);
        assert!(
            h.durability_alert().is_some(),
            "precondition: the freeze was detected"
        );
        assert!(
            !marker_lines(&h).is_empty(),
            "CONTROL: the episode must actually have been recorded — otherwise the clean boot \
             below proves nothing"
        );
    }

    boot_reconcile(&db).expect(
        "D5-B1 REGRESSION: the tenant no longer boots. A durability diagnostic that consumes a \
         ledger seq forks the chain at a seq the mirror already holds, and the gated auto-heal \
         refuses — serve exits non-zero. The diagnostic must not be on the chain.",
    );

    // And the tenant is genuinely usable again: the heal replayed the mirror's
    // tail, so a fresh Handle writes and mirrors normally.
    let h = handle(&db);
    commit_one(&h, "after-recovery");
    assert_eq!(
        mirror_entries(&db),
        db_head_seq(&h),
        "after the heal the two stores must be back in lockstep"
    );
}

/// The alert must survive the restart the banner tells the operator to perform
/// — the D7.4a property, now carried by the marker rather than by a ledger row.
///
/// And it must survive with the RIGHT breach (N2). Before the marker there was
/// nothing to read the detected code from, so restore hard-coded `wal_vanished`
/// and every D5 mirror freeze came back after a restart claiming the WAL had
/// vanished — losing, on the one surface the operator reads, the distinction a
/// recovery actually turns on.
///
/// Mutation-verified twice: skip the `record_loss` call in
/// `raise_mirror_freeze_alert` (keeping the in-memory alert) and the first
/// assertion goes RED — the in-process banner alone looks identical until you
/// restart. Hard-code `WalBreach::WalVanished` in `restore_durability_alert`
/// and only the breach assertion goes RED.
#[test]
fn the_alert_survives_a_restart_with_the_breach_it_was_detected_as() {
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
    let alert = reopened.durability_alert().expect(
        "a restart is not an acknowledgement — the alert must be re-derived from the durable \
         record",
    );
    assert_eq!(
        alert.breach,
        WalBreach::AuditMirrorFrozen,
        "N2: the restart must report the breach that was DETECTED, not a hard-coded guess"
    );
}

/// Acknowledging clears the banner permanently — including across the restart
/// that used to bring it back.
///
/// The marker's `ack` record is what does it, and it is APPENDED rather than
/// deleting the loss line: taking a banner down must not erase the record of
/// what raised it. (The attributable act stays on the chain, where the route
/// writes `db.durability_alert_acknowledged` — an operator event belongs there;
/// a machine diagnostic does not.)
///
/// Mutation-verified: make `clear_durability_alert` skip `record_ack` and this
/// goes RED on the reopen — the exact "operator watched the banner drop and it
/// came back next boot" defect D7.4b closed, in the marker's terms.
#[test]
fn an_acknowledgement_keeps_the_banner_down_across_a_restart() {
    let tmp = Tmp::new("ack-sticks");
    let db = tmp.db();
    seed(&db);

    {
        let h = handle(&db);
        commit_one(&h, "one");
        commit_one(&h, "two");
        regress_db_head_through_handle(&h, 1);
        assert!(h.durability_alert().is_some(), "precondition: alert raised");
        h.clear_durability_alert()
            .expect("the operator acknowledges");
        assert!(
            h.durability_alert().is_none(),
            "the banner goes down in-session"
        );
        assert!(
            marker_lines(&h).iter().any(|l| l.starts_with("v1\tloss\t")),
            "the loss record must SURVIVE the acknowledgement — clearing a banner is not \
             erasing the evidence"
        );
    }

    let reopened = handle(&db);
    assert!(
        reopened.durability_alert().is_none(),
        "an acknowledged loss must stay acknowledged across a restart"
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
        marker_lines(&h).is_empty(),
        "and it must not record a durability-loss episode either"
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
/// Mutation-verified: make the `MirrorDivergent` arm's `regressed` test
/// `last_synced_head.is_none() ||` its current condition — i.e. treat "never in
/// lockstep" as a loss — and this goes RED while every other pin here stays
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
        marker_lines(&h).is_empty(),
        "and it must not record a durability-loss episode either"
    );
}

/// **D5-B2 — a co-resident CLI advancing the mirror is not a durability loss.**
///
/// The NAV resubmission family (`submit-invoice`, `poll-ack`,
/// `retry-submission`, …) writes audit rows and syncs the mirror in its own
/// process. Its rows land in the mirror; a *different* DuckDB instance does not
/// replay another process's WAL, so from serve's Handle the mirror is suddenly
/// AHEAD — pixel-identical to a truncation freeze. Raising there would put a
/// permanent "stop and recover" banner up after sanctioned maintenance, which
/// is the round-1 "alarm the operator learns to dismiss".
///
/// The discriminator is that OUR head did not move. A truncation costs this
/// Handle rows it had already mirrored; somebody else's append does not. So the
/// raise requires the DB head to have fallen BELOW `last_synced_head`, and this
/// case — mirror ahead, our head unchanged — stays quiet.
///
/// (Post-D9 this is also structurally excluded: `serve` holds the F-E whole-DB
/// writer flock for its entire process lifetime and every DB-mutating one-shot
/// takes it first, so a co-resident CLI writer REFUSES to run. This pin is the
/// belt to that braces — it holds even if a future writer slips the flock, and
/// it is the reason the alarm's meaning is "we lost rows" rather than "the
/// mirror moved".)
///
/// Mutation-verified: weaken the arm's `regressed` test to `now <= prev` and
/// this goes RED while every other pin here stays green.
#[test]
fn a_mirror_advanced_by_someone_else_must_not_raise() {
    let tmp = Tmp::new("b2-coresident-cli");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    commit_one(&h, "one");
    commit_one(&h, "two");

    // A co-resident writer appends to the MIRROR only, exactly as another
    // process's `sync_mirror` would look from here: our DB head never moves.
    let head_before = db_head_seq(&h);
    append_foreign_mirror_line(&db);
    assert_eq!(
        mirror_entries(&db),
        head_before + 1,
        "precondition: the mirror is now one ahead of our head"
    );

    // Our next guard drops onto that. Deliberately WITHOUT committing a row:
    // committing would move our own head and stop this testing the thing it is
    // named for (our head must be UNCHANGED — equal to `last_synced_head`, not
    // below it — which is exactly where the `<` in the discriminator lives).
    drop_a_guard(&h);
    drop_a_guard(&h);
    assert_eq!(
        db_head_seq(&h),
        head_before,
        "precondition: our own audit head has NOT moved"
    );

    assert!(
        h.durability_alert().is_none(),
        "D5-B2: the mirror moved, but nothing OF OURS was lost — our audit head never fell \
         below what we had already mirrored. A banner here is up after routine NAV maintenance."
    );
    assert!(
        marker_lines(&h).is_empty(),
        "and no episode is recorded either"
    );
}

/// A continuous fault must not become one marker record per write.
///
/// The divergence does not resolve inside the process, so every later drop sees
/// it again. Without the one-shot latch a busy tenant would bury the marker
/// under identical episodes, and the operator reading it could not tell one
/// incident from a thousand writes.
///
/// The follow-up drops deliberately commit NOTHING. A drop that appends a row
/// pushes our own head back up past `last_synced_head`, so the regression
/// discriminator stops matching and the latch is never reached — the first
/// version of this test did exactly that and stayed green with the latch
/// removed. The guards below leave the head where the freeze left it, which is
/// the state a frozen mirror actually persists in.
///
/// Mutation-verified: remove the `!freeze_reported` guard from the
/// `MirrorDivergent` arm and this goes RED with four records instead of one.
#[test]
fn the_freeze_is_recorded_once_per_episode() {
    let tmp = Tmp::new("once-per-episode");
    let db = tmp.db();
    seed(&db);
    let h = handle(&db);

    commit_one(&h, "one");
    commit_one(&h, "two");
    regress_db_head_through_handle(&h, 1);
    assert_eq!(marker_lines(&h).len(), 1, "precondition: raised once");

    drop_a_guard(&h);
    drop_a_guard(&h);
    drop_a_guard(&h);

    assert_eq!(
        marker_lines(&h).len(),
        1,
        "the freeze is one episode, not one per write"
    );
    assert!(
        h.durability_alert().is_some(),
        "and the sticky alert stays up throughout"
    );
}

/// **The alarm fires in BOTH `wal_fence_enabled` states — D5 is ungated.**
///
/// The FENCE (D7) fails `durable_ack`, and a false positive there is a
/// money-path outage; that is the only reason the flag exists. D5 raises an
/// alarm and refuses nothing, so it is ungated — the same reasoning that left
/// the D7 boot re-derivation ungated (D7.4a).
///
/// This test used to assert only that the flag DEFAULTS to false, which is a
/// statement about `HandleConfig`, not about D5 (N3). It stayed green with D5
/// fully gated on the flag — a pin that cannot fail. It then ran the freeze
/// against an explicitly DISARMED config.
///
/// **D7.6 (2026-08-13) sharpened it again, for the reason N3 existed in the
/// first place.** A residual `assert!(!HandleConfig::default().wal_fence_enabled)`
/// had stayed behind — a second statement about the DEFAULT hiding inside a test
/// about the FLAG — and arming the fence turned it red without D5 changing at
/// all. Re-pointing it at the new default would only re-arm the same landmine
/// for the next flip, so the pin now drives the freeze under BOTH configs.
/// "Ungated" is a claim about both branches; it is now asserted on both, and no
/// future flag flip can make it vacuous or red.
///
/// Mutation-verified: wrap the `MirrorDivergent` raise in
/// `if handle.config.wal_fence_enabled` and the DISARMED iteration goes RED
/// while the armed one stays green — exactly the asymmetry that mutation
/// introduces; invert the gate to `!wal_fence_enabled` and the ARMED iteration
/// goes red instead.
#[test]
fn the_alarm_fires_in_both_wal_fence_states() {
    for armed in [false, true] {
        let cfg = HandleConfig {
            wal_fence_enabled: armed,
            ..Default::default()
        };
        let tmp = Tmp::new(if armed {
            "ungated-armed"
        } else {
            "ungated-disarmed"
        });
        let db = tmp.db();
        seed(&db);
        let h = Handle::open(&db, tenant(), cfg).expect("open with the fence set explicitly");

        commit_one(&h, "one");
        commit_one(&h, "two");
        regress_db_head_through_handle(&h, 1);

        assert!(
            h.durability_alert().is_some(),
            "D5 must fire with the WAL fence {} — it refuses no write, so it carries none of \
             the risk that flag exists to hold back, and gating it either way would make it \
             dead code in one of the two postures this tree ships",
            if armed { "ARMED" } else { "DISARMED" }
        );
        assert_eq!(
            h.durability_alert().map(|a| a.breach),
            Some(WalBreach::AuditMirrorFrozen),
            "and it must stay D5's OWN breach code in both states: arming the fence must not \
             reclassify a mirror freeze as a WAL truncation"
        );
    }
}

// ── THE MARKER'S OWN FAILURE DIRECTION (round-5) ────────────────────────────

/// Write raw bytes as the whole marker file, standing in for whatever a crash
/// or a foreign hand left there.
fn overwrite_marker(h: &Handle, body: &str) {
    std::fs::write(h.durability_marker_path(), body).expect("write the marker");
}

/// **R5-N1 — a TORN record must fail toward the banner UP.**
///
/// A record goes down with one `write_all`, but a crash can still cut it. Of
/// the 75 truncation points on a real loss record, six leave the *event field*
/// incomplete — `v`, `v1`, `v1\t`, `v1\tl`, `v1\tlo`, `v1\tlos`. The first cut
/// of the reader skipped those with a `warn!`, so a torn FIRST record left the
/// banner DOWN: the exact opposite of the posture the module documents, and a
/// way for damage to silence the alarm rather than raise it.
///
/// The rule now is that a line counts as an acknowledgement only if it parses
/// completely as one, and every other non-empty line counts as a loss. All six
/// shapes are checked one by one rather than sampled, because "we fixed the
/// class" is the claim, and a class is not a sample. A torn `ack` is in here
/// too: an acknowledgement whose write was interrupted is not one.
///
/// The other direction is the last case: a genuinely BLANK marker must stay
/// quiet, or every healthy tenant that ever touched the file wears a banner.
///
/// Mutation-verified: restore the `_ => tracing::warn!(...)` skip arm and every
/// torn case below goes RED while the blank case stays green.
#[test]
fn a_torn_marker_record_counts_as_a_loss() {
    let tmp = Tmp::new("torn-marker");
    let db = tmp.db();
    seed(&db);

    // The six truncation points that cut the event field, plus a torn ack, plus
    // a record from a format this build cannot read.
    for torn in [
        "v",
        "v1",
        "v1\t",
        "v1\tl",
        "v1\tlo",
        "v1\tlos",
        "v1\tack",
        "v1\tack\t",
        "v2\tloss\t2026-08-13T10:00:00Z\tsomething\tnew\t7",
    ] {
        let h = handle(&db);
        overwrite_marker(&h, torn);
        drop(h);

        assert!(
            handle(&db).durability_alert().is_some(),
            "a torn or unreadable marker record ({torn:?}) left the banner DOWN. Damaging this \
             file must never be a way to silence the alarm — anything that is not a complete \
             acknowledgement has to count as a loss."
        );
    }

    // ...and the other direction, which is what stops this being a rule that
    // just says "always raise".
    let h = handle(&db);
    overwrite_marker(&h, "\n   \n\n");
    drop(h);
    assert!(
        handle(&db).durability_alert().is_none(),
        "a blank marker is not an incident; raising here would put a banner on every tenant \
         that ever touched the file"
    );
}

/// A torn record raises the banner, and the operator can still take it down.
///
/// This is the half that keeps R5-N1's fix from creating the failure it is
/// meant to avoid. The torn record is counted at UNIX_EPOCH precisely so a real
/// acknowledgement out-ranks it — an alarm that cannot be cleared is one an
/// operator routes around, and then it is worse than nothing on the day it is
/// right.
///
/// It found a real defect on its first run, which is the reason it is written
/// as a round-trip through the acknowledge path rather than as an assertion
/// about UNIX_EPOCH. A torn record does not end in a newline, so appending the
/// acknowledgement onto it SPLICED the two into one line that parsed as
/// neither: the ack was swallowed and the banner became genuinely permanent —
/// the failure the UNIX_EPOCH stamp exists to prevent, reintroduced one layer
/// down in `append_line`. Terminating a torn record before appending is the
/// fix.
///
/// Mutation-verified twice: stamp the torn record at `OffsetDateTime::now_utc()`
/// instead of UNIX_EPOCH in `durability_marker::read` (the acknowledgement can
/// no longer out-rank it), or drop the terminate-a-torn-record step from
/// `append_line` (the acknowledgement is spliced away). Either goes RED.
#[test]
fn a_torn_marker_record_is_still_acknowledgeable() {
    let tmp = Tmp::new("torn-ackable");
    let db = tmp.db();
    seed(&db);

    let h = handle(&db);
    overwrite_marker(&h, "v1\tlos");
    drop(h);

    let h = handle(&db);
    assert!(
        h.durability_alert().is_some(),
        "precondition: the torn record raised the banner"
    );
    h.clear_durability_alert()
        .expect("a readable, writable marker must always be acknowledgeable");
    drop(h);

    assert!(
        handle(&db).durability_alert().is_none(),
        "the acknowledgement must out-rank a torn record across a restart, or the banner is \
         permanent and the operator learns to work around it"
    );
}

/// **R5-N2 — a marker that cannot be read or written keeps the banner up, and
/// says so.**
///
/// The docstring used to promise that everything raised here stays
/// acknowledgeable. For damaged CONTENT that is true (the test above). For a
/// broken FILE it was not: nothing can be read, so no acknowledgement is ever
/// seen, and the same fault blocks the write, so `clear_durability_alert`
/// returns `Err` and the banner is stuck until the filesystem is fixed.
///
/// The disposition is to say that plainly rather than to soften the behaviour.
/// The alternative — clearing the flag when the durable half could not be
/// written — is exactly the amnesia D7.4b closed: the operator would watch the
/// banner drop, and the next boot would raise it again with no record that
/// anyone acknowledged anything. So this pins BOTH halves: the banner is up,
/// and the acknowledge attempt FAILS rather than silently succeeding.
///
/// A directory at the marker path stands in for the whole class (permission
/// denied, EIO, a read-only volume): every member reaches these same two arms,
/// and this one reproduces deterministically without depending on the test
/// process's privileges.
///
/// Mutation-verified: make `clear_durability_alert` ignore the `record_ack`
/// error and clear anyway, and the second half goes RED.
#[test]
fn an_unreadable_marker_keeps_the_banner_up_and_refuses_to_clear() {
    let tmp = Tmp::new("unreadable-marker");
    let db = tmp.db();
    seed(&db);

    let marker = handle(&db).durability_marker_path().to_path_buf();
    std::fs::create_dir(&marker).expect("occupy the marker path");

    let h = handle(&db);
    assert!(
        h.durability_alert().is_some(),
        "a marker that exists but cannot be read must raise: an unreadable alarm must not be a \
         silent one"
    );
    let err = h.clear_durability_alert().expect_err(
        "acknowledging must FAIL while the durable half cannot be written — clearing the flag \
         anyway is the amnesia D7.4b closed",
    );
    assert!(
        h.durability_alert().is_some(),
        "and the banner must still be up after the failed clear"
    );
    assert!(
        err.to_string().contains("durability-alert marker"),
        "the error must name the marker so the operator knows which filesystem fault to fix, \
         got: {err}"
    );

    std::fs::remove_dir(&marker).expect("clean up");
}
