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
//! # No new primitive
//!
//! This crate invents **no** durability primitive. It reuses, verbatim,
//! [`aberp_audit_ledger::sync_mirror`] / [`aberp_audit_ledger::LedgerMeta`]
//! (and, in H4, the `aberp_snapshot` checkpoint primitives). It only *routes*
//! access through one instance and *calls* those primitives at the post-commit
//! point.
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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId};
use duckdb::Connection;

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
}

/// Convenience alias — the shared handle is always reached as `Arc<Handle>`
/// (cloned into `AppState` and every daemon `Deps`).
pub type HandleArc = std::sync::Arc<Handle>;

/// The process-wide shared DuckDB handle (ADR-0099 H3). Construct once at boot
/// ([`Handle::open`]); share as `Arc<Handle>` into `AppState` and every daemon
/// spawn. **Send + Sync**: the `Connection` (which is `Send` but not `Sync`)
/// lives behind a `Mutex`, and reads are served by owned `try_clone`s.
pub struct Handle {
    db_path: PathBuf,
    mirror_path: PathBuf,
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
        let conn = open_runtime_connection(db_path, &config)?;
        // Capture the coalescing window before `config` moves into the struct.
        let min_interval = config.min_checkpoint_interval;

        Ok(Arc::new(Handle {
            db_path: db_path.to_path_buf(),
            mirror_path,
            meta,
            tenant: tenant.as_str().to_string(),
            config,
            inner: Mutex::new(Inner {
                conn: Some(conn),
                debouncer: CheckpointDebouncer::new(min_interval),
            }),
        }))
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

    /// Acquire the **serialized writer** over the shared instance. The returned
    /// [`WriteGuard`] derefs to the one `&mut Connection`; run the existing
    /// transaction body against it exactly as before. When the guard drops, the
    /// post-commit hook fires (lockstep mirror append). Holding the guard blocks
    /// other writers — process-wide write serialization is the intended
    /// single-writer discipline (a throughput ceiling, acceptable for a
    /// single-operator CNC-shop ERP).
    pub fn write(&self) -> Result<WriteGuard<'_>, DbError> {
        // Bug 5: recover a poisoned writer in-place instead of returning
        // `DbError::Poisoned` forever (which would brick every write path for the
        // whole process). See [`Self::lock_recovering`].
        let mut inner = self.lock_recovering()?;
        self.ensure_open(&mut inner)?;
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
    pub fn read(&self) -> Result<Connection, DbError> {
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
/// one (H4). An UNKNOWN pragma is NOT harmless — DuckDB errors HARD on an
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
        if let Some(conn) = self.inner.conn.as_ref() {
            if let Err(e) = aberp_audit_ledger::sync_mirror(conn, &handle.meta, &handle.mirror_path)
            {
                tracing::warn!(
                    error = %e,
                    mirror = %handle.mirror_path.display(),
                    "aberp-db: lockstep sync_mirror failed (post-commit); mirror will \
                     reconcile on the next write or at the pre-snapshot fsync"
                );
            }
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
    }
}
