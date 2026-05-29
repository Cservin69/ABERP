//! Session-149 — in-process secrets cache so the OS keychain is read
//! ONLY during the boot prompt-burst, never lazily mid-session.
//!
//! # The problem this fixes
//!
//! The SMTP password keychain item (`aberp.smtp.<tenant>`) was read
//! lazily — first when the operator opened Maintenance → Tenants (the
//! `GET /api/smtp-config` handler probed `password_is_set`), and again
//! on the first email send / test-connection. On a freshly-rebuilt
//! binary the macOS keychain ACL resets, so that lazy read popped an
//! access-prompt at a *surprising* time, well AFTER boot. The NAV
//! credentials blob + session token ARE read at boot and get
//! Always-Allow'd in one contiguous burst; the SMTP password item was
//! not, so it never joined the burst and re-prompted later.
//!
//! # The fix
//!
//! Read the SMTP password at boot (iff `[seller.smtp]` is present),
//! stash it here behind an `Arc<RwLock<…>>`, and route every post-boot
//! consumer (Settings GET/PUT, email send, test connection) through
//! this cache. After boot NO `security-framework` READ API is hit for
//! SMTP — the cache answers everything.
//!
//! Operator-initiated WRITES (rotate the password via Settings PUT,
//! Test Connection) still touch the keychain at the moment of the
//! click — that's deliberate operator action, not a lazy background
//! read. After such a write the caller refreshes this cache via
//! [`SecretsCache::refresh_smtp_password_after_write`] so later reads
//! stay cache-served.
//!
//! # Scope note — NAV credentials are deliberately NOT cached here
//!
//! A159 mandates that `NavCredentials` are loaded fresh per request and
//! never stashed on `AppState`; only the non-secret `operator_login`
//! lives on the boot state. Those per-request reads are SILENT
//! post-boot because the NAV blob item is Always-Allow'd in the boot
//! burst, so they never produced the surprise-prompt symptom this
//! session targets. Caching NAV creds would reverse A159 — out of
//! scope here; see the session-149 report heads-up.
//!
//! # Security
//!
//! - The cached password is wrapped in `Zeroizing<String>` (its buffer
//!   is overwritten on drop).
//! - In-memory ONLY — never written to disk, never logged. No `Debug`
//!   that could leak the secret.

use std::path::Path;
use std::sync::{Arc, RwLock};

use zeroize::Zeroizing;

use crate::smtp_config;
use crate::smtp_credentials::{self, SmtpCredentialsError};

/// In-process secrets cache, populated once during the boot burst.
/// Cheap to clone (the inner slot is `Arc`-shared) so it rides on the
/// cloned [`crate::serve::AppState`] handed to every request handler.
#[derive(Clone)]
pub struct SecretsCache {
    /// `Some(pw)` iff an SMTP password was present in the keychain at
    /// boot (or has since been rotated via the Settings PUT). `None`
    /// means no SMTP password is configured for this tenant.
    smtp_password: Arc<RwLock<Option<Zeroizing<String>>>>,
}

impl SecretsCache {
    /// Read the SMTP password from the OS keychain at boot IFF the
    /// `[seller.smtp]` section is present in `seller_toml_path`. This
    /// is the ONLY SMTP keychain READ in the process lifetime; it runs
    /// inside the boot prompt-burst alongside the NAV blob +
    /// session-token reads so the OS consolidates the Always-Allow.
    ///
    /// Never hard-fails boot. CLAUDE.md rule 12 (fail loud) is honoured
    /// by the `tracing::warn!` emits — a missing or locked keychain
    /// leaves the slot `None`, and the first email send then surfaces a
    /// typed `SmtpPasswordMissing` to the operator rather than crashing
    /// the app at startup.
    pub fn init_at_boot(tenant: &str, seller_toml_path: &Path) -> Self {
        let smtp_configured =
            matches!(smtp_config::read_smtp_config(seller_toml_path), Ok(Some(_)));
        let smtp_password = if smtp_configured {
            match smtp_credentials::read_password(tenant) {
                Ok(pw) => Some(pw),
                Err(SmtpCredentialsError::Missing { .. }) => {
                    tracing::warn!(
                        "[seller.smtp] is configured but no SMTP password is set in the OS \
                         keychain — leaving the cache slot empty; email send will surface \
                         SmtpPasswordMissing until the operator sets one via Maintenance → Tenants"
                    );
                    None
                }
                Err(SmtpCredentialsError::Backend(msg)) => {
                    tracing::warn!(
                        error = %msg,
                        "OS keychain backend error reading the SMTP password at boot — leaving \
                         the cache slot empty; NOT hard-failing boot (CLAUDE.md rule 12: loud, \
                         not fatal — the operator can re-set the password from Settings)"
                    );
                    None
                }
            }
        } else {
            // No `[seller.smtp]` section → operator has not configured
            // SMTP. Skip the keychain read entirely (no prompt), leave
            // the slot empty. The first Settings PUT is the operator's
            // first keychain interaction for SMTP — expected, not a
            // regression.
            None
        };
        Self {
            smtp_password: Arc::new(RwLock::new(smtp_password)),
        }
    }

    /// An empty cache (no secrets). Used by the boot fallback when the
    /// seller.toml path cannot be resolved, and by route-layer
    /// integration tests that construct an `AppState` without touching
    /// the keychain.
    pub fn empty() -> Self {
        Self {
            smtp_password: Arc::new(RwLock::new(None)),
        }
    }

    /// The cached SMTP password, cloned. `None` iff no SMTP password is
    /// configured for this tenant. Reading this NEVER touches the OS
    /// keychain.
    pub fn smtp_password(&self) -> Option<Zeroizing<String>> {
        self.smtp_password
            .read()
            .expect("SecretsCache.smtp_password RwLock poisoned")
            .clone()
    }

    /// Whether an SMTP password is cached. Surfaced to the Settings
    /// page's `passwordSet` flag WITHOUT materialising the secret and
    /// WITHOUT a keychain read.
    pub fn is_smtp_password_set(&self) -> bool {
        self.smtp_password
            .read()
            .expect("SecretsCache.smtp_password RwLock poisoned")
            .is_some()
    }

    /// Update the cached SMTP password after the operator rotates it
    /// via the Settings PUT (which writes the keychain at click-time).
    /// Subsequent reads see the new value from cache — no re-read of
    /// the keychain.
    pub fn refresh_smtp_password_after_write(&self, new: Zeroizing<String>) {
        *self
            .smtp_password
            .write()
            .expect("SecretsCache.smtp_password RwLock poisoned") = Some(new);
    }
}

// The load-bearing "no keychain read after boot" test (session-149
// Part E) lives in `tests/secrets_cache_boot.rs` as an isolated
// integration-test binary. It installs a process-global mock keychain
// builder, which would race other lib unit tests if placed here — so
// it gets its own process, matching the repo convention for keychain
// tests (serve_settings_routes.rs, serve_setup_nav_credentials_route.rs).
