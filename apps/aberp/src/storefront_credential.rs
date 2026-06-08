//! S289 / PR-270 — shared, hot-reloadable storefront credential
//! ([`[quote_intake]`](crate::quote_intake_config) `base_url` + keychain
//! bearer).
//!
//! ## Why this exists
//!
//! Two daemons push/pull against the same storefront surface (design
//! doc §4 / §14-C, SPOC posture from [[aberp-smtp-spoc]]):
//! - [`crate::catalogue_push`] PUTs `quoting_materials` to
//!   `{base_url}/api/catalogue/materials`;
//! - the `aberp-quote-intake` crate GETs / writebacks against
//!   `{base_url}/api/quotes`.
//!
//! Pre-S289 both daemons cached `base_url` + bearer at boot. Ervin's
//! 2026-06-08 test (PR-269 / PROD_v2.27.4) flipped the SPA → Maintenance
//! → Quote Intake → Base URL from `https://abenerp.com` to
//! `http://localhost:5173`; quote-intake picked up the new URL after an
//! ABERP restart but the catalogue-push daemon — same SPOC source —
//! kept hitting prod in subsequent cycles, exposing the
//! [[trust-code-not-operator]] gap: cached-at-boot is fragile when the
//! operator can change the URL via the SPA.
//!
//! ## What this gives
//!
//! A single `Arc<StorefrontCredentialHandle>` lives in
//! [`crate::serve::AppState`]:
//! - the boot block resolves the credential ONCE (env-var override or
//!   seller.toml + keychain) and calls [`set`](StorefrontCredentialHandle::set);
//! - the PUT `/api/quote-intake/config` route calls `set` again after a
//!   successful seller.toml + keychain write — operator changes the URL,
//!   the next daemon cycle sees the new value, NO restart needed (brief
//!   C);
//! - the catalogue-push daemon calls
//!   [`snapshot`](StorefrontCredentialHandle::snapshot) at the top of
//!   every push cycle and uses the returned URL + bearer for that
//!   request only.
//!
//! ## SPOC surgical scope
//!
//! The brief offered two paths (consolidate vs split). We consolidate:
//! same handle covers both daemons. Per CLAUDE.md "surgical changes",
//! the `aberp-quote-intake` crate stays unmodified in this PR — its
//! daemon remains restart-required for URL changes (matching the
//! observed behaviour Ervin saw). Hot-reload is added to the daemon
//! that was actually observed to drift (catalogue-push). If a later
//! session needs quote-intake to also hot-reload, it can take a
//! [`StorefrontCredentialHandle`] arc the same way.

use std::sync::{Arc, RwLock};

use zeroize::Zeroizing;

/// One operator-edit-visible snapshot of the storefront credential.
/// `None` snapshots ⇒ the storefront isn't configured this boot (env
/// vars unset AND seller.toml `[quote_intake]` disabled/missing); a
/// daemon that reads `None` should skip its cycle gracefully.
#[derive(Clone)]
pub struct StorefrontCredentialSnapshot {
    /// No trailing slash (the resolver normalises it).
    pub base_url: String,
    /// Bearer token. Wrapped in `Zeroizing` so the clone is wiped on drop.
    pub bearer: Zeroizing<String>,
}

impl std::fmt::Debug for StorefrontCredentialSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorefrontCredentialSnapshot")
            .field("base_url", &self.base_url)
            .field("bearer", &"<redacted>")
            .finish()
    }
}

/// Shared, hot-reloadable storefront credential. Cheap to clone (it's an
/// `Arc<RwLock<…>>` under the hood).
pub struct StorefrontCredentialHandle {
    inner: RwLock<Option<StorefrontCredentialSnapshot>>,
}

impl std::fmt::Debug for StorefrontCredentialHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read().ok();
        f.debug_struct("StorefrontCredentialHandle")
            .field("configured", &inner.as_ref().map(|g| g.is_some()))
            .finish()
    }
}

impl StorefrontCredentialHandle {
    /// A dormant handle — no credential resolved yet. Stored in `AppState`
    /// at construction so every test path sees a live (if dormant) handle.
    pub fn dormant() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(None),
        })
    }

    /// Replace the current snapshot. Called once at boot after the
    /// resolve, and again after every successful PUT
    /// `/api/quote-intake/config`. Setting to a fresh `Some` is how
    /// operator-visible URL/bearer changes propagate to the daemons
    /// without a restart.
    pub fn set(&self, base_url: String, bearer: Zeroizing<String>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = Some(StorefrontCredentialSnapshot { base_url, bearer });
        }
    }

    /// Wipe the current snapshot — operator set `enabled=false` or
    /// disabled the storefront. The daemons will skip their next cycle
    /// gracefully.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = None;
        }
    }

    /// Current snapshot, cheaply cloned. `None` ⇒ the storefront isn't
    /// configured this boot.
    pub fn snapshot(&self) -> Option<StorefrontCredentialSnapshot> {
        self.inner.read().ok().and_then(|g| g.clone())
    }

    /// `true` iff a snapshot is present. Cheap; used by the
    /// boot-time dev-mode warning + the catalogue-push daemon's
    /// short-circuit when dormant.
    pub fn is_configured(&self) -> bool {
        self.inner.read().ok().is_some_and(|g| g.is_some())
    }
}

/// Production host the dev-mode warning compares against (brief B).
/// Public so the test pin in `serve.rs` can reference it without a
/// magic string.
pub const PROD_STOREFRONT_HOST: &str = "https://abenerp.com";

/// S291 / PR-272 — env-var override name for the sister-service base
/// URL. `dev-test.sh` sets this so a local-loopback dev session uses
/// `http://localhost:5173` regardless of the seller.toml / SPA value.
/// Surgical scope: env wins at boot only; subsequent SPA edits still
/// take effect via `StorefrontCredentialHandle::set` — that matches the
/// existing hot-reload posture for `/api/quote-intake/config`.
pub const SISTER_SERVICE_BASE_URL_ENV: &str = "ABERP_SISTER_SERVICE_BASE_URL";

/// Resolve the sister-service base-URL override from the environment.
/// Returns `None` if unset, empty, or whitespace-only. The trimmed
/// value is returned with no further validation — bad URLs surface at
/// the daemon's HTTP call, matching how toml + SPA-saved URLs are
/// treated today (one validation path, not two).
pub fn read_sister_service_base_url_override() -> Option<String> {
    std::env::var(SISTER_SERVICE_BASE_URL_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// One-shot fail-loud warning, fired AT BOOT only. When
/// `ABERP_DEV_MODE=1` (or `=true`) is set but the configured
/// `base_url` points at production, log a single WARN telling the
/// operator they probably want to flip the URL in Settings → Quote
/// Intake. Per brief pushback 3+4: the only trigger is the env var —
/// no inference from random URL patterns, and an operator legitimately
/// pushing to prod (no dev-mode env) never sees the warning.
///
/// Returns `true` iff the warning fired (test hook).
pub fn emit_dev_mode_prod_url_warning(base_url: &str) -> bool {
    if !dev_mode_enabled() {
        return false;
    }
    let normalised = base_url.trim().trim_end_matches('/');
    // Match the exact host — not `abenerp.com.malicious.example`. We
    // already stripped trailing `/`, so the host is the prefix; anything
    // following must be a path delimiter.
    let after_host = match normalised.strip_prefix(PROD_STOREFRONT_HOST) {
        None => return false,
        Some(rest) => rest,
    };
    if !after_host.is_empty() && !after_host.starts_with('/') {
        return false;
    }
    tracing::warn!(
        base_url = %normalised,
        "ABERP_DEV_MODE=1 but the sister-service Base URL points at \
         production (`{PROD_STOREFRONT_HOST}`). If you're testing \
         locally, change the URL in Settings → Quote Intake (the \
         quote-intake AND catalogue-push daemons both read this one \
         setting). If you really mean to push to prod, unset \
         ABERP_DEV_MODE to silence this warning."
    );
    true
}

fn dev_mode_enabled() -> bool {
    std::env::var("ABERP_DEV_MODE")
        .ok()
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_env() {
        std::env::remove_var("ABERP_DEV_MODE");
        std::env::remove_var(SISTER_SERVICE_BASE_URL_ENV);
    }

    #[test]
    fn dormant_handle_has_no_snapshot() {
        let h = StorefrontCredentialHandle::dormant();
        assert!(h.snapshot().is_none());
        assert!(!h.is_configured());
    }

    #[test]
    fn set_then_snapshot_returns_clone_with_url_and_bearer() {
        let h = StorefrontCredentialHandle::dormant();
        h.set(
            "http://localhost:5173".to_string(),
            Zeroizing::new("secret-A".to_string()),
        );
        let snap = h.snapshot().expect("snapshot");
        assert_eq!(snap.base_url, "http://localhost:5173");
        assert_eq!(&*snap.bearer, "secret-A");
        assert!(h.is_configured());
    }

    #[test]
    fn set_replaces_previous_snapshot() {
        let h = StorefrontCredentialHandle::dormant();
        h.set("https://abenerp.com".into(), Zeroizing::new("old".into()));
        h.set("http://localhost:5173".into(), Zeroizing::new("new".into()));
        let snap = h.snapshot().expect("snapshot");
        assert_eq!(snap.base_url, "http://localhost:5173");
        assert_eq!(&*snap.bearer, "new");
    }

    #[test]
    fn clear_wipes_snapshot() {
        let h = StorefrontCredentialHandle::dormant();
        h.set("https://x".into(), Zeroizing::new("t".into()));
        assert!(h.is_configured());
        h.clear();
        assert!(!h.is_configured());
        assert!(h.snapshot().is_none());
    }

    #[test]
    fn debug_does_not_leak_bearer() {
        let snap = StorefrontCredentialSnapshot {
            base_url: "https://x".into(),
            bearer: Zeroizing::new("S3CRET-LEAK".into()),
        };
        let s = format!("{snap:?}");
        assert!(!s.contains("S3CRET-LEAK"), "{s}");
        assert!(s.contains("<redacted>"), "{s}");
    }

    #[test]
    fn debug_handle_shows_configured_flag() {
        let h = StorefrontCredentialHandle::dormant();
        let s = format!("{h:?}");
        assert!(s.contains("Some(false)"), "{s}");
        h.set("https://x".into(), Zeroizing::new("t".into()));
        let s2 = format!("{h:?}");
        assert!(s2.contains("Some(true)"), "{s2}");
    }

    #[test]
    fn dev_mode_warning_silent_when_env_unset() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        assert!(!emit_dev_mode_prod_url_warning(PROD_STOREFRONT_HOST));
    }

    #[test]
    fn dev_mode_warning_silent_for_localhost_under_dev_mode() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var("ABERP_DEV_MODE", "1");
        assert!(!emit_dev_mode_prod_url_warning("http://localhost:5173"));
        clear_env();
    }

    #[test]
    fn dev_mode_warning_fires_for_prod_url_under_dev_mode() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var("ABERP_DEV_MODE", "true");
        assert!(emit_dev_mode_prod_url_warning(PROD_STOREFRONT_HOST));
        // Trailing slash normalised away.
        assert!(emit_dev_mode_prod_url_warning("https://abenerp.com/"));
        clear_env();
    }

    #[test]
    fn dev_mode_warning_silent_when_dev_mode_false_string() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var("ABERP_DEV_MODE", "false");
        assert!(!emit_dev_mode_prod_url_warning(PROD_STOREFRONT_HOST));
        clear_env();
    }

    #[test]
    fn sister_service_override_returns_none_when_env_unset() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        assert_eq!(read_sister_service_base_url_override(), None);
    }

    #[test]
    fn sister_service_override_returns_trimmed_value() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var(SISTER_SERVICE_BASE_URL_ENV, "  http://localhost:5173  ");
        assert_eq!(
            read_sister_service_base_url_override(),
            Some("http://localhost:5173".to_string())
        );
        clear_env();
    }

    #[test]
    fn sister_service_override_treats_blank_as_unset() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var(SISTER_SERVICE_BASE_URL_ENV, "   ");
        assert_eq!(read_sister_service_base_url_override(), None);
        clear_env();
    }

    #[test]
    fn dev_mode_warning_silent_for_subdomain_of_prod() {
        let _g = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        std::env::set_var("ABERP_DEV_MODE", "1");
        // `abenerp.com.malicious.example` would NOT be the prod host —
        // we starts_with the full scheme+host so a near-match URL does
        // not trip the warning.
        assert!(!emit_dev_mode_prod_url_warning(
            "https://abenerp.com.malicious.example"
        ));
        clear_env();
    }
}
