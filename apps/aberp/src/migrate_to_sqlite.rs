//! ADR-0108 Step 4 — **the migrator and the reconciliation gate.** The cheap
//! abort point.
//!
//! This is where the exercise either proves out or stops having cost four
//! scaffolding PRs. It carries the **ledger family only** — `audit_ledger` and
//! `audit_ledger_anchors` — from a read-only DuckDB into a fresh
//! `aberp.sqlite`, and then a **separate invocation** re-derives every number
//! from both sides and compares.
//!
//! # The four preconditions, all refusals, none of which waits
//!
//! 1. **The tenant's whole-DB writer lock, acquired and held for the whole
//!    run** (`acquire_or_refuse`). CLAUDE.md rule 13 applies to the migrator:
//!    it is a *fresh opener*, and a fresh opener reads Handle-WAL-resident
//!    DuckDB **stale**. If a DEV `serve` is live, an unguarded migrator
//!    silently migrates a short snapshot — and in the audit-ledger family
//!    "short" means "missing the most recent invoices". Refuses; never waits,
//!    never forces. Precedent: the 2026-07-19 repair tool refuses while the
//!    whole-DB flock is held (`a8a6da3`).
//! 2. **No unfolded `aberp.duckdb.wal`.** A read-only open cannot replay it, so
//!    those bytes are committed data the migrator would not see and would not
//!    miss loudly. Holding the lock does **not** make this redundant: the lock
//!    stops a *concurrent* writer, the WAL check stops a *previously crashed*
//!    one.
//! 3. **DEV-only** (C-II) — nothing under `~/.aberp/`, via the same pure
//!    `engine_path_agrees` from Step 1, in both directions (the source must be
//!    a `.duckdb`, the target a `.sqlite`).
//! 4. **A pre-migration snapshot that verifies**, including the `.wal` pairing
//!    and B1's two tamper-evidence counts.
//!
//! # The ledger inversion (B1 / Q7): the TABLE is the source
//!
//! The mirror is a **cross-check arm, never a source**, and the reason is worth
//! restating at the code that would otherwise do it the other way:
//!
//! `MirrorEntry` has **no `session_id`, no `session_pubkey`, no `event_sig`**
//! (`crates/audit-ledger/src/mirror.rs`), and `to_entry()` sets all three to
//! `None` — its own comment says the mirror "is a hash-chain DIVERGENCE
//! detector and does not carry the session-signing columns". Replaying it
//! therefore **strips the entire S441/ADR-0087 signature layer** and drops
//! `audit_ledger_anchors` completely, because the mirror never held it.
//!
//! And every obvious check is blind to that. `compute_entry_hash` deliberately
//! excludes the session fields, so `verify_chain` passes; the head hashes
//! match; `PRAGMA integrity_check` passes; the `typeof()` sweep passes.
//! `verify_chain_signed` does not save it either — its anti-strip arm fires
//! only on `(Some(session_id), _, _)`, and mirror replay nulls `session_id`
//! too, so every entry lands on the `legacy / unsigned — allowed` arm. With
//! anchors gone, `ChainVerdict.fully_anchored` reads **`true`**.
//!
//! **So the gate turns on two `COUNT(*)` equalities, not on any verdict flag**,
//! and [`ReconcileReport`] treats a zero baseline as absent coverage rather
//! than as a green `0 == 0`. `t18_*` runs the migrator in the rejected
//! mirror-as-source mode and asserts the gate goes red — a gate against B1 that
//! has never been shown to catch B1 is not a gate.
//!
//! # What Step 4 deliberately does NOT do
//!
//! * **No other family crosses.** ADR-0108 §7 Step 4 also lists the remaining
//!   families, but their STRICT DDL and their `ensure_columns` call sites are
//!   Steps 5/7/8's work — carrying their rows here would mean writing all of
//!   that schema now, which is the storage-changing work this step exists to
//!   decide *whether to do*.
//! * **No `Ledger::verify_chain` on the SQLite side.** That API is
//!   DuckDB-typed until `audit-ledger` crosses the seam in Step 5. What runs
//!   here instead is a structural link walk (`prev_hash[n] == entry_hash[n-1]`,
//!   `seq` contiguous from 1) plus head-hash equality against a DuckDB side
//!   where the full typed `verify_chain` DOES run. Stated, not glossed.
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::cli::{MigrateReconcileArgs, MigrateToSqliteArgs};

/// Where the migrator reads the ledger from.
///
/// [`LedgerSource::Mirror`] is the design ADR-0108 Q7 **rejected**, kept
/// reachable from the library face **only** so `t18_*` can run it and prove the
/// reconciliation gate catches it. The CLI never constructs it: `run` passes
/// [`LedgerSource::Table`] unconditionally, so there is no operator-reachable
/// path to the rejected mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerSource {
    /// The `audit_ledger` TABLE — the source of truth (B1 / Q7).
    Table,
    /// The ADR-0030 mirror. **Rejected by design**: `MirrorEntry` carries no
    /// `session_id` / `session_pubkey` / `event_sig` and never held
    /// `audit_ledger_anchors`, so replaying it strips the whole
    /// tamper-evidence layer while leaving every hash check green.
    RejectedMirrorReplay,
}

/// STRICT DDL for the carried ledger (ADR-0108 §3 R1/R2/R3 + M1).
///
/// Types are chosen per column, not by analogy. `prev_hash` / `binary_hash` /
/// `payload` / `entry_hash` are **BLOB** (R3 — in SQLite `BLOB` and `TEXT` are
/// distinct storage classes that never compare equal, so one `&str` bind where
/// `&[u8]` belongs makes a chain-link lookup return "not found"). The three
/// session columns are declared `VARCHAR` upstream and bound as `&str`, so they
/// cross as **TEXT**; they are the ones with *no hash-chain check behind them*
/// (`compute_entry_hash` excludes them), which makes `typeof()` their only
/// structural guard.
///
/// No `UNIQUE`, matching the DuckDB schema and `[[no-sql-specific]]`: the
/// uniqueness invariant lives in Rust, and ADR-0019 keeps it there.
const SQLITE_LEDGER_DDL: &str = "
CREATE TABLE IF NOT EXISTS audit_ledger (
    id              TEXT    NOT NULL,
    seq             INTEGER NOT NULL,
    prev_hash       BLOB    NOT NULL,
    time_wall       TEXT    NOT NULL,
    time_mono       INTEGER NOT NULL,
    actor           TEXT    NOT NULL,
    binary_hash     BLOB    NOT NULL,
    tenant_id       TEXT    NOT NULL,
    kind            TEXT    NOT NULL,
    payload         BLOB    NOT NULL,
    idempotency_key TEXT,
    entry_hash      BLOB    NOT NULL,
    session_id      TEXT,
    session_pubkey  TEXT,
    event_sig       TEXT
) STRICT;
";

/// STRICT DDL for the anchors (ADR-0108 §6.3, B1).
///
/// **`chain_head_hash_at_anchor` is TEXT, not BLOB**, and that is not a slip:
/// it is a hex `VARCHAR` upstream and `anchor_preimage` consumes it as `&str`
/// (`chain/verify.rs`). Typing it `BLOB` by analogy with the other hashes would
/// break anchor verification. `timestamp_token_bytes` is the only BLOB here.
const SQLITE_ANCHORS_DDL: &str = "
CREATE TABLE IF NOT EXISTS audit_ledger_anchors (
    id                        TEXT NOT NULL,
    tenant_id                 TEXT NOT NULL,
    session_id                TEXT NOT NULL,
    kind                      TEXT NOT NULL,
    chain_head_hash_at_anchor TEXT NOT NULL,
    timestamp_token_bytes     BLOB,
    tsa_identifier            TEXT NOT NULL,
    tsa_status                TEXT NOT NULL,
    created_at_utc            TEXT NOT NULL
) STRICT;
";

/// One `audit_ledger` row, carried verbatim.
type LedgerRow = (
    String,         // id
    i64,            // seq
    Vec<u8>,        // prev_hash
    String,         // time_wall
    i64,            // time_mono
    String,         // actor
    Vec<u8>,        // binary_hash
    String,         // tenant_id
    String,         // kind
    Vec<u8>,        // payload
    Option<String>, // idempotency_key
    Vec<u8>,        // entry_hash
    Option<String>, // session_id
    Option<String>, // session_pubkey
    Option<String>, // event_sig
);

/// What the migrator did. Deliberately NOT what the gate compares — the gate is
/// a separate invocation that re-derives every number from disk, because a
/// count the migrator produced and then checks against itself passes vacuously
/// on any read-side loss (B4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateOutcome {
    pub sqlite_path: PathBuf,
    pub entries_carried: u64,
    pub anchors_carried: u64,
    /// ADR-0108 Step 5 — what the invoice family carried. All zeros when the
    /// source is a ledger-only database (a legitimate shape; the gate holds the
    /// two sides to it symmetrically).
    pub billing: crate::migrate_billing::BillingCarry,
    /// SHA-256 of the DuckDB file, taken after the run. C-I says this must
    /// equal the digest taken before it (T-19 arm 2).
    pub duckdb_sha256_after: String,
}
// ---------------------------------------------------------------------------
// `aberp migrate-to-sqlite`
// ---------------------------------------------------------------------------

/// CLI face. Always [`LedgerSource::Table`] — the rejected mode has no
/// operator-reachable path.
pub fn run(args: &MigrateToSqliteArgs) -> Result<()> {
    let out = migrate_families(
        &args.db,
        &args.to,
        &args.tenant,
        &args.from_snapshot,
        LedgerSource::Table,
    )?;
    println!(
        "PASS migrate-to-sqlite → {} ({} ledger entries, {} anchors)",
        out.sqlite_path.display(),
        out.entries_carried,
        out.anchors_carried
    );
    let b = out.billing;
    println!(
        "  invoice family: {} invoices, {} lines, {} reservations, {} sequence bucket(s), \
         {} series",
        b.invoices, b.invoice_lines, b.reservations, b.sequence_state, b.series
    );
    println!(
        "  DuckDB SHA-256 after the run: {} (C-I: must equal the pre-migration manifest's)",
        out.duckdb_sha256_after
    );
    println!(
        "  NEXT: aberp migrate-reconcile --db {} --sqlite {} --tenant {}",
        args.db.display(),
        args.to.display(),
        args.tenant
    );
    Ok(())
}

/// Carry the fused transactional core from a **read-only** DuckDB into a fresh
/// SQLite file: `audit_ledger` + `audit_ledger_anchors` (Step 4), and — when
/// the source has it — the invoice family (Step 5, [`crate::migrate_billing`]).
///
/// See the module docs for the four preconditions and the ledger inversion.
/// Both families cross under the same held writer lock and inside the same
/// digest window, so C-I is measured once across the whole run.
pub fn migrate_families(
    duckdb_path: &Path,
    sqlite_path: &Path,
    tenant: &str,
    snapshot_dir: &Path,
    source: LedgerSource,
) -> Result<MigrateOutcome> {
    // --- precondition 3 (C-II + C-I), first: it is pure and cheap, and it must
    //     run before anything creates a file next to a path we should not be
    //     touching at all.
    crate::premigration::ensure_dev_only(duckdb_path)?;
    crate::premigration::ensure_dev_only(sqlite_path)?;
    let prod_root = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aberp"));
    aberp_db::engine_path::engine_path_agrees(
        aberp_db::engine_path::Engine::DuckDb,
        duckdb_path,
        prod_root.as_deref(),
    )
    .map_err(|e| anyhow!("source: {e}"))?;
    aberp_db::engine_path::engine_path_agrees(
        aberp_db::engine_path::Engine::Sqlite,
        sqlite_path,
        prod_root.as_deref(),
    )
    .map_err(|e| anyhow!("target: {e}"))?;

    // --- precondition 1: the writer lock, HELD for the whole run. ---
    // Rule 13: a fresh opener reads Handle-WAL-resident DuckDB stale. Refuses;
    // never waits, never forces.
    let _writer_lock =
        crate::db_writer_lock::acquire_or_refuse(duckdb_path, tenant, "aberp migrate-to-sqlite")?;

    // --- precondition 2: no unfolded WAL. ---
    if let Some(len) = aberp_db::readonly::unfolded_wal_len(duckdb_path)? {
        bail!(
            "refusing to migrate: {} holds an unfolded WAL of {len} bytes. A read-only open \
             cannot replay it, so those committed rows would be silently absent from the \
             carried ledger. Boot the DuckDB build once for a clean shutdown, then retry.",
            aberp_db::readonly::wal_path_for(duckdb_path).display()
        );
    }

    // --- precondition 4: the snapshot exists AND verifies. ---
    let manifest = snapshot_dir.join(crate::premigration::MANIFEST_FILENAME);
    if !manifest.is_file() {
        bail!(
            "no pre-migration manifest at {} — take one with `aberp premigration-snapshot` \
             before migrating (there is no rollback target without it)",
            manifest.display()
        );
    }
    // NOTE: `run_verify` acquires the writer lock itself, and we hold it. Call
    // the inner form so this does not self-deadlock — the lock is already ours
    // for the whole run, which is exactly the guarantee `run_verify` wants.
    let pre = crate::premigration::verify_against_manifest_locked(duckdb_path, tenant, &manifest)?;
    if !pre.mismatches.is_empty() {
        bail!(
            "the pre-migration snapshot does NOT verify against {} — refusing to migrate from \
             a database that already disagrees with its own baseline:\n  {}",
            duckdb_path.display(),
            pre.mismatches.join("\n  ")
        );
    }

    if sqlite_path.exists() {
        bail!(
            "{} already exists — refusing to overwrite. Delete it, or roll back with \
             run/rollback_to_duckdb.sh, then retry.",
            sqlite_path.display()
        );
    }

    // C-I, measured rather than asserted: digest before, digest after.
    let before = sha256_file(duckdb_path)?;

    // --- the carry ---
    let src = aberp_db::readonly::open_read_only(duckdb_path)
        .with_context(|| format!("open {} read-only", duckdb_path.display()))?;
    let mut dst = aberp_db::sqlite::open_hardened(sqlite_path)
        .with_context(|| format!("create {}", sqlite_path.display()))?;
    dst.execute_batch(SQLITE_LEDGER_DDL)?;
    dst.execute_batch(SQLITE_ANCHORS_DDL)?;

    let rows = read_ledger_rows(&src)?;
    let anchors = match source {
        LedgerSource::Table => read_anchor_rows(&src)?,
        // The mirror never held the anchors table at all.
        LedgerSource::RejectedMirrorReplay => Vec::new(),
    };

    let tx = aberp_db::engine::begin_immediate(&mut dst)
        .map_err(|e| anyhow!("begin the carry transaction: {e}"))?;
    for r in &rows {
        let (sid, spk, sig) = match source {
            LedgerSource::Table => (r.12.as_deref(), r.13.as_deref(), r.14.as_deref()),
            // THE strip. `MirrorEntry` has no such fields and `to_entry()` sets
            // all three to None.
            LedgerSource::RejectedMirrorReplay => (None, None, None),
        };
        tx.execute(
            "INSERT INTO audit_ledger
               (id, seq, prev_hash, time_wall, time_mono, actor, binary_hash, tenant_id,
                kind, payload, idempotency_key, entry_hash, session_id, session_pubkey, event_sig)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            aberp_db::engine::params![
                r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11, sid, spk, sig
            ],
        )?;
    }
    for a in &anchors {
        tx.execute(
            "INSERT INTO audit_ledger_anchors
               (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
                timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            aberp_db::engine::params![a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7, a.8],
        )?;
    }
    tx.commit()?;

    // --- ADR-0108 Step 5 — the invoice family, same lock, same digest window.
    //
    // Conditional on the source HAVING the family, because a ledger-only
    // database is a legitimate shape (Step 4's fixtures are exactly that). It
    // is not a fail-open: `reconcile_billing` holds the two sides to the
    // presence answer symmetrically, so "carried nothing because the tables
    // were not there" and "carried nothing because the carry was skipped" are
    // distinguishable at the gate.
    let billing = if crate::migrate_billing::family_present_duckdb(&src)? {
        crate::migrate_billing::ensure_billing_schema(&dst)?;
        crate::migrate_billing::carry_billing(&src, &mut dst)?
    } else {
        crate::migrate_billing::BillingCarry::default()
    };

    drop(dst);
    drop(src);

    let after = sha256_file(duckdb_path)?;
    if before != after {
        bail!(
            "C-I VIOLATED: {} changed during the migration ({before} → {after}). The DuckDB \
             file must be byte-unmodified — the whole rollback argument rests on it.",
            duckdb_path.display()
        );
    }

    Ok(MigrateOutcome {
        sqlite_path: sqlite_path.to_path_buf(),
        entries_carried: rows.len() as u64,
        anchors_carried: anchors.len() as u64,
        billing,
        duckdb_sha256_after: after,
    })
}
// ---------------------------------------------------------------------------
// `aberp migrate-reconcile` — a SEPARATE invocation (B4)
// ---------------------------------------------------------------------------

/// The gate's verdict. Every entry is a NUMBER the gate re-derived from disk;
/// **no number here was produced by the migrator** (B4): a row count compared
/// against the migrator's own in-memory extraction count compares the migrator
/// against itself and passes vacuously on any read-side loss — including the
/// `event_sig` and anchor counts B1 turns on.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub checks: Vec<String>,
    pub hard_stops: Vec<String>,
}

/// CLI face.
pub fn run_reconcile(args: &MigrateReconcileArgs) -> Result<()> {
    let r = reconcile(&args.db, &args.sqlite, &args.tenant)?;
    for c in &r.checks {
        println!("  {c}");
    }
    if r.hard_stops.is_empty() {
        println!("PASS migrate-reconcile ({} checks)", r.checks.len());
        return Ok(());
    }
    for s in &r.hard_stops {
        eprintln!("  ✗ HARD STOP: {s}");
    }
    bail!(
        "FAIL migrate-reconcile: {} hard stop(s). Nothing is promoted; roll back with \
         run/rollback_to_duckdb.sh. Do NOT force-fix.",
        r.hard_stops.len()
    )
}

/// Re-open both databases independently and compare.
pub fn reconcile(duckdb_path: &Path, sqlite_path: &Path, tenant: &str) -> Result<ReconcileReport> {
    crate::premigration::ensure_dev_only(duckdb_path)?;
    // S449 — the target path was unguarded. `open_hardened` CREATES the file it
    // is given, so a typo'd `--sqlite` under `~/.aberp/` would have written a
    // fresh database into the production root before the first query failed.
    // C-II is "nothing in ADR-0108 §7 may read, write, or stat production
    // data", and the gate is the one command an operator runs by hand.
    crate::premigration::ensure_dev_only(sqlite_path)?;
    let prod_root = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aberp"));
    aberp_db::engine_path::engine_path_agrees(
        aberp_db::engine_path::Engine::DuckDb,
        duckdb_path,
        prod_root.as_deref(),
    )
    .map_err(|e| anyhow!("source: {e}"))?;
    aberp_db::engine_path::engine_path_agrees(
        aberp_db::engine_path::Engine::Sqlite,
        sqlite_path,
        prod_root.as_deref(),
    )
    .map_err(|e| anyhow!("target: {e}"))?;

    let _writer_lock =
        crate::db_writer_lock::acquire_or_refuse(duckdb_path, tenant, "aberp migrate-reconcile")?;

    // S449 (B4) — the precondition every OTHER DuckDB-reading entry point in
    // this plan carries, and the only one that was missing it.
    // `run_snapshot`, `verify_against_manifest_locked` and `migrate_ledger` all
    // refuse on an unfolded WAL; the gate did not. The writer lock stops a
    // CONCURRENT writer, not a previously CRASHED one — so a DuckDB build that
    // wrote rows and died between the migrate and the gate leaves a WAL the
    // read-only open cannot replay, and the gate would compare the full SQLite
    // side against a SHORT DuckDB extraction. If those hidden rows are the ones
    // the migrator already carried, every equality above matches and the gate
    // reports PASS on a DuckDB file that is ahead of what was migrated.
    if let Some(len) = aberp_db::readonly::unfolded_wal_len(duckdb_path)? {
        bail!(
            "refusing to reconcile: {} holds an unfolded WAL of {len} bytes. A read-only open \
             cannot replay it, so every count below would be taken from a SHORT extraction and \
             an equality against it would be meaningless. Boot the DuckDB build once for a \
             clean shutdown, then re-run the gate.",
            aberp_db::readonly::wal_path_for(duckdb_path).display()
        );
    }

    let src = aberp_db::readonly::open_read_only(duckdb_path)?;
    let dst = aberp_db::sqlite::open_hardened(sqlite_path)?;
    let mut r = ReconcileReport::default();

    let eq = |label: &str, a: String, b: String, r: &mut ReconcileReport| {
        if a == b {
            r.checks.push(format!("✓ {label} = {a}"));
        } else {
            r.hard_stops
                .push(format!("{label}: DuckDB {a}, SQLite {b}"));
        }
    };

    // ---- B1's two count equalities. The ONLY checks that catch a strip. ----
    //
    // The non-zero precondition first: an equality between two zeros is not a
    // check, and `ChainVerdict.fully_anchored` reads `true` on exactly that
    // input. So the DuckDB side must have coverage before its equality means
    // anything.
    let duck_signed: i64 = src.query_row(
        "SELECT count(*) FROM audit_ledger WHERE event_sig IS NOT NULL",
        [],
        |x| x.get(0),
    )?;
    let lite_signed: i64 = dst.query_row(
        "SELECT count(*) FROM audit_ledger WHERE event_sig IS NOT NULL",
        [],
        |x| x.get(0),
    )?;
    let duck_anchors: i64 =
        src.query_row("SELECT count(*) FROM audit_ledger_anchors", [], |x| {
            x.get(0)
        })?;
    let lite_anchors: i64 =
        dst.query_row("SELECT count(*) FROM audit_ledger_anchors", [], |x| {
            x.get(0)
        })?;

    // S449 — the OTHER TWO signature columns. `event_sig` alone is not the
    // tamper-evidence: a signature is only checkable against the public key
    // that made it, so a carry that keeps every `event_sig` and nulls
    // `session_pubkey` leaves a ledger whose signatures can never be verified
    // again — and the two equalities above are BLIND to it (both sides still
    // count the same non-NULL `event_sig`s, the anchor counts still match, the
    // `typeof` sweep only looks at non-NULL values, and `compute_entry_hash`
    // excludes all three columns so the chain walk and the head hash are
    // unmoved). The mirror-replay strip nulls all three at once, which is why
    // the `event_sig` arm caught it and this gap did not show up in T-18.
    //
    // Steps 5-9 add more paths into this file than the two `LedgerSource` arms,
    // so the gate is per-COLUMN or it is nothing — the same lesson §3.4 draws
    // for the Q2 sweep.
    let mut extra: Vec<(&str, i64, i64)> = Vec::new();
    for col in ["session_id", "session_pubkey"] {
        let d: i64 = src.query_row(
            &format!("SELECT count(*) FROM audit_ledger WHERE {col} IS NOT NULL"),
            [],
            |x| x.get(0),
        )?;
        let l: i64 = dst.query_row(
            &format!("SELECT count(*) FROM audit_ledger WHERE {col} IS NOT NULL"),
            [],
            |x| x.get(0),
        )?;
        extra.push((col, d, l));
    }

    if duck_signed == 0 {
        r.hard_stops.push(
            "the DuckDB side has 0 audit_ledger rows with a non-NULL event_sig — there is no \
             tamper-evidence coverage to carry, so an equality here would be between two zeros \
             (ADR-0108 B1)"
                .to_string(),
        );
    }
    if duck_anchors == 0 {
        r.hard_stops.push(
            "the DuckDB side has 0 audit_ledger_anchors rows — no anchor coverage to carry \
             (ADR-0108 B1)"
                .to_string(),
        );
    }
    eq(
        "audit_ledger rows with a non-NULL event_sig (B1)",
        duck_signed.to_string(),
        lite_signed.to_string(),
        &mut r,
    );
    eq(
        "audit_ledger_anchors row count (B1)",
        duck_anchors.to_string(),
        lite_anchors.to_string(),
        &mut r,
    );
    // S449 — same shape, same non-zero precondition, for the two columns that
    // make an `event_sig` checkable.
    for (col, d, l) in &extra {
        if *d == 0 {
            r.hard_stops.push(format!(
                "the DuckDB side has 0 audit_ledger rows with a non-NULL {col} — an \
                 `event_sig` without its {col} can never be verified again, so an equality \
                 here would be between two zeros (ADR-0108 B1)"
            ));
        }
        eq(
            &format!("audit_ledger rows with a non-NULL {col} (B1)"),
            d.to_string(),
            l.to_string(),
            &mut r,
        );
    }

    // ---- row counts + head hash ----
    let duck_rows: i64 = src.query_row("SELECT count(*) FROM audit_ledger", [], |x| x.get(0))?;
    let lite_rows: i64 = dst.query_row("SELECT count(*) FROM audit_ledger", [], |x| x.get(0))?;
    eq(
        "audit_ledger row count",
        duck_rows.to_string(),
        lite_rows.to_string(),
        &mut r,
    );

    let duck_head: (i64, Vec<u8>) = src.query_row(
        "SELECT seq, entry_hash FROM audit_ledger ORDER BY seq DESC LIMIT 1",
        [],
        |x| Ok((x.get(0)?, x.get(1)?)),
    )?;
    let lite_head: (i64, Vec<u8>) = dst.query_row(
        "SELECT seq, entry_hash FROM audit_ledger ORDER BY seq DESC LIMIT 1",
        [],
        |x| Ok((x.get(0)?, x.get(1)?)),
    )?;
    eq(
        "head seq",
        duck_head.0.to_string(),
        lite_head.0.to_string(),
        &mut r,
    );
    eq(
        "head entry_hash",
        hex::encode(&duck_head.1),
        hex::encode(&lite_head.1),
        &mut r,
    );

    // ---- the typed chain verification, on the DuckDB side ----
    // `Ledger::verify_chain` is DuckDB-typed until `audit-ledger` crosses the
    // seam in Step 5, so the SQLite side gets the structural walk below
    // instead. Stated rather than glossed.
    let meta_tenant = aberp_audit_ledger::TenantId::new(tenant.to_string())
        .ok_or_else(|| anyhow!("invalid tenant id: {tenant:?}"))?;
    let ledger = aberp_audit_ledger::Ledger::from_connection(
        aberp_db::readonly::open_read_only(duckdb_path)?,
        meta_tenant,
        aberp_audit_ledger::BinaryHash::from_bytes([0u8; 32]),
    );
    match ledger.verify_chain() {
        Ok(n) => r.checks.push(format!(
            "✓ DuckDB verify_chain genesis→head OK ({n} entries)"
        )),
        Err(e) => r
            .hard_stops
            .push(format!("DuckDB verify_chain FAILED: {e}")),
    }

    // ---- the structural link walk, on the SQLite side ----
    // Not a substitute for `verify_chain` and not presented as one: it asserts
    // `seq` is contiguous from 1 and each row's `prev_hash` is the previous
    // row's `entry_hash`. Combined with head-hash equality against a DuckDB
    // side that DID run the typed verification, it is what Step 4 can honestly
    // claim.
    {
        let mut stmt =
            dst.prepare("SELECT seq, prev_hash, entry_hash FROM audit_ledger ORDER BY seq ASC")?;
        let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut broken = 0usize;
        for (i, (seq, prev, _)) in rows.iter().enumerate() {
            if *seq != (i as i64) + 1 {
                r.hard_stops.push(format!(
                    "SQLite audit_ledger seq is not contiguous: row {} has seq {seq}",
                    i + 1
                ));
                broken += 1;
                break;
            }
            if i > 0 && *prev != rows[i - 1].2 {
                r.hard_stops.push(format!(
                    "SQLite chain link broken at seq {seq}: prev_hash != the previous row's \
                     entry_hash"
                ));
                broken += 1;
                break;
            }
        }
        if broken == 0 {
            r.checks.push(format!(
                "✓ SQLite structural link walk OK ({} entries, seq contiguous, every \
                 prev_hash matches)",
                rows.len()
            ));
        }
    }

    // ---- M1's typeof() sweep, over EVERY row (§8 T-2) ----
    // The three session columns matter most: `compute_entry_hash` excludes
    // them, so `typeof` is their only structural guard. And
    // `chain_head_hash_at_anchor` must be 'text' — typing it 'blob' by analogy
    // with the other hashes breaks `anchor_preimage`, which consumes it as
    // `&str`.
    let sweep: &[(&str, &str, &str)] = &[
        ("audit_ledger", "prev_hash", "blob"),
        ("audit_ledger", "binary_hash", "blob"),
        ("audit_ledger", "payload", "blob"),
        ("audit_ledger", "entry_hash", "blob"),
        ("audit_ledger", "session_id", "text"),
        ("audit_ledger", "session_pubkey", "text"),
        ("audit_ledger", "event_sig", "text"),
        ("audit_ledger_anchors", "timestamp_token_bytes", "blob"),
        ("audit_ledger_anchors", "chain_head_hash_at_anchor", "text"),
    ];
    for (table, col, want) in sweep {
        // NULL is permitted for the nullable columns; what must never appear is
        // the WRONG non-null class.
        let bad: i64 = dst.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
            [want],
            |x| x.get(0),
        )?;
        if bad == 0 {
            r.checks
                .push(format!("✓ typeof({table}.{col}) = '{want}' on every row"));
        } else {
            r.hard_stops.push(format!(
                "{table}.{col}: {bad} row(s) are not '{want}' — in SQLite BLOB and TEXT never \
                 compare equal, so a wrong storage class here silently breaks lookups"
            ));
        }
    }

    // ---- integrity + the mirror's three-arm cross-check ----
    let integrity: String = dst.query_row("PRAGMA integrity_check", [], |x| x.get(0))?;
    if integrity == "ok" {
        r.checks.push("✓ PRAGMA integrity_check = ok".to_string());
    } else {
        r.hard_stops
            .push(format!("PRAGMA integrity_check = {integrity}"));
    }

    classify_mirror(duckdb_path, duck_head.0, &mut r)?;

    // ---- ADR-0108 Step 5 — the invoice family's arm ----
    // Same two connections, so the same B4 guarantee holds: every number is
    // re-derived from disk by the gate, none of them by the migrator.
    crate::migrate_billing::reconcile_billing(&src, &dst, &mut r.checks, &mut r.hard_stops)?;

    Ok(r)
}

/// The mirror is a **cross-check arm, never a source** (Q7). A flat
/// "SQLite head == DuckDB head == mirror tail" equality would hard-stop on the
/// 2026-07-19 shape with no route forward, so the divergence is *classified*:
///
/// * *mirror == table* → proceed.
/// * *mirror **ahead*** (the 2026-07-19 shape) → stop, do **not** migrate and
///   do **not** force-fix; route to the existing heal path, let it settle, then
///   re-run the migrator from the top. **A migration is not a repair tool.**
/// * *table **ahead*** → hard stop, no heal. This direction means the fsync'd
///   mirror missed a committed append — a durability failure in the artefact
///   the whole scheme leans on, and it must not be papered over by a migration.
fn classify_mirror(duckdb_path: &Path, db_head_seq: i64, r: &mut ReconcileReport) -> Result<()> {
    let mirror_path = aberp_audit_ledger::mirror_path_for(duckdb_path);
    if !mirror_path.exists() {
        r.hard_stops.push(format!(
            "no audit mirror at {} — the ADR-0030 divergence detector is the cross-check arm \
             this gate needs; its absence is not something to proceed past",
            mirror_path.display()
        ));
        return Ok(());
    }
    let entries = aberp_audit_ledger::read_mirror_entries(&mirror_path)
        .with_context(|| format!("read mirror {}", mirror_path.display()))?;
    let tail = entries.last().map(|e| e.seq() as i64).unwrap_or(0);
    match tail.cmp(&db_head_seq) {
        std::cmp::Ordering::Equal => {
            r.checks
                .push(format!("✓ mirror tail == DuckDB head (seq {tail})"));
        }
        std::cmp::Ordering::Greater => r.hard_stops.push(format!(
            "mirror is AHEAD of the DB table (mirror {tail} > db {db_head_seq}) — the \
             2026-07-19 shape. STOP: route to the existing heal path \
             (`heal_from_mirror_ahead`), let it settle, then re-run the migrator from the top. \
             A migration is not a repair tool."
        )),
        std::cmp::Ordering::Less => r.hard_stops.push(format!(
            "DB table is AHEAD of the mirror (db {db_head_seq} > mirror {tail}) — the fsync'd \
             mirror missed a committed append. That is a durability failure in the artefact \
             this scheme leans on; it must not be papered over by a migration. HARD STOP, no \
             heal."
        )),
    }
    Ok(())
}

fn sha256_file(p: &Path) -> Result<String> {
    let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

fn read_ledger_rows(conn: &duckdb::Connection) -> Result<Vec<LedgerRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, seq, prev_hash, time_wall, time_mono, actor, binary_hash, tenant_id,
                kind, payload, idempotency_key, entry_hash, session_id, session_pubkey, event_sig
         FROM audit_ledger ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map([], |x| {
        Ok((
            x.get(0)?,
            x.get(1)?,
            x.get(2)?,
            x.get(3)?,
            x.get(4)?,
            x.get(5)?,
            x.get(6)?,
            x.get(7)?,
            x.get(8)?,
            x.get(9)?,
            x.get(10)?,
            x.get(11)?,
            x.get(12)?,
            x.get(13)?,
            x.get(14)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

type AnchorRow = (
    String,
    String,
    String,
    String,
    String,
    Option<Vec<u8>>,
    String,
    String,
    String,
);

fn read_anchor_rows(conn: &duckdb::Connection) -> Result<Vec<AnchorRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
                timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc
         FROM audit_ledger_anchors ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |x| {
        Ok((
            x.get(0)?,
            x.get(1)?,
            x.get(2)?,
            x.get(3)?,
            x.get(4)?,
            x.get(5)?,
            x.get(6)?,
            x.get(7)?,
            x.get(8)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
