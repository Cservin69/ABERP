//! OS-keychain reader for the four NAV credential artifacts.
//!
//! Service-and-account naming convention (stable across platforms via
//! the `keyring` crate's abstraction; per ADR-0007 §Secrets and
//! ADR-0020 §3):
//!
//!   service:  `aberp.nav.<tenant_id>`
//!   account:  one of [`ITEM_LOGIN`], [`ITEM_PASSWORD`], [`ITEM_SIGN_KEY`],
//!             [`ITEM_CHANGE_KEY`]
//!
//! On macOS this maps to the system keychain "Where" + "Account" fields,
//! viewable via `security find-generic-password -s "aberp.nav.<tenant>"`.
//! On Linux/SecretService and Windows Credential Manager the mapping is
//! analogous and handled by `keyring`.
//!
//! This module deliberately does NOT expose a "list-all" or "delete-all"
//! API. Population is an operator-initiated flow (out of scope for
//! PR-7-A); rotation is operator-initiated per ADR-0009 §4.

use keyring::Entry;
use zeroize::Zeroizing;

use crate::error::NavTransportError;

// ----- item name constants -----------------------------------------
//
// Named here (not inlined as string literals at call sites) so a
// future rename is a single point-of-edit and a grep across the repo
// finds every reference. The values themselves are part of the
// on-disk contract with the operator's keychain and must NOT change
// silently — a rename effectively orphans the operator's existing
// keychain entries, which is a tooling-affecting change.

/// Account name for the technical-user login (operator-visible string).
pub const ITEM_LOGIN: &str = "technical_user.login";

/// Account name for the technical-user password (plaintext at rest).
pub const ITEM_PASSWORD: &str = "technical_user.password";

/// Account name for the `xmlSignKey` per ADR-0009 §4.
pub const ITEM_SIGN_KEY: &str = "xml_sign_key";

/// Account name for the `xmlChangeKey` per ADR-0009 §4.
pub const ITEM_CHANGE_KEY: &str = "xml_change_key";

/// Compose the keychain `service` field for a tenant. Public so the
/// operator-tooling (a future PR) and the unit test can agree on the
/// naming without duplicating the format string.
pub fn service_name(tenant_id: &str) -> String {
    format!("aberp.nav.{tenant_id}")
}

/// Read one secret from the keychain. The secret is wrapped in
/// `Zeroizing<String>` so the buffer is overwritten on drop.
///
/// Two distinct failure modes are returned as distinct typed errors:
///
///   1. `NavTransportError::KeychainItemMissing` — the keychain backend
///      reports the entry doesn't exist. Operator action: populate.
///   2. `NavTransportError::KeychainBackend`     — the backend itself
///      errored (locked keychain, permission denied, unsupported
///      platform). Operator action: triage the underlying error.
///
/// CLAUDE.md rule 12 (fail loud): there is NO third path that returns
/// an empty string or a default. Missing means missing.
pub fn read_secret(
    tenant_id: &str,
    item: &'static str,
) -> Result<Zeroizing<String>, NavTransportError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, item)
        .map_err(|e| NavTransportError::KeychainBackend { item, source: e })?;
    match entry.get_password() {
        Ok(secret) => Ok(Zeroizing::new(secret)),
        Err(keyring::Error::NoEntry) => Err(NavTransportError::KeychainItemMissing {
            tenant_id: tenant_id.to_string(),
            item,
        }),
        Err(other) => Err(NavTransportError::KeychainBackend {
            item,
            source: other,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naming-convention guard. If a future contributor changes the
    /// `service_name` format, this test fails — and the rename is a
    /// breaking change for any tenant whose keychain is already
    /// populated, so the failure forces the discussion.
    #[test]
    fn service_name_format_is_stable() {
        assert_eq!(service_name("acme"), "aberp.nav.acme");
        assert_eq!(service_name("t-uuid-1234"), "aberp.nav.t-uuid-1234");
    }

    /// Item-name guard. Same reasoning as `service_name_format_is_stable`
    /// — the strings are part of the on-disk operator contract.
    #[test]
    fn item_names_are_stable() {
        assert_eq!(ITEM_LOGIN, "technical_user.login");
        assert_eq!(ITEM_PASSWORD, "technical_user.password");
        assert_eq!(ITEM_SIGN_KEY, "xml_sign_key");
        assert_eq!(ITEM_CHANGE_KEY, "xml_change_key");
    }
}
