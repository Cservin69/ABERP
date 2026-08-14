//! `aberp-db` — ADR-0099 H3 (PROD-HARDEN-2027): **one process-wide DuckDB access path.**
//!
//! # Why this crate exists (the runtime audit-ledger fork primitive)
//!
//! The `serve` process hosts many subsystems (pricing / quote-intake /
//! catalogue-push / email-relay / email-outbox / pdf-rerender daemons + every
//! request handler) that each call their **own** `duckdb::Connection::open` /
//! `Ledger::open` / `append_reopen` on the **same** single-file tenant DB, in
//! read-write, concurrently. DuckDB single-file storage is **single-writer**;
//! N separate `Connection::open` calls are N independent `Database` instances =
//! N checkpoint actors racing one file — the `duckdb#23046` torn-metadata path.
//! Independently, two openers off the same audit head both self-assign the next
//! `seq` and **fork the audit ledger** (the Defense line forked 4× —
//! seq 369→416→428→515 — precisely because openers were migrated piecemeal).
//!
//! [`Handle`] is the seam the codebase *assumed* it had but never built:
//! **exactly one** `Database`, all runtime DB access routed through it.
//!
//! # What it guarantees (H3)
//!
//! * **Single instance.** The live tenant DB is opened **once** at boot.
//!   [`Handle::write`] hands out the one shared connection behind a mutex
//!   (writes are serialized — one writer, never an interleave); [`Handle::read`]
//!   hands out a [`duckdb::Connection::try_clone`] of the **same** instance
//!   (shared buffer cache, no second OS open). Nothing else opens the live path
//!   at runtime.
//! * **Durable, lockstep post-commit mirror.** After every committed write the
//!   [`WriteGuard`] runs a **lockstep** [`aberp_audit_ledger::sync_mirror`] (the
//!   mirror tracks the DB continuously — closes the mirror-lag gap at the
//!   source). The handle **disables DuckDB's implicit checkpoint-on-close** so a
//!   runtime connection drop never folds the WAL in place (F-A, below).
//!
//! # H3 / H4 seam — the runtime checkpoint is DISABLED here
//!
//! The **debounced** validated durable checkpoint (D2) is coded and unit-tested
//! (the pure [`debounce`] module) but its runtime FOLD
//! (`aberp_snapshot::live_durable_checkpoint`) is **DISABLED in H3**
//! ([`HandleConfig::checkpoint_enabled`] defaults `false`) and lands in the
//! successor step **H4**. So [`Handle::run_durable_checkpoint_locked`] is a
//! clearly-marked stub that is never reached at runtime while
//! `checkpoint_enabled == false`; H4 swaps the stub for the real
//! `live_durable_checkpoint` call. See the LOCKED plan `PROD-HARDEN-2027.v1.0`.
//!
//! # The single-instance coherence dividend (S335)
//!
//! The pre-fix daemons *deliberately* re-opened per write (`S335`): separate
//! `Connection::open` instances do not share a buffer cache, so a persistent
//! connection would read a **stale chain head** and fork the audit `seq`.
//! Collapsing onto **one** instance dissolves that hazard: a `try_clone` of the
//! shared instance *does* observe every committed row (one shared cache), and
//! [`Handle::write`] serializes writes behind the writer mutex.
//!
//! # ADR-0110 D3 — the durable-ack boundary (the one primitive this crate owns)
//!
//! The H3/H4 seam above left a hole that cost ~22 h of business rows on
//! 2026-08-08: the pragmas that stop the WAL folding shipped, and the
//! replacement checkpoint never did, so a committed money-write lived **only**
//! in an `fsync`-less `<db>.wal` until the next boot happened to fold it.
//!
//! [`Handle::durable_ack`] closes that: after a money-path commit it `fsync`s
//! the main file, the WAL, and the parent directory, so the acked rows are in
//! the power-loss durable set. It does **not** fold — see its docs for why
//! ADR-0110 Option B beats Option A here — so nothing about the boot openers,
//! the snapshot path, or the mirror changes.
//!
//! That `fsync` is the ONLY durability primitive this crate owns. Everything
//! else is reused verbatim: [`aberp_audit_ledger::sync_mirror`] /
//! [`aberp_audit_ledger::LedgerMeta`] (and, if H4 is ever built, the
//! `aberp_snapshot` checkpoint primitives). The crate still only *routes*
//! access through one instance and *calls* those primitives at the post-commit
//! point.
//!
//! **H4 is still unbuilt, and D3 does not build it.**
//! [`Handle::run_durable_checkpoint_locked`] remains the stub below and
//! [`HandleConfig::checkpoint_enabled`] still defaults `false`. The consequence
//! to keep in view: the WAL is durable but still **unbounded** at runtime, and
//! it is still the boot fold that truncates it. ADR-0110 D7 (boot stops folding
//! blind) therefore still depends on H4, not on D3.
//!
//! # Prod adaptation vs. the editions source (`ABERP-Editions` 1e6097d)
//!
//! Ported faithfully from the production-proven editions consolidation, with
//! three deliberate prod adaptations:
//!   1. **No `ensure_not_prod_path` guard.** In editions that guard stops a
//!      Defense/dev build from opening the real prod DB. The prod tree *is* the
//!      prod build acting on the prod DB, so the guard is meaningless and the
//!      prod `aberp-snapshot` omits it by design (`crash_safe.rs`).
//!   2. **`checkpoint_enabled` defaults `false`** (H3; H4 flips it — see above).
//!   3. The `aberp-snapshot` dependency is deferred to H4 (nothing in H3 calls a
//!      snapshot primitive), so [`run_durable_checkpoint_locked`] is a stub.

pub mod debounce;
// ADR-0099 F-E — the CROSS-process counterpart of this crate's in-process
// single writer. Lives here, not in `apps/aberp`, because DB-mutating one-shots
// outside that package (`aberp-inventory`'s `rebuild-stock-cache`) must take the
// same lock, and two copies of the path derivation would be no lock at all.
// Not reachable from `Handle` — the caller acquires it BEFORE any DB open.
pub mod db_writer_lock;
// ADR-0110 D5 — the NON-CHAINED durability-alert marker (`<db>.durability-alert`).
// A machine-spawned durability diagnostic must never consume a ledger seq: the
// freeze is detected exactly when the DB head has regressed below the mirror's,
// so an append there forks the two chains and REFUSES the next boot (D5-B1).
// Private: the Handle owns every read and write of it.
mod durability_marker;
// The pure DB-path shape rule that the boot guard trio's third check calls.
// Not reachable from `Handle`.
pub mod engine_path;
// Prod incident 2026-08-03 — boot-time rebuild of the non-constraint ART
// indexes. Not reachable from `Handle`: serve boot calls it on the boot-phase
// connection, BEFORE the Handle opens.
pub mod index_integrity;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId};
use duckdb::Connection;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::debounce::CheckpointDebouncer;

/// Typed error surface (ADR-0021 Part A — no `anyhow` in a library crate).
/// The `apps/aberp` daemons wrap these with their own `anyhow` `.context()`.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The shared writer mutex was poisoned by a panic in another holder.
    ///
    /// Retained for API back-compat, but ADR-0099 H3 / Bug 5 means
    /// [`Handle::write`] / [`Handle::read`] no longer surface this: a poisoned
    /// writer is now RECOVERED in-place (`clear_poison` + integrity re-verify)
    /// rather than bricking the whole process forever. See
    /// [`Handle::recover_from_poison`].
    #[error("aberp-db writer lock poisoned")]
    Poisoned,

    /// ADR-0099 H3 / Bug 5 — a poisoning panic was recovered (`clear_poison`),
    /// but the POST-POISON integrity re-verify FAILED: on the freshly re-opened
    /// instance the audit hash-chain did not verify genesis→head. That is real
    /// corruption (not a benign prior panic), so it is surfaced HARD rather than
    /// served from a bad DB. See [`Handle::recover_from_poison`].
    #[error("aberp-db post-poison integrity re-verify failed: {0}")]
    PoisonRecoveryFailed(String),

    /// Underlying DuckDB error (open / try_clone / runtime pragma).
    #[error("duckdb: {0}")]
    Duck(#[from] duckdb::Error),

    /// A schema operation could not be completed, or completed but did not
    /// produce the columns it claimed to. Separate from [`DbError::Duck`]
    /// because the post-condition failure is OUR refusal on an engine that
    /// reported success: "the ALTER returned Ok" and "the column is there" are
    /// different facts.
    #[error("schema: {0}")]
    Schema(String),

    /// ADR-0110 D3 — a durable-ack `fsync` failed. Its own variant, not folded
    /// into [`DbError::Duck`], because it is the ONE error whose meaning is
    /// "the transaction committed but we cannot promise it survives a power
    /// loss". A money-path caller must surface this rather than ack (R3 /
    /// CLAUDE.md rule 11); it must never be downgraded to a `warn!`.
    #[error("durable-ack fsync failed for {path}: {source}")]
    DurableAck {
        path: PathBuf,
        source: std::io::Error,
    },

    /// ADR-0110 D5 — a write to the NON-CHAINED durability-alert marker failed.
    ///
    /// Its own variant because it has one caller and one meaning: the operator
    /// pressed Acknowledge and the DURABLE half of taking the banner down did
    /// not land. [`Handle::clear_durability_alert`] returns this instead of
    /// clearing, so the route fails loudly with the banner still up. Clearing
    /// memory over a failed durable clear is the amnesia D7.4b closed.
    #[error("durability-alert marker write failed for {path}: {source}")]
    DurabilityMarker {
        path: PathBuf,
        source: std::io::Error,
    },

    /// **ADR-0110 D7 — the WAL fence fired: this is DURABILITY LOSS, not a
    /// failed `fsync`.**
    ///
    /// Between two of this [`Handle`]'s own observations the live `<db>.wal`
    /// vanished, shrank below the Handle's monotone high-water, or changed
    /// inode — or the main DB file changed inode. Nothing this Handle does can
    /// produce that shape (its F-A pragmas exist precisely to stop it), so it
    /// means a **foreign** DuckDB instance opened the tenant DB with DEFAULT
    /// pragmas and, on close, FOLDED and TRUNCATED the WAL out from under the
    /// writer. Past that point `commit()` keeps returning `Ok` while the bytes
    /// reach no file.
    ///
    /// Distinct from [`DbError::DurableAck`] on purpose. That one means "the
    /// `fsync` did not complete"; this one means "the `fsync` would have
    /// completed and told you nothing" — D3's ack `fsync`s PATHS, and a WAL
    /// truncated out of existence is simply absent, which the pre-D7 code
    /// treated as a legitimate skip on the way to `Ok(())`. Same green light,
    /// no bytes behind it.
    ///
    /// # Not a hard stop
    ///
    /// Raising this does **not** brick the Handle. The breach latch is
    /// consumed when it is reported, so the next [`Handle::durable_ack`] is
    /// evaluated on a fresh baseline and succeeds if the tenant is healthy
    /// again: writes keep being served. What persists is the sticky
    /// [`Handle::durability_alert`] the operator sees, plus the loss record in
    /// the non-chained `<db>.durability-alert` marker (ADR-0110 §15.3 — NOT an
    /// `audit_ledger` row: on a truncation that append forks the audit chain and
    /// refuses the next boot).
    #[error(
        "DURABILITY LOSS — {breach} on the live tenant DB at {db} \
         (wal {wal}; expected {expected}, observed {observed}). A foreign DuckDB \
         opener folded and truncated this Handle's WAL; commits since then may \
         have returned Ok without reaching stable storage. STOP AND RECOVER."
    )]
    WalTruncatedUnderWriter {
        breach: WalBreach,
        db: PathBuf,
        wal: PathBuf,
        /// What the watermark said: a byte high-water for the two size
        /// breaches, an inode number for the two identity breaches.
        expected: u64,
        /// What the filesystem said, in the same unit as `expected`.
        observed: u64,
    },
}

/// ADR-0110 D7 — which shape of durability fault a detector saw. A closed,
/// fixed vocabulary: [`WalBreach::code`] is written verbatim into the
/// `<db>.durability-alert` marker, so it must never carry an operator string or
/// a path.
///
/// The name is D7's and predates the second detector. It is kept as-is
/// deliberately: [`WalBreach::AuditMirrorFrozen`] (ADR-0110 D5) is not a WAL
/// shape, but renaming a merged public type + its `/health` wire vocabulary to
/// say so would churn the API for cosmetics. What the type actually
/// discriminates is "which fault put the sticky [`DurabilityAlert`] up".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalBreach {
    /// `<db>.wal` is GONE and this Handle had previously seen bytes in it.
    /// This is the 00012 shape: the foreign close folded the WAL into the main
    /// file and unlinked it. Pre-D7 this was the SKIP branch of
    /// `if wal_path.exists()`.
    WalVanished,
    /// `<db>.wal` is present but SHORTER than this Handle's monotone
    /// high-water. The WAL is append-only under the F-A pragmas, so the only
    /// way it shrinks is a checkpoint we did not perform.
    WalShrank,
    /// `<db>.wal` is present and long enough, but it is a DIFFERENT inode. A
    /// fold that truncated and recreated the file leaves exactly this trace,
    /// and byte counts alone would miss it.
    WalReplaced,
    /// The main DB file changed inode under the running Handle — something
    /// swapped the live file (a restore, or a fold-and-rename). Our open `fd`
    /// still points at the old inode, so every subsequent commit is written to
    /// a file no longer reachable by name.
    MainReplaced,
    /// **ADR-0110 D5** — the lockstep [`aberp_audit_ledger::sync_mirror`] at
    /// [`WriteGuard::drop`] answered `MirrorDivergent`, so the audit mirror
    /// REFUSED the append and is frozen for the rest of this process: every
    /// audit row from here on lands in the DB alone and never reaches
    /// `fsync`'d storage.
    ///
    /// Not a WAL shape (see the type docs), but the same class of fault and
    /// the same operator instruction: writes are no longer durable in the way
    /// the system promises. It is also the DOWNSTREAM signature of
    /// [`WalBreach::WalVanished`] — a truncation regresses the DB head below
    /// the append-only mirror's, which is exactly what `sync_mirror` refuses
    /// on — so seeing this without a preceding WAL breach means the fence was
    /// disarmed, not that nothing was truncated.
    AuditMirrorFrozen,
}

impl WalBreach {
    /// Stable machine code for the audit payload. `&'static str`, never
    /// derived from anything an operator or a filesystem supplied.
    pub fn code(self) -> &'static str {
        match self {
            WalBreach::WalVanished => "wal_vanished",
            WalBreach::WalShrank => "wal_shrank",
            WalBreach::WalReplaced => "wal_replaced",
            WalBreach::MainReplaced => "main_db_file_replaced",
            WalBreach::AuditMirrorFrozen => "audit_mirror_frozen",
        }
    }

    /// Inverse of [`Self::code`] — used by the D5 marker reader so a restart
    /// re-raises with the breach that was actually detected instead of a
    /// hard-coded guess (ADR-0110 D5 / N2). `None` for a code this build does
    /// not know, which a newer format could produce; the caller decides what an
    /// unknown shape means rather than being handed a wrong one.
    pub(crate) fn from_code(code: &str) -> Option<Self> {
        match code {
            "wal_vanished" => Some(WalBreach::WalVanished),
            "wal_shrank" => Some(WalBreach::WalShrank),
            "wal_replaced" => Some(WalBreach::WalReplaced),
            "main_db_file_replaced" => Some(WalBreach::MainReplaced),
            "audit_mirror_frozen" => Some(WalBreach::AuditMirrorFrozen),
            _ => None,
        }
    }
}

impl std::fmt::Display for WalBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            WalBreach::WalVanished => "the write-ahead log VANISHED",
            WalBreach::WalShrank => "the write-ahead log SHRANK below our high-water",
            WalBreach::WalReplaced => "the write-ahead log was REPLACED (inode changed)",
            WalBreach::MainReplaced => "the main DB file was REPLACED (inode changed)",
            WalBreach::AuditMirrorFrozen => {
                "the audit mirror REFUSED further appends (it diverged from the DB), so writes \
                 are no longer being mirrored to fsync'd storage"
            }
        };
        f.write_str(s)
    }
}

/// ADR-0110 D7 — the sticky operator-facing durability alert.
///
/// Set the first time the fence fires and kept until
/// [`Handle::clear_durability_alert`] is called explicitly. Deliberately NOT
/// cleared by a subsequent healthy ack: "it stopped happening" is not "it did
/// not happen", and the rows that went missing do not come back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurabilityAlert {
    /// What the fence saw, on the FIRST detection.
    pub breach: WalBreach,
    /// Operator-facing sentence. Rendered verbatim by the SPA banner.
    pub message: String,
    /// Wall-clock of the FIRST detection. Later breaches are logged and
    /// audited but do not move this — the operator wants to know when the
    /// tenant started losing writes, not when it last did.
    pub detected_at: SystemTime,
}

/// ADR-0110 D7 — a file's `(dev, ino)` identity, as `stat`/`fstat` reports it.
///
/// `None` on non-unix (see [`file_id`]): every identity comparison degrades to
/// "not checked" there rather than to a false positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

/// The `(dev, ino)` of an already-`stat`ed file.
///
/// Unix-only by construction. On other targets there is no cheap stable inode
/// notion, so this returns `None` and the fence falls back to the byte
/// high-water alone — weaker, never wrong.
fn file_id(md: &std::fs::Metadata) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileId {
            dev: md.dev(),
            ino: md.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        None
    }
}

/// ADR-0110 D7 — what this [`Handle`] last saw of its own durable set.
///
/// Sampled at every [`WriteGuard::drop`] (so, under the writer lock, once per
/// committed write) and again at every [`Handle::durable_ack`]. See
/// [`Handle::observe_durable_set`] for why the byte counter is a MONOTONE
/// high-water rather than a last-seen value — that is the whole race-tolerance
/// argument.
#[derive(Debug, Default)]
struct WalMark {
    /// Monotone high-water of `<db>.wal`'s length in bytes. Raised by every
    /// observation, never lowered except by a fold we performed ourselves
    /// (`folded_by_us`) or when a breach re-baselines. `0` means "this Handle
    /// has never seen a non-empty WAL", which is also the boot state — and is
    /// why the first ack after a boot fold cannot fire the fence.
    wal_high_water: u64,
    /// Identity of the `<db>.wal` we last saw. `None` until one exists.
    wal_id: Option<FileId>,
    /// Identity of the main DB file we last saw. `None` until first observed.
    main_id: Option<FileId>,
    /// Set by the Handle immediately before IT drops and reopens the shared
    /// connection (today: post-poison recovery). Reopening replays the WAL and
    /// may legitimately fold it, which is the ONLY sanctioned shrink. The next
    /// observation consumes the flag and re-baselines instead of firing.
    folded_by_us: bool,
    /// A breach detected by an observation and not yet reported.
    /// [`WriteGuard::drop`] cannot return an error, so a breach it finds is
    /// latched here for the next [`Handle::durable_ack`] to take. Taking it
    /// clears it — that is what keeps a fired fence from turning into a
    /// permanent write refusal.
    breach: Option<Breach>,
}

/// ADR-0110 D7 / R2-B1 — the newest `time_wall` of each durability audit kind
/// within ONE store. Gathered separately from the mirror and from the DB,
/// because after a real truncation the two disagree about which rows they have:
/// the mirror keeps the loss and refuses everything after it, the DB may have
/// lost the loss and is the only store still accepting the acknowledgement.
#[derive(Debug, Default)]
struct DurabilityAuditTimes {
    loss: Option<OffsetDateTime>,
    ack: Option<OffsetDateTime>,
}

/// What one `stat` of `<db>.wal` told us. The third arm is the point: N2 (PR
/// #61 adversarial) — an `Err` that is not `NotFound` means "we could not
/// look", which must never be collapsed into "it is gone".
enum WalStat {
    Present(std::fs::Metadata),
    Absent,
    Unreadable,
}

/// A latched breach plus the two numbers that justify it. Both are BYTES for
/// [`WalBreach::WalVanished`] / [`WalBreach::WalShrank`] and INODE NUMBERS for
/// [`WalBreach::WalReplaced`] / [`WalBreach::MainReplaced`] — the pair always
/// reads "what the watermark said" vs "what the filesystem said", which is the
/// only thing an operator or an audit reader needs from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Breach {
    kind: WalBreach,
    expected: u64,
    observed: u64,
}

/// Tunables for a [`Handle`]. [`HandleConfig::default`] is the ADR-0099 H3
/// posture (checkpoint DISABLED); tests dial the checkpoint window.
#[derive(Debug, Clone)]
pub struct HandleConfig {
    /// Coalescing window for the (H4) post-write durable checkpoint. Retained so
    /// the pure D2 [`debounce`] logic is exercised; inert while
    /// `checkpoint_enabled == false`.
    pub min_checkpoint_interval: Duration,
    /// Whether to run the debounced durable checkpoint at all.
    ///
    /// **H3: always `false`.** The runtime validated checkpoint is H4's step
    /// (see the crate-level H3/H4 seam docs). Tests may flip it on ONLY once H4
    /// lands the `aberp_snapshot::live_durable_checkpoint` fold.
    pub checkpoint_enabled: bool,
    /// Whether to issue `PRAGMA disable_checkpoint_on_shutdown` (+ the
    /// `wal_autocheckpoint` raise) on each runtime connection so dropping it
    /// never folds the WAL in place (the vulnerable in-place checkpoint). This
    /// is the F-A engine-adapter pragma; always `true` in production.
    pub disable_implicit_close_checkpoint: bool,
    /// **ADR-0110 D7.6 — the WAL fence. ARMED: defaults `true` as of
    /// 2026-08-13.**
    ///
    /// When `true` — the production posture from here on — [`WriteGuard::drop`]
    /// samples the WAL watermark and [`Handle::durable_ack`] refuses on a
    /// truncation ([`DbError::WalTruncatedUnderWriter`]). When `false` neither
    /// happens and `durable_ack` behaves exactly as it did under D3; that state
    /// is kept, and still pinned, because it is what every regression bisect
    /// through the dark period lands on.
    ///
    /// **The operator-visible consequence of the flip:** a real WAL truncation
    /// now fails that ack, raises the sticky [`Handle::durability_alert`], and
    /// puts the full-width red banner in front of the operator. Before the flip
    /// the same event was silent. This is the point of D7 — but it does mean the
    /// banner is now reachable in production, so the first thing to check when
    /// one appears is that it is a real fold and not a regression in the
    /// no-false-positive set below.
    ///
    /// # Why it shipped OFF for two months (PR #61 adversarial, B1)
    ///
    /// Arming the fence while ANY opener can still fold this Handle's WAL turns
    /// a silent durability bug into a routine money-path outage: the breach is
    /// armed by the fold, the NEXT invoice issuance or mark-paid fails
    /// `durable_ack`, and that failure PROPAGATES via `?` (the D3-C cut-gate
    /// enforces exactly that propagation) — a committed invoice reported as
    /// failed, NAV handoff skipped. Strictly worse than the bug it detects, so
    /// the detection landed DARK while its two causes were closed in turn.
    ///
    /// # Both causes are now closed (D8, then D9)
    ///
    /// 1. **In-serve openers — closed by D8.** GROUP A in
    ///    `tools/adr0099_read_fork_structural_baseline.txt` is EMPTY: no
    ///    in-serve request route or daemon opens a foreign connection to the
    ///    tenant DB.
    /// 2. **CLI-against-live openers (GROUP B) — closed by D9.** These are
    ///    separate OS processes that cannot borrow this in-process Handle, so
    ///    the fix is not migration but MUTUAL EXCLUSION: every DB-mutating
    ///    one-shot takes the F-E whole-DB writer flock
    ///    ([`crate::db_writer_lock::acquire_or_refuse`]) before it opens the DB,
    ///    and therefore REFUSES against a live serve rather than folding its
    ///    WAL. `drain-submission-queue`, `drain-pending-retries`,
    ///    `export-invoice-bundle`, `recover-from-nav` and `mark-abandoned`
    ///    already did; `rebuild-stock-cache` — a *documented* ADR-0061 §3
    ///    recovery path, i.e. the one an operator runs while the shop is live —
    ///    was the last that did not. D9 fenced it.
    ///
    /// `print_invoice::render_to_bytes` still holds no flock and is knowingly
    /// NOT on this gate: it opens via [`Handle::open_default`], whose
    /// `disable_checkpoint_on_shutdown` pragma means its close cannot fold a
    /// WAL. It is a stale-*read* debt (ADR-0110 §13.2), not a fold hazard.
    ///
    /// # The third precondition, and why it was NOT optional (ADR-0110 §15.3)
    ///
    /// D8 and D9 closed the two *fold* causes. A third stood in the way and was
    /// specific to arming: the fence's own diagnostic used to be an
    /// `audit_ledger` append, and a truncation is exactly the state in which an
    /// append forks the two chains and REFUSES the next boot (D5-B1). An armed
    /// fence firing on a real truncation would therefore have bricked the tenant
    /// with its own alarm. [`Handle::raise_durability_alert`] now records to the
    /// non-chained [`crate::durability_marker`], the same store D5 uses, which
    /// is what makes this flag safe to turn on. Pinned by
    /// `the_d5_b1_scenario_driven_through_the_armed_fence_must_boot_cleanly`.
    ///
    /// # What arming rests on staying true
    ///
    /// That nothing legitimately folds this Handle's WAL. `adr0110_d7_wal_fence.rs`
    /// pins the healthy-box silence directly — boot, boot onto a pre-existing
    /// WAL, the first ack after a boot fold, concurrent daemon writes,
    /// auto-checkpoint, a copy-based snapshot, the `.creating-*` staging sweep
    /// and the flock-refusing CLIs — all with the fence ARMED. A new opener that
    /// folds the WAL is now a money-path outage, not a silent bug: the
    /// `cut_gate_read_fork` / `cut_gate_write_fork` / `cut_gate_opener_census`
    /// gates and `adr0110_d9_flock_shape.rs` are what keep one from being added.
    pub wal_fence_enabled: bool,
}

impl Default for HandleConfig {
    fn default() -> Self {
        Self {
            min_checkpoint_interval: debounce::DEFAULT_MIN_CHECKPOINT_INTERVAL,
            // H3: the runtime durable checkpoint is DISABLED (H4's step). The
            // single-instance discipline (no concurrent separate openers) makes
            // DuckDB's own bounded auto-checkpoint safe in the interim.
            checkpoint_enabled: false,
            disable_implicit_close_checkpoint: true,
            // ADR-0110 D7.6 (2026-08-13): the fence is ARMED. Its three
            // preconditions are closed — D8 emptied GROUP A, D9 flock-fenced the
            // last CLI fold-trigger, and the fence's diagnostic now goes to the
            // non-chained marker instead of forking the audit chain (§15.3). A
            // real WAL truncation now fails the ack and raises the red banner.
            wal_fence_enabled: true,
        }
    }
}

/// Mutable state behind the single writer mutex.
struct Inner {
    /// The one shared runtime connection. `Option` because the (H4) debounced
    /// durable checkpoint must **drop** it (so the validated checkpoint is the
    /// *only* opener while it swaps the live file) and then **reopen** on the
    /// freshly-installed inode. `None` only transiently, under the lock.
    conn: Option<Connection>,
    /// D2 cadence coordinator (pure; see [`debounce`]).
    debouncer: CheckpointDebouncer,
    /// **ADR-0110 D5 — the benign-vs-live discriminator.** The audit head seq
    /// the last SUCCESSFUL lockstep [`aberp_audit_ledger::sync_mirror`] saw
    /// (its return value is the mirror head after the append, which is the DB's
    /// max seq at that instant). `None` until one has succeeded on this Handle.
    ///
    /// Two questions are answered by this one number, and both have to hold
    /// before a `MirrorDivergent` is a durability fault:
    ///
    /// 1. **`None` ⇒ never in lockstep.** Diverging from a state this Handle
    ///    never saw agree means the tenant ARRIVED diverged — the boot
    ///    reconciler's business, and in `serve` unreachable
    ///    (`ensure_consistent_with_db` runs before `open_tenant_handle` and
    ///    either heals it or refuses the boot). Raising there would put the
    ///    banner up on a state a boot resolves.
    /// 2. **`Some(n)` and the DB head is still ≥ `n` ⇒ we lost nothing**
    ///    (D5-B2). A mirror that is ahead because somebody ELSE appended to it
    ///    is not our durability failing; a mirror that is ahead because OUR
    ///    database head fell back below what we had already mirrored is. Only
    ///    the second is a loss, and only the second raises. See
    ///    [`WriteGuard::drop`] for why that distinction is the whole of B2.
    ///
    /// Lives in `Inner`, not behind its own lock, precisely because it is only
    /// ever touched from `WriteGuard::drop` — i.e. under the writer mutex this
    /// very guard holds. No second lock to order, no atomic to reason about.
    last_synced_head: Option<u64>,
    /// **ADR-0110 D5** — one-shot latch: the mirror freeze has been raised and
    /// audited once for this Handle. The divergence is CONTINUOUS (it does not
    /// resolve without a boot reconcile), so without this every subsequent
    /// write would append another marker loss record. One record per episode;
    /// the sticky alert carries it from there.
    mirror_freeze_reported: bool,
}

/// Convenience alias — the shared handle is always reached as `Arc<Handle>`
/// (cloned into `AppState` and every daemon `Deps`).
pub type HandleArc = std::sync::Arc<Handle>;

/// Process-wide monotonic id source for [`Handle`] instances. Consumed ONLY by
/// the debug/test re-entrancy tripwire (below) to tell one Handle's writer mutex
/// from another's — re-entrancy that deadlocks is per-mutex (per-Handle), so a
/// thread legitimately holding Handle A's guard while acquiring Handle B's must
/// NOT trip.
static NEXT_HANDLE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(debug_assertions)]
thread_local! {
    /// Ids of the [`Handle`]s whose [`WriteGuard`] THIS thread currently holds.
    ///
    /// The writer `Mutex` is **non-reentrant**: a second [`Handle::write`] — or
    /// ANY [`Handle::read`], which locks the same mutex to `try_clone` — issued
    /// while this thread already holds the guard blocks forever on the lock. That
    /// is a HUNG prod: invoicing stops with no error to read, which is as bad as
    /// corruption and harder to diagnose. This lets `write()`/`read()` PANIC
    /// loudly at the re-entrant acquire instead, so the whole test suite becomes
    /// the deadlock trace and a future nested acquire fails in CI rather than
    /// hanging prod (ADR-0099 H3 §re-entrancy tripwire). Debug/test only — zero
    /// release overhead, prod runtime behaviour unchanged.
    static HELD_WRITE_IDS: std::cell::RefCell<Vec<u64>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The process-wide shared DuckDB handle (ADR-0099 H3). Construct once at boot
/// ([`Handle::open`]); share as `Arc<Handle>` into `AppState` and every daemon
/// spawn. **Send + Sync**: the `Connection` (which is `Send` but not `Sync`)
/// lives behind a `Mutex`, and reads are served by owned `try_clone`s.
pub struct Handle {
    /// Process-unique id (from [`NEXT_HANDLE_ID`]). Consumed ONLY by the
    /// debug/test re-entrancy tripwire to identify THIS Handle's writer mutex.
    id: u64,
    db_path: PathBuf,
    /// `<db>.wal` — DuckDB's write-ahead log for [`Self::db_path`]. Precomputed
    /// once because [`Self::durable_ack`] needs it on every money-path ack.
    wal_path: PathBuf,
    mirror_path: PathBuf,
    /// ADR-0110 D5 — `<db>.durability-alert`, the NON-CHAINED marker that
    /// carries a durability-loss episode across a restart. Precomputed here for
    /// the same reason [`Self::wal_path`] is: the one derivation, in one place.
    marker_path: PathBuf,
    /// ADR-0110 D3 — the durability journal: every path this handle has
    /// actually `fsync`'d, in first-sync order, deduped.
    ///
    /// Behind its **own** mutex, never the writer's: [`Self::durable_ack`] runs
    /// AFTER the [`WriteGuard`] has dropped and deliberately does not take the
    /// writer lock (see its docs), so recording must not reach for it either.
    ///
    /// This is not telemetry. It is the seam that lets the ADR-0110 D6b
    /// power-loss spec DERIVE its durable set from what the write path really
    /// did, instead of hard-coding a list that silently rots the moment this
    /// file changes. See `apps/aberp/tests/adr0110_d6b_ondisk_durability.rs`.
    synced: Mutex<Vec<PathBuf>>,
    /// ADR-0110 D7 — the WAL fence's watermark. Behind its **own** mutex for
    /// the same reason [`Self::synced`] is: it is touched from
    /// [`Self::durable_ack`], which runs after the [`WriteGuard`] has dropped
    /// and deliberately takes no writer lock.
    ///
    /// That mutex is also load-bearing for correctness, not just for `&mut`:
    /// [`Self::observe_durable_set`] `stat`s the files **while holding it**, so
    /// observations are totally ordered and the monotone high-water can never
    /// be raised by a concurrent writer *between* another thread's `stat` and
    /// its comparison. See that method for the full race argument.
    wal_watermark: Mutex<WalMark>,
    /// ADR-0110 D7 — the sticky operator alert. `Some` from the first fence
    /// fire until [`Self::clear_durability_alert`]; surfaced on `GET /health`
    /// as `durability_alert` and rendered by the SPA's red banner.
    durability_alert: Mutex<Option<DurabilityAlert>>,
    /// Built **once** per process (S341 semantics): tenant + binary hash. The
    /// lockstep [`aberp_audit_ledger::sync_mirror`] needs it on every commit.
    meta: LedgerMeta,
    /// Plain-string tenant (retained for the H4 checkpoint call).
    tenant: String,
    config: HandleConfig,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("db_path", &self.db_path)
            .field("mirror_path", &self.mirror_path)
            .field("tenant", &self.tenant)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Handle {
    /// Open the live tenant DB **once** and return the shared handle.
    ///
    /// * Derives the mirror path with [`aberp_audit_ledger::mirror_path_for`]
    ///   (`<db>.audit.log`) — the same convention every other call site uses.
    /// * Builds [`LedgerMeta`] once (S341).
    ///
    /// Call **after** the H2 `provision_atomic` / `probe_open_or_preserve` boot
    /// chokepoint (`serve.rs`), when the live file is known-good.
    ///
    /// NOTE: unlike the editions source, prod does NOT call `ensure_not_prod_path`
    /// — the prod build legitimately operates on the prod DB (see the crate docs).
    pub fn open(
        db_path: &Path,
        tenant: TenantId,
        config: HandleConfig,
    ) -> Result<Arc<Handle>, DbError> {
        let mirror_path = aberp_audit_ledger::mirror_path_for(db_path);
        // The handle's internal meta is consumed ONLY by the post-commit
        // `sync_mirror` lockstep, which reads `meta.tenant_id()` and NOTHING
        // else (it appends already-hashed DB rows verbatim and never reads
        // `binary_hash`). So the binary hash — background-computed at boot and
        // not ready when the handle is built — is intentionally a fixed
        // placeholder here. Daemons that *create* audit rows build their OWN
        // `LedgerMeta` with the real `binary_hash` they `wait()` for; they never
        // use this meta for `append_in_tx`.
        let meta = LedgerMeta::new(tenant.clone(), BinaryHash::from_bytes([0u8; 32]));
        // SERVE_HANDLE_LIVE (ADR-0099 H3 Addendum 3) — 2026-07-28. The tripwire
        // hooked `Ledger::open` and `DuckDbBillingStore::open` but NOT the Handle
        // constructor itself, so a SECOND Handle opened in-serve on the same file
        // was invisible to it. That gap is not theoretical: PR #40 moved
        // `print_invoice::render_to_bytes` from a bare `Ledger::open` (hooked) to
        // its own `Handle::open_default` (unhooked) while
        // `email_invoice::send_invoice_email` still reached it in-serve — the
        // detector went quiet on a path that still forked. `open_runtime_connection`
        // below is a real second OS open; the `disable_checkpoint_on_shutdown`
        // pragma stops it TEARING the live WAL, but it still does not REPLAY it, so
        // it reads the last-checkpointed subset exactly like the openers it
        // replaced. Registration happens AFTER this call in `serve::run`, so serve's
        // own boot open cannot trip on itself. Debug/test only, like every other arm.
        aberp_audit_ledger::serve_tripwire::assert_no_serve_handle(db_path, "Handle::open");
        let conn = open_runtime_connection(db_path, &config)?;
        // Capture the coalescing window before `config` moves into the struct.
        let min_interval = config.min_checkpoint_interval;

        let handle = Arc::new(Handle {
            id: NEXT_HANDLE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            db_path: db_path.to_path_buf(),
            wal_path: wal_path_for(db_path),
            marker_path: durability_marker::marker_path_for(db_path),
            mirror_path,
            synced: Mutex::new(Vec::new()),
            // ADR-0110 D7. Deliberately the DEFAULT (high-water 0, no
            // identities, no breach) rather than a boot-time sample: at boot
            // the WAL has just been replayed and folded by our own open, so
            // there is no honest "before" to compare against. The first
            // observation baselines; the fence can only fire on the SECOND.
            wal_watermark: Mutex::new(WalMark::default()),
            durability_alert: Mutex::new(None),
            meta,
            tenant: tenant.as_str().to_string(),
            config,
            inner: Mutex::new(Inner {
                conn: Some(conn),
                debouncer: CheckpointDebouncer::new(min_interval),
                // ADR-0110 D5. A fresh Handle has seen nothing, so its first
                // drop can only BASELINE — the mirror-freeze alarm, like the
                // D7 fence, can fire on the second observation at the earliest.
                last_synced_head: None,
                mirror_freeze_reported: false,
            }),
        });
        // ADR-0110 D7 / B2 — re-derive a sticky durability alert from the
        // surviving audit mirror, as part of CONSTRUCTING the Handle.
        //
        // Here rather than at serve's call site for two reasons. It cannot be
        // forgotten by a new boot path: every route to a live Handle goes
        // through this constructor. And it keeps `serve::open_tenant_handle` a
        // single tail expression — binding the fresh Handle to a local and then
        // reading through it is precisely the shape the CHECK N structural
        // read-fork rule flags, and that rule is right to flag it; the fix is
        // not to allow-list the one function whose job is opening the Handle.
        //
        // Ungated by `wal_fence_enabled` on purpose: a loss recorded while the
        // fence was armed must still resurface on a boot where it is disarmed.
        // Turning a safety flag off must not erase an outstanding alarm.
        handle.restore_durability_alert();
        Ok(handle)
    }

    /// Production constructor: [`HandleConfig::default`] (H3 posture — checkpoint
    /// disabled).
    pub fn open_default(db_path: &Path, tenant: TenantId) -> Result<Arc<Handle>, DbError> {
        Self::open(db_path, tenant, HandleConfig::default())
    }

    /// The live DB path (for callers that still need it for log messages or to
    /// pass to a path-taking helper — *not* to open it).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The mirror (`<db>.audit.log`) path.
    pub fn mirror_path(&self) -> &Path {
        &self.mirror_path
    }

    /// ADR-0110 D5 — the durability-alert marker (`<db>.durability-alert`).
    ///
    /// Exposed so callers that need to reason about the tenant's on-disk
    /// artifacts (the D5 pins; anything enumerating what a recovery must
    /// preserve) name it through the one derivation instead of rebuilding the
    /// path — a second copy of that rule would eventually be a second store.
    pub fn durability_marker_path(&self) -> &Path {
        &self.marker_path
    }

    /// **ADR-0110 D3 — the durable-ack boundary.** Force the just-committed
    /// money-path write onto stable storage, so an acknowledged write survives
    /// a power loss (ADR-0110 R1).
    ///
    /// # The defect this closes
    ///
    /// Before this existed, `grep -rn 'sync_all\|sync_data\|fsync'
    /// crates/aberp-db/src/` returned **zero functional hits**. A committed
    /// invoice lived only in an un-`fsync`'d `<db>.wal`; the main file advanced
    /// only at the next boot's fold. On 2026-08-08 a force-restart kept the
    /// `fsync`'d audit mirror and lost ~22 h of business rows — a flawless
    /// ledger sitting on frozen rows (ADR-0110 §0, §2.2, §2.7 hypothesis H-A).
    ///
    /// # Why `fsync` the WAL rather than fold it (Option B, not Option A)
    ///
    /// ADR-0110 §4 Option B. DuckDB pushes its WAL records out to `<db>.wal` at
    /// commit and **replays them on the next open**, so an `fsync`'d WAL is a
    /// complete, self-describing durable record of the commit. Folding instead
    /// (Option A / H4) would rewrite the main file on **every** invoice, which
    /// (a) re-opens the in-place `duckdb#23046` torn-metadata path the F-A
    /// pragmas exist to close, (b) costs the whole WAL per ack rather than one
    /// small append-only flush, and (c) needs a `live_durable_checkpoint`
    /// primitive that does not exist in this tree. Option B changes **nothing**
    /// about what the boot openers see: the WAL still replays and still folds
    /// at boot exactly as before.
    ///
    /// # What it `fsync`s, and why each one
    ///
    /// 1. **`<db>` (the main file)** — the WAL is replayed *against* the main
    ///    file. If the last boot fold is still sitting in the page cache, a
    ///    durable WAL replayed onto a half-written base is not a recovery. Free
    ///    when clean, which at runtime it always is (nothing folds in place).
    /// 2. **`<db>.wal`** — the file that actually holds the acked rows. Skipped
    ///    (not an error) when absent: no WAL means the rows are already in the
    ///    main file, and the contract holds either way.
    /// 3. **The parent directory** — so a WAL *created* by this commit has a
    ///    durable directory entry. Without it the whole file can vanish. Not
    ///    recorded in the journal: a directory is not a durable-set member, it
    ///    is what makes the members findable.
    ///
    /// The audit mirror is deliberately absent: [`WriteGuard::drop`] has
    /// already `fsync`'d it via [`aberp_audit_ledger::sync_mirror`], which runs
    /// before this is ever called.
    ///
    /// # Call it AFTER the guard drops
    ///
    /// This takes **no** lock. It does not need one — `fsync` flushes whatever
    /// bytes are in the page cache and can never tear a file, and our own
    /// commit's records were complete before `commit()` returned. Taking the
    /// writer mutex would be actively wrong: money paths call this at the ack,
    /// where a re-acquire trips the re-entrancy tripwire if the guard is still
    /// alive, and needlessly serializes every other writer behind an `fsync`
    /// otherwise. `drop(guard)` first, then call this.
    ///
    /// # ADR-0110 D7 — the fence in front of all of that
    ///
    /// Everything above assumes the files we are about to `fsync` are still
    /// OUR files. On 2026-08-12 that assumption broke: a foreign
    /// `Connection::open` on the tenant DB carrying DuckDB's DEFAULT pragmas
    /// FOLDS and TRUNCATES the live Handle's WAL when it closes, and every
    /// subsequent Handle commit then returns `Ok` while reaching no file.
    ///
    /// D3's ack was blind to it *by construction*, because it `fsync`s
    /// **paths**: after the truncation `<db>.wal` is gone, so
    /// `if wal_path.exists()` SKIPPED it, the main-file `fsync` succeeded, and
    /// this returned `Ok(())`. A green light with nothing behind it — strictly
    /// worse than no ack at all, because it is believed.
    ///
    /// So the ack now begins with [`Self::observe_durable_set`], and that
    /// `exists()` test is no longer where the missing-WAL decision is made: a
    /// WAL that is absent *after this Handle has seen bytes in it* is
    /// [`WalBreach::WalVanished`], not a skip. The `fsync` calls themselves
    /// additionally `fstat` the descriptor they opened and refuse to certify an
    /// inode that is not the one the watermark recorded (the D3 "`fsync` the
    /// wrong inode and report success" residual).
    ///
    /// **The fence presupposes the F-A pragmas** (`HandleConfig::
    /// disable_implicit_close_checkpoint`, always `true` in this tree). They
    /// are what make "our WAL only ever grows" true. A Handle configured
    /// without them would let DuckDB fold legitimately, and the fence would be
    /// reporting the engine's own bookkeeping as a loss.
    ///
    /// # Errors
    ///
    /// [`DbError::DurableAck`] if any `fsync` fails.
    /// [`DbError::WalTruncatedUnderWriter`] if the fence fires — a strictly
    /// worse fact, and the one that outranks everything else this method can
    /// report. **Propagate both — never downgrade either to a `warn!`.** The
    /// business transaction has already committed at this point, so the caller
    /// is choosing between "tell the operator it failed when it may have
    /// landed" and "promise durability we did not achieve". ADR-0110 R3 /
    /// CLAUDE.md rule 11 pick the first; the inverted failure mode is named in
    /// ADR-0110 §7.7.
    ///
    /// A fired fence is **not** a hard stop (Ervin, 2026-08-12): the latch is
    /// consumed as it is reported, so the next ack starts from a fresh
    /// baseline and the app keeps serving. What persists is the sticky
    /// [`Self::durability_alert`] and the loss record in the non-chained
    /// `<db>.durability-alert` marker.
    ///
    /// # Honest scope
    ///
    /// **On macOS this IS a device flush.** `File::sync_all` does not call
    /// `fsync(2)` on Apple targets: the pinned 1.97.0 stdlib routes it to
    /// `fcntl(fd, F_FULLFSYNC)` (`std/src/fs.rs` `sync_all` → `inner.fsync()` →
    /// `sys/fs/unix.rs`, `#[cfg(target_vendor = "apple")] os_fsync`). So the
    /// bytes are pushed past the drive's own write cache, not merely handed to
    /// the OS — which is what makes an acked write survive a **power loss** and
    /// not just a process kill.
    ///
    /// It is also the same primitive `crash_safe.rs::fsync_file` and the audit
    /// mirror already use, and the mirror is the store that lost nothing on
    /// 2026-08-08 — so the choice is the tree's evidenced idiom as well as the
    /// strong one.
    ///
    /// The residual is one step further down: the guarantee bottoms out at the
    /// **drive honouring the flush**. Apple guarantees that for the internal
    /// NVMe; a third-party external enclosure may lie about it. A tenant on
    /// external storage is therefore outside what this can promise (ADR-0110
    /// §12.4).
    pub fn durable_ack(&self) -> Result<(), DbError> {
        // ── ADR-0110 D7 — FENCE FIRST ────────────────────────────────────
        // `wal_fence_enabled` defaults TRUE as of D7.6 (2026-08-13): D8 emptied
        // GROUP A, D9 flock-fenced the last CLI fold-trigger, and the fence's
        // diagnostic moved off the audit chain (§15.3). The flag is kept, not
        // deleted, because the disarmed body is exactly the D3 body and that is
        // what a bisect through the dark period needs to land on.
        //
        // One live observation at the ack itself, then take whatever breach is
        // latched: this one's, or one a `WriteGuard::drop` found and could not
        // return (a `Drop` has nowhere to put an error). Taking it CLEARS it,
        // which is what keeps a fired fence from becoming a permanent write
        // refusal.
        let breach = if self.config.wal_fence_enabled {
            self.observe_durable_set();
            self.take_breach()
        } else {
            None
        };

        // Salvage anyway. Even on a breach the `fsync` is worth issuing: the
        // foreign close FOLDS before it truncates, so rows may genuinely be in
        // the main file now, just not on stable storage.
        let fsync_outcome = self.fsync_durable_set();

        // The breach OUTRANKS an `fsync` error. "Something we could not sync"
        // is a smaller fact than "the file we were about to sync is not the
        // one we wrote to".
        match breach {
            Some(b) => Err(self.raise_durability_alert(b)),
            None => fsync_outcome,
        }
    }

    /// `fsync` the durable set — main file, then WAL, then the parent
    /// directory. Split out of [`Self::durable_ack`] only so the fence above it
    /// reads as the first thing that happens.
    fn fsync_durable_set(&self) -> Result<(), DbError> {
        // Main file first, then the WAL: the WAL is only meaningful on top of a
        // durable base. Both before the directory entry that names them.
        self.fsync_and_record(&self.db_path)?;
        // Still an `exists()` test, but when the fence is armed it is no longer
        // where the missing-WAL DECISION is made — `observe_durable_set` has
        // already ruled on that. Here it means only "there is nothing to sync",
        // which on a Handle that has never written a WAL byte is the plain
        // truth.
        if self.wal_path.exists() {
            self.fsync_and_record(&self.wal_path)?;
        }
        if let Some(parent) = self.db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            // The directory is not a durable-set member, it is what makes the
            // members findable.
            fsync_path(parent)?;
        }
        Ok(())
    }

    /// `fsync` `path` and record it in the durability journal. Recording only
    /// happens on SUCCESS — the journal must mean "this is on stable storage",
    /// never "we tried".
    ///
    /// # A2, and why there is no `fstat`-compare here (PR #61 adversarial)
    ///
    /// An earlier revision re-`fstat`ed the descriptor this opens and compared
    /// it against the watermark, on the theory that opening BY PATH could
    /// `fsync` an inode that is not the one we wrote to. That check was
    /// **deleted**, for three reasons that all point the same way:
    ///
    /// * it was near-unreachable — [`Handle::observe_durable_set`] runs
    ///   immediately before and RE-BASELINES the recorded inode, so by the time
    ///   this ran the "expected" identity was already the swapped one;
    /// * it was unpinned — no test could distinguish its presence from its
    ///   absence, and its docstring claimed a mutation result that was false;
    /// * it was redundant — an inode swap is caught one step earlier by
    ///   `detect_breach` rule 4 ([`WalBreach::MainReplaced`]), which is what
    ///   `swapping_the_inode_between_commit_and_ack_must_fail_the_ack`
    ///   actually exercises.
    ///
    /// A check that cannot fire, cannot be tested, and duplicates a check that
    /// can is not defence in depth; it is a second thing to maintain and a
    /// false claim in the docs (CLAUDE.md rule 12).
    fn fsync_and_record(&self, path: &Path) -> Result<(), DbError> {
        let f = std::fs::File::open(path).map_err(|source| DbError::DurableAck {
            path: path.to_path_buf(),
            source,
        })?;
        f.sync_all().map_err(|source| DbError::DurableAck {
            path: path.to_path_buf(),
            source,
        })?;
        // A poisoned journal mutex must not fail a write that IS now durable:
        // the journal is evidence, not the contract. Recover in place.
        let mut synced = match self.synced.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                self.synced.clear_poison();
                poisoned.into_inner()
            }
        };
        if !synced.iter().any(|p| p == path) {
            synced.push(path.to_path_buf());
        }
        Ok(())
    }

    /// ADR-0110 D3 — the durability journal: every file this handle has
    /// `fsync`'d via [`Self::durable_ack`], in first-sync order.
    ///
    /// Read by the ADR-0110 D6b power-loss spec to build its durable set out of
    /// what the write path actually certified, so that deleting the `fsync`
    /// deletes the file from the set and turns the spec red. Directories are
    /// never listed (see [`Self::durable_ack`]).
    pub fn fsynced_paths(&self) -> Vec<PathBuf> {
        match self.synced.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    // ── ADR-0110 D7 — the WAL fence ──────────────────────────────────────

    /// Take ONE ordered observation of this Handle's durable set (`<db>` and
    /// `<db>.wal`) and fold it into the watermark, latching a breach if what
    /// the filesystem says contradicts what we last saw.
    ///
    /// Called from exactly two places, and they are the same call: at every
    /// [`WriteGuard::drop`] — right after the lockstep `sync_mirror`, so under
    /// the writer lock, once per committed write — and at the top of
    /// [`Self::durable_ack`]. Sampling at the drop is what makes the fence able
    /// to see a truncation that happened BETWEEN two writes: the intervening
    /// commit re-creates a small WAL, so by ack time a naive
    /// stat-and-compare would find a perfectly self-consistent state and miss
    /// the loss entirely. The high-water is what remembers.
    ///
    /// # Why the byte counter is a MONOTONE high-water (the race argument)
    ///
    /// The Handle serializes writers, but `durable_ack` deliberately takes no
    /// writer lock, and daemons commit concurrently with a money path's ack. So
    /// between our sample and our comparison the WAL can legitimately GROW. A
    /// last-seen-length check would read that as drift; a high-water check
    /// reads it as what it is. Growth is always fine; only a SHRINK is a fact
    /// about the world that our own pragmas say cannot happen.
    ///
    /// Two more things make it race-tolerant rather than merely optimistic:
    ///
    /// 1. **The `stat` happens INSIDE the watermark mutex**, not before it.
    ///    That closes the one race a high-water alone would not: thread A
    ///    `stat`s 900 bytes, thread B's observation raises the water to 1000,
    ///    thread A then compares 900 against 1000 and fires on a healthy
    ///    tenant. Holding the lock across `stat`-compare-update totally orders
    ///    observations, so the water a comparison sees is always one the
    ///    comparison's own `stat` already includes.
    ///
    ///    Consequence worth being explicit about, since it is easy to claim
    ///    otherwise: *given* that ordering, a last-seen length would already
    ///    equal the high-water on a healthy tenant, so `max` is not what
    ///    closes the race — the lock is. `max` states the invariant the
    ///    `wal_high_water` field's name asserts, and keeps the comparison
    ///    correct if a future observation path is ever added that does not
    ///    hold the lock across its `stat`. Neither mutation (last-seen
    ///    assignment; `stat` moved outside the lock with a 2 ms window) could
    ///    be made to go red, and the test that covers this says so.
    /// 2. **The only sanctioned shrink is one we performed**
    ///    ([`WalMark::folded_by_us`], set around our own drop-and-reopen).
    ///    Nothing else in this tree folds this WAL: the F-A pragmas disable
    ///    both the close-checkpoint and the auto-checkpoint, `take.rs` snapshots
    ///    a COPY (pinned by `snapshot_does_not_fold_the_handles_wal`), and H4's
    ///    fold is still a stub.
    ///
    /// # What cannot fire it
    ///
    /// * **A boot fold.** A fresh Handle's watermark is the default — high-water
    ///   `0`, no identities — so the first observation only baselines. The fence
    ///   needs a "before" and a boot has none.
    /// * **Booting onto a pre-existing WAL.** Same reason: whatever is there at
    ///   the first observation becomes the baseline, however large.
    /// * **Concurrent daemon writes.** Growth only; see above.
    /// * **A legitimate reopen** (post-poison recovery). `folded_by_us`
    ///   re-baselines and consumes itself.
    /// * **A `stat` that fails for any reason other than ENOENT** — see N2
    ///   below. "I could not look" is not "it is gone".
    fn observe_durable_set(&self) {
        let mut mark = self.lock_watermark();

        // Inside the lock, deliberately — see the race argument above.
        //
        // N2 (PR #61 adversarial): ONLY `NotFound` may be read as "the WAL is
        // absent". Any other `stat` error — ESTALE, EIO, ETIMEDOUT, the things
        // a NAS or a removable mount produces when it hiccups — means we could
        // not look, and must resolve to "not checked" rather than to
        // `WalVanished`, which is the single most severe verdict this fence
        // can reach. `.ok()` collapsed those two into the same `None` and so
        // turned a flaky mount into a durability-loss alarm.
        let wal_md = match std::fs::metadata(&self.wal_path) {
            Ok(md) => WalStat::Present(md),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => WalStat::Absent,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    wal = %self.wal_path.display(),
                    "aberp-db: ADR-0110 D7 — could not stat the WAL (not ENOENT); treating this \
                     observation as NOT CHECKED rather than as a truncation. The watermark is \
                     left untouched, so the next readable observation still compares against \
                     the last thing we actually saw."
                );
                WalStat::Unreadable
            }
        };
        // Same rule for the main file, and it already had the softer landing:
        // an unreadable main file leaves `main_id` `None`, and `detect_breach`
        // rule 4 requires `Some` on BOTH sides, so it resolves to "not
        // checked" and the `fsync`'s own error reports the real problem.
        let main_md = std::fs::metadata(&self.db_path).ok();

        // An unreadable WAL must not move the watermark at all. Returning here
        // rather than falling through is what makes "not checked" mean it:
        // falling through would raise the high-water to 0 or clear `wal_id`,
        // quietly destroying the baseline the next observation compares to.
        let wal_md = match wal_md {
            WalStat::Unreadable => return,
            WalStat::Absent => None,
            WalStat::Present(md) => Some(md),
        };
        let wal_len = wal_md.as_ref().map_or(0, |m| m.len());
        let wal_id = wal_md.as_ref().and_then(file_id);
        let main_id = main_md.as_ref().and_then(file_id);

        // A fold WE performed is the one legitimate shrink. Consume the flag
        // and re-baseline on whatever the reopen left behind.
        if mark.folded_by_us {
            mark.folded_by_us = false;
            mark.wal_high_water = wal_len;
            mark.wal_id = wal_id;
            mark.main_id = main_id.or(mark.main_id);
            return;
        }

        if let Some(kind) = detect_breach(&mark, wal_md.is_some(), wal_len, wal_id, main_id) {
            let (expected, observed) = match kind {
                WalBreach::WalVanished | WalBreach::WalShrank => (mark.wal_high_water, wal_len),
                WalBreach::WalReplaced => (
                    mark.wal_id.map_or(0, |f| f.ino),
                    wal_id.map_or(0, |f| f.ino),
                ),
                WalBreach::MainReplaced => (
                    mark.main_id.map_or(0, |f| f.ino),
                    main_id.map_or(0, |f| f.ino),
                ),
                // [`detect_breach`] is the WAL detector and returns only the
                // four WAL shapes; D5's kind is raised directly by
                // `WriteGuard::drop` and never latched here. Spelled out rather
                // than folded into a `_` arm so that adding a kind to
                // `detect_breach` has to come here and say what its two numbers
                // mean, instead of silently inheriting someone else's.
                WalBreach::AuditMirrorFrozen => unreachable!(
                    "detect_breach cannot return AuditMirrorFrozen — it inspects the WAL, \
                     not the audit mirror"
                ),
            };
            // Latch — a `Drop` cannot return, so `durable_ack` reports it. Keep
            // the FIRST unreported breach: it names what actually went wrong,
            // and the ones that follow are its echoes.
            mark.breach.get_or_insert(Breach {
                kind,
                expected,
                observed,
            });
            // RE-BASELINE onto the post-truncation reality. This is what makes
            // "keep serving" true rather than aspirational: without it every
            // later ack would re-detect the same historical shrink and every
            // money path would 5xx forever, which is the sticky refusal Ervin
            // explicitly ruled out.
            mark.wal_high_water = wal_len;
            mark.wal_id = wal_id;
            mark.main_id = main_id.or(mark.main_id);
            return;
        }

        // Healthy. Raise the water (never lower it) and refresh identities.
        mark.wal_high_water = mark.wal_high_water.max(wal_len);
        if wal_id.is_some() {
            mark.wal_id = wal_id;
        }
        if main_id.is_some() {
            mark.main_id = main_id;
        }
    }

    /// Take the latched breach, clearing it. Taking is what stops one
    /// truncation from failing every future ack (see [`Self::durable_ack`]).
    fn take_breach(&self) -> Option<Breach> {
        self.lock_watermark().breach.take()
    }

    /// Announce that THIS Handle is about to fold its own WAL (by dropping and
    /// reopening the shared connection, which replays and may checkpoint it).
    /// The next observation re-baselines instead of firing the fence.
    fn note_self_fold(&self) {
        self.lock_watermark().folded_by_us = true;
    }

    /// The watermark mutex, poison-recovered in place. A panic elsewhere must
    /// not brick the fence — a fence that stops answering is a fence that
    /// silently stops protecting.
    fn lock_watermark(&self) -> MutexGuard<'_, WalMark> {
        match self.wal_watermark.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                self.wal_watermark.clear_poison();
                poisoned.into_inner()
            }
        }
    }

    /// The sticky durability alert, if the fence has ever fired.
    ///
    /// Surfaced on `GET /health` and rendered as the SPA's red banner. Survives
    /// every subsequent healthy ack — only [`Self::clear_durability_alert`]
    /// takes it down.
    pub fn durability_alert(&self) -> Option<DurabilityAlert> {
        match self.durability_alert.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Explicitly clear the sticky durability alert. The operator acknowledges
    /// a durability loss; the code does not decide it stopped mattering.
    ///
    /// **Call this only alongside a durable `db.durability_alert_acknowledged`
    /// audit row** (`serve::acknowledge_durability_alert` is the one production
    /// caller). Clearing the in-memory flag on its own is not an
    /// acknowledgement — it is amnesia, and the next boot's
    /// [`Self::restore_durability_alert`] would correctly bring the banner
    /// straight back.
    ///
    /// # ADR-0110 D5 — the durable half now includes the marker
    ///
    /// The ledger ack row alone no longer takes the banner down, because a D5
    /// episode never reached the ledger: it lives in
    /// [`crate::durability_marker`]. So this appends the marker's own `ack`
    /// record FIRST and clears the in-memory flag only if that succeeded —
    /// the same ordering, and the same reason, as the route's audit-row-first
    /// rule. A failed marker write returns `Err` with the banner still UP,
    /// which is the safe direction: clearing memory while the durable half
    /// stayed raised is precisely the "operator watched the banner drop and it
    /// came back next boot" defect D7.4b fixed.
    ///
    /// # When the marker file itself is broken (R5-N2)
    ///
    /// If the marker cannot be written — permissions, a full or read-only
    /// volume, something else occupying the path — this returns `Err` on every
    /// attempt and **the banner cannot be cleared until that is fixed**. That
    /// is deliberate and it is not a bug to route around: the alternative is
    /// clearing a real durability loss with no durable record that anyone
    /// acknowledged it, which is the amnesia D7.4b closed. The failure is an
    /// operator-attention filesystem fault on a path beside the tenant DB,
    /// logged at ERROR with the path on every attempt; fixing it makes the
    /// alert immediately acknowledgeable again. See
    /// [`crate::durability_marker::read`] for the same split on the read side.
    pub fn clear_durability_alert(&self) -> Result<(), DbError> {
        durability_marker::record_ack(&self.marker_path, OffsetDateTime::now_utc()).map_err(
            |e| {
                tracing::error!(
                    error = %e,
                    marker = %self.marker_path.display(),
                    "aberp-db: ADR-0110 D5 — could NOT record the acknowledgement in the \
                     durability-alert marker; leaving the banner UP rather than clearing a \
                     flag whose durable half is still raised"
                );
                DbError::DurabilityMarker {
                    path: self.marker_path.clone(),
                    source: e,
                }
            },
        )?;
        let mut slot = match self.durability_alert.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                self.durability_alert.clear_poison();
                poisoned.into_inner()
            }
        };
        *slot = None;
        Ok(())
    }

    /// **ADR-0110 D7 / B2 — re-derive the sticky alert at boot, from BOTH
    /// durable stores.**
    ///
    /// # The defect this closes
    ///
    /// The banner tells the operator to *stop and recover*. Before B2, the
    /// restart it asks for was the **mute button**: the alert lived only in
    /// this process's memory, so it died with the process, and the
    /// `db.durability_loss_detected` row went with it — the fence wrote that
    /// row into the very database whose WAL was just truncated, so the DB copy
    /// was exactly the copy most likely to be lost. "Keep serving" degraded to
    /// "keep serving and forget", which is the failure mode the whole design
    /// exists to avoid.
    ///
    /// # Why BOTH stores, and not the mirror alone (R2-B1)
    ///
    /// The first cut read the mirror only, on the sound reasoning that the
    /// mirror is `fsync`'d and is the store that lost nothing on 2026-08-08.
    /// That is right about the LOSS row and wrong about the ACKNOWLEDGEMENT,
    /// and the gap made the Acknowledge button inert in the one scenario it
    /// exists for:
    ///
    /// 1. A real truncation costs the DB its WAL-resident rows, so on the next
    ///    boot the **DB head REGRESSES**. The mirror is append-only, so it
    ///    keeps the higher seq.
    /// 2. The mirror is now AHEAD of the DB, and
    ///    [`aberp_audit_ledger::sync_mirror`] answers that with
    ///    `MirrorDivergent` — it appends **nothing**.
    /// 3. [`WriteGuard::drop`] only `warn!`s on a failed mirror sync, so the
    ///    mirror stays frozen for the rest of the process: every audit row
    ///    written after the incident is refused, including the
    ///    acknowledgement. (R3-N2: within a process, not permanently. Serve's
    ///    boot reconcile attempts a gated auto-heal that replays the DB up to
    ///    the mirror head and un-freezes it — or refuses, in which case serve
    ///    does not boot.)
    /// 4. The ack still commits to the DB and the route still returns 200, so
    ///    the operator watches the banner drop and believes it is done — and
    ///    the next boot, reading the mirror alone, re-raises it. Forever.
    ///
    /// So each store is consulted for what it is actually authoritative about:
    /// the **mirror** survives a truncation and is where the loss lives; the
    /// **DB** is what still accepts writes afterwards and is where the ack
    /// lives. Reading both needs no new artifact and no new failure mode.
    ///
    /// With the boot reconcile in the picture (R3-N2) the DB half is
    /// **defence in depth** rather than the common path: after a healed boot
    /// the mirror takes appends again and would carry the ack by itself. The
    /// window the DB half still covers is real but narrow — a mid-process
    /// `sync_mirror` divergence, and the span between an acknowledgement and
    /// the next boot's reconcile. It is pinned directly by
    /// `an_ack_that_reached_only_the_db_still_clears_a_loss_that_reached_only_the_mirror`.
    ///
    /// # ADR-0110 D5 — and a THIRD source, which is now the live one
    ///
    /// D5 (route (a), Ervin 2026-08-13) records a mirror freeze in the
    /// non-chained [`crate::durability_marker`] instead of the ledger, because
    /// an append at that moment forks the two chains and refuses the next boot
    /// (D5-B1). So the marker is read here too, and it is the source that
    /// carries every D5 episode.
    ///
    /// # …and since D7.6 the marker is the ONLY writer
    ///
    /// D7's fence records to the marker too (§15.3), so no code path in this
    /// tree still appends `db.durability_loss_detected`. The two ledger halves
    /// are nevertheless KEPT, not retired: a prod tenant recovered from incident
    /// 00012 may already hold such a row, and dropping the reader to "clean up"
    /// would silently stop re-raising it. They are backward-compat parsing, and
    /// they are the reason the ack half still has to read the DB at all —
    /// `an_ack_that_reached_only_the_db_still_clears_a_loss_that_reached_only_the_mirror`
    /// pins exactly that window.
    ///
    /// # The rule
    ///
    /// Raise the alert iff a loss exists in ANY of the three sources — the
    /// marker, the mirror, or the DB — AND no acknowledgement in any of them is
    /// **strictly newer** than it.
    ///
    /// Ordering is by RFC3339 `time_wall`, not by `seq`. `seq` cannot do it:
    /// after the regression above the two stores' sequence spaces overlap, so a
    /// DB ack at seq 12 may post-date a mirror loss at seq 40. A lexicographic
    /// string compare cannot do it either — `time`'s Rfc3339 emits
    /// variable-precision fractional seconds, so `…:00.5Z` sorts ABOVE
    /// `…:00.5000001Z`. Both stores format this field identically, so parsing
    /// and comparing instants is exact.
    ///
    /// A TIE keeps the banner UP. An ack is always causally after the loss it
    /// answers, so equal instants mean a clock too coarse to distinguish them —
    /// and the safe reading of "I cannot tell" is that the loss stands.
    ///
    /// # Failure posture
    ///
    /// Best-effort and quiet: a missing mirror (a fresh tenant) is the normal
    /// case, and an unreadable one is logged, not fatal. This must never stop
    /// `serve` booting — refusing to boot over a *historical* warning would
    /// turn a durability alert into an outage.
    ///
    /// The mirror read goes through [`aberp_audit_ledger::read_mirror_under_tail_policy`]
    /// (R2-B2), the same reader the boot reconciler uses, NOT the strict
    /// [`aberp_audit_ledger::read_mirror_entries`]. The strict one rejects an
    /// unterminated final line — the commonest crash artifact there is, and
    /// precisely the condition most likely to co-occur with a durability
    /// incident — which would have made a torn tail silently swallow the alarm.
    /// The tail policy hands back the chain-reverified intact prefix instead.
    pub fn restore_durability_alert(&self) {
        let mirror_rows = self.mirror_audit_times();
        let db_rows = self.db_audit_times();
        let marker = durability_marker::read(&self.marker_path);

        // Newest of each kind across ALL THREE sources.
        let newest = |a: Option<OffsetDateTime>, b: Option<OffsetDateTime>| match (a, b) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (x, y) => x.or(y),
        };
        let marker_loss = marker.loss.map(|(at, _)| at);
        let loss = newest(newest(mirror_rows.loss, db_rows.loss), marker_loss);
        let ack = newest(newest(mirror_rows.ack, db_rows.ack), marker.ack);

        let Some(loss) = loss else { return };
        // Strictly newer, so a tie keeps the banner up (see the docs).
        if ack.is_some_and(|a| a > loss) {
            tracing::info!(
                db = %self.db_path.display(),
                "aberp-db: ADR-0110 D7 — a past durability loss is recorded, and an operator \
                 acknowledgement post-dates it; the banner stays down"
            );
            return;
        }

        // ADR-0110 D5 / N2 — report the breach that was actually DETECTED.
        //
        // Before the marker there was nothing to read it from: the machine code
        // is not cheaply recoverable from either ledger store (the mirror
        // base64-encodes the payload), so this hard-coded `WalVanished` — the
        // shape the 00012 mechanism produces and the one the generic message
        // describes. That was a guess, and after a restart it turned every D5
        // mirror-freeze into a reported WAL truncation on `/health` and in the
        // banner, losing the one distinction a recovery actually turns on.
        //
        // The marker stores the real code, so when the marker holds the newest
        // loss, its breach is used. The ledger-sourced path keeps the old
        // default, because for those rows the guess is still the best available
        // answer — and it is right for the fence rows that dominate them.
        let breach = match marker.loss {
            Some((at, Some(code))) if at == loss => code,
            _ => WalBreach::WalVanished,
        };
        let message = format!(
            "Durability loss detected on the tenant database (recorded {}): {breach}. Stop and \
             recover. This alert survived a restart and stays up until it is acknowledged.",
            loss.format(&Rfc3339)
                .unwrap_or_else(|_| "at an unreadable time".to_string()),
        );
        tracing::error!(
            db = %self.db_path.display(),
            breach = breach.code(),
            "aberp-db: ADR-0110 D7/D5 — RE-RAISING an UNACKNOWLEDGED durability loss found in \
             the durable record. A restart is not an acknowledgement."
        );
        self.set_sticky_alert(breach, message);
    }

    /// The DB's audit head seq, read on an ALREADY-HELD connection.
    ///
    /// One indexed aggregate, and it is only ever called on the D5 divergence
    /// path — never on the healthy write path, which stays bit-for-bit what it
    /// was. Takes `&Connection` rather than reaching for [`Self::read`],
    /// because its caller is inside [`WriteGuard::drop`] and `read()` locks the
    /// same writer mutex that guard is holding (a self-deadlock; in debug the
    /// re-entrancy tripwire panics first).
    ///
    /// `None` on any query failure. The caller treats that as "cannot tell",
    /// not as "regressed" — see the `MirrorDivergent` arm.
    fn audit_head_seq(&self, conn: &Connection) -> Option<u64> {
        match conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM audit_ledger", [], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(v) => Some(v as u64),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    db = %self.db_path.display(),
                    "aberp-db: ADR-0110 D5 — could not read the audit head to decide whether \
                     this divergence cost us rows; not raising on a question we could not put"
                );
                None
            }
        }
    }

    /// Set the sticky operator alert, keeping any alert already up.
    ///
    /// `get_or_insert`, never overwrite: the operator needs to know when the
    /// tenant STARTED losing writes, not when it last did. Poison-recovering
    /// for the same reason every other lock in this file is — a panic in an
    /// unrelated holder must not be able to take the alarm down.
    fn set_sticky_alert(&self, breach: WalBreach, message: String) {
        let mut slot = match self.durability_alert.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                self.durability_alert.clear_poison();
                poisoned.into_inner()
            }
        };
        slot.get_or_insert(DurabilityAlert {
            breach,
            message,
            detected_at: SystemTime::now(),
        });
    }

    /// Newest `time_wall` of each durability kind in the audit MIRROR.
    ///
    /// Tail-tolerant (R2-B2): a torn trailing line yields the intact prefix
    /// rather than an error, because the alarm must survive the crash artifact
    /// most likely to accompany the incident that raised it.
    fn mirror_audit_times(&self) -> DurabilityAuditTimes {
        use aberp_audit_ledger::MirrorTailPolicy;
        let entries = match aberp_audit_ledger::read_mirror_under_tail_policy(&self.mirror_path) {
            Ok(MirrorTailPolicy::Clean(e)) => e,
            Ok(MirrorTailPolicy::TornTail { entries, .. }) => {
                tracing::warn!(
                    mirror = %self.mirror_path.display(),
                    intact_entries = entries.len(),
                    "aberp-db: ADR-0110 D7 — the audit mirror has a TORN TRAILING LINE; \
                     re-deriving the durability alert from the chain-reverified intact prefix. \
                     Not trimming here — that is the boot reconciler's job."
                );
                entries
            }
            Ok(MirrorTailPolicy::DeepCorrupt { reason }) => {
                tracing::error!(
                    reason = %reason,
                    mirror = %self.mirror_path.display(),
                    "aberp-db: ADR-0110 D7 — the audit mirror is DEEPLY corrupt, so it cannot be \
                     consulted for a durability alert. The DB is still checked below."
                );
                Vec::new()
            }
            Err(e) => {
                // A fresh tenant has no mirror yet; that is not news.
                if !matches!(&e, aberp_audit_ledger::AppendError::MirrorIo(io)
                    if io.kind() == std::io::ErrorKind::NotFound)
                {
                    tracing::warn!(
                        error = %e,
                        mirror = %self.mirror_path.display(),
                        "aberp-db: ADR-0110 D7 — could not read the audit mirror to re-derive a \
                         durability alert. The DB is still checked below."
                    );
                }
                Vec::new()
            }
        };
        let newest_of = |kind: EventKind| -> Option<OffsetDateTime> {
            entries
                .iter()
                .filter(|e| e.kind == kind.as_str())
                .filter_map(|e| OffsetDateTime::parse(&e.time_wall, &Rfc3339).ok())
                .max()
        };
        DurabilityAuditTimes {
            loss: newest_of(EventKind::DbDurabilityLossDetected),
            ack: newest_of(EventKind::DbDurabilityAlertAcknowledged),
        }
    }

    /// Newest `time_wall` of each durability kind in the live DB.
    ///
    /// This is the half that makes the Acknowledge button work after a real
    /// loss: post-incident the mirror is frozen (`MirrorDivergent`), so the DB
    /// is the only store still accepting the acknowledgement.
    ///
    /// Deliberately a narrow per-kind `SELECT` rather than
    /// [`aberp_audit_ledger::Ledger::entries`]: building a `Ledger` VERIFIES
    /// the hash chain, and the tenant we are booting has just had rows
    /// truncated out from under it — the one situation where a chain verify is
    /// expected to fail. Re-deriving an alert must not depend on the chain
    /// being intact, precisely because the alert means it may not be.
    ///
    /// # R3-N1 — SELECT the rows, PARSE each, then `.max()`. Never `SQL MAX`.
    ///
    /// `time_wall` is a **VARCHAR**, so `SELECT MAX(time_wall)` is a
    /// LEXICOGRAPHIC max — exactly the comparison the `time` dependency and its
    /// Cargo.toml note exist to avoid, reintroduced in SQL. `time`'s Rfc3339
    /// trims trailing zeros, so the two orders disagree on ordinary
    /// same-second stamps: `"…10:00:00Z"` sorts ABOVE `"…10:00:00.9Z"` because
    /// `'Z'` (0x5A) > `'.'` (0x2E).
    ///
    /// It failed toward **banner down**, twice over. MAX could hand back an
    /// older loss row and let an earlier acknowledgement out-rank it; and
    /// because MAX collapses the column BEFORE anything is parsed, a single
    /// malformed stamp that won the string compare was selected, failed to
    /// parse, and took the entire DB-side loss verdict with it — good rows
    /// included. Parsing first makes one bad row cost exactly that row.
    fn db_audit_times(&self) -> DurabilityAuditTimes {
        let mut out = DurabilityAuditTimes::default();
        let conn = match self.read() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    db = %self.db_path.display(),
                    "aberp-db: ADR-0110 D7 — could not read the DB to re-derive a durability \
                     alert; falling back to the audit mirror alone"
                );
                return out;
            }
        };
        let newest_of = |kind: EventKind| -> Option<OffsetDateTime> {
            let mut stmt = conn
                .prepare("SELECT time_wall FROM audit_ledger WHERE kind = ?")
                .ok()?;
            let rows = stmt
                .query_map([kind.as_str()], |row| row.get::<_, String>(0))
                .ok()?;
            rows.filter_map(|r| r.ok())
                // Parse EACH row and drop only the ones that will not parse —
                // the same shape `mirror_audit_times` uses.
                .filter_map(|s| OffsetDateTime::parse(&s, &Rfc3339).ok())
                .max()
        };
        out.loss = newest_of(EventKind::DbDurabilityLossDetected);
        out.ack = newest_of(EventKind::DbDurabilityAlertAcknowledged);
        out
    }

    /// Record a fired fence three ways — log, sticky alert, **marker** — and
    /// return the error the money path will surface.
    ///
    /// Ervin's decision (2026-08-12): the app KEEPS SERVING. No sticky write
    /// refusal, no process abort. This ack fails loudly because its own
    /// durability is genuinely unknown; the persistence lives in the alert and
    /// the marker, not in a latch that poisons every later write.
    ///
    /// # Why the durable record is the MARKER and never the ledger (D5-B1,
    /// applied to D7 — ADR-0110 §15.3, the precondition for arming the fence)
    ///
    /// The first cut appended a `db.durability_loss_detected` row to the
    /// hash-chained `audit_ledger` here. D5 proved that shape can BRICK the
    /// tenant, and the mechanism is identical for D7:
    ///
    /// A WAL truncation is exactly what regresses the DB's audit head below the
    /// append-only, `fsync`'d mirror's. An append at that moment consumes the
    /// next DB `seq` — one the mirror already holds a *different* entry for.
    /// The chains fork there, and the next boot's gated auto-heal proves
    /// benignness by matching the DB head's `entry_hash` against the mirror's at
    /// the same seq. Forked ⇒ refused ⇒ `ensure_consistent_with_db` answers
    /// `MirrorAheadOfDb` ⇒ `serve` exits non-zero and does not boot. The alarm
    /// that says "stop and recover" would be what stopped the operator
    /// recovering.
    ///
    /// Ervin's rule (2026-08-13, route (a)): **a machine-spawned durability
    /// diagnostic is not a business event and must never consume a ledger
    /// seq.** D5 already obeyed it; D7 does now, and §15.3 named that as a
    /// PRECONDITION for arming `wal_fence_enabled`, not an optional tidy-up.
    /// Both triggers therefore land in the SAME non-chained
    /// [`crate::durability_marker`] and share one re-derivation, one
    /// acknowledge path, one `/health` field and one banner — the marker
    /// distinguishes them by its `trigger` column, and carries the real
    /// [`WalBreach`] code so a restart reports the breach that was DETECTED
    /// rather than a guess (the D5 N2 defect, which the ledger could not fix
    /// because the mirror base64-encodes the payload).
    ///
    /// The ledger readers in [`Self::restore_durability_alert`] are KEPT: a
    /// prod tenant recovered from incident 00012 may hold a legacy
    /// `db.durability_loss_detected` row, and retiring the reader would silently
    /// stop re-raising it. Only the WRITER moved.
    ///
    /// # No opener, no writer lock, no deadlock
    ///
    /// The old audit path took [`Self::write`] and a `try_clone` on the breach
    /// path. Both are gone: this is one `OpenOptions::append` on a sidecar file
    /// and one `sync_all`. Nothing here can nest a `write()`, so the fence path
    /// no longer has a lock-ordering question to get right at all.
    fn raise_durability_alert(&self, breach: Breach) -> DbError {
        tracing::error!(
            db = %self.db_path.display(),
            wal = %self.wal_path.display(),
            breach = breach.kind.code(),
            expected = breach.expected,
            observed = breach.observed,
            "aberp-db: ADR-0110 D7 DURABILITY LOSS DETECTED — {}. A foreign DuckDB opener \
             folded and truncated this Handle's WAL; commits since then may have returned Ok \
             without reaching stable storage. The app keeps serving; the operator alert is \
             sticky until explicitly cleared.",
            breach.kind
        );

        self.set_sticky_alert(
            breach.kind,
            format!(
                "Durability loss detected on the tenant database: {}. \
                 Recent writes may not have reached disk. Stop and recover.",
                breach.kind
            ),
        );

        // Best-effort, and loud when it fails: the in-memory alert is already
        // up, so a failed marker write costs the restart-survival half, not the
        // banner in front of the operator right now. The `detail` column
        // carries `expected` — the WAL high-water for a truncation, the
        // recorded inode for a replacement — which is the forensic half the
        // observed value is only meaningful against.
        if let Err(e) = durability_marker::record_loss(
            &self.marker_path,
            OffsetDateTime::now_utc(),
            "wal_truncated_under_writer",
            breach.kind,
            breach.expected,
        ) {
            tracing::error!(
                error = %e,
                marker = %self.marker_path.display(),
                "aberp-db: ADR-0110 D7 — could NOT record the durability-alert marker. The \
                 detection was logged loudly, the sticky alert is set and the ack still fails, \
                 but this alert will not survive a restart."
            );
        }

        DbError::WalTruncatedUnderWriter {
            breach: breach.kind,
            db: self.db_path.clone(),
            wal: self.wal_path.clone(),
            expected: breach.expected,
            observed: breach.observed,
        }
    }

    /// **ADR-0110 D5 — a frozen audit mirror is a durability fault, raised on
    /// the SAME surface D7 built.** Called from [`WriteGuard::drop`] and
    /// nowhere else.
    ///
    /// # What happened, and why a `warn!` was the wrong answer
    ///
    /// `sync_mirror` answered `MirrorDivergent`: the mirror's head does not
    /// agree with the DB's, so it appended NOTHING. That state is not
    /// self-healing inside a process — the next write re-derives the same
    /// divergence and refuses again. The mirror is frozen, and the mirror is
    /// the `fsync`'d store. So from this moment every audit row (a D7
    /// acknowledgement, a *second* durability loss) exists in the DB alone,
    /// and the DB is the store that has just been shown to lose rows.
    ///
    /// # Why the alert goes to the MARKER and never to the ledger (D5-B1)
    ///
    /// The first cut appended a `db.durability_loss_detected` row here. That
    /// could BRICK the tenant, and it is the whole reason this method now
    /// touches no SQL at all.
    ///
    /// The freeze is detected exactly when the DB head has regressed below the
    /// append-only mirror's. An append at that moment consumes the next DB
    /// `seq` — one the mirror already holds a *different* entry for. The chains
    /// fork there, and the next boot's gated auto-heal proves benignness by
    /// matching the DB head's `entry_hash` against the mirror's at the same
    /// seq. Forked ⇒ refused ⇒ `serve` exits non-zero. The diagnostic that says
    /// "stop and recover" would have been what stopped the operator recovering.
    ///
    /// Ervin's rule (2026-08-13): a machine-spawned durability diagnostic is
    /// not a business event and must never consume a ledger seq. It goes to
    /// [`crate::durability_marker`], which is append-only, `fsync`'d, chained to
    /// nothing, and writable while the mirror is frozen — the whole point.
    /// Everything the operator sees is unchanged: the same sticky alert, the
    /// same `GET /health durability_alert`, the same red banner, the same
    /// acknowledge route, and the same survives-a-restart property, now
    /// re-derived from the marker by [`Self::restore_durability_alert`].
    ///
    /// # Not gated on `wal_fence_enabled`
    ///
    /// The D7 flag exists because the FENCE fails `durable_ack`, and a false
    /// positive there is a money-path outage (ADR-0110 §5 D7.6). This raises an
    /// alarm and refuses nothing — the write already committed and the guard is
    /// on its way out — so it carries none of that risk, and the same reasoning
    /// that left the boot re-derivation ungated (D7.4a) applies verbatim.
    /// Gating it would also make it dead code in production, where the flag is
    /// `false`.
    ///
    /// # No new opener, no deadlock
    ///
    /// Since D5-B1 this path performs no database access whatsoever: one
    /// `OpenOptions::append` on a sidecar file, one `sync_all`. No
    /// `Connection::open` (the GROUP-A census is unchanged), no `try_clone`, no
    /// transaction, and no `AUDIT_APPEND_LOCK`. It must still never reach for
    /// [`Self::write`] / [`Self::read`] — the writer mutex is held by the very
    /// guard whose `drop` is running, and both would self-deadlock (in debug
    /// the re-entrancy tripwire panics first, because deregistration is the
    /// last thing `drop` does) — and now there is nothing on this path that
    /// could be tempted to.
    fn raise_mirror_freeze_alert(&self, mirror_head_seq: u64, db_head_seq: u64, reason: &str) {
        tracing::error!(
            db = %self.db_path.display(),
            mirror = %self.mirror_path.display(),
            mirror_head_seq,
            db_head_seq,
            reason,
            "aberp-db: ADR-0110 D5 DURABILITY LOSS DETECTED — the audit mirror REFUSED the \
             lockstep append (MirrorDivergent) and this Handle's own DB head has REGRESSED \
             below what it had already mirrored. The mirror is now FROZEN: audit rows written \
             from here on reach the DB only, never fsync'd storage. The app keeps serving; the \
             operator alert is sticky until explicitly acknowledged."
        );

        self.set_sticky_alert(
            WalBreach::AuditMirrorFrozen,
            format!(
                "Durability loss detected on the tenant database: {} (the database head fell \
                 from audit seq {mirror_head_seq} to {db_head_seq}). Stop and recover.",
                WalBreach::AuditMirrorFrozen
            ),
        );

        // Best-effort, and loud when it fails: the in-memory alert is already
        // up, so a failed marker write costs the restart-survival half, not the
        // banner in front of the operator right now.
        if let Err(e) = durability_marker::record_loss(
            &self.marker_path,
            OffsetDateTime::now_utc(),
            "audit_mirror_sync_refused",
            WalBreach::AuditMirrorFrozen,
            mirror_head_seq,
        ) {
            tracing::error!(
                error = %e,
                marker = %self.marker_path.display(),
                "aberp-db: ADR-0110 D5 — could NOT record the durability-alert marker. The \
                 detection was logged loudly and the sticky alert is set, but this alert will \
                 not survive a restart."
            );
        }
    }

    /// Acquire the **serialized writer** over the shared instance. The returned
    /// [`WriteGuard`] derefs to the one `&mut Connection`; run the existing
    /// transaction body against it exactly as before. When the guard drops, the
    /// post-commit hook fires (lockstep mirror append). Holding the guard blocks
    /// other writers — process-wide write serialization is the intended
    /// single-writer discipline (a throughput ceiling, acceptable for a
    /// single-operator CNC-shop ERP).
    pub fn write(&self) -> Result<WriteGuard<'_>, DbError> {
        // Re-entrancy tripwire (ADR-0099 H3): PANIC before the lock if this thread
        // already holds THIS Handle's write guard — the mutex is non-reentrant, so
        // the acquire below would deadlock (hung prod). Debug/test only.
        #[cfg(debug_assertions)]
        self.assert_not_reentrant("write");
        // Bug 5: recover a poisoned writer in-place instead of returning
        // `DbError::Poisoned` forever (which would brick every write path for the
        // whole process). See [`Self::lock_recovering`].
        let mut inner = self.lock_recovering()?;
        self.ensure_open(&mut inner)?;
        // Register the held guard AFTER a clean acquire (a failed lock/ensure_open
        // above returns via `?` and must not leave a phantom entry). Deregistered
        // in `WriteGuard::drop`.
        #[cfg(debug_assertions)]
        self.register_write_held();
        Ok(WriteGuard {
            handle: self,
            inner,
        })
    }

    /// A read connection: an owned [`duckdb::Connection::try_clone`] of the
    /// **same** instance (shared buffer cache; **not** a second OS open). The
    /// writer mutex is held only for the duration of the clone (cheap), not for
    /// the caller's query, so reads do not serialize behind each other.
    ///
    /// This `try_clone` is the SOLE read path (S335 coherence): a separate
    /// instance would not replay the live writer's WAL, so post-commit
    /// (WAL-only) writes would be invisible to it; a `try_clone` of the shared
    /// instance is coherent.
    ///
    /// Taking the writer mutex here is deliberate, not incidental: it is what
    /// makes a nested `read()`-inside-`write()` resolve against the **Rust**
    /// `std::sync::Mutex` — panicking on the tripwire in debug and deadlocking
    /// on the mutex in release — rather than becoming a timing-dependent
    /// engine-level wait. It keeps [`Self::assert_not_reentrant`] load-bearing
    /// rather than decoration, and keeps "single-writer" its literal meaning.
    pub fn read(&self) -> Result<Connection, DbError> {
        // Re-entrancy tripwire (ADR-0099 H3): `read()` locks the SAME writer mutex
        // to `try_clone`, so a read issued while this thread holds the write guard
        // ALSO deadlocks. PANIC before the lock. Debug/test only.
        #[cfg(debug_assertions)]
        self.assert_not_reentrant("read");
        // Bug 5: same poison-recovery as write() — a reader must not be bricked
        // by another holder's panic either.
        let mut inner = self.lock_recovering()?;
        self.ensure_open(&mut inner)?;
        let clone = inner
            .conn
            .as_ref()
            .expect("ensure_open guarantees Some")
            .try_clone()?;
        Ok(clone)
    }

    /// Loop-idle hook (D2 "+ one at loop-idle"). A daemon calls this when its
    /// queue drains; if the file is dirty since the last checkpoint we take one
    /// now (the cheapest moment), even inside the 1-min window.
    ///
    /// H3: a no-op while `checkpoint_enabled == false` (returns immediately).
    pub fn checkpoint_on_idle(&self) {
        if !self.config.checkpoint_enabled {
            return;
        }
        // Bug 5: route the idle-checkpoint lock through the SAME poison-recovery
        // path as write()/read() (never silently swallow a poisoned mutex).
        let mut inner = match self.lock_recovering() {
            Ok(inner) => inner,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    db = %self.db_path.display(),
                    "aberp-db: idle checkpoint skipped — writer poison-recovery returned a HARD error (integrity re-verify failed)"
                );
                return;
            }
        };
        if inner.debouncer.should_checkpoint_on_idle() {
            self.run_durable_checkpoint_locked(&mut inner);
        }
    }

    /// Re-entrancy tripwire (ADR-0099 H3). PANIC if this thread already holds
    /// THIS Handle's write guard: a nested [`Self::write`] — or any [`Self::read`],
    /// which locks the same mutex — would deadlock the non-reentrant writer mutex
    /// (a hung prod: invoicing stops with no error). The fix a caller must make is
    /// NEVER to re-acquire: pass the outer guard's `&Connection`/`&mut Connection`
    /// down instead. Debug/test only.
    #[cfg(debug_assertions)]
    fn assert_not_reentrant(&self, op: &str) {
        HELD_WRITE_IDS.with(|held| {
            if held.borrow().contains(&self.id) {
                panic!(
                    "aberp-db RE-ENTRANCY TRIPWIRE: Handle::{op}() on the live DB at \
                     {} while this thread ALREADY holds this Handle's write guard. \
                     The writer Mutex is non-reentrant — a nested acquire DEADLOCKS \
                     (hung prod: invoicing stops with no error). Restructure to pass \
                     the outer guard's &Connection / &mut Connection down instead of \
                     re-acquiring db.write()/db.read(). (ADR-0099 H3 §re-entrancy \
                     tripwire.)",
                    self.db_path.display()
                );
            }
        });
    }

    /// Record that this thread now holds THIS Handle's write guard (tripwire
    /// bookkeeping). Debug/test only.
    #[cfg(debug_assertions)]
    fn register_write_held(&self) {
        HELD_WRITE_IDS.with(|held| held.borrow_mut().push(self.id));
    }

    /// Drop the most-recent held-guard record for `id` (tripwire bookkeeping,
    /// called from [`WriteGuard::drop`]). `rposition` so correctly-nested guards
    /// on DIFFERENT Handles unwind LIFO. Debug/test only.
    #[cfg(debug_assertions)]
    fn deregister_write_held(id: u64) {
        HELD_WRITE_IDS.with(|held| {
            let mut v = held.borrow_mut();
            if let Some(pos) = v.iter().rposition(|&x| x == id) {
                v.remove(pos);
            }
        });
    }

    /// (Re)open the shared connection if it is not currently present.
    fn ensure_open(&self, inner: &mut Inner) -> Result<(), DbError> {
        if inner.conn.is_none() {
            inner.conn = Some(open_runtime_connection(&self.db_path, &self.config)?);
        }
        Ok(())
    }

    /// Acquire the writer mutex, RECOVERING from a poisoning panic instead of
    /// surfacing [`DbError::Poisoned`] forever (ADR-0099 H3 / Bug 5).
    ///
    /// Before the shared Handle a daemon that panicked mid-write hurt only
    /// itself. The shared Handle makes a panic while holding the [`WriteGuard`]
    /// poison the ONE process-wide writer mutex — bricking every write path
    /// (all daemons + every request handler) until a process restart: a NEW
    /// single point of failure the shared instance introduced. This heals it: on
    /// a poisoned lock we [`Mutex::clear_poison`], reclaim the guard via
    /// [`std::sync::PoisonError::into_inner`], and run
    /// [`Self::recover_from_poison`]. A benign prior panic that left the DB
    /// CONSISTENT resumes; only a FAILED integrity re-verify is a hard error.
    fn lock_recovering(&self) -> Result<MutexGuard<'_, Inner>, DbError> {
        match self.inner.lock() {
            Ok(guard) => Ok(guard),
            Err(poisoned) => {
                self.inner.clear_poison();
                let mut guard = poisoned.into_inner();
                self.recover_from_poison(&mut guard)?;
                Ok(guard)
            }
        }
    }

    /// Post-poison recovery (Bug 5). Reopen the shared connection FRESH and
    /// re-verify the audit hash-chain genesis→head; loud log + a durable audit
    /// row on success. Returns [`DbError::PoisonRecoveryFailed`] ONLY when the
    /// chain does not verify (real corruption — surfaced, never swallowed).
    fn recover_from_poison(&self, inner: &mut Inner) -> Result<(), DbError> {
        tracing::error!(
            db = %self.db_path.display(),
            "aberp-db: writer mutex POISONED by a panic in a prior guard holder; recovering (clear_poison + drop/reopen + post-poison integrity re-verify) per ADR-0099 H3 / Bug 5 — a poisoned shared writer must NOT brick the whole process"
        );

        // (1) The panicking holder may have left the shared connection mid-
        //     transaction / indeterminate. Drop and reopen FRESH on the same live
        //     inode so recovery starts clean. A failure to reopen IS a hard error
        //     (the DB genuinely will not open) and propagates via `?`.
        //
        //     ADR-0110 D7: dropping the last connection destroys the DuckDB
        //     `Database`, and the reopen REPLAYS the WAL and may fold it. That
        //     is the one shrink this Handle is allowed to cause, so declare it
        //     — otherwise the fence would read our own recovery as the foreign
        //     truncation it exists to catch, and nag on a box that is fine.
        self.note_self_fold();
        inner.conn = None;
        self.ensure_open(inner)?;

        // (2) POST-POISON INTEGRITY RE-VERIFY: verify the audit hash-chain
        //     genesis→head on a try_clone of the freshly-reopened shared instance.
        //     A mere prior panic that left the DB consistent must NOT permanently
        //     brick the process; only a FAILED verify is a hard error.
        let probe = inner
            .conn
            .as_ref()
            .expect("ensure_open guarantees Some")
            .try_clone()?;
        let ledger = Ledger::from_connection(
            probe,
            self.meta.tenant_id().clone(),
            BinaryHash::from_bytes([0u8; 32]),
        );
        let head_seq = match ledger.verify_chain() {
            Ok(seq) => seq,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    db = %self.db_path.display(),
                    "aberp-db: post-poison integrity re-verify FAILED (audit chain does NOT verify genesis→head) — surfacing a HARD error; this is real corruption, not a benign prior panic"
                );
                return Err(DbError::PoisonRecoveryFailed(e.to_string()));
            }
        };

        tracing::warn!(
            db = %self.db_path.display(),
            head_seq,
            "aberp-db: poison-recovery integrity re-verify PASSED (audit chain intact genesis→head); shared writer RESUMED"
        );

        // (3) Audit the recovery (Bug 5: "must log+audit"). Best-effort: the
        //     mutex is already healed, so a failure to write the forensic row must
        //     not re-brick the writer.
        self.emit_poison_recovery_audit(inner, head_seq);
        Ok(())
    }

    /// Append the poison-recovery forensic audit row. Reuses
    /// [`EventKind::DbAutoRecovered`] (a system/durability event) with a
    /// SCHEMA-VALID `DbAutoRecoveredPayload`: only its free-form `trigger` string
    /// carries a new value (`writer_poison_recovered`) and the single variable is
    /// a machine `u64`, so the payload is hand-formatted (no `serde_json` dep, no
    /// decoder-shape risk). Best-effort by contract; the recovery already
    /// succeeded and was logged loudly before this is attempted.
    fn emit_poison_recovery_audit(&self, inner: &Inner, recovered_head_seq: u64) {
        let probe = match inner.conn.as_ref().map(|c| c.try_clone()) {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                tracing::error!(
                    error = %e,
                    db = %self.db_path.display(),
                    "aberp-db: poison-recovery audit row SKIPPED (try_clone failed); recovery itself succeeded and was logged"
                );
                return;
            }
            None => return,
        };
        // Injection-free: `recovered_max_seq` is the only interpolation and it is
        // a `u64`. Field set + names match `DbAutoRecoveredPayload` exactly so any
        // typed decoder round-trips it (Option -> null).
        let payload = format!(
            "{{\"trigger\":\"writer_poison_recovered\",\"source_snapshot_seq\":0,\
             \"snapshot_audit_count\":0,\"replayed_entries\":0,\
             \"recovered_max_seq\":{recovered_head_seq},\"retained_corrupt_db\":null}}"
        )
        .into_bytes();
        let session_id = format!(
            "aberp-db-poison-recovery-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let actor = Actor::from_local_cli(session_id, "system:aberp-db");
        let mut ledger = Ledger::from_connection(
            probe,
            self.meta.tenant_id().clone(),
            BinaryHash::from_bytes([0u8; 32]),
        );
        match ledger.append(EventKind::DbAutoRecovered, payload, actor, None) {
            Ok(_) => tracing::warn!(
                db = %self.db_path.display(),
                recovered_head_seq,
                "aberp-db: poison-recovery AUDITED (db.auto_recovered, trigger=writer_poison_recovered)"
            ),
            Err(e) => tracing::error!(
                error = %e,
                db = %self.db_path.display(),
                "aberp-db: poison-recovery audit-row append FAILED (non-fatal; the writer is already recovered and the recovery was logged loudly)"
            ),
        }
    }

    /// Run the validated, debounced durable checkpoint **while holding the
    /// writer lock**, quiescing the shared connection around it.
    ///
    /// # H4 SEAM (PROD-HARDEN-2027 §H4) — STUB in H3
    ///
    /// In H3 `checkpoint_enabled` is ALWAYS `false` ([`HandleConfig::default`]),
    /// so this method is **never reached at runtime** — the [`WriteGuard`] drop
    /// and [`Handle::checkpoint_on_idle`] both gate on `checkpoint_enabled`. It
    /// is retained (compiling, structurally aligned with the editions
    /// `1e6097d` form) so H4 is a one-line swap: drop this stub for the real
    ///
    /// ```ignore
    /// inner.conn = None;                                   // quiesce
    /// match aberp_snapshot::live_durable_checkpoint(&self.db_path, &self.tenant) { .. }
    /// inner.conn = Some(open_runtime_connection(&self.db_path, &self.config)?); // reopen
    /// ```
    ///
    /// (which is why the `aberp-snapshot` dep is deferred to H4). If this stub is
    /// ever reached with `checkpoint_enabled == true` before H4 wires the real
    /// fold, that is a wiring bug: log LOUD and fold NOTHING, but keep the
    /// debouncer window consistent so we do not hot-loop.
    ///
    /// **H4 MUST call [`Self::note_self_fold`] before the quiesce.** The real
    /// fold shrinks the WAL, and ADR-0110 D7's fence treats an undeclared
    /// shrink as a foreign truncation. The stub does NOT call it, correctly:
    /// it folds nothing, so there is nothing to declare.
    fn run_durable_checkpoint_locked(&self, inner: &mut Inner) {
        tracing::error!(
            db = %self.db_path.display(),
            "aberp-db: run_durable_checkpoint_locked reached while the runtime checkpoint is DISABLED (H3) — the validated fold lands in H4; folding NOTHING this tick"
        );
        // Keep the D2 window consistent (record the tick) so a mis-enabled
        // checkpoint does not spin every guard-drop.
        inner.debouncer.record_checkpoint(Instant::now());
    }
}

/// `<db>.wal` — DuckDB's WAL sibling for `db_path`. The extension is
/// **appended** to the whole file name (`aberp.duckdb` → `aberp.duckdb.wal`),
/// not substituted, which `Path::set_extension` would get wrong.
///
/// Deliberately re-derived here rather than reused from
/// `aberp_snapshot::crash_safe`: that helper is private, and `aberp-snapshot`
/// is intentionally NOT a dependency of this crate (see the Cargo.toml note).
/// One three-line path join does not justify inverting that.
/// ADR-0110 D7 — the fence predicate, pulled out of
/// [`Handle::observe_durable_set`] so the four rules read as four rules.
///
/// Ordered most-specific-first, and each arm is written so that "we have no
/// prior knowledge" falls through to `None`. A fence that fires on ignorance
/// would nag a healthy box, and an alarm that cries wolf is an alarm the
/// operator learns to dismiss — which costs more than not having it.
fn detect_breach(
    mark: &WalMark,
    wal_present: bool,
    wal_len: u64,
    wal_id: Option<FileId>,
    main_id: Option<FileId>,
) -> Option<WalBreach> {
    // 1. The WAL is gone and we had seen bytes in it. THE 00012 SHAPE. Pre-D7
    //    this was the `if wal_path.exists()` skip on the way to `Ok(())`.
    //    Guarded on a non-zero high-water, so a Handle that has never written
    //    a WAL byte (a boot, a read-only process) cannot trip on its absence.
    if !wal_present && mark.wal_high_water > 0 {
        return Some(WalBreach::WalVanished);
    }
    // 2. The WAL is present but SHORTER than we have already seen it. Under the
    //    F-A pragmas the WAL is append-only, so this cannot happen to us.
    if wal_present && wal_len < mark.wal_high_water {
        return Some(WalBreach::WalShrank);
    }
    // 3. Same name, different inode: a truncate-and-recreate that byte counts
    //    alone would sail straight past. Only checked when BOTH sides are
    //    known — an unknown prior, or a non-unix target, means "not checked".
    if let (Some(seen), Some(now)) = (mark.wal_id, wal_id) {
        if seen != now {
            return Some(WalBreach::WalReplaced);
        }
    }
    // 4. The main file was swapped under the running Handle. Note the `Some`
    //    on both sides again: a main file that is merely UNREADABLE is not an
    //    identity change, and is left to the `fsync`'s own `ENOENT` so it keeps
    //    reporting as the `DurableAck` error it has always been.
    if let (Some(seen), Some(now)) = (mark.main_id, main_id) {
        if seen != now {
            return Some(WalBreach::MainReplaced);
        }
    }
    None
}

fn wal_path_for(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(".wal");
    PathBuf::from(name)
}

/// `fsync` a path's contents + metadata. Works for regular files and, on POSIX,
/// for directories — opening read-only and `sync_all`-ing the fd is the
/// canonical way to persist either (the same shape as
/// `aberp_snapshot::crash_safe::fsync_file`).
///
/// Unlike that helper's directory arm, a failure here is **hard**. It is
/// reached only from [`Handle::durable_ack`], where "we could not make the
/// acked write durable" is precisely the fact that must not be swallowed
/// (ADR-0110 R3).
fn fsync_path(path: &Path) -> Result<(), DbError> {
    let f = std::fs::File::open(path).map_err(|source| DbError::DurableAck {
        path: path.to_path_buf(),
        source,
    })?;
    f.sync_all().map_err(|source| DbError::DurableAck {
        path: path.to_path_buf(),
        source,
    })
}

/// Open one runtime connection to the live tenant DB and apply the
/// single-writer hardening pragmas.
///
/// # F-A — authorized engine-adapter PRAGMA (policy marker)
///
/// ADR-0021 `[[no-SQL-specific]]` bars SQL-engine-specific statements from the
/// business layer. This is the ONE authorized exception: an engine-adapter
/// pragma that exists to make the single-writer discipline safe. `aberp-db` is
/// the DuckDB engine adapter, so the pragma belongs here and NOWHERE else. The
/// cut-gate (`tools/cut_gate_*`) asserts this marker + the pragma are present
/// (F-A pragma-presence check).
///
/// `disable_checkpoint_on_shutdown` stops DuckDB folding the WAL into the live
/// file when the connection closes; `wal_autocheckpoint` raised to effectively
/// infinite stops the in-place auto-fold DURING operation. Together they ensure
/// the only checkpoint that ever touches the live file is the validated logical
/// one (H4).
///
/// **ADR-0110 §2.2 correction.** That last sentence was true and load-bearing
/// and H4 was never built, so for two releases "the only checkpoint" was *no
/// checkpoint* and the live file simply never advanced at runtime. The pragmas
/// stay — folding in place is still the `duckdb#23046` hazard — but durability
/// no longer waits on H4: [`Handle::durable_ack`] `fsync`s the WAL at the money
/// -path ack, so the acked rows are durable *without* a fold. An UNKNOWN pragma
/// is NOT harmless — DuckDB errors HARD on an
/// unrecognised pragma (duckdb#10127), so a future rename/typo makes
/// `Handle::open` fail and `serve` refuse to boot (fail-hard: loud), not
/// silently degrade. The spellings are confirmed VALID against libduckdb 1.5.3
/// in the e2e build.
fn open_runtime_connection(db_path: &Path, config: &HandleConfig) -> Result<Connection, DbError> {
    let conn = Connection::open(db_path)?;
    if config.disable_implicit_close_checkpoint {
        // F-A engine-adapter pragma (see the fn docs). No in-place WAL fold.
        conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown;")?;
        conn.execute_batch("PRAGMA wal_autocheckpoint='1TB';")?;
    }
    Ok(conn)
}

/// RAII writer over the shared instance. Derefs to the one `&mut Connection`.
/// On drop it runs the post-commit hook: a **lockstep** mirror append (always —
/// the mirror tracks the DB continuously) and a **debounced** durable checkpoint
/// (D2; H3-disabled). Both are best-effort + loudly logged: the business
/// transaction has already committed by the time the guard drops, so a hook
/// failure must not unwind it.
pub struct WriteGuard<'h> {
    handle: &'h Handle,
    inner: MutexGuard<'h, Inner>,
}

impl WriteGuard<'_> {
    /// The shared writer connection. Run the existing transaction body
    /// (`BEGIN … COMMIT`) against this exactly as the pre-fix code ran it against
    /// its freshly-opened owned connection.
    pub fn conn(&mut self) -> &mut Connection {
        self.inner
            .conn
            .as_mut()
            .expect("write() guarantees an open connection")
    }
}

impl std::ops::Deref for WriteGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.inner
            .conn
            .as_ref()
            .expect("write() guarantees an open connection")
    }
}

impl std::ops::DerefMut for WriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.inner
            .conn
            .as_mut()
            .expect("write() guarantees an open connection")
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let handle = self.handle;

        // LOCKSTEP mirror append (always; cheap; closes the mirror-lag gap at
        // the source). Uses the shared connection + the once-built meta, so it
        // sees exactly what the just-finished txn committed.
        //
        // ADR-0110 D5 — the outcome is CLASSIFIED, not just logged. Read the
        // two pieces of state out of `Inner` first: the arms below need
        // `&Connection` out of the same struct, and the writes-back happen once
        // that borrow ends.
        let last_synced_head = self.inner.last_synced_head;
        let freeze_reported = self.inner.mirror_freeze_reported;
        let mut new_synced_head = None;
        let mut now_reported = false;
        if let Some(conn) = self.inner.conn.as_ref() {
            match aberp_audit_ledger::sync_mirror(conn, &handle.meta, &handle.mirror_path) {
                // The mirror took the append: the two stores agree, under our
                // own writing, at this head. Both halves of D5's discrimination
                // rest on that number.
                Ok(head) => new_synced_head = Some(head),
                // ADR-0110 D5 — THE APPEND-REFUSED CASE. `MirrorDivergent` is
                // not a transient I/O hiccup that "reconciles on the next
                // write": `sync_mirror` re-derives the same divergence every
                // time, appends NOTHING, and does so until a boot reconcile
                // heals it. So from here the mirror is FROZEN — every later
                // audit row (a D7 acknowledgement, a second durability loss)
                // lands in the DB alone and never reaches `fsync`'d storage.
                // Before D5 that was a `warn!` and nothing else, which made a
                // silently-degraded durability posture indistinguishable from a
                // healthy one.
                //
                // Two gates, and NEITHER is `wal_fence_enabled` (see
                // `raise_mirror_freeze_alert`):
                //
                // * `last_synced_head` must be `Some` — we saw the two stores
                //   agree before they stopped agreeing;
                // * the DB head must have FALLEN BELOW it. That is D5-B2. A
                //   co-resident CLI mirror-writer (the NAV resubmission family)
                //   advances the mirror without our instance seeing its rows,
                //   which looks exactly like a truncation from here — except
                //   that OUR head has not moved. A truncation costs us rows we
                //   had already mirrored; sanctioned maintenance does not. Only
                //   the first is a durability loss, and a banner that could not
                //   tell them apart would be up after routine maintenance,
                //   which is the alarm-the-operator-dismisses failure.
                Err(aberp_audit_ledger::AppendError::MirrorDivergent { seq, reason }) => {
                    // ONE head read, on the divergence path only — never on the
                    // healthy write path. `None` means we could not ask, and we
                    // do not raise on a question we could not put: a regression
                    // we cannot demonstrate is not one we should alarm on.
                    let db_head = handle.audit_head_seq(conn);
                    let regressed = matches!((last_synced_head, db_head), (Some(prev), Some(now)) if now < prev);
                    if regressed && !freeze_reported {
                        handle.raise_mirror_freeze_alert(seq, db_head.unwrap_or(0), &reason);
                        now_reported = true;
                    } else {
                        // A divergence we deliberately do NOT raise. It still
                        // gets its own line, because the generic message below
                        // would be a LIE here: this will not reconcile on the
                        // next write.
                        tracing::warn!(
                            mirror = %handle.mirror_path.display(),
                            mirror_head_seq = seq,
                            db_head_seq = db_head,
                            last_synced_head,
                            mirror_freeze_already_reported = freeze_reported,
                            reason,
                            "aberp-db: ADR-0110 D5 — the audit mirror is REFUSING appends and \
                             will keep refusing until a boot reconcile heals it (or refuses the \
                             boot). NOT raising the durability alert: this Handle's own audit \
                             head has not fallen below what it had already mirrored, so nothing \
                             OF OURS was lost — the mirror was advanced by something else, or \
                             the tenant arrived diverged, or the episode is already on the \
                             banner."
                        );
                    }
                }
                Err(e) => {
                    // `MirrorIo` / `MirrorCorrupt` keep the pre-D5 posture: for
                    // those the message below is true — the next write really
                    // can clear a transient I/O failure, and a torn tail is the
                    // boot reconciler's to trim. Widening the alarm to cover
                    // them would widen its false-positive surface for nothing.
                    tracing::warn!(
                        error = %e,
                        mirror = %handle.mirror_path.display(),
                        "aberp-db: lockstep sync_mirror failed (post-commit); mirror will \
                         reconcile on the next write or at the pre-snapshot fsync"
                    );
                }
            }
        }
        if let Some(head) = new_synced_head {
            self.inner.last_synced_head = Some(head);
        }
        self.inner.mirror_freeze_reported = freeze_reported || now_reported;

        // ADR-0110 D7 — SAMPLE THE WATERMARK. Here, and not in `durable_ack`
        // alone, because this is the ordered point: writes are serialized
        // behind the guard we are dropping, the transaction has committed, and
        // the WAL bytes it produced are on the file. An ack-time sample by
        // itself would be blind to a truncation that happened BETWEEN two
        // writes — the intervening commit re-creates a small, self-consistent
        // WAL and the loss reads as normal. The high-water this records is what
        // remembers otherwise.
        //
        // Gated with the ack's own check (B1). ARMED as of D7.6, so this
        // `stat` + watermark-mutex pair IS on the production guard-drop path
        // now; with the flag off the drop is bit-for-bit the pre-D7 one.
        if handle.config.wal_fence_enabled {
            handle.observe_durable_set();
        }

        // DEBOUNCED durable checkpoint (D2). Mark dirty, then fire only if the
        // coalescing window allows AND the checkpoint is enabled. H3: disabled,
        // so the branch never runs (the H4 seam — see
        // `run_durable_checkpoint_locked`).
        self.inner.debouncer.note_write();
        if handle.config.checkpoint_enabled
            && self.inner.debouncer.should_checkpoint_now(Instant::now())
        {
            // Reborrow split: `run_durable_checkpoint_locked` needs `&mut Inner`.
            let inner: &mut Inner = &mut self.inner;
            handle.run_durable_checkpoint_locked(inner);
        }

        // Re-entrancy tripwire bookkeeping: this thread no longer holds this
        // Handle's write guard. Done LAST so the guard counts as held for the
        // whole drop body (the mirror sync above touches `inner.conn` directly,
        // never re-acquiring, so it is safe). Debug/test only.
        #[cfg(debug_assertions)]
        Handle::deregister_write_held(handle.id);
    }
}

/// ADR-0110 D7 — deterministic unit pins for the fence PREDICATE.
///
/// The filesystem-level pins live in `tests/adr0110_d7_wal_fence.rs`, and they
/// carry the acceptance argument. These cover what a wall-clock integration
/// test cannot reach honestly: the exact table of watermark-vs-observed
/// combinations, including the ones that must stay SILENT. A false positive is
/// the failure mode that would make an operator learn to ignore the banner, so
/// the silent cases are pinned one by one rather than sampled.
#[cfg(test)]
mod fence_tests {
    use super::*;

    /// A seeded scratch tenant DB under `$TMPDIR`. Returns `(dir, db_path)`.
    fn scratch_db(tag: &str) -> (std::path::PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aberp-d7-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let db = dir.join("aberp.duckdb");
        {
            let c = Connection::open(&db).expect("seed open");
            aberp_audit_ledger::ensure_schema(&c).expect("schema");
        }
        (dir, db)
    }

    fn id(ino: u64) -> Option<FileId> {
        Some(FileId { dev: 1, ino })
    }

    /// A watermark that has seen a 1000-byte WAL on inode 10 and a main file on
    /// inode 20 — a Handle mid-flight on a healthy tenant.
    fn mid_flight() -> WalMark {
        WalMark {
            wal_high_water: 1000,
            wal_id: id(10),
            main_id: id(20),
            folded_by_us: false,
            breach: None,
        }
    }

    #[test]
    fn a_fresh_watermark_cannot_fire_on_anything() {
        // THE BOOT CASE, exhaustively: a Handle that has observed nothing has no
        // "before", and a fence without a before must stay silent whatever it
        // finds — a missing WAL, a huge WAL, any inode at all.
        let fresh = WalMark::default();
        assert_eq!(detect_breach(&fresh, false, 0, None, None), None);
        assert_eq!(detect_breach(&fresh, true, 999_999, id(7), id(8)), None);
        assert_eq!(detect_breach(&fresh, false, 0, None, id(8)), None);
    }

    #[test]
    fn growth_is_never_a_breach() {
        // The single most likely false positive: a concurrent daemon commit
        // grows the WAL between a money path's sample and its ack.
        let m = mid_flight();
        assert_eq!(detect_breach(&m, true, 1000, id(10), id(20)), None);
        assert_eq!(detect_breach(&m, true, 1001, id(10), id(20)), None);
        assert_eq!(detect_breach(&m, true, u64::MAX, id(10), id(20)), None);
    }

    #[test]
    fn the_four_breaches_are_each_detected_and_classified() {
        let m = mid_flight();
        // The 00012 shape: folded away entirely.
        assert_eq!(
            detect_breach(&m, false, 0, None, id(20)),
            Some(WalBreach::WalVanished)
        );
        // Folded and re-created smaller.
        assert_eq!(
            detect_breach(&m, true, 999, id(10), id(20)),
            Some(WalBreach::WalShrank)
        );
        // Truncated and re-created at the same size — bytes alone would miss it.
        assert_eq!(
            detect_breach(&m, true, 1000, id(11), id(20)),
            Some(WalBreach::WalReplaced)
        );
        // The live file swapped under us.
        assert_eq!(
            detect_breach(&m, true, 1000, id(10), id(21)),
            Some(WalBreach::MainReplaced)
        );
    }

    #[test]
    fn an_unknown_side_is_never_an_identity_breach() {
        // "We cannot tell" must resolve to silence, not to an alarm. This is the
        // non-unix path (`file_id` returns `None`) and the first-observation
        // path, and it is why both identity rules test `Some` on BOTH sides.
        let m = mid_flight();
        assert_eq!(detect_breach(&m, true, 1000, None, None), None);
        let no_prior = WalMark {
            wal_id: None,
            main_id: None,
            ..mid_flight()
        };
        assert_eq!(detect_breach(&no_prior, true, 1000, id(99), id(99)), None);
    }

    #[test]
    fn a_main_file_that_merely_vanished_is_left_to_the_fsync() {
        // `durable_ack_fault_injection.rs` deletes the main DB file and requires
        // `DbError::DurableAck` naming it. The fence must not intercept that and
        // relabel it as a WAL truncation: "I could not open it" and "it is not
        // the file I wrote to" are different facts and the operator acts on them
        // differently.
        let m = mid_flight();
        assert_eq!(detect_breach(&m, true, 1000, id(10), None), None);
    }

    /// The `folded_by_us` escape hatch, end to end on a real Handle.
    ///
    /// Honest note on why this is a unit test and not an integration one: the
    /// only production drop-and-reopen (post-poison recovery) does NOT in fact
    /// fold on the pinned libduckdb 1.5.3 — measured 2026-08-12, the WAL GREW
    /// 1270 → 2118 bytes across a recovery, because the F-A pragmas are on the
    /// connection being closed and the reopen's replay does not truncate. So
    /// `Handle::note_self_fold`'s call site there is INSURANCE against an engine
    /// behaviour we do not control, not a line any integration test can turn
    /// red. This pins the mechanism itself instead, so H4 — whose fold really
    /// will shrink the WAL — can rely on it.
    #[test]
    fn a_declared_self_fold_re_baselines_instead_of_firing() {
        let dir = std::env::temp_dir().join(format!(
            "aberp-d7-selffold-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let db = dir.join("aberp.duckdb");
        {
            let c = Connection::open(&db).expect("seed");
            aberp_audit_ledger::ensure_schema(&c).expect("schema");
        }
        let h = Handle::open_default(&db, TenantId::new("selffold".to_string()).unwrap())
            .expect("open handle");

        // Pretend we have seen a big WAL, then shrink it to nothing.
        {
            let mut mark = h.lock_watermark();
            mark.wal_high_water = 4096;
            mark.wal_id = Some(FileId { dev: 1, ino: 1 });
        }
        // Undeclared: that is a breach.
        h.observe_durable_set();
        assert!(
            h.take_breach().is_some(),
            "an undeclared shrink from 4096 bytes to nothing MUST latch a breach"
        );

        // Declared: same shrink, no breach, and the water re-baselines.
        {
            let mut mark = h.lock_watermark();
            mark.wal_high_water = 4096;
            mark.wal_id = Some(FileId { dev: 1, ino: 1 });
        }
        h.note_self_fold();
        h.observe_durable_set();
        assert!(
            h.take_breach().is_none(),
            "a fold this Handle DECLARED is the one legitimate shrink and must not fire"
        );
        assert_eq!(
            h.lock_watermark().wal_high_water,
            0,
            "a declared fold must RE-BASELINE the high-water, not leave a stale one behind that \
             fires on the very next observation"
        );
        assert!(
            !h.lock_watermark().folded_by_us,
            "the flag is single-shot: it must be consumed by the observation it excuses"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **R2-N1 — the disarmed fence must not even LOOK.**
    ///
    /// The integration test of the same name could only assert
    /// `durability_alert().is_none()`, which holds whether or not the watermark
    /// was sampled — un-gating `observe_durable_set` in `WriteGuard::drop` left
    /// all thirteen fence tests green. It was vacuous exactly where it mattered.
    ///
    /// The risk it was meant to cover is a landmine for whoever flips the flag
    /// on: with the fence disarmed `durable_ack` never calls `take_breach`, so a
    /// breach latched by an un-gated sampler would sit in the watermark
    /// indefinitely and fire on the first ack after the flag goes true — an
    /// alarm about a truncation that happened days earlier, on a process that
    /// has been running fine since.
    ///
    /// This is a crate-internal test so it can read the private watermark
    /// directly, which is the only way to assert "nothing was sampled" rather
    /// than "nothing surfaced".
    ///
    /// **D7.6 (2026-08-13): now built on an EXPLICITLY disarmed config**, not on
    /// `HandleConfig::default()`, which is armed from this head on. The property
    /// under test is "the disarmed code path adds nothing to the hot path", and
    /// that is a statement about the FLAG — leaving it keyed to the default
    /// would have made it a statement about the default instead, which is the
    /// D5-N3 vacuity in reverse. It still guards the same landmine: the
    /// disarmed body is what a bisect through the dark period runs.
    #[test]
    fn a_disarmed_fence_leaves_the_watermark_completely_untouched() {
        let (dir, db) = scratch_db("disarmed-watermark");
        let h = Handle::open(
            &db,
            TenantId::new("disarmed".to_string()).unwrap(),
            HandleConfig {
                wal_fence_enabled: false,
                ..Default::default()
            },
        )
        .expect("open disarmed handle");

        // A committed write — the sampling site in `WriteGuard::drop`.
        {
            let g = h.write().expect("writer");
            g.execute_batch(
                "CREATE TABLE IF NOT EXISTS probe(x INTEGER); INSERT INTO probe VALUES (1);",
            )
            .expect("dirty the WAL");
        }
        h.durable_ack().expect("disarmed ack succeeds");

        let mark = h.lock_watermark();
        assert_eq!(
            mark.wal_high_water, 0,
            "R2-N1 REGRESSION: a DISARMED fence sampled the WAL high-water. Disarmed must mean \
             the guard drop is bit-for-bit pre-D7 — no stat, no watermark mutex, nothing on the \
             hot path of every committed write in the process."
        );
        assert!(
            mark.wal_id.is_none() && mark.main_id.is_none(),
            "R2-N1 REGRESSION: a DISARMED fence recorded file identities"
        );
        assert!(
            mark.breach.is_none(),
            "R2-N1 REGRESSION: a DISARMED fence LATCHED a breach. Nothing consumes that latch \
             while the flag is false, so it would sit there and fire on the first ack after \
             someone flips the flag on — an alarm about a truncation from days ago, on a \
             process that has been healthy since."
        );
        drop(mark);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The armed counterpart, so the test above cannot pass because the
    /// watermark is simply never populated by anything.
    #[test]
    fn an_armed_fence_does_populate_the_watermark() {
        let (dir, db) = scratch_db("armed-watermark");
        let h = Handle::open(
            &db,
            TenantId::new("armed".to_string()).unwrap(),
            HandleConfig {
                wal_fence_enabled: true,
                ..Default::default()
            },
        )
        .expect("open armed handle");
        {
            let g = h.write().expect("writer");
            g.execute_batch(
                "CREATE TABLE IF NOT EXISTS probe(x INTEGER); INSERT INTO probe VALUES (1);",
            )
            .expect("dirty the WAL");
        }
        let mark = h.lock_watermark();
        assert!(
            mark.wal_high_water > 0,
            "an ARMED fence must sample the WAL at the guard drop — if it does not, the \
             disarmed test above proves nothing"
        );
        drop(mark);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Taking a breach CLEARS it. This is the "keep serving, do not degrade to
    /// forget" hinge: leave it latched and every later ack re-reports the same
    /// historical truncation, which is the sticky write refusal that was
    /// explicitly ruled out; clear it without recording and the loss is
    /// forgotten. The alert and the audit row are what carry it forward.
    #[test]
    fn taking_a_breach_clears_the_latch() {
        let m = WalMark {
            breach: Some(Breach {
                kind: WalBreach::WalVanished,
                expected: 1000,
                observed: 0,
            }),
            ..mid_flight()
        };
        let mut m = m;
        assert!(m.breach.take().is_some());
        assert!(m.breach.take().is_none(), "the latch is single-shot");
    }
}
