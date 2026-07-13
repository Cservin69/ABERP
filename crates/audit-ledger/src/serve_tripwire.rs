//! `SERVE_HANDLE_LIVE` runtime tripwire — ADR-0099 H3, Addendum 3.
//!
//! When `aberp serve` holds the ONE shared `aberp_db::Handle` on a tenant DB,
//! ANY independent live-DB open of that SAME file (a fresh `Connection` / `Ledger`
//! / `DuckDbBillingStore`) is an audit-ledger fork: it self-assigns sequence
//! numbers off a head the Handle's WAL-resident writes are invisible to (coherence
//! model Q2, `apps/aberp/tests/h3_handle_coherence_model.rs`). Two writers off the
//! same head both claim the next seq and tear the ledger.
//!
//! The static gates cannot fully close this. CHECK M/N are file-scoped; the
//! write/read-fork allow-lists exempt separate-process CLI one-shots by *fn name*.
//! But `issue_storno` / `issue_modification` / `poll_ack` / `submit_invoice` (and
//! the `pre_tx_setup` / `*_from_inputs` helpers they share) are DUAL-CONTEXT — the
//! same fn runs BOTH as a flock-fenced CLI one-shot AND in-serve. A per-fn
//! allow-list cannot tell "reached via the flocked CLI `run`" from "reached via the
//! in-serve handler"; a fenced CLI fn newly wired into serve would slip the gate
//! (ADR-0099 §"CHECK N residual STATIC LIMITATION"). This tripwire fires on the
//! OPEN itself — regardless of which fn or crate did it — so no static scoping is
//! required.
//!
//! DEBUG/TEST ONLY, exactly like the writer-mutex re-entrancy tripwire (ADR-0099
//! §re-entrancy tripwire, commit `ad72022`): [`assert_no_serve_handle`] panics
//! under `debug_assertions` (which every `cargo test` build sets) and is a
//! zero-cost no-op in release. The whole test suite becomes the fork trace.
//!
//! ARMING. The check is always compiled in debug/test, but it only fires once a
//! serve Handle is REGISTERED for a path via [`register_serve_handle`]. serve
//! registers at boot behind the `ABERP_SERVE_HANDLE_TRIPWIRE` env arm, which is
//! OFF by default so the in-flight not-yet-migrated in-serve forks (the 24 the
//! write/read-fork gates still list) do not trip it mid-migration. It is flipped
//! ON as the FINAL step of the invoice-family migration — the same "arm at zero"
//! posture as `ENFORCE_WRITE_FORK=1`. Tests arm it directly by calling
//! [`register_serve_handle`], so the mechanism is proven regardless of the arm.

use std::path::Path;

/// Guard returned by [`register_serve_handle`]. While it is alive, its tenant-DB
/// path is registered as serve-live; dropping it (serve shutdown / test end)
/// deregisters. Refcounted, so nested registrations of the same path are safe.
#[derive(Debug)]
pub struct ServeHandleGuard {
    // Populated only in debug/test builds; `None` in release (the tripwire is a
    // no-op there, so there is nothing to deregister on drop).
    #[allow(dead_code)]
    key: Option<std::path::PathBuf>,
}

impl Drop for ServeHandleGuard {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        if let Some(key) = self.key.take() {
            imp::deregister(&key);
        }
    }
}

/// Register that a serve `Handle` is live on `path`. Hold the returned guard for
/// the Handle's lifetime; drop deregisters. No-op in release.
pub fn register_serve_handle(path: impl AsRef<Path>) -> ServeHandleGuard {
    #[cfg(debug_assertions)]
    let key = Some(imp::register(path.as_ref()));
    #[cfg(not(debug_assertions))]
    let key = {
        let _ = path;
        None
    };
    ServeHandleGuard { key }
}

/// Fail loudly (debug/test) if `path` currently has a live serve `Handle` — i.e.
/// an independent opener (`opener`, e.g. `"Ledger::open"`) is forking a DB the
/// shared Handle owns. No-op in release, and a no-op when nothing is registered.
pub fn assert_no_serve_handle(path: impl AsRef<Path>, opener: &str) {
    #[cfg(debug_assertions)]
    imp::assert_absent(path.as_ref(), opener);
    #[cfg(not(debug_assertions))]
    let _ = (path, opener);
}

/// Test/introspection helper: is a serve Handle currently registered on `path`?
pub fn is_serve_handle_live(path: impl AsRef<Path>) -> bool {
    #[cfg(debug_assertions)]
    let live = imp::is_live(path.as_ref());
    #[cfg(not(debug_assertions))]
    let live = {
        let _ = path;
        false
    };
    live
}

#[cfg(debug_assertions)]
mod imp {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn registry() -> &'static Mutex<HashMap<PathBuf, usize>> {
        static R: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
        R.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Normalise so a register and a later open of the "same" file compare equal
    /// even across `./` / symlink spellings. `canonicalize` needs the file to
    /// exist (it does — serve opened it); fall back to the raw path otherwise.
    fn norm(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    pub(super) fn register(path: &Path) -> PathBuf {
        let key = norm(path);
        let mut g = registry().lock().unwrap_or_else(|e| e.into_inner());
        *g.entry(key.clone()).or_insert(0) += 1;
        key
    }

    pub(super) fn deregister(key: &Path) {
        let mut g = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.get_mut(key) {
            *c -= 1;
            if *c == 0 {
                g.remove(key);
            }
        }
    }

    pub(super) fn is_live(path: &Path) -> bool {
        let key = norm(path);
        let g = registry().lock().unwrap_or_else(|e| e.into_inner());
        g.get(&key).is_some_and(|c| *c > 0)
    }

    pub(super) fn assert_absent(path: &Path, opener: &str) {
        if is_live(path) {
            panic!(
                "SERVE_HANDLE_LIVE tripwire: `{opener}` opened an INDEPENDENT connection to \
                 {} while this process holds the shared serve `aberp_db::Handle` on that tenant \
                 DB. That forks the audit ledger — the fresh open self-assigns seqs off a head \
                 the Handle's WAL-resident writes are invisible to (coherence model Q2), tearing \
                 the ledger for the next reader. Route this DB access through the shared Handle \
                 (`db.write()` / `db.read()`), not a fresh open. \
                 (ADR-0099 H3 Addendum 3 §SERVE_HANDLE_LIVE tripwire.)",
                path.display()
            );
        }
    }
}
