//! ADR-0099 H2 / ADR-0095 §1·§2·§4 — crash-safe initial DB creation + the
//! validated boot safe-open chokepoint (Class 3).
//!
//! Backported faithfully from the production-proven editions
//! (`Cservin69/ABERP-Editions` `crates/aberp-snapshot/src/{crash_safe,recover}.rs`
//! @ `1a56872`), scoped to exactly what H2 governs — no recovery engine (that is
//! H5). Two invariants:
//!
//!   1. **Atomic creation.** When the tenant DB does not yet exist,
//!      [`provision_atomic`] builds it ASIDE at `<db>.creating-<tag>.duckdb`,
//!      folds its WAL, then atomically renames it onto the live path and writes
//!      a verified-good marker. A crash mid-creation leaves only a disposable
//!      temp (swept on the next boot) — NEVER a torn file at the live path.
//!   2. **Safe-open-on-boot.** [`probe_open_or_preserve`] is the SINGLE validated
//!      probe-open the boot runs BEFORE any subsystem opens the DB. A torn /
//!      unopenable live file (the torn-checkpoint signature, "Failed to load
//!      metadata pointer", ADR-0095 root cause #1) is detected HERE, preserved
//!      byte-for-byte to `<db>.CORRUPT-<tag>`, and the boot REFUSES. H2 stops at
//!      **refuse-with-evidence**; the guarded auto-recovery is H5.
//!
//! The crash-safety lives in [`atomic_install`] + the marker functions, which
//! operate on PLAIN FILES (no DuckDB) and are exhaustively unit-tested below —
//! including the "crash between write and rename leaves the old good DB intact"
//! property and a real-subprocess crash-injection test that uses plain files, so
//! the load-bearing COMMIT property runs in EVERY CI arm with no DuckDB. The
//! DuckDB-backed provision/probe end-to-end lives in
//! `tests/crash_safe_boot_e2e.rs` (needs the bundled libduckdb build → the CI
//! gate), exactly like the crate's other DuckDB-touching integration suites.
//!
//! **Prod-backport adaptation (flagged):** the editions entrypoints each call
//! `ensure_not_prod_path` first, so an *editions* build can never act on the
//! FROZEN prod line (`~/.aberp/`, ADR-0093). That guard is DELIBERATELY OMITTED
//! here — in the prod tree the live DB *is* the prod line H2 must provision, so
//! porting the guard would refuse the very path this module exists to create.

use std::path::{Path, PathBuf};

use duckdb::Connection;
use serde::{Deserialize, Serialize};

use crate::take::sha256_file;
use crate::{Result, SnapshotError};

/// Suffix of the verified-good checkpoint marker written beside a DB.
pub const CKPT_MARKER_SUFFIX: &str = ".ckpt-ok";

/// Infix of the aside build path used during atomic initial creation
/// (`<db>.creating-<tag>.duckdb`). Distinct from the `.CORRUPT-` evidence infix
/// so stale-staging cleanup never touches retained evidence.
const CREATING_INFIX: &str = ".creating-";

/// Infix of the retained torn-DB evidence copy (`<db>.CORRUPT-<tag>`).
const CORRUPT_INFIX: &str = ".CORRUPT-";

/// Verified-good checkpoint marker (`<db>.ckpt-ok`). Records the identity of the
/// file that was last durably installed, so a later boot can tell whether a
/// fresh checkpoint is still needed (ADR-0095 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointMarker {
    /// Hex SHA-256 of the `*.duckdb` file at the moment it was installed.
    pub sha256: String,
    /// Byte size of that file.
    pub byte_size: u64,
    /// Unix seconds when the marker was written.
    pub created_at_unix: i64,
}

/// `<db>.ckpt-ok` marker path.
pub fn marker_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push(CKPT_MARKER_SUFFIX);
    PathBuf::from(os)
}

/// DuckDB names the WAL by appending `.wal` to the FULL filename
/// (`x.duckdb` → `x.duckdb.wal`) — NOT `Path::with_extension`.
fn wal_sibling(db: &Path) -> PathBuf {
    let mut os = db.as_os_str().to_owned();
    os.push(".wal");
    PathBuf::from(os)
}

/// A sibling path `<db><suffix>` in the same directory as the DB.
fn sibling(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

/// Process + nanosecond tag so concurrent / repeated runs never collide.
fn unique_tag() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

/// `fsync` a regular file's contents + metadata to disk.
fn fsync_file(path: &Path) -> Result<()> {
    let f = std::fs::File::open(path).map_err(|e| SnapshotError::io(path, e))?;
    f.sync_all().map_err(|e| SnapshotError::io(path, e))
}

/// `fsync` a directory so a rename/create within it is durable. Opening a
/// directory read-only and `sync_all`-ing its fd is the canonical POSIX way to
/// persist a directory entry change. A platform that refuses to open a directory
/// (rare) is a soft failure: the rename already happened, so we log and continue
/// rather than fail the whole install.
fn fsync_dir(dir: &Path) -> Result<()> {
    match std::fs::File::open(dir) {
        Ok(f) => f.sync_all().map_err(|e| SnapshotError::io(dir, e)),
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %e,
                "could not open directory to fsync it after rename; rename already \
                 completed so continuing (durability of the swap is best-effort here)"
            );
            Ok(())
        }
    }
}

/// **The crash-safe commit primitive.** Durably replace `target` with the
/// finished `staged` file:
///
///   1. drop any WAL beside `staged` (a checkpointed file is self-contained),
///   2. `fsync` `staged` so its bytes are on disk BEFORE the swap,
///   3. atomic `rename(staged → target)` — the swap is all-or-nothing,
///   4. drop any stale WAL beside `target` (an old WAL would corrupt the fresh
///      self-contained file on next open),
///   5. `fsync` the parent directory so the rename itself is durable.
///
/// Crash semantics: a crash before step 3 leaves the **old** `target` intact
/// (and a removable `staged`); a crash after step 3 leaves the **new** `target`
/// intact. There is no torn intermediate `target` at any point — which is the
/// whole point.
///
/// # Errors
///
/// [`SnapshotError::Io`] if the fsync or rename fails.
pub fn atomic_install(staged: &Path, target: &Path) -> Result<()> {
    let staged_wal = wal_sibling(staged);
    if staged_wal.exists() {
        let _ = std::fs::remove_file(&staged_wal);
    }
    fsync_file(staged)?;
    std::fs::rename(staged, target).map_err(|e| SnapshotError::io(target, e))?;
    let target_wal = wal_sibling(target);
    if target_wal.exists() {
        std::fs::remove_file(&target_wal).map_err(|e| SnapshotError::io(&target_wal, e))?;
    }
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Write (and `fsync`) the verified-good marker for `db_path`, recording the
/// file's current SHA-256 + size. The marker write is itself durable: the marker
/// file is fsync'd and so is its parent directory (ADR-0095 §4).
///
/// # Errors
///
/// [`SnapshotError::Io`] on a read/write/fsync failure, or
/// [`SnapshotError::BadMeta`] if the marker cannot be serialised.
pub fn write_marker(db_path: &Path) -> Result<CheckpointMarker> {
    let sha256 = sha256_file(db_path)?;
    let byte_size = std::fs::metadata(db_path)
        .map_err(|e| SnapshotError::io(db_path, e))?
        .len();
    let created_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let marker = CheckpointMarker {
        sha256,
        byte_size,
        created_at_unix,
    };
    let path = marker_path(db_path);
    let bytes = serde_json::to_vec_pretty(&marker).map_err(|e| SnapshotError::BadMeta {
        path: path.clone(),
        detail: format!("serialize checkpoint marker: {e}"),
    })?;
    std::fs::write(&path, bytes).map_err(|e| SnapshotError::io(&path, e))?;
    fsync_file(&path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fsync_dir(parent)?;
    }
    Ok(marker)
}

/// Read the verified-good marker beside `db_path`, if present + parseable.
pub fn read_marker(db_path: &Path) -> Option<CheckpointMarker> {
    let path = marker_path(db_path);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Byte size of the `<db>.wal` sibling, or `0` if it is absent/unreadable.
fn wal_len(wal: &Path) -> u64 {
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

/// `true` iff a verified-good checkpoint already covers the CURRENT file — i.e.
/// a marker exists and its SHA-256 matches the file on disk right now, AND no
/// pending WAL sits beside it. A non-empty `<db>.wal` means committed data the
/// main-file hash does not cover (ADR-0098 R6 / NEW-1(a)); a missing marker, an
/// unreadable marker, or a SHA mismatch (the DB was written since the last
/// checkpoint) all mean "checkpoint missing".
pub fn checkpoint_is_current(db_path: &Path) -> bool {
    if wal_len(&wal_sibling(db_path)) > 0 {
        return false;
    }
    let Some(marker) = read_marker(db_path) else {
        return false;
    };
    match sha256_file(db_path) {
        Ok(actual) => actual == marker.sha256,
        Err(_) => false,
    }
}

/// Open the freshly-built temp DB once and `CHECKPOINT` it so its WAL is folded
/// in and the file is self-contained before the atomic swap.
fn checkpoint_file(db: &Path) -> Result<()> {
    let conn = Connection::open(db)?;
    conn.execute_batch("CHECKPOINT;")?;
    Ok(())
}

/// Remove a temp file and any DuckDB WAL beside it. Best-effort.
fn cleanup_temp(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    let wal = wal_sibling(path);
    if wal.exists() {
        let _ = std::fs::remove_file(&wal);
    }
}

/// Remove orphan `<db><infix>*` siblings (e.g. `.creating-*`) left by a crash,
/// without ever touching the live DB or the retained `.CORRUPT-*` evidence
/// (distinct infixes). Pure best-effort cleanup that never fails the caller.
fn cleanup_siblings_with_infix(db_path: &Path, infix: &str) {
    let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return;
    };
    let Some(stem) = db_path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix = format!("{stem}{infix}");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Copy a torn/replaced live DB aside to `<db>.CORRUPT-<tag>` and return the
/// retained path. A COPY (not a move): the original stays in place so a refuse
/// arm leaves the operator with BOTH the original live file and the evidence
/// copy (H5's guarded recovery consumes it later).
fn preserve_corrupt_db(db_path: &Path) -> Result<PathBuf> {
    let dest = sibling(db_path, &format!("{CORRUPT_INFIX}{}", unique_tag()));
    std::fs::copy(db_path, &dest).map_err(|e| SnapshotError::io(&dest, e))?;
    Ok(dest)
}

/// Sweep any leftover `<db>.creating-*` staging siblings from a prior provision
/// that a crash interrupted (ADR-0095 §2). Best-effort; never fails the caller
/// and never touches the live DB or retained `.CORRUPT-*` evidence. The boot
/// chokepoint calls this on BOTH the missing- and present-DB paths so an
/// interrupted prior creation cannot leave staging litter behind on a DB that
/// then opens fine. [`provision_atomic`] also sweeps internally (defense in
/// depth).
pub fn cleanup_stale_staging(db_path: &Path) {
    cleanup_siblings_with_infix(db_path, CREATING_INFIX);
}

/// **Atomic initial DB creation (ADR-0095 §1·§2).** Build the fresh tenant DB
/// aside via `init` (which writes to the `<db>.creating-<tag>.duckdb` path it is
/// handed, NEVER the live path), fold its WAL so it is a single self-contained
/// file, then atomically swap it over the final path with the crash-safe commit
/// + verified-good marker. A crash before the rename leaves only a disposable
/// temp (swept next boot); the live path is never written with a torn file.
///
/// `init` receives the aside path and must lay down whatever seed schema /
/// genesis makes the file a valid, openable DuckDB (the boot caller passes the
/// billing-schema seed; the remaining subsystem schemas complete idempotently on
/// the now-safely-present live DB).
///
/// # Errors
///
/// [`SnapshotError::Provision`] if `init` fails (the live path is never
/// written); otherwise any I/O or DuckDB error from the checkpoint / atomic
/// install / marker write.
pub fn provision_atomic<F, E>(db_path: &Path, init: F) -> Result<()>
where
    F: FnOnce(&Path) -> std::result::Result<(), E>,
    E: std::fmt::Display,
{
    // Clear any orphan temp from a crashed prior creation (the next-boot cleanup
    // ADR-0095 §2 promises) so it can never accumulate or be reused. (Editions
    // calls `ensure_not_prod_path` here first; deliberately omitted in the prod
    // backport — see the module docs — because the live DB IS the prod line.)
    cleanup_siblings_with_infix(db_path, CREATING_INFIX);

    let creating = sibling(db_path, &format!("{CREATING_INFIX}{}.duckdb", unique_tag()));
    cleanup_temp(&creating);

    // Build the fresh DB aside (never the live path).
    init(&creating).map_err(|e| SnapshotError::Provision {
        path: creating.clone(),
        detail: e.to_string(),
    })?;

    // Fold the WAL so the temp is a single self-contained file, then swap it
    // over the final path with the crash-safe commit + verified-good marker.
    checkpoint_file(&creating)?;
    atomic_install(&creating, db_path)?;
    write_marker(db_path)?;
    Ok(())
}

/// Open `db_path` and force DuckDB to actually parse the file header + load the
/// catalog, so a torn *body* (not just a torn header) is rejected here and not
/// by a later subsystem open. Returns the DuckDB error text on failure.
///
/// `PRAGMA database_list;` is the exact catalog-touch the editions boot-crash
/// e2e uses to prove a torn file will not open cleanly; any DuckDB — including a
/// freshly provisioned, empty one — answers it, so a clean DB always passes.
fn probe_open(db_path: &Path) -> std::result::Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA database_list;")
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// **The single validated boot safe-open (ADR-0099 H2 / ADR-0095 §1).** Probe
/// the live DB once, BEFORE any boot subsystem opens it. A clean open returns
/// `Ok(())` and the boot proceeds. A torn / corrupt live file is caught HERE:
/// the corrupt file is PRESERVED byte-for-byte to `<db>.CORRUPT-<tag>` (a COPY —
/// the original is left in place, untouched) and [`SnapshotError::DbCorruptPreserved`]
/// is returned so the boot caller REFUSES to serve. H2 stops at
/// refuse-with-evidence; the guarded auto-recovery is H5.
///
/// # Errors
///
/// [`SnapshotError::DbCorruptPreserved`] if the probe-open fails (evidence
/// preserved), or [`SnapshotError::Io`] if preserving the evidence itself fails.
pub fn probe_open_or_preserve(db_path: &Path) -> Result<()> {
    match probe_open(db_path) {
        Ok(()) => Ok(()),
        Err(detail) => {
            let preserved = preserve_corrupt_db(db_path)?;
            Err(SnapshotError::DbCorruptPreserved {
                path: db_path.to_path_buf(),
                preserved,
                detail,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    //! Plain-file crash-injection + cleanup unit tests. These use PLAIN FILES
    //! (no DuckDB at runtime) so they exercise the load-bearing crash-safe
    //! COMMIT property anywhere, in every CI arm. The DuckDB-backed provision /
    //! probe end-to-end lives in `tests/crash_safe_boot_e2e.rs` (CI gate).

    use super::*;
    use std::process::Command;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static C: AtomicU64 = AtomicU64::new(0);
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let seq = C.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "aberp-crashsafe-{label}-{}-{n}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn join(&self, n: &str) -> PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── atomic_install: the crash-safe COMMIT property ────────────────────

    #[test]
    fn atomic_install_replaces_target_with_staged() {
        let t = Tmp::new("replace");
        let target = t.join("live.duckdb");
        let staged = t.join("live.duckdb.creating-x.duckdb");
        std::fs::write(&target, b"OLD-GOOD").unwrap();
        std::fs::write(&staged, b"NEW-GOOD-CHECKPOINTED").unwrap();

        atomic_install(&staged, &target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"NEW-GOOD-CHECKPOINTED");
        assert!(!staged.exists(), "staged file consumed by the rename");
    }

    #[test]
    fn crash_before_rename_leaves_old_good_db_intact() {
        // Simulate a crash AFTER building the staging file but BEFORE the atomic
        // install runs: the live file must still be the old good one, fully
        // readable, and the orphan staging file is just removable.
        let t = Tmp::new("crash");
        let target = t.join("live.duckdb");
        let staged = t.join("live.duckdb.creating-x.duckdb");
        std::fs::write(&target, b"OLD-GOOD").unwrap();
        std::fs::write(&staged, b"NEW-not-yet-committed").unwrap();

        // …crash here (atomic_install never called)…

        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"OLD-GOOD",
            "the live DB must survive a crash before the swap, untorn"
        );
        std::fs::remove_file(&staged).unwrap();
        assert!(target.exists());
    }

    #[test]
    fn atomic_install_clears_stale_target_wal() {
        let t = Tmp::new("wal");
        let target = t.join("live.duckdb");
        let target_wal = t.join("live.duckdb.wal");
        let staged = t.join("live.duckdb.creating-x.duckdb");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&target_wal, b"stale-wal-from-old-file").unwrap();
        std::fs::write(&staged, b"NEW-self-contained").unwrap();

        atomic_install(&staged, &target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"NEW-self-contained");
        assert!(
            !target_wal.exists(),
            "the stale WAL beside the live file must be cleared so the fresh \
             self-contained file is not corrupted on next open"
        );
    }

    // ── marker / checkpoint currency ──────────────────────────────────────

    #[test]
    fn marker_roundtrips_and_tracks_currency() {
        let t = Tmp::new("marker");
        let db = t.join("live.duckdb");
        std::fs::write(&db, b"checkpointed-bytes-v1").unwrap();

        let m = write_marker(&db).unwrap();
        assert_eq!(read_marker(&db).unwrap(), m, "marker round-trips on disk");
        assert!(
            checkpoint_is_current(&db),
            "no WAL + matching marker => current"
        );

        // A NON-EMPTY WAL sibling => committed data the main hash does not cover.
        let wal = wal_sibling(&db);
        std::fs::write(&wal, b"pending-commit-bytes").unwrap();
        assert!(
            !checkpoint_is_current(&db),
            "a non-empty pending WAL must force NOT-current"
        );

        // An EMPTY (size-0) WAL is NOT a pending commit and stays current.
        std::fs::write(&wal, b"").unwrap();
        assert!(
            checkpoint_is_current(&db),
            "an empty size-0 WAL is not a pending commit => still current"
        );
        std::fs::remove_file(&wal).unwrap();

        // The file changing under the marker => not current.
        std::fs::write(&db, b"checkpointed-bytes-v2-CHANGED").unwrap();
        assert!(
            !checkpoint_is_current(&db),
            "a marker staled by a later write must read as NOT current"
        );
    }

    #[test]
    fn checkpoint_is_current_false_without_a_marker() {
        let t = Tmp::new("no-marker");
        let db = t.join("live.duckdb");
        std::fs::write(&db, b"bytes-but-no-marker").unwrap();
        assert!(
            !checkpoint_is_current(&db),
            "a DB with no marker is never 'current'"
        );
    }

    // ── stale-staging cleanup: sweep `.creating-*`, keep `.CORRUPT-*` ─────

    #[test]
    fn cleanup_stale_staging_removes_creating_but_keeps_evidence_and_live() {
        let t = Tmp::new("stale");
        let db = t.join("aberp.duckdb");
        std::fs::write(&db, b"LIVE-GOOD").unwrap();
        // Two orphan `.creating-*` staging leftovers from interrupted provisions.
        let s1 = t.join("aberp.duckdb.creating-111-222.duckdb");
        let s2 = t.join("aberp.duckdb.creating-333-444.duckdb");
        std::fs::write(&s1, b"half").unwrap();
        std::fs::write(&s2, b"half").unwrap();
        // A retained `.CORRUPT-*` evidence copy — must NOT be swept.
        let ev = t.join("aberp.duckdb.CORRUPT-999-888");
        std::fs::write(&ev, b"preserved-torn-evidence").unwrap();

        cleanup_stale_staging(&db);

        assert!(
            !s1.exists() && !s2.exists(),
            "all `.creating-*` staging swept"
        );
        assert!(
            ev.exists(),
            "`.CORRUPT-*` evidence is never swept (distinct infix)"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            b"LIVE-GOOD",
            "the live DB is never touched by the sweep"
        );
    }

    #[test]
    fn preserve_corrupt_db_copies_aside_and_leaves_original_intact() {
        let t = Tmp::new("preserve");
        let db = t.join("aberp.duckdb");
        std::fs::write(&db, b"TORN-DUCKDB-BYTES").unwrap();

        let kept = preserve_corrupt_db(&db).unwrap();

        assert!(kept.exists(), "the corrupt DB was retained as evidence");
        assert!(
            kept.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains(".CORRUPT-"),
            "evidence lands at `<db>.CORRUPT-<tag>`"
        );
        assert_eq!(
            std::fs::read(&kept).unwrap(),
            b"TORN-DUCKDB-BYTES",
            "evidence is a byte-for-byte copy"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            b"TORN-DUCKDB-BYTES",
            "the original live file is a COPY source — left in place, untouched"
        );
    }

    // ── the load-bearing crash-injection property (real subprocess) ───────

    /// When set, this process is the CRASH CHILD: it performs the staging write
    /// of an atomic-create then hard-aborts BEFORE the rename, simulating a
    /// power loss mid initial-creation.
    const CRASH_ENV: &str = "ABERP_CRASHSAFE_CRASH_CHILD";
    /// The exact libtest name of the crash test, used to re-exec just it.
    const CRASH_TEST: &str =
        "crash_safe::tests::provision_atomic_crash_before_rename_never_leaves_torn_live_file";

    /// RED-before/GREEN-after: a `duckdb::Connection::open(&args.db)` that
    /// created the file directly at the live path (the pre-H2 boot behaviour)
    /// would leave a TORN file there on a mid-creation crash. With atomic
    /// creation the aside temp survives but the live path is NEVER written, and
    /// the interrupted install is finished with ZERO manual steps on the retry.
    #[test]
    fn provision_atomic_crash_before_rename_never_leaves_torn_live_file() {
        // ── CHILD MODE ──────────────────────────────────────────────────
        // Do the staging write to the `.creating-` temp (NEVER the live path)
        // and then crash hard, before any rename.
        if let Ok(staging) = std::env::var(CRASH_ENV) {
            let staging = PathBuf::from(staging);
            std::fs::write(&staging, b"HALF-WRITTEN-DB-BYTES").unwrap();
            // …power loss here… the rename to the live path never happens.
            std::process::abort();
        }

        // ── PARENT MODE ─────────────────────────────────────────────────
        let t = Tmp::new("crash-create");
        let live = t.join("aberp.duckdb");
        let staging = t.join("aberp.duckdb.creating-child.duckdb");

        // Re-exec ONLY this test, in child (crash) mode.
        let exe = std::env::current_exe().expect("current_exe");
        let status = Command::new(exe)
            .args(["--exact", CRASH_TEST])
            .env(CRASH_ENV, &staging)
            .env("RUST_TEST_THREADS", "1")
            .status()
            .expect("spawn crash child");
        assert!(
            !status.success(),
            "the child must have crashed (aborted), not exited 0"
        );

        // THE LOAD-BEARING PROPERTY: a crash mid initial-creation leaves the
        // temp aside but NEVER a (torn) file at the live path.
        assert!(
            !live.exists(),
            "a crash before the atomic rename must never leave a file at the live path"
        );
        assert!(
            staging.exists(),
            "the half-written temp survives, aside from the live path"
        );

        // RECOVERY with ZERO manual steps: the next attempt finishes the
        // crash-safe commit (the REAL atomic_install + verified-good marker) and
        // the live path becomes the good, openable file.
        std::fs::write(&staging, b"COMPLETE-SELF-CONTAINED-DB").unwrap();
        atomic_install(&staging, &live).expect("atomic_install");
        write_marker(&live).expect("write_marker");
        assert_eq!(
            std::fs::read(&live).unwrap(),
            b"COMPLETE-SELF-CONTAINED-DB",
            "the live path is the good rebuilt file"
        );
        assert!(
            checkpoint_is_current(&live),
            "a verified-good marker now covers the installed file"
        );
    }
}
