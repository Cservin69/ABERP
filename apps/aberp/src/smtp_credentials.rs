//! PR-92 — SMTP credentials in the OS keychain.
//!
//! Mirrors [`aberp_nav_transport::credentials::keychain`]'s posture:
//! the SMTP password lives in the OS keychain ONLY, never on disk, never
//! in TOML, never in logs. Non-secret SMTP settings (host, port,
//! from-address, username, security mode) live in `[seller.smtp]` of
//! `~/.aberp/<tenant>/seller.toml` per the keychain/TOML split anchored
//! by [`aberp-tenant-management`].
//!
//! Service-and-account naming convention (stable across platforms via
//! the `keyring` crate's abstraction):
//!
//!   service:  `aberp.smtp.<tenant_id>`
//!   account:  `smtp_password`
//!
//! On macOS this maps to the system keychain "Where" + "Account"
//! fields, viewable via `security find-generic-password -s
//! "aberp.smtp.<tenant>"`. The single-item shape (one secret per
//! tenant) means one keychain ACL prompt per fresh-build boot — no
//! consolidated-blob layout is needed because there is only one
//! secret to store.
//!
//! # Security
//!
//! - Read/write/delete are the only operations exposed.
//! - The password is wrapped in `Zeroizing<String>` on read so the
//!   buffer is overwritten on drop.
//! - No `Debug` impl on the password string — accidental
//!   `tracing::debug!(?password)` would not compile.

use keyring::Entry;
use zeroize::Zeroizing;

/// Item-name for the SMTP password keychain entry. Named here (not
/// inlined as a string literal at call sites) so a future rename is a
/// single point-of-edit and a grep across the repo finds every
/// reference. The value is part of the on-disk contract with the
/// operator's keychain and must NOT change silently.
pub const ITEM_SMTP_PASSWORD: &str = "smtp_password";

/// Compose the keychain `service` field for a tenant. Public so the
/// operator-tooling (CLI + HTTP route) and the unit test can agree on
/// the naming without duplicating the format string.
///
/// Mirrors [`aberp_nav_transport::credentials::keychain::service_name`]
/// but with the `aberp.smtp.` prefix so the NAV and SMTP keychain
/// items never collide for the same tenant.
pub fn service_name(tenant_id: &str) -> String {
    format!("aberp.smtp.{tenant_id}")
}

/// Typed error surface for SMTP-credential keychain operations. The
/// two arms map to distinct operator-actionable failure modes; a
/// generic `anyhow` wrap would mask the "missing vs broken" choice
/// the route layer needs to render correct UX.
#[derive(Debug)]
pub enum SmtpCredentialsError {
    /// The keychain backend reports the entry does not exist for
    /// this tenant. Operator action: populate via the SPA's SMTP
    /// settings page.
    Missing { tenant_id: String },
    /// The OS keychain backend itself errored (locked keychain,
    /// permission denied, unsupported platform). Operator action:
    /// triage the underlying error.
    Backend(String),
}

impl std::fmt::Display for SmtpCredentialsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmtpCredentialsError::Missing { tenant_id } => write!(
                f,
                "SMTP password is not set in the keychain for tenant `{tenant_id}`"
            ),
            SmtpCredentialsError::Backend(msg) => write!(f, "keychain backend error: {msg}"),
        }
    }
}

impl std::error::Error for SmtpCredentialsError {}

/// Write the SMTP password to the OS keychain for `tenant_id`.
/// Overwrites any existing entry. CLAUDE.md rule 12: the validation
/// (non-empty) happens at the route layer; this seam writes whatever
/// it's given so the rotation surface stays simple.
///
/// The password parameter is taken by value as `&str` (not
/// `Zeroizing`) because the keychain crate's `set_password` takes
/// `&str` — the secret materialises in the keyring entry's buffer
/// regardless. The caller is responsible for not retaining the
/// string after this returns.
pub fn write_password(tenant_id: &str, password: &str) -> Result<(), SmtpCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_SMTP_PASSWORD)
        .map_err(|e| SmtpCredentialsError::Backend(format!("Entry::new: {e}")))?;
    entry
        .set_password(password)
        .map_err(|e| SmtpCredentialsError::Backend(format!("set_password: {e}")))
}

/// Read the SMTP password from the OS keychain for `tenant_id`. The
/// returned string is wrapped in `Zeroizing<String>` so its buffer is
/// overwritten on drop.
///
/// Two distinct failure modes are returned as distinct typed errors:
///
///   1. `Missing` — the keychain backend reports the entry doesn't
///      exist. Operator action: populate.
///   2. `Backend` — the backend itself errored. Operator action:
///      triage.
///
/// CLAUDE.md rule 12 (fail loud): there is NO third path that returns
/// an empty string or a default. Missing means missing.
pub fn read_password(tenant_id: &str) -> Result<Zeroizing<String>, SmtpCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_SMTP_PASSWORD)
        .map_err(|e| SmtpCredentialsError::Backend(format!("Entry::new: {e}")))?;
    match entry.get_password() {
        Ok(s) => Ok(Zeroizing::new(s)),
        Err(keyring::Error::NoEntry) => Err(SmtpCredentialsError::Missing {
            tenant_id: tenant_id.to_string(),
        }),
        Err(other) => Err(SmtpCredentialsError::Backend(format!(
            "get_password: {other}"
        ))),
    }
}

/// Probe whether the SMTP password is populated for `tenant_id`. Used
/// by the GET-config route to surface a `password_set: bool` flag to
/// the SPA without leaking the password itself. Loud-fails on backend
/// errors per CLAUDE.md rule 12 — a silent fall-through to `false`
/// would mask a locked keychain as "password not set".
pub fn password_is_set(tenant_id: &str) -> Result<bool, SmtpCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_SMTP_PASSWORD)
        .map_err(|e| SmtpCredentialsError::Backend(format!("Entry::new: {e}")))?;
    match entry.get_password() {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(other) => Err(SmtpCredentialsError::Backend(format!(
            "get_password: {other}"
        ))),
    }
}

/// Delete the SMTP password keychain entry for `tenant_id`. Used by
/// the (future) tenant-deletion path. Returns `Ok(true)` if a delete
/// happened, `Ok(false)` if the entry was absent (idempotent).
#[allow(dead_code)]
pub fn delete_password(tenant_id: &str) -> Result<bool, SmtpCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_SMTP_PASSWORD)
        .map_err(|e| SmtpCredentialsError::Backend(format!("Entry::new: {e}")))?;
    match entry.delete_password() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(other) => Err(SmtpCredentialsError::Backend(format!(
            "delete_password: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naming-convention guard. If a future contributor changes the
    /// `service_name` format, this test fails — and the rename is a
    /// breaking change for any tenant whose keychain is already
    /// populated, so the failure forces the discussion. Mirror of the
    /// PR-57 NAV-side stability test.
    #[test]
    fn service_name_format_is_stable() {
        assert_eq!(service_name("acme"), "aberp.smtp.acme");
        assert_eq!(service_name("t-uuid-1234"), "aberp.smtp.t-uuid-1234");
    }

    /// Item-name guard. Same reasoning as `service_name_format_is_stable`
    /// — the string is part of the on-disk operator contract.
    #[test]
    fn item_name_is_stable() {
        assert_eq!(ITEM_SMTP_PASSWORD, "smtp_password");
    }

    /// The SMTP service-name MUST be distinct from the NAV service-name
    /// for the same tenant — collision would let either subsystem
    /// overwrite the other's password. Pin the prefix-level fork.
    #[test]
    fn smtp_service_name_does_not_collide_with_nav() {
        let tenant = "production";
        let smtp = service_name(tenant);
        let nav = aberp_nav_transport::credentials::keychain::service_name(tenant);
        assert_ne!(smtp, nav);
        assert!(smtp.starts_with("aberp.smtp."));
        assert!(nav.starts_with("aberp.nav."));
    }
}
