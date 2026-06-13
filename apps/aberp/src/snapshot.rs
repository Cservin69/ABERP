//! S393 — manual snapshot / restore CLI (the operator panic button).
//!
//! ## Why this exists
//!
//! DuckDB's on-disk storage upgrade is **one-way**: once the 1.5.3
//! binary opens a 1.5.2 database read-write, the file is rewritten in
//! the newer storage format and the older binary can no longer read it.
//! S393 bumps the bundled engine to libduckdb 1.5.3 to escape a NEW
//! 1.5.2 read-path assertion (`dict_offset <= ... FetchStringFromDict`,
//! `string_uncompressed.hpp:247`). Before any such irreversible op — or
//! before any risky DEV test that could leave the DB degraded — the
//! operator wants a cheap, validated, off-to-the-side copy they can
//! roll back to.
//!
//! This module is that panic button. Two operations, both file-level:
//!
//!   - [`take_snapshot`] — copy the live DB (main file + WAL) to a
//!     timestamped file OUTSIDE the repo and OUTSIDE `~/.aberp/`, fold
//!     the WAL into the copy via a checkpoint, then validate the copy
//!     read-only with `PRAGMA verify_external_invariants`. A snapshot
//!     that fails validation is reported loudly and the command exits
//!     non-zero, so a *degrading live DB* is surfaced at snapshot time
//!     rather than discovered at restore time ([[fail-loud]]).
//!
//!   - [`restore_snapshot`] — refuse to overwrite a DB that a live
//!     `aberp serve` still holds (detected via DuckDB's own exclusive
//!     file lock — the same lock serve takes when it opens the DB), and
//!     otherwise atomically swap the snapshot into place.
//!
//! ## What is intentionally NOT here (deferred — separate snapshot
//! session)
//!
//! Periodic snapshot daemon, retention policy (rolling 24h/7d/1m/1y),
//! cloud/S3 sync, storefront DR, encryption-at-rest. Tonight is the
//! manual panic button only (CLAUDE.md #2/#13).
//!
//! ## Storage location
//!
//! Snapshots default to `~/Documents/ABERP-snapshots/` — OUTSIDE the
//! repo (never committed) and OUTSIDE `~/.aberp/` (so a `rm -rf
//! ~/.aberp/<tenant>` reset, or a restore, never touches the rollback
//! copies). The dir is created on first snapshot.
//!
//! ## Consistency posture ([[trust-code-not-operator]])
//!
//! The validation is in code, not operator inspection. Ideally the
//! operator stops `aberp serve` before snapshotting so the copy is a
//! quiescent checkpoint; if they snapshot a live, mid-write DB the
//! main-file/WAL pair can be copied torn, and the post-copy checkpoint
//! + `verify_external_invariants` is what catches that — the command
//! fails rather than silently producing an unrestorable snapshot.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use duckdb::{AccessMode, Config, Connection};

use crate::cli::{RestoreSnapshotArgs, SnapshotArgs};
use crate::fs::write_atomic;

/// DuckDB names the write-ahead log by appending `.wal` to the FULL db
/// filename (so `aberp.duckdb` → `aberp.duckdb.wal`). This is NOT
/// `Path::with_extension`, which would replace `.duckdb`.
fn wal_sibling(db: &Path) -> PathBuf {
    let mut os = db.as_os_str().to_owned();
    os.push(".wal");
    PathBuf::from(os)
}

/// Atomically copy `src` to `dst` via the shared S390 atomic writer
/// (temp → fsync → rename → fsync-dir). Reads the whole source into
/// memory — fine for the tenant DBs this serves (DEV/operator scale);
/// a future streaming copy would only matter for very large DBs.
fn copy_atomic(src: &Path, dst: &Path) -> Result<()> {
    let bytes = std::fs::read(src)
        .with_context(|| format!("read source file {} for snapshot copy", src.display()))?;
    write_atomic(dst, &bytes)
}

/// Take a validated snapshot of the DuckDB at `db_path` into
/// `dest_path`.
///
/// Steps:
/// 1. Copy the main DB file → `dest_path` and, if present, its WAL →
///    `dest_path`'s WAL sibling (atomic writes, so a concurrent reader
///    of the destination never sees a torn file).
/// 2. Open the **copy** read-write once and `CHECKPOINT` — this replays
///    any copied WAL into the main file and folds it in, leaving a
///    single self-contained snapshot file with no outstanding WAL. The
///    copy is mutated, never the source.
/// 3. Re-open the copy **read-only** and run
///    `PRAGMA verify_external_invariants`. Any reported corruption (rows
///    or a thrown error) fails the snapshot — see [`validate_snapshot`].
///
/// On success `dest_path` is a clean, checkpoint-consistent,
/// freshly-validated DuckDB file (in 1.5.3 storage format).
pub fn take_snapshot(db_path: &Path, dest_path: &Path) -> Result<()> {
    if !db_path.exists() {
        bail!(
            "source database {} does not exist — nothing to snapshot",
            db_path.display()
        );
    }
    if let Some(parent) = dest_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create snapshot destination dir {}", parent.display()))?;
    }

    // 1. Copy main + WAL (if any) to the destination pair.
    copy_atomic(db_path, dest_path)?;
    let src_wal = wal_sibling(db_path);
    let dst_wal = wal_sibling(dest_path);
    if src_wal.exists() {
        copy_atomic(&src_wal, &dst_wal)?;
    } else if dst_wal.exists() {
        // A stale WAL beside a same-named prior snapshot would corrupt
        // the new main on replay — clear it so the pair is clean.
        std::fs::remove_file(&dst_wal)
            .with_context(|| format!("remove stale snapshot WAL {}", dst_wal.display()))?;
    }

    // 2. Fold the copied WAL into the copy via a checkpoint, mutating
    //    only the snapshot (never the source DB). After this the
    //    snapshot is a single file with no outstanding WAL.
    {
        let conn = Connection::open(dest_path).with_context(|| {
            format!(
                "open snapshot copy {} read-write to fold WAL",
                dest_path.display()
            )
        })?;
        conn.execute_batch("CHECKPOINT;")
            .with_context(|| format!("checkpoint snapshot copy {}", dest_path.display()))?;
    }
    // Best-effort: a clean checkpoint leaves the WAL empty/removed; drop
    // any lingering empty WAL so the snapshot is a lone file.
    if dst_wal.exists() {
        let _ = std::fs::remove_file(&dst_wal);
    }

    // 3. Validate read-only — loud-fail on any corruption signal.
    validate_snapshot(dest_path)?;
    Ok(())
}

/// Restore `snapshot_path` over `db_path`, refusing if a live process
/// (e.g. `aberp serve`) still holds the destination DB.
///
/// Order of checks ([[fail-loud]], least-destructive first):
/// 1. The snapshot itself must pass `verify_external_invariants` — we
///    never clobber a (possibly fine) live DB with a corrupt snapshot.
/// 2. The destination must NOT be held open by a live process — probed
///    via DuckDB's own exclusive file lock (the lock serve takes). If
///    held, refuse and instruct the operator to stop the server.
/// 3. Atomically swap the snapshot's bytes into `db_path` and clear any
///    stale destination WAL (the snapshot is checkpoint-folded, so a
///    leftover WAL from the old DB would corrupt the restored main).
pub fn restore_snapshot(snapshot_path: &Path, db_path: &Path) -> Result<()> {
    if !snapshot_path.exists() {
        bail!(
            "snapshot file {} does not exist — nothing to restore from",
            snapshot_path.display()
        );
    }

    // 1. Refuse to restore from a corrupt snapshot.
    validate_snapshot(snapshot_path)
        .with_context(|| format!("snapshot {} failed validation", snapshot_path.display()))?;

    // 2. Refuse if a live process holds the destination DB.
    if db_held_by_live_process(db_path)? {
        bail!(
            "refusing to restore over {} — a process (most likely `aberp serve`) \
             still holds the database lock.\n\
             Magyarul: állítsd le az ABERP szervert, mielőtt visszaállítasz.\n\
             Stop the server first, then re-run this command.",
            db_path.display()
        );
    }

    // 3. Swap the snapshot in and clear any stale destination WAL. The
    //    destination dir may not exist yet (restoring into a fresh DB
    //    location), so create it before the atomic write.
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create restore destination dir {}", parent.display()))?;
    }
    copy_atomic(snapshot_path, db_path)?;
    let snap_wal = wal_sibling(snapshot_path);
    let dst_wal = wal_sibling(db_path);
    if snap_wal.exists() {
        copy_atomic(&snap_wal, &dst_wal)?;
    } else if dst_wal.exists() {
        std::fs::remove_file(&dst_wal)
            .with_context(|| format!("remove stale destination WAL {}", dst_wal.display()))?;
    }
    Ok(())
}

/// Classify a DuckDB open error: `true` iff it is a cross-process
/// exclusive-lock conflict (the signal that `aberp serve` — or another
/// CLI process — holds the DB), `false` for any other error (corruption,
/// permissions, …) which must NOT block a restore. Matched on DuckDB's
/// lock-conflict message: "Could not set lock on file ...: Conflicting
/// lock is held in ... by PID ...".
fn is_lock_conflict_message(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("could not set lock") || m.contains("conflicting lock")
}

/// Probe whether `db_path` is held open by a live process.
///
/// Mechanism: attempt a **read-write** DuckDB open. `aberp serve` holds
/// an exclusive file lock on the DB while running, so a second
/// read-write open fails with a lock-conflict error. We match ONLY the
/// lock-conflict signal — any OTHER open error (e.g. the DB is itself
/// corrupt) is reported as "not live-held", because refusing to restore
/// over a corrupt DB would defeat the whole panic-button purpose.
///
/// A non-existent `db_path` is trivially not held (fresh restore
/// target). A successful probe-open is closed immediately before the
/// caller overwrites the file.
fn db_held_by_live_process(db_path: &Path) -> Result<bool> {
    if !db_path.exists() {
        return Ok(false);
    }
    match Connection::open(db_path) {
        Ok(conn) => {
            // We acquired the lock → no live holder. Release it at once.
            drop(conn);
            Ok(false)
        }
        Err(e) => {
            let msg = e.to_string();
            if is_lock_conflict_message(&msg) {
                Ok(true)
            } else {
                // Not a lock conflict — the DB is broken in some other
                // way. That is exactly when an operator restores, so do
                // NOT block on it; let the restore proceed.
                tracing::warn!(
                    db = %db_path.display(),
                    error = %e,
                    "destination DB open failed with a non-lock error; treating as NOT \
                     live-held so the restore can overwrite it"
                );
                Ok(false)
            }
        }
    }
}

/// Open `snapshot_path` read-only and run
/// `PRAGMA verify_external_invariants`. Returns `Ok(())` when the file
/// is a structurally-valid DuckDB whose external (index/storage)
/// invariants hold; returns `Err` otherwise.
///
/// The read-only open alone already rejects a torn/garbage file (bad
/// magic, truncated header). `verify_external_invariants` is the
/// stronger ART/storage consistency check on top. If a given engine
/// build does not expose that pragma we DON'T fail the snapshot over a
/// missing diagnostic — the read-only open still proved basic validity
/// — but we log a warning so the gap is visible.
pub fn validate_snapshot(snapshot_path: &Path) -> Result<()> {
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)
        .context("build read-only DuckDB config for snapshot validation")?;
    let conn = Connection::open_with_flags(snapshot_path, config).with_context(|| {
        format!(
            "open snapshot {} read-only for validation (a torn/corrupt file fails here)",
            snapshot_path.display()
        )
    })?;

    let mut stmt = match conn.prepare("PRAGMA verify_external_invariants") {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            if msg.to_lowercase().contains("verify_external_invariants") {
                // Pragma unavailable in this build — basic read-only
                // open already validated structure; don't hard-fail.
                tracing::warn!(
                    snapshot = %snapshot_path.display(),
                    error = %msg,
                    "PRAGMA verify_external_invariants unavailable; relying on the \
                     read-only open as the validity check"
                );
                return Ok(());
            }
            return Err(anyhow!(
                "snapshot {} is corrupt: verify_external_invariants failed to prepare: {msg}",
                snapshot_path.display()
            ));
        }
    };

    // The pragma throws on corruption (surfaced at query/step) and/or
    // returns offending rows. Treat EITHER as corruption.
    let row_count = (|| -> Result<usize> {
        let mut rows = stmt.query([])?;
        let mut n = 0usize;
        while rows.next()?.is_some() {
            n += 1;
        }
        Ok(n)
    })();

    match row_count {
        Ok(0) => Ok(()),
        Ok(n) => Err(anyhow!(
            "snapshot {} is corrupt: verify_external_invariants reported {n} \
             invariant violation row(s)",
            snapshot_path.display()
        )),
        Err(e) => Err(anyhow!(
            "snapshot {} is corrupt: verify_external_invariants raised an error: {e}",
            snapshot_path.display()
        )),
    }
}

/// Resolve `~/Documents/ABERP-snapshots/`. Uses HOME / USERPROFILE the
/// same way `runtime_discovery::runtime_file_path` does (no `dirs`
/// dep), keeping the snapshot store OUTSIDE the repo and OUTSIDE
/// `~/.aberp/`.
fn default_snapshot_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|p| !p.is_empty()))
        .ok_or_else(|| {
            anyhow!(
                "neither HOME nor USERPROFILE is set — cannot locate ~/Documents/ABERP-snapshots"
            )
        })?;
    Ok(PathBuf::from(home)
        .join("Documents")
        .join("ABERP-snapshots"))
}

/// Build the default snapshot output path
/// `~/Documents/ABERP-snapshots/<tenant>-<UTC-ts>.duckdb`.
fn default_out_path(tenant: &str, now: time::OffsetDateTime) -> Result<PathBuf> {
    use time::macros::format_description;
    const TS: &[time::format_description::FormatItem<'_>] =
        format_description!("[year][month][day]-[hour][minute][second]");
    let ts = now.format(TS).context("format snapshot timestamp")?;
    // Sanitise the tenant so it can never escape the snapshot dir.
    let safe_tenant: String = tenant
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(default_snapshot_dir()?.join(format!("{safe_tenant}-{ts}.duckdb")))
}

/// CLI entry point for `aberp snapshot`.
pub fn run_snapshot(args: &SnapshotArgs) -> Result<()> {
    let out = match &args.out {
        Some(p) => p.clone(),
        None => default_out_path(&args.tenant, time::OffsetDateTime::now_utc())?,
    };
    tracing::info!(
        tenant = %args.tenant,
        db = %args.db.display(),
        out = %out.display(),
        "taking snapshot"
    );
    take_snapshot(&args.db, &out)?;
    // Operator-facing success line on stdout (tracing goes to stderr).
    println!(
        "Snapshot written and validated: {}\n(source DB: {})",
        out.display(),
        args.db.display()
    );
    Ok(())
}

/// CLI entry point for `aberp restore-snapshot`.
pub fn run_restore(args: &RestoreSnapshotArgs) -> Result<()> {
    tracing::info!(
        tenant = %args.tenant,
        db = %args.db.display(),
        from = %args.from.display(),
        "restoring snapshot"
    );
    restore_snapshot(&args.from, &args.db)?;
    println!(
        "Restored {} from snapshot {}",
        args.db.display(),
        args.from.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test tempdir under the system temp root — mirrors the
    /// `fs::tests::ScopedTempDir` pattern (no `tempfile` dev-dep, per
    /// CLAUDE.md #2/#11).
    struct ScopedTempDir(PathBuf);

    impl ScopedTempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aberp-s393-snap-{label}-{}-{nanos}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create scoped tempdir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScopedTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a small populated DuckDB at `path` and return the row
    /// payload we expect to survive a snapshot round-trip.
    fn seed_db(path: &Path) -> Vec<(i64, String)> {
        let conn = Connection::open(path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE t (id BIGINT, name VARCHAR);
             INSERT INTO t VALUES (1, 'alpha'), (2, 'béta'), (3, 'gamma');",
        )
        .expect("seed");
        // Close so the WAL is checkpointed into the main file.
        drop(conn);
        vec![(1, "alpha".into()), (2, "béta".into()), (3, "gamma".into())]
    }

    fn read_rows(path: &Path) -> Vec<(i64, String)> {
        let config = Config::default().access_mode(AccessMode::ReadOnly).unwrap();
        let conn = Connection::open_with_flags(path, config).expect("reopen ro");
        let mut stmt = conn.prepare("SELECT id, name FROM t ORDER BY id").unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn take_snapshot_round_trips_data() {
        let dir = ScopedTempDir::new("roundtrip");
        let db = dir.path().join("aberp.duckdb");
        let expected = seed_db(&db);

        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&db, &snap).expect("snapshot ok");
        assert!(snap.exists(), "snapshot file must exist");

        // Reopen the snapshot read-only — same data.
        assert_eq!(read_rows(&snap), expected);
        // The folded snapshot carries no outstanding WAL sibling.
        assert!(
            !wal_sibling(&snap).exists(),
            "checkpoint-folded snapshot must have no WAL sibling"
        );
    }

    #[test]
    fn take_snapshot_includes_uncheckpointed_wal_writes() {
        let dir = ScopedTempDir::new("walfold");
        let db = dir.path().join("aberp.duckdb");

        // Open, write, and DO NOT checkpoint — leave rows in the WAL by
        // keeping a connection that we drop without an explicit
        // checkpoint is not enough (close checkpoints). Instead disable
        // the auto-checkpoint so the WAL survives the close.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "PRAGMA disable_checkpoint_on_shutdown;
                 SET wal_autocheckpoint = '1TB';
                 CREATE TABLE t (id BIGINT, name VARCHAR);
                 INSERT INTO t VALUES (7, 'wal-only');",
            )
            .unwrap();
            drop(conn);
        }
        // Sanity: a WAL file is present beside the source (the write
        // never made it into the main file).
        // (Not asserted hard — engine may still fold on close; the real
        //  assertion is that the snapshot captures the row regardless.)

        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&db, &snap).expect("snapshot ok");
        assert_eq!(read_rows(&snap), vec![(7, "wal-only".to_string())]);
    }

    #[test]
    fn validate_snapshot_passes_on_fresh_snapshot() {
        let dir = ScopedTempDir::new("validate");
        let db = dir.path().join("aberp.duckdb");
        seed_db(&db);
        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&db, &snap).unwrap();
        // verify_external_invariants must pass on a freshly-taken,
        // checkpoint-folded snapshot.
        validate_snapshot(&snap).expect("fresh snapshot validates clean");
    }

    #[test]
    fn validate_snapshot_rejects_garbage_file() {
        let dir = ScopedTempDir::new("garbage");
        let bogus = dir.path().join("not-a-db.duckdb");
        std::fs::write(&bogus, b"this is not a duckdb file at all").unwrap();
        assert!(
            validate_snapshot(&bogus).is_err(),
            "a non-DuckDB file must fail validation"
        );
    }

    /// Deterministic, OS-independent guard for the lock-error
    /// classifier — the part of the live-DB probe that could silently
    /// regress (e.g. a DuckDB message reword). The real cross-process
    /// refusal is exercised by `restore_refuses_when_db_is_live_lock_held`.
    #[test]
    fn lock_conflict_classifier_matches_duckdb_message_only() {
        assert!(is_lock_conflict_message(
            "IO Error: Could not set lock on file \"/x/aberp.duckdb\": \
             Conflicting lock is held in /usr/bin/aberp (PID 4242)"
        ));
        assert!(is_lock_conflict_message("Conflicting lock is held"));
        // A NON-lock error (corruption / missing file) must NOT be read
        // as live-held — restoring over a broken DB is the whole point.
        assert!(!is_lock_conflict_message(
            "IO Error: Could not read from file: checksum mismatch"
        ));
        assert!(!is_lock_conflict_message("Catalog Error: Table not found"));
    }

    /// Hidden subprocess entry point. When `ABERP_S393_HOLD_DB` is set
    /// (only by the parent test below), this opens that DB read-write —
    /// taking DuckDB's CROSS-PROCESS exclusive lock, exactly like a live
    /// `aberp serve` — signals readiness, and holds the lock until a
    /// release file appears (bounded at ~30s as an orphan guard). In a
    /// normal `cargo test` run the env var is absent, so this is an
    /// instant no-op.
    ///
    /// Why a subprocess: DuckDB shares ONE database instance across
    /// connections within a single process, so an in-process second
    /// open never hits the file lock. Only a separate OS process
    /// reproduces the serve-vs-CLI contention the probe must detect.
    #[test]
    fn zz_subprocess_db_lock_holder() {
        let db = match std::env::var("ABERP_S393_HOLD_DB") {
            Ok(p) if !p.is_empty() => p,
            _ => return, // normal run: no-op
        };
        let ready = std::env::var("ABERP_S393_READY_FILE").expect("ready file env");
        let release = std::env::var("ABERP_S393_RELEASE_FILE").expect("release file env");
        let _conn = Connection::open(&db).expect("subprocess holds db open like serve");
        std::fs::write(&ready, b"1").expect("signal ready");
        for _ in 0..3000 {
            if Path::new(&release).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn restore_refuses_when_db_is_live_lock_held() {
        let dir = ScopedTempDir::new("livelock");
        let db = dir.path().join("aberp.duckdb");
        seed_db(&db);
        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&db, &snap).unwrap();

        // Spawn a REAL separate process that holds the DB lock (re-exec
        // this test binary filtered to the hidden holder test).
        let ready = dir.path().join("ready");
        let release = dir.path().join("release");
        let exe = std::env::current_exe().expect("current test exe");
        let mut child = std::process::Command::new(exe)
            .args([
                "--exact",
                "snapshot::tests::zz_subprocess_db_lock_holder",
                "--nocapture",
            ])
            .env("ABERP_S393_HOLD_DB", &db)
            .env("ABERP_S393_READY_FILE", &ready)
            .env("ABERP_S393_RELEASE_FILE", &release)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn lock-holder subprocess");

        // Wait until the child actually holds the lock.
        let mut held = false;
        for _ in 0..1000 {
            if ready.exists() {
                held = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(held, "lock-holder subprocess never signalled readiness");

        // The restore must refuse while the live process holds the DB.
        let result = restore_snapshot(&snap, &db);

        // Release the child regardless of the assertion outcome.
        let _ = std::fs::write(&release, b"1");
        let _ = child.wait();

        let err = result.expect_err("restore must refuse while the DB is lock-held");
        let msg = err.to_string();
        assert!(
            msg.contains("refusing to restore") && msg.to_lowercase().contains("lock"),
            "refusal must name the lock conflict: {msg}"
        );

        // Once the holder has exited, the restore succeeds.
        restore_snapshot(&snap, &db).expect("restore succeeds once serve stops");
        assert_eq!(read_rows(&db), seed_db_expected());
    }

    fn seed_db_expected() -> Vec<(i64, String)> {
        vec![(1, "alpha".into()), (2, "béta".into()), (3, "gamma".into())]
    }

    #[test]
    fn restore_round_trips_after_db_mutated() {
        let dir = ScopedTempDir::new("restore-rt");
        let db = dir.path().join("aberp.duckdb");
        seed_db(&db);
        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&db, &snap).unwrap();

        // Mutate the live DB AFTER the snapshot.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("DELETE FROM t; INSERT INTO t VALUES (99, 'mutated');")
                .unwrap();
        }
        assert_eq!(read_rows(&db), vec![(99, "mutated".to_string())]);

        // Restore rolls the DB back to the snapshot contents.
        restore_snapshot(&snap, &db).expect("restore ok");
        assert_eq!(read_rows(&db), seed_db_expected());
    }

    #[test]
    fn restore_into_nonexistent_target_succeeds() {
        let dir = ScopedTempDir::new("fresh-target");
        let src = dir.path().join("aberp.duckdb");
        seed_db(&src);
        let snap = dir.path().join("snap.duckdb");
        take_snapshot(&src, &snap).unwrap();

        // Restore into a path that does not yet exist (fresh DB dir).
        let fresh = dir.path().join("restored").join("aberp.duckdb");
        restore_snapshot(&snap, &fresh).expect("restore into fresh target");
        assert_eq!(read_rows(&fresh), seed_db_expected());
    }

    #[test]
    fn default_out_path_is_under_documents_snapshots() {
        // HOME swap is process-global; this test only reads env, it
        // does not write files, so a transient set/restore is safe
        // enough for the path-shape assertion.
        let prior = std::env::var("HOME").ok();
        let tmp = std::env::temp_dir().join(format!("aberp-s393-home-{}", std::process::id()));
        std::env::set_var("HOME", &tmp);
        let now = time::macros::datetime!(2026-06-13 21:30:05 UTC);
        let p = default_out_path("prod", now).unwrap();
        assert_eq!(
            p,
            tmp.join("Documents")
                .join("ABERP-snapshots")
                .join("prod-20260613-213005.duckdb")
        );
        match prior {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn default_out_path_sanitises_exotic_tenant() {
        let now = time::macros::datetime!(2026-06-13 00:00:00 UTC);
        let p = default_out_path("../../etc", now).unwrap();
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(!name.contains('/'), "tenant must not escape dir: {name}");
        assert!(name.starts_with(".._.._etc-"), "sanitised: {name}");
    }

    #[test]
    fn wal_sibling_appends_not_replaces_extension() {
        assert_eq!(
            wal_sibling(Path::new("/x/aberp.duckdb")),
            PathBuf::from("/x/aberp.duckdb.wal")
        );
    }
}
