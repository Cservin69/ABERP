//! ADR-0099 H3 (F-E) — cross-process **whole-DB single-writer** advisory lock.
//!
//! The shared in-process `aberp_db::Handle` (H3) guarantees ONE DuckDB instance
//! *within* a process. It cannot see a SEPARATE process. The Defense line's
//! worst durability incident was exactly that: TWO `aberp serve` instances on
//! one tenant DB, each with its own writer, forking the audit ledger — no
//! cross-process lock stood in the way.
//!
//! This closes that class. It is the whole-DB counterpart to the per-invoice
//! [`crate::submission_lock`]: one `fs2` advisory file lock (flock on
//! Linux/macOS, `LockFileEx` on Windows — the SAME primitive the audit-ledger
//! mirror writer uses) keyed per **tenant DB**. `aberp serve` acquires it at
//! boot and holds it for the whole process lifetime; a second `serve` (or a
//! DB-mutating CLI one-shot) that finds it held REFUSES rather than opening a
//! second concurrent writer.
//!
//! Non-blocking by design (`try_lock_exclusive`): a held lock is reported
//! immediately so the caller emits a clear "another writer is running" refusal
//! instead of stalling. Loud-fail on a genuine fs error (never silently skip the
//! lock — a skipped whole-DB lock silently re-opens the two-writer corruption
//! window).

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;

/// RAII guard holding the exclusive whole-DB writer lock for one tenant DB. The
/// lock releases when this drops (the file handle closes; flock releases on
/// close). The lock FILE is intentionally NOT deleted on drop — unlinking races
/// a peer opening it; an empty leftover file per tenant DB is negligible (same
/// posture as [`crate::submission_lock`]).
#[must_use = "dropping the guard immediately releases the whole-DB writer lock"]
pub struct DbWriterLockGuard {
    _file: std::fs::File,
    path: PathBuf,
}

impl DbWriterLockGuard {
    /// The on-disk lock-file path (for diagnostics/logging).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Derive the whole-DB writer lock-file path next to the tenant DB. One DuckDB
/// file per tenant, so a lock file in its parent dir keyed by tenant is the
/// cross-process rendezvous point. The tenant is sanitised (non
/// `[A-Za-z0-9._-]` → `_`) so an exotic tenant string can never escape the
/// directory or collide via path separators.
fn lock_path_for(db_path: &Path, tenant: &str) -> Result<PathBuf> {
    let parent = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "tenant db path `{}` has no parent dir for the whole-DB writer lock",
                db_path.display()
            )
        })?;
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    Ok(parent.join(format!(".aberp-db-writer.{}.lock", sanitize(tenant))))
}

/// Try to acquire the exclusive whole-DB writer lock for `tenant`'s DB.
///
/// - `Ok(Some(guard))` — acquired; hold the guard for the writer's lifetime.
/// - `Ok(None)` — another process holds it (another `serve` or a DB-mutating CLI
///   one-shot is running). Non-blocking: returns immediately.
/// - `Err(_)` — the lock file could not be opened (fs error). Loud-fail rather
///   than silently skipping the lock.
pub fn try_acquire(db_path: &Path, tenant: &str) -> Result<Option<DbWriterLockGuard>> {
    let path = lock_path_for(db_path, tenant)?;
    let file = OpenOptions::new()
        .create(true)
        // Pure flock handle — contents are never read/written, so do not
        // truncate an existing one (never race-clobber a peer's handle).
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open whole-DB writer lock file {}", path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(DbWriterLockGuard { _file: file, path })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(anyhow!(
            "acquire exclusive whole-DB writer lock {}: {e}",
            path.display()
        )),
    }
}

/// Acquire the whole-DB writer lock or REFUSE with a one-line, operator-facing
/// message. Used by `aberp serve` at boot and by every DB-mutating CLI
/// subcommand (a separate process with no shared `Handle`): if the lock is held,
/// a second writer must NOT open the DB.
///
/// `who` names the caller (`"aberp serve"`, `"aberp submit-invoice"`, …) so the
/// refusal is self-explanatory.
pub fn acquire_or_refuse(db_path: &Path, tenant: &str, who: &str) -> Result<DbWriterLockGuard> {
    match try_acquire(db_path, tenant)? {
        Some(guard) => Ok(guard),
        None => Err(anyhow!(
            "{who}: another ABERP writer is already running on tenant `{tenant}` \
             (aberp serve is running, or another DB-mutating command is in progress) — \
             ABERP is single-writer per tenant DB; stop the other writer and retry"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_db(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aberp-h3-dblock-{tag}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("tenant.duckdb")
    }

    /// The headline F-E invariant: only ONE whole-DB writer at a time. A second
    /// acquire while the first guard is alive is refused (models a second
    /// `aberp serve` finding the DB already locked → refuse to boot).
    #[test]
    fn only_one_whole_db_writer_at_a_time() {
        let db = scratch_db("excl");
        let first = try_acquire(&db, "prod")
            .expect("first acquire ok")
            .expect("first acquire must get the lock");
        let second = try_acquire(&db, "prod").expect("second acquire ok");
        assert!(
            second.is_none(),
            "a second whole-DB writer must NOT acquire the lock while the first holds it"
        );
        // acquire_or_refuse surfaces a clear refusal message while contended.
        match acquire_or_refuse(&db, "prod", "aberp serve") {
            Ok(_) => panic!("acquire_or_refuse must refuse while contended"),
            Err(e) => assert!(
                e.to_string().contains("single-writer"),
                "refusal message must explain the single-writer rule: {e}"
            ),
        }
        // Releasing the first lets the next acquire succeed.
        drop(first);
        let third = acquire_or_refuse(&db, "prod", "aberp serve")
            .expect("after release the lock is free again");
        drop(third);
    }

    /// Different tenants never contend (one lock per tenant DB).
    #[test]
    fn distinct_tenants_do_not_contend() {
        let db_a = scratch_db("tenant-a");
        let db_b = scratch_db("tenant-b");
        let a = try_acquire(&db_a, "tenant-a")
            .unwrap()
            .expect("tenant-a acquires");
        let b = try_acquire(&db_b, "tenant-b")
            .unwrap()
            .expect("tenant-b acquires independently");
        drop(a);
        drop(b);
    }

    /// Sanitisation keeps an exotic tenant string inside the parent dir.
    #[test]
    fn lock_path_sanitises_tenant() {
        let db = Path::new("/tmp/aberp/x/tenant.duckdb");
        let p = lock_path_for(db, "ten/../ant").unwrap();
        assert_eq!(p.parent().unwrap(), Path::new("/tmp/aberp/x"));
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(!name.contains('/'), "sanitised name must not contain '/': {name}");
    }
}
