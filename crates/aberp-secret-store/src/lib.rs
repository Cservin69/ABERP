//! `SecretStore` — the single seam over every OS-keychain access in
//! ABERP (ADR-0100 Phase 1, item 2).
//!
//! Before this crate, nine (in fact ten — see ADR-0100 §5 flag) call
//! sites across eight modules built a `keyring::Entry` directly and
//! read/wrote/deleted it. That scattering is what ADR-0100 Phase 3
//! must replace with a self-hosted secrets backend (`sops`+`age`
//! behind this same trait). So the seam lands first, in Phase 1, as a
//! **pure refactor with zero behaviour change**: every access funnels
//! through [`SecretStore`], and the only implementation is
//! [`KeychainSecretStore`], which is byte-for-byte the same
//! `keyring::Entry` dance the call sites did inline.
//!
//! # Invariants this crate holds
//!
//! - **Service/account naming is caller-owned.** This crate never
//!   composes a service or account string — the callers pass the exact
//!   `aberp.<domain>.<tenant>` service + item strings that are part of
//!   the operator's on-disk keychain contract (ADR-0007 §Secrets,
//!   ADR-0020 §3). A rename would orphan existing entries, so the
//!   strings stay where they are guarded (e.g.
//!   `nav-transport`'s `service_name`, `serve.rs`'s
//!   `keychain_service_for`).
//! - **Secrets are `Zeroizing`.** [`SecretStore::get`] returns
//!   `Zeroizing<String>`; the buffer is wiped on drop.
//! - **No secret in any error or log.** [`SecretStoreError`] carries the
//!   non-secret service + account names and the backend's diagnostic
//!   string only — never a secret value (CLAUDE.md rule 11).
//! - **Missing ≠ backend failure.** `get`/`delete` distinguish "entry
//!   absent" (`Ok(None)` / `Ok(false)`) from "backend errored"
//!   (`Err`), because several call sites mint-on-absent or soft-degrade
//!   on absent while loud-failing on a backend error (CLAUDE.md rule 12).
//!
//! # Phase-3 swap point
//!
//! [`keychain_store`] is the one place that names the concrete backend.
//! When ADR-0100 Phase 3 introduces the `sops`+`age` store, it becomes
//! a factory that consults config and returns the configured
//! `Box<dyn SecretStore>`. Under the desktop build it keeps returning
//! [`KeychainSecretStore`], so the desktop stays keychain-backed.

use zeroize::Zeroizing;

/// A backend that stores opaque string secrets keyed by
/// `(service, account)`. The only implementor today is
/// [`KeychainSecretStore`]; ADR-0100 Phase 3 adds a self-hosted one
/// behind the same trait.
///
/// Object-safe on purpose (`&dyn SecretStore` / `Box<dyn SecretStore>`)
/// so the Phase-3 factory can return the configured backend without
/// generics rippling through every call site.
pub trait SecretStore {
    /// Read the secret at `(service, account)`.
    ///
    /// - `Ok(Some(_))` — present (even if the stored value is the empty
    ///   string; callers keep their own empty-string handling).
    /// - `Ok(None)` — no such entry.
    /// - `Err(_)` — the backend itself failed (locked keychain,
    ///   permission denied, unsupported platform).
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<String>>, SecretStoreError>;

    /// Write (create or overwrite) the secret at `(service, account)`.
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError>;

    /// Delete the entry at `(service, account)`.
    ///
    /// - `Ok(true)` — an entry existed and was deleted.
    /// - `Ok(false)` — no such entry (idempotent no-op).
    /// - `Err(_)` — the backend itself failed.
    fn delete(&self, service: &str, account: &str) -> Result<bool, SecretStoreError>;
}

/// The keychain-backed [`SecretStore`] — the only implementation in
/// Phase 1. Zero-field: it holds no state, just wraps `keyring::Entry`,
/// so a caller can construct one inline exactly where it used to call
/// `keyring::Entry::new(...)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeychainSecretStore;

impl KeychainSecretStore {
    /// Construct the keychain-backed store. Cheap and stateless.
    pub const fn new() -> Self {
        KeychainSecretStore
    }
}

impl SecretStore for KeychainSecretStore {
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<String>>, SecretStoreError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| SecretStoreError::backend(service, account, &e))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(SecretStoreError::backend(service, account, &other)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| SecretStoreError::backend(service, account, &e))?;
        entry
            .set_password(secret)
            .map_err(|e| SecretStoreError::backend(service, account, &e))
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, SecretStoreError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| SecretStoreError::backend(service, account, &e))?;
        match entry.delete_password() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(other) => Err(SecretStoreError::backend(service, account, &other)),
        }
    }
}

/// The active [`SecretStore`] for this build.
///
/// Phase 1: always the keychain — every desktop and (future) `saas`
/// build reads/writes the OS keychain, unchanged. ADR-0100 Phase 3
/// turns this into the backend-selection factory (`sops`+`age` when
/// configured, keychain otherwise); every call site already goes
/// through it, so Phase 3 changes only this function's body.
pub fn keychain_store() -> KeychainSecretStore {
    KeychainSecretStore::new()
}

/// The failure surface of a [`SecretStore`]. Carries the **non-secret**
/// service + account names and the backend's diagnostic string so an
/// operator can triage — and deliberately never the secret value
/// (CLAUDE.md rule 11). "Entry absent" is NOT an error here (see
/// [`SecretStore::get`]); this fires only on a genuine backend failure.
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    /// The keychain backend itself failed (locked keychain, permission
    /// denied, unsupported platform, backend construction failure).
    #[error("secret-store backend failure for service `{service}` account `{account}`: {kind}")]
    Backend {
        service: String,
        account: String,
        /// The backend diagnostic (`keyring::Error`'s `Display`), which
        /// describes the lookup/backend failure and never contains the
        /// stored secret value.
        kind: String,
    },
}

impl SecretStoreError {
    fn backend(service: &str, account: &str, source: &keyring::Error) -> Self {
        SecretStoreError::Backend {
            service: service.to_string(),
            account: account.to_string(),
            kind: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence,
    };
    use keyring::Error as KeyringError;
    use std::collections::HashMap;
    use std::sync::{Mutex, Once, OnceLock};

    // A SHARED in-process mock keychain. keyring 2.3's built-in mock gives
    // each `Entry` its OWN storage, so a `set` on one `Entry::new` is
    // invisible to a `get` on a freshly-built `Entry` for the same
    // (service, account) — which is exactly how `KeychainSecretStore`
    // operates (fresh `Entry` per call). We therefore back the mock with
    // one shared map, mirroring `apps/aberp/tests/secrets_cache_boot.rs`.
    fn shared_store() -> &'static Mutex<HashMap<(String, String), String>> {
        static STORE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
        STORE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    #[derive(Debug)]
    struct MockCredential {
        service: String,
        account: String,
    }

    impl CredentialApi for MockCredential {
        fn set_password(&self, password: &str) -> keyring::Result<()> {
            shared_store().lock().expect("store poisoned").insert(
                (self.service.clone(), self.account.clone()),
                password.to_string(),
            );
            Ok(())
        }

        fn get_password(&self) -> keyring::Result<String> {
            match shared_store()
                .lock()
                .expect("store poisoned")
                .get(&(self.service.clone(), self.account.clone()))
            {
                Some(p) => Ok(p.clone()),
                None => Err(KeyringError::NoEntry),
            }
        }

        fn delete_password(&self) -> keyring::Result<()> {
            let mut store = shared_store().lock().expect("store poisoned");
            if store
                .remove(&(self.service.clone(), self.account.clone()))
                .is_some()
            {
                Ok(())
            } else {
                Err(KeyringError::NoEntry)
            }
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[derive(Debug)]
    struct MockBuilder;

    impl CredentialBuilderApi for MockBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(MockCredential {
                service: service.to_string(),
                account: user.to_string(),
            }))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }

    fn mock_store() -> KeychainSecretStore {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            keyring::set_default_credential_builder(Box::new(MockBuilder));
        });
        KeychainSecretStore::new()
    }

    /// Round-trips get/set/delete across the exact `aberp.<domain>.<tenant>`
    /// service + item names of every secret category ADR-0100 §1.1
    /// enumerates. Doubles as a naming-convention pin: the strings here
    /// mirror the on-disk operator contract.
    #[test]
    fn round_trips_every_secret_category() {
        let store = mock_store();
        // (service, account) per category. Tenant "t1" stands in for the
        // real per-tenant suffix.
        let categories = [
            ("aberp.nav.t1", "nav_credentials_blob"),
            ("aberp.nav.t1", "session_token"),
            ("aberp.smtp.t1", "smtp_password"),
            ("aberp.cad.t1", "cad_blob_key"),
            ("aberp.audit_service.t1", "audit_service_signing_key"),
            ("aberp.storefront.t1", "storefront_origin_secret"),
            ("aberp.email_relay.t1", "email_relay_token"),
            ("aberp.quote_intake.t1", "quote_intake_token"),
        ];

        for (service, account) in categories {
            // Absent → Ok(None), delete of absent → Ok(false).
            assert!(
                store.get(service, account).unwrap().is_none(),
                "{service}/{account} should start absent"
            );
            assert!(
                !store.delete(service, account).unwrap(),
                "{service}/{account} delete-when-absent should be Ok(false)"
            );

            // Set then get returns the exact value, Zeroizing-wrapped.
            let secret = format!("secret-for-{account}");
            store.set(service, account, &secret).unwrap();
            let got = store.get(service, account).unwrap();
            assert_eq!(
                got.as_deref().map(|z| z.as_str()),
                Some(secret.as_str()),
                "{service}/{account} round-trip mismatch"
            );

            // Overwrite is honoured.
            store.set(service, account, "rotated").unwrap();
            assert_eq!(
                store
                    .get(service, account)
                    .unwrap()
                    .as_deref()
                    .map(|z| z.as_str()),
                Some("rotated")
            );

            // Delete of present → Ok(true), then absent again.
            assert!(store.delete(service, account).unwrap());
            assert!(store.get(service, account).unwrap().is_none());
        }
    }

    /// An empty stored value is "present", not "absent" — callers own the
    /// empty-string policy (e.g. serve.rs treats an empty session token
    /// as re-mintable).
    #[test]
    fn empty_value_is_present_not_absent() {
        let store = mock_store();
        store.set("aberp.nav.empt", "session_token", "").unwrap();
        let got = store.get("aberp.nav.empt", "session_token").unwrap();
        assert_eq!(got.as_deref().map(|z| z.as_str()), Some(""));
    }

    /// The error surface never contains a secret value — only the
    /// non-secret service/account and the backend diagnostic.
    #[test]
    fn error_display_carries_no_secret() {
        let err = SecretStoreError::Backend {
            service: "aberp.nav.t1".to_string(),
            account: "session_token".to_string(),
            kind: "No matching entry found in secure storage".to_string(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("aberp.nav.t1"));
        assert!(rendered.contains("session_token"));
        assert!(!rendered.contains("secret-for"));
    }
}
