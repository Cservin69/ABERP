//! **ADR-0110 D3 — does `durable_ack` actually reach the filesystem?**
//!
//! # The hole this closes
//!
//! Every other D3 pin observes the durable-ack through its *journal*: the D6b
//! power-loss spec derives its durable set from `Handle::fsynced_paths`, and
//! `tools/cut_gate_durable_ack.sh` checks the call sites statically. Both are
//! blind to the same mutation — **record the path in the journal but never
//! `sync_all` it**:
//!
//! ```ignore
//! fn fsync_and_record(&self, path: &Path) -> Result<(), DbError> {
//!     // fsync_path(path)?;          <-- deleted
//!     self.synced.lock()...push(path.to_path_buf());
//!     Ok(())
//! }
//! ```
//!
//! Against that tree the four D6b durability tests **pass**, the cut-gate
//! **passes**, and `clippy -D warnings` is **clean** — a silent revert to the
//! 2026-08-08 loss with every gate green. The journal is only as truthful as
//! the code that writes it, and nothing was checking that.
//!
//! # What this file does instead
//!
//! It stops trusting the journal and makes the filesystem answer. Break the
//! reach — delete the main DB file out from under an open [`Handle`] — and
//! require [`Handle::durable_ack`] to **fail**. A `durable_ack` that does not
//! touch the filesystem cannot notice, so it returns `Ok` and this goes RED.
//!
//! That makes the assertion do double duty, which is why it is worth its
//! twenty lines:
//!
//! 1. **The reach is real.** Only code that actually opens and `sync_all`s the
//!    path can observe the failure.
//! 2. **The failure surfaces.** ADR-0110 R3 / CLAUDE.md rule 11: "we could not
//!    make the acked write durable" is precisely the fact that must not be
//!    swallowed. A `durable_ack` that logged and returned `Ok` would also fail
//!    here.
//!
//! # Mutation verification
//!
//! A pin that cannot go red is not a pin. Verified in both directions before
//! landing: delete the `fsync_path(path)?` line from `fsync_and_record` and
//! this file goes RED while every other D3 gate stays green — which is the
//! whole point of it existing.
//!
//! # Scope
//!
//! `$TMPDIR` only. Nothing here touches `~/.aberp/**` or any tenant database.

use std::path::PathBuf;

use aberp_audit_ledger::TenantId;
use aberp_db::{DbError, Handle};

/// A scratch tenant directory under `$TMPDIR`, unique per process + call.
fn scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0110-d3-fault-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).expect("mkdir scratch tenant dir");
    dir
}

fn tenant() -> TenantId {
    TenantId::new("tenant-adr0110-d3-fault").expect("test tenant id is valid")
}

/// **The reach test.** With the main DB file removed from under the handle,
/// `durable_ack` must return `Err` — it cannot `fsync` a path that is not
/// there, and it must say so rather than claim success.
///
/// Deleting the *path* (not the inode) is what makes this hermetic: the
/// `Handle`'s already-open `Connection` keeps its file descriptor and stays
/// perfectly usable, so nothing else in the process is disturbed and the test
/// has no cleanup hazard. Only a fresh `File::open` of the path — which is
/// exactly what the durable-ack reach does — sees the `ENOENT`.
#[test]
fn durable_ack_fails_loud_when_the_filesystem_reach_is_broken() {
    let dir = scratch_dir("reach");
    let db = dir.join("aberp.duckdb");
    let handle = Handle::open_default(&db, tenant()).expect("open shared Handle");

    // Sanity: on an intact tenant the ack succeeds and journals the main file.
    // Without this the assertion below could pass on a `durable_ack` that is
    // broken for some entirely different reason.
    handle
        .durable_ack()
        .expect("durable_ack must succeed on an intact tenant");
    assert!(
        handle.fsynced_paths().iter().any(|p| p == &db),
        "precondition: an intact durable_ack must journal the main DB file; \
         journal was {:?}",
        handle.fsynced_paths(),
    );

    // ── Break the reach ────────────────────────────────────────────────────
    std::fs::remove_file(&db).expect("remove the main DB file out from under the handle");

    let err = handle.durable_ack().expect_err(
        "ADR-0110 D3 REGRESSION: durable_ack returned Ok with the main DB file \
         DELETED. It therefore never opened or fsync'd it, so the durability \
         journal is recording syncs that are not happening — every other D3 gate \
         (the D6b power-loss tiers, the cut-gate) reads that journal and would \
         stay green through a total revert to the 2026-08-08 loss.",
    );

    // The error must be the typed durability failure naming the path, not some
    // incidental DuckDB error: a money path is going to turn this into an
    // operator-facing 5xx, so it has to say WHICH file could not be made
    // durable (R3 — never silently, and never uselessly).
    match err {
        DbError::DurableAck { ref path, .. } => {
            assert_eq!(path, &db, "DurableAck must name the file it could not sync")
        }
        other => panic!(
            "expected DbError::DurableAck naming {}, got {other:?}",
            db.display()
        ),
    }
    assert!(
        err.to_string().contains("durable-ack"),
        "the error text must be greppable as a durability fault; got: {err}"
    );
}
