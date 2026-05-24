//! Per-tenant NAV credentials per ADR-0009 §4 + ADR-0020 §3.
//!
//! The four artifacts NAV's application-level authentication actually
//! requires (no client X.509 — that posture is corrected by ADR-0020):
//!
//!   `nav.technical_user.login`      — the technical-user identifier
//!   `nav.technical_user.password`   — plaintext at rest; SHA-512-hashed
//!                                     per request; zeroized on drop
//!   `nav.xml_sign_key`              — input to the SHA3-512 requestSignature
//!   `nav.xml_change_key`            — AES-128/ECB key for exchangeToken
//!                                     decryption (per ADR-0021 §A9 the
//!                                     ECB use is protocol-imposed by NAV
//!                                     and must not generalize)
//!
//! All four are required. Partial loading is refused per CLAUDE.md
//! rule 12 — a half-populated keychain produces a hard error, not a
//! "submit and hope" silent path.
//!
//! In-memory protection: every secret is wrapped in `Zeroizing<String>`
//! (zeroize crate, ADR-0007 §Secrets) so the buffer is overwritten when
//! the `NavCredentials` is dropped. The `Debug` impl is hand-written to
//! redact the secrets so accidental `tracing` or `dbg!` output does not
//! leak them. None of the secret getters returns `&str` directly — they
//! return `&[u8]` (or `&Zeroizing<String>`) so the caller's clone path
//! is visible.

use zeroize::Zeroizing;

use crate::error::NavTransportError;

pub mod keychain;

/// All four NAV credential artifacts for a single tenant.
///
/// Constructed via [`NavCredentials::load_from_keychain`] in production,
/// or via [`NavCredentials::from_parts`] in tests. There is no `Default`
/// — a default-constructed credential set is exactly the silent-fallback
/// failure mode the project refuses.
pub struct NavCredentials {
    tenant_id: String,
    login: Zeroizing<String>,
    password: Zeroizing<String>,
    sign_key: Zeroizing<String>,
    change_key: Zeroizing<String>,
}

impl NavCredentials {
    /// Load all four artifacts from the OS keychain for the named tenant.
    /// Returns the typed error if any artifact is missing (loud per
    /// CLAUDE.md rule 12) or if the keychain backend itself errors.
    ///
    /// The keychain item naming convention (service =
    /// `aberp.nav.<tenant_id>`, item = `technical_user.login` etc.)
    /// is documented in [`keychain::item_path`]. An operator can list
    /// what's populated with the platform's native keychain tool
    /// (macOS: `security find-generic-password -s "aberp.nav.<id>"`).
    pub fn load_from_keychain(tenant_id: &str) -> Result<Self, NavTransportError> {
        let login = keychain::read_secret(tenant_id, keychain::ITEM_LOGIN)?;
        let password = keychain::read_secret(tenant_id, keychain::ITEM_PASSWORD)?;
        let sign_key = keychain::read_secret(tenant_id, keychain::ITEM_SIGN_KEY)?;
        let change_key = keychain::read_secret(tenant_id, keychain::ITEM_CHANGE_KEY)?;
        Ok(Self {
            tenant_id: tenant_id.to_string(),
            login,
            password,
            sign_key,
            change_key,
        })
    }

    /// Tenant identifier the credentials were loaded for. Useful for
    /// `tracing` spans + Actor derivation (ADR-0008 §"Entry shape").
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Technical-user login. Read-only; never logged. Returned as `&str`
    /// only because the login itself is not a secret (it's the operator-
    /// visible identifier), but the value still came out of the keychain
    /// so its lifetime is tied to `self`.
    pub fn login(&self) -> &str {
        &self.login
    }

    /// Password bytes for the per-request SHA-512 hash. Returned as
    /// `&[u8]` so the caller cannot accidentally pass it to `Display`.
    /// Used by ADR-0009 §4's `passwordHash` computation in PR-7-B.
    pub fn password_bytes(&self) -> &[u8] {
        self.password.as_bytes()
    }

    /// XML sign key bytes — the SHA3-512 input per ADR-0009 §4 +
    /// ADR-0020 §2 `requestSignature` calculation. PR-7-B consumer.
    pub fn sign_key_bytes(&self) -> &[u8] {
        self.sign_key.as_bytes()
    }

    /// XML change key bytes — the AES-128/ECB key used to decrypt the
    /// exchangeToken per ADR-0009 §4 + ADR-0021 §A9. PR-7-B consumer.
    pub fn change_key_bytes(&self) -> &[u8] {
        self.change_key.as_bytes()
    }

    /// Test-support constructor. Production callers MUST use
    /// `load_from_keychain`. This is gated behind a test/feature flag
    /// (see the `#[cfg]` line) so production code paths cannot accidentally
    /// reach for it — same pattern as `Actor::test_only`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_parts(
        tenant_id: &str,
        login: &str,
        password: &str,
        sign_key: &str,
        change_key: &str,
    ) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            login: Zeroizing::new(login.to_string()),
            password: Zeroizing::new(password.to_string()),
            sign_key: Zeroizing::new(sign_key.to_string()),
            change_key: Zeroizing::new(change_key.to_string()),
        }
    }
}

/// Hand-written `Debug` to redact every secret. The default
/// `#[derive(Debug)]` would include the `Zeroizing<String>` bytes,
/// which would land in `tracing`/`dbg!` output and stay in process
/// memory longer than the secret itself. Tenant_id and login are
/// shown because they are operator-visible identifiers, not secrets.
impl std::fmt::Debug for NavCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavCredentials")
            .field("tenant_id", &self.tenant_id)
            .field("login", &self.login.as_str())
            .field("password", &"<redacted>")
            .field("sign_key", &"<redacted>")
            .field("change_key", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The debug output must never include the secret material. If a
    /// future contributor switches to `#[derive(Debug)]`, this test
    /// fails — exactly the trap CLAUDE.md rule 12 names.
    #[test]
    fn debug_redacts_secrets() {
        let c =
            NavCredentials::from_parts("t-1", "tech-user", "pw-secret", "sk-secret", "ck-secret");
        let s = format!("{c:?}");
        assert!(s.contains("t-1"), "tenant_id should be shown");
        assert!(s.contains("tech-user"), "login should be shown");
        assert!(
            !s.contains("pw-secret"),
            "password must be redacted, got: {s}"
        );
        assert!(
            !s.contains("sk-secret"),
            "sign_key must be redacted, got: {s}"
        );
        assert!(
            !s.contains("ck-secret"),
            "change_key must be redacted, got: {s}"
        );
    }

    /// Accessor sanity — useful guard against future field rename
    /// drift where one accessor returns the wrong inner field.
    #[test]
    fn accessors_return_their_own_field() {
        let c = NavCredentials::from_parts("t-1", "lg", "pw", "sk", "ck");
        assert_eq!(c.tenant_id(), "t-1");
        assert_eq!(c.login(), "lg");
        assert_eq!(c.password_bytes(), b"pw");
        assert_eq!(c.sign_key_bytes(), b"sk");
        assert_eq!(c.change_key_bytes(), b"ck");
    }
}
