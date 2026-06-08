//! S281 / PR-266 — Email-relay bearer token in the OS keychain.
//!
//! Mirrors [`crate::quote_intake_credentials`] one-to-one (same shape,
//! different service name) so a compromised email-relay token does NOT
//! grant access to the quote-intake surface and vice-versa per
//! ADR-0007 §Auth — "two tokens → independent rotation."
//!
//! Service-and-account naming:
//!
//!   service:  `aberp.email_relay.<tenant_id>`
//!   account:  `email_relay_token`
//!
//! # Why a dedicated entry instead of reusing the session token
//!
//! The session token is the operator's own bearer for the SPA. The
//! storefront is a sister-service caller, identified by its own
//! `submitter` field in the audit log; conflating them would tie
//! rotation of one to the other.

use keyring::Entry;
use zeroize::Zeroizing;

/// Item-name for the email-relay bearer-token keychain entry.
pub const ITEM_EMAIL_RELAY_TOKEN: &str = "email_relay_token";

/// Compose the keychain `service` field for a tenant.
pub fn service_name(tenant_id: &str) -> String {
    format!("aberp.email_relay.{tenant_id}")
}

#[derive(Debug)]
pub enum EmailRelayCredentialsError {
    Missing { tenant_id: String },
    Backend(String),
}

impl std::fmt::Display for EmailRelayCredentialsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailRelayCredentialsError::Missing { tenant_id } => write!(
                f,
                "email-relay bearer token is not set in the keychain for tenant `{tenant_id}`"
            ),
            EmailRelayCredentialsError::Backend(msg) => {
                write!(f, "keychain backend error: {msg}")
            }
        }
    }
}

impl std::error::Error for EmailRelayCredentialsError {}

/// Write the bearer token to the OS keychain for `tenant_id`.
/// Overwrites any existing entry. Per CLAUDE.md rule 12 the validation
/// (non-empty) happens at the route layer; this seam writes whatever
/// it's given so the rotation surface stays simple.
pub fn write_token(tenant_id: &str, token: &str) -> Result<(), EmailRelayCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_EMAIL_RELAY_TOKEN)
        .map_err(|e| EmailRelayCredentialsError::Backend(format!("Entry::new: {e}")))?;
    entry
        .set_password(token)
        .map_err(|e| EmailRelayCredentialsError::Backend(format!("set_password: {e}")))
}

/// Read the bearer token from the OS keychain. Wrapped in `Zeroizing`.
pub fn read_token(tenant_id: &str) -> Result<Zeroizing<String>, EmailRelayCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_EMAIL_RELAY_TOKEN)
        .map_err(|e| EmailRelayCredentialsError::Backend(format!("Entry::new: {e}")))?;
    match entry.get_password() {
        Ok(s) => Ok(Zeroizing::new(s)),
        Err(keyring::Error::NoEntry) => Err(EmailRelayCredentialsError::Missing {
            tenant_id: tenant_id.to_string(),
        }),
        Err(other) => Err(EmailRelayCredentialsError::Backend(format!(
            "get_password: {other}"
        ))),
    }
}

/// Delete the bearer-token keychain entry for `tenant_id`. Idempotent.
#[allow(dead_code)]
pub fn delete_token(tenant_id: &str) -> Result<bool, EmailRelayCredentialsError> {
    let service = service_name(tenant_id);
    let entry = Entry::new(&service, ITEM_EMAIL_RELAY_TOKEN)
        .map_err(|e| EmailRelayCredentialsError::Backend(format!("Entry::new: {e}")))?;
    match entry.delete_password() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(other) => Err(EmailRelayCredentialsError::Backend(format!(
            "delete_password: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_format_is_stable() {
        assert_eq!(service_name("acme"), "aberp.email_relay.acme");
        assert_eq!(service_name("t-uuid-1234"), "aberp.email_relay.t-uuid-1234");
    }

    #[test]
    fn item_name_is_stable() {
        assert_eq!(ITEM_EMAIL_RELAY_TOKEN, "email_relay_token");
    }

    /// S281 / PR-266 — the storefront emails relay through ABERP via
    /// THIS dedicated token, never via the quote-intake token. A
    /// service-name collision would couple rotation of the two surfaces
    /// (ADR-0007 §Auth — "two tokens → independent rotation"). Pin the
    /// distinctness so a future contributor renaming one keychain
    /// service can't silently overlap with the other.
    #[test]
    fn email_relay_service_name_is_distinct_from_quote_intake_and_smtp() {
        let tenant = "production";
        let relay = service_name(tenant);
        let qi = crate::quote_intake_credentials::service_name(tenant);
        let smtp = crate::smtp_credentials::service_name(tenant);
        let nav = aberp_nav_transport::credentials::keychain::service_name(tenant);
        assert_ne!(relay, qi);
        assert_ne!(relay, smtp);
        assert_ne!(relay, nav);
        assert!(relay.starts_with("aberp.email_relay."));
    }
}
