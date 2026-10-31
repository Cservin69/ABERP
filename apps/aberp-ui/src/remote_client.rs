//! ADR-0111 — remote mode: this shell drives the `aberp serve` running on
//! the machine that owns the tenant DB, over the Thunderbolt Bridge,
//! instead of spawning a local backend.
//!
//! Remote mode is selected by `ABERP_REMOTE_URL`; nothing about it is
//! inferred. Trust is configured, never discovered:
//!
//!   - `ABERP_REMOTE_URL=https://<lan-ipv4>:<port>` — an IP literal on an
//!     RFC 1918 / link-local address (no DNS, no mDNS: a `.local` name can
//!     resolve to the Wi-Fi address).
//!   - `ABERP_REMOTE_FINGERPRINT=<64 hex>` — the backend's
//!     `~/.aberp/serve/<tenant>/loopback.fingerprint.sha256`, copied once.
//!     It replaces the handshake line the local spawn would have printed.
//!   - The bearer lives in THIS machine's keychain under
//!     `aberp.nav.<tenant>` / `remote_session_token`, copied once from the
//!     backend machine's keychain.
//!
//! Before the SPA mounts, `/remote/hello` must report the same tenant, the
//! same build profile (`--features production` on both or neither) and a
//! `ready` boot state. First-run wizards are refused here: setup belongs on
//! the machine that owns the DB.
//!
//! Attach mode: `ABERP_REMOTE_URL=https://127.0.0.1:<port>` is the MacBook's
//! own shell attaching to the backend the LaunchAgent runs (the backend holds
//! the single-writer lock, so the shell cannot spawn a second one). Same
//! fingerprint pin and handshake, but the bearer is the loopback
//! `session_token`, which already lives in that machine's keychain.

use std::net::SocketAddrV4;

use anyhow::{anyhow, bail, Context, Result};

/// Keychain account holding the remote bearer — must match
/// `apps/aberp/src/remote_listener.rs::REMOTE_SESSION_TOKEN_ACCOUNT`.
pub const REMOTE_SESSION_TOKEN_ACCOUNT: &str = "remote_session_token";

/// Keychain account of the loopback bearer, used in attach mode — must match
/// `apps/aberp/src/serve.rs::SESSION_TOKEN_ACCOUNT`.
pub const LOOPBACK_SESSION_TOKEN_ACCOUNT: &str = "session_token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBackendConfig {
    /// `https://<ip>:<port>` with no trailing slash.
    pub url: String,
    /// Lower-case 64-hex SHA-256 of the backend's leaf cert DER.
    pub fingerprint_hex: String,
    /// Keychain account of the bearer to present: the remote token over the
    /// bridge, the loopback token in attach mode.
    pub token_account: &'static str,
}

/// `Ok(None)` when `ABERP_REMOTE_URL` is unset (the normal local spawn).
pub fn config_from_env() -> Result<Option<RemoteBackendConfig>> {
    let url = std::env::var("ABERP_REMOTE_URL").ok();
    let fingerprint = std::env::var("ABERP_REMOTE_FINGERPRINT").ok();
    parse_config(url.as_deref(), fingerprint.as_deref())
}

pub fn parse_config(
    url: Option<&str>,
    fingerprint: Option<&str>,
) -> Result<Option<RemoteBackendConfig>> {
    let Some(url) = url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let authority = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("ABERP_REMOTE_URL=`{url}` must start with https://"))?
        .trim_end_matches('/');
    let addr: SocketAddrV4 = authority.parse().with_context(|| {
        format!("ABERP_REMOTE_URL=`{url}` must be https://<ipv4>:<port> with no path")
    })?;
    let ip = *addr.ip();
    let attach_local = ip == std::net::Ipv4Addr::LOCALHOST;
    if addr.port() == 0
        || ip.is_unspecified()
        || !(attach_local || ip.is_private() || ip.is_link_local())
    {
        bail!("ABERP_REMOTE_URL=`{url}` must name the Thunderbolt Bridge peer (RFC 1918 or link-local IPv4) or 127.0.0.1 to attach locally, with a fixed port");
    }

    let fingerprint = fingerprint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("ABERP_REMOTE_URL is set but ABERP_REMOTE_FINGERPRINT is not — refusing an unpinned remote backend"))?;
    let fingerprint = fingerprint
        .strip_prefix("sha256:")
        .unwrap_or(fingerprint)
        .to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("ABERP_REMOTE_FINGERPRINT must be 64 hex characters (SHA-256)");
    }

    Ok(Some(RemoteBackendConfig {
        url: format!("https://{addr}"),
        fingerprint_hex: fingerprint,
        token_account: if attach_local {
            LOOPBACK_SESSION_TOKEN_ACCOUNT
        } else {
            REMOTE_SESSION_TOKEN_ACCOUNT
        },
    }))
}

#[derive(Debug)]
pub struct RemoteHello {
    pub tenant: String,
    pub state: String,
    pub is_production_build: bool,
}

impl RemoteHello {
    /// Strict read of the `/remote/hello` body: a missing or mistyped field
    /// is an error, never a default.
    pub fn from_json(v: &serde_json::Value) -> Result<Self> {
        let field = |name: &str| {
            v.get(name)
                .ok_or_else(|| anyhow!("/remote/hello: missing `{name}`"))
        };
        Ok(RemoteHello {
            tenant: field("tenant")?
                .as_str()
                .ok_or_else(|| anyhow!("/remote/hello: `tenant` is not a string"))?
                .to_string(),
            state: field("state")?
                .as_str()
                .ok_or_else(|| anyhow!("/remote/hello: `state` is not a string"))?
                .to_string(),
            is_production_build: field("is_production_build")?
                .as_bool()
                .ok_or_else(|| anyhow!("/remote/hello: `is_production_build` is not a bool"))?,
        })
    }
}

/// Refuse to mount the SPA unless the remote backend is the one we meant.
pub fn check_hello(hello: &RemoteHello, tenant: &str, local_is_production: bool) -> Result<()> {
    if hello.tenant != tenant {
        bail!(
            "remote backend serves tenant `{}` but this shell was started for `{tenant}`",
            hello.tenant
        );
    }
    if hello.is_production_build != local_is_production {
        bail!(
            "build-profile mismatch: remote backend production={}, this shell production={local_is_production} — \
             build both with the same --features",
            hello.is_production_build
        );
    }
    if hello.state != "ready" {
        bail!(
            "remote backend is in state `{}` — finish setup on the machine that holds the database first",
            hello.state
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";

    #[test]
    fn unset_url_is_local_mode() {
        assert_eq!(parse_config(None, Some(FP)).unwrap(), None);
    }

    #[test]
    fn valid_config_normalises() {
        let prefixed = format!("sha256:{}", FP.to_uppercase());
        let cfg = parse_config(Some("https://10.69.0.1:8443/"), Some(prefixed.as_str()))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.url, "https://10.69.0.1:8443");
        assert_eq!(cfg.fingerprint_hex, FP);
        assert_eq!(cfg.token_account, REMOTE_SESSION_TOKEN_ACCOUNT);
    }

    #[test]
    fn loopback_is_attach_mode_with_the_loopback_token() {
        let cfg = parse_config(Some("https://127.0.0.1:18443"), Some(FP))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.url, "https://127.0.0.1:18443");
        assert_eq!(cfg.token_account, LOOPBACK_SESSION_TOKEN_ACCOUNT);
        // Attach mode is still pinned: no fingerprint, no connection.
        assert!(parse_config(Some("https://127.0.0.1:18443"), None).is_err());
    }

    #[test]
    fn non_lan_or_non_tls_urls_are_refused() {
        for url in [
            "http://10.69.0.1:8443",
            "https://example.com:8443",
            "https://Ervins-MacBook-Pro.local:8443",
            "https://localhost:18443",
            "https://127.0.0.2:18443",
            "https://8.8.8.8:8443",
            "https://0.0.0.0:8443",
            "https://10.69.0.1",
            "https://10.69.0.1:8443/api",
        ] {
            assert!(
                parse_config(Some(url), Some(FP)).is_err(),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn fingerprint_is_mandatory_and_shaped() {
        assert!(parse_config(Some("https://10.69.0.1:8443"), None).is_err());
        assert!(parse_config(Some("https://10.69.0.1:8443"), Some("abcd")).is_err());
    }

    #[test]
    fn hello_parse_is_strict() {
        let v =
            serde_json::json!({"tenant": "prod", "state": "ready", "is_production_build": true});
        let h = RemoteHello::from_json(&v).unwrap();
        assert_eq!(h.tenant, "prod");
        assert!(
            RemoteHello::from_json(&serde_json::json!({"tenant": "prod", "state": "ready"}))
                .is_err()
        );
        assert!(RemoteHello::from_json(
            &serde_json::json!({"tenant": 1, "state": "ready", "is_production_build": true})
        )
        .is_err());
    }

    #[test]
    fn hello_mismatches_are_refused() {
        let ok = RemoteHello {
            tenant: "prod".into(),
            state: "ready".into(),
            is_production_build: true,
        };
        assert!(check_hello(&ok, "prod", true).is_ok());
        assert!(check_hello(&ok, "test", true).is_err());
        assert!(check_hello(&ok, "prod", false).is_err());
        let setup = RemoteHello {
            state: "needs-setup".into(),
            ..ok
        };
        assert!(check_hello(&setup, "prod", true).is_err());
    }
}
