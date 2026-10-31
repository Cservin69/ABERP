//! ADR-0111 — the Thunderbolt-Bridge remote listener.
//!
//! One authoritative tenant DB lives on ONE machine (Ervin's MacBook). A
//! second machine (the M5 Mac Studio) runs the same Tauri shell in remote
//! mode and talks to THIS backend over a point-to-point Thunderbolt Bridge
//! link, so both machines issue invoices against one DB, one numbering
//! allocator and one audit chain. No DB file is ever copied or synced.
//!
//! The loopback listener is untouched: `transport_bind_host()` stays
//! `127.0.0.1` and the local shell keeps its own token. This module adds a
//! SECOND, opt-in listener that exists only when `ABERP_REMOTE_BIND` is set,
//! and every layer of it fails closed:
//!
//!   1. **Interface pin.** The socket is bound to the configured interface
//!      with `IP_BOUND_IF` (macOS), not just to an address. macOS is a
//!      weak-host stack: a socket bound only to `10.69.0.1` would accept a
//!      packet addressed to `10.69.0.1` that arrived on Wi-Fi. `IP_BOUND_IF`
//!      makes the kernel drop it. Boot also refuses unless the address is
//!      actually assigned to that interface.
//!   2. **Address shape.** Bind and peer addresses must be RFC 1918 or
//!      link-local IPv4. A public or wildcard address is a boot refusal.
//!   3. **Peer allowlist.** Exactly one client IPv4 may talk to the listener.
//!   4. **Separate bearer.** A dedicated keychain token
//!      (`aberp.nav.<tenant>` / `remote_session_token`) — revocable without
//!      touching the local shell's token. Required on every route,
//!      including `/health`.
//!   5. **Route deny-list.** Secret provisioning, restore, tenant lifecycle
//!      and the first-prod-launch ceremony stay at the machine that owns the
//!      DB (see [`remote_route_denied`]).
//!   6. **TLS pin.** Same self-signed cert as loopback; the remote shell pins
//!      its SHA-256 fingerprint exactly like the local shell does.
//!
//! Misconfiguration is a loud boot failure (CLAUDE.md rule 12), never a
//! silent fallback to "no remote listener" or to a wider bind.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{ConnectInfo, MatchedPath, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

/// Keychain account (under the tenant's `aberp.nav.<tenant>` service) that
/// holds the remote client's bearer token. Distinct from the loopback
/// `session_token` so revoking the remote machine never locks out the
/// local shell.
pub const REMOTE_SESSION_TOKEN_ACCOUNT: &str = "remote_session_token";

/// Interface used when `ABERP_REMOTE_IFACE` is unset. On macOS the
/// Thunderbolt Bridge service is `bridge0`.
pub const DEFAULT_REMOTE_IFACE: &str = "bridge0";

/// Listen backlog for the remote socket. One client; a small queue is plenty.
const REMOTE_LISTEN_BACKLOG: i32 = 16;

/// Validated remote-listener configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListenerConfig {
    pub bind: SocketAddrV4,
    pub iface: String,
    pub peer: Ipv4Addr,
}

/// Read the remote-listener configuration from the environment.
///
/// - `ABERP_REMOTE_BIND` unset → `Ok(None)`: no remote listener (the default).
/// - `ABERP_REMOTE_BIND=<ipv4>:<port>` → the remote listener is required;
///   `ABERP_REMOTE_PEER=<ipv4>` must be set too, `ABERP_REMOTE_IFACE`
///   defaults to [`DEFAULT_REMOTE_IFACE`]. Any invalid value is an error.
pub fn config_from_env() -> Result<Option<RemoteListenerConfig>> {
    let bind = std::env::var("ABERP_REMOTE_BIND").ok();
    let iface = std::env::var("ABERP_REMOTE_IFACE").ok();
    let peer = std::env::var("ABERP_REMOTE_PEER").ok();
    let cfg = parse_config(bind.as_deref(), iface.as_deref(), peer.as_deref())?;
    if cfg.is_some() && crate::build_profile::IS_SAAS_BUILD {
        bail!(
            "ABERP_REMOTE_BIND is set on a `saas` build — the Thunderbolt-Bridge \
             listener is a desktop-only feature (ADR-0111); the SaaS edge is ADR-0100 Phase 6"
        );
    }
    Ok(cfg)
}

/// Pure parse + validate step behind [`config_from_env`].
pub fn parse_config(
    bind: Option<&str>,
    iface: Option<&str>,
    peer: Option<&str>,
) -> Result<Option<RemoteListenerConfig>> {
    let Some(bind_raw) = bind.map(str::trim).filter(|s| !s.is_empty()) else {
        if peer.is_some_and(|p| !p.trim().is_empty()) {
            bail!("ABERP_REMOTE_PEER is set but ABERP_REMOTE_BIND is not — refusing a half-configured remote listener");
        }
        return Ok(None);
    };

    let bind: SocketAddrV4 = bind_raw.parse().with_context(|| {
        format!("ABERP_REMOTE_BIND=`{bind_raw}` is not an IPv4 `<address>:<port>`")
    })?;
    if bind.port() == 0 {
        bail!("ABERP_REMOTE_BIND=`{bind_raw}` must name a fixed port — the remote shell is configured with it");
    }
    validate_lan_ip("ABERP_REMOTE_BIND", *bind.ip())?;

    let iface = iface
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_REMOTE_IFACE)
        .to_string();
    if iface.starts_with("lo") {
        bail!("ABERP_REMOTE_IFACE=`{iface}` is a loopback interface — the loopback listener already exists");
    }

    let peer_raw = peer
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!("ABERP_REMOTE_BIND is set but ABERP_REMOTE_PEER is not — the remote listener only accepts one named client")
        })?;
    let peer: Ipv4Addr = peer_raw
        .parse()
        .with_context(|| format!("ABERP_REMOTE_PEER=`{peer_raw}` is not an IPv4 address"))?;
    validate_lan_ip("ABERP_REMOTE_PEER", peer)?;
    if peer == *bind.ip() {
        bail!("ABERP_REMOTE_PEER equals the bind address {peer} — it must be the OTHER machine");
    }

    Ok(Some(RemoteListenerConfig { bind, iface, peer }))
}

/// Only RFC 1918 private or 169.254/16 link-local unicast addresses are
/// acceptable for a point-to-point cable link.
fn validate_lan_ip(var: &str, ip: Ipv4Addr) -> Result<()> {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_broadcast() || ip.is_multicast() {
        bail!("{var}={ip} is not a unicast LAN address");
    }
    if !(ip.is_private() || ip.is_link_local()) {
        bail!("{var}={ip} is not an RFC 1918 or link-local address — refusing to expose ABERP beyond the Thunderbolt Bridge");
    }
    Ok(())
}

/// Bind the remote listener: verify the address is on the interface, pin
/// the socket to that interface (`IP_BOUND_IF`), bind, listen.
pub fn bind(cfg: &RemoteListenerConfig) -> Result<TcpListener> {
    ensure_ip_on_interface(*cfg.bind.ip(), &cfg.iface)?;

    let index = nix::net::if_::if_nametoindex(cfg.iface.as_str())
        .with_context(|| format!("resolve interface index for `{}`", cfg.iface))?;
    let index = NonZeroU32::new(index)
        .ok_or_else(|| anyhow!("interface `{}` resolved to index 0", cfg.iface))?;

    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .context("create remote listener socket")?;
    socket
        .set_reuse_address(true)
        .context("SO_REUSEADDR on remote listener socket")?;
    socket
        .bind_device_by_index_v4(Some(index))
        .with_context(|| format!("IP_BOUND_IF `{}` on remote listener socket", cfg.iface))?;
    socket
        .bind(&SocketAddr::V4(cfg.bind).into())
        .with_context(|| format!("bind remote listener at {}", cfg.bind))?;
    socket
        .listen(REMOTE_LISTEN_BACKLOG)
        .context("listen on remote listener socket")?;
    socket
        .set_nonblocking(true)
        .context("set remote listener socket non-blocking")?;
    Ok(socket.into())
}

fn ensure_ip_on_interface(ip: Ipv4Addr, iface: &str) -> Result<()> {
    let addrs = nix::ifaddrs::getifaddrs().context("enumerate interface addresses")?;
    let mut seen_iface = false;
    for ifa in addrs {
        if ifa.interface_name != iface {
            continue;
        }
        seen_iface = true;
        let assigned = ifa
            .address
            .as_ref()
            .and_then(|a| a.as_sockaddr_in())
            .map(|sin| *SocketAddrV4::from(*sin).ip());
        if assigned == Some(ip) {
            return Ok(());
        }
    }
    if seen_iface {
        bail!("{ip} is not assigned to `{iface}` — set the Thunderbolt Bridge to a manual address first");
    }
    bail!("interface `{iface}` does not exist (is the Thunderbolt cable connected?)")
}

/// Routes the remote shell may NOT call. These either provision secrets on
/// the DB-owning machine, rewrite the DB from a backup, re-point or restart
/// the one backend both machines share, or record a legal-grade operator
/// consent that belongs at the DB-owning machine. Matched against the axum
/// route TEMPLATE (`MatchedPath`), so encoding tricks cannot slip past it.
pub fn remote_route_denied(method: &Method, route: &str) -> bool {
    const DENIED: &[&str] = &[
        "/health/acknowledge-first-prod-launch",
        "/api/setup-nav-credentials",
        "/api/setup-seller-info",
        "/api/rotate-nav-credential",
        "/api/snapshots/restore",
        "/api/restore-from-nav-outgoing",
        "/api/restore-lock/abandon",
        "/api/tenants/:slug/switch",
        "/api/tenants/:slug/archive",
        "/api/tenants/:slug/restore",
        "/api/tenants/:slug/toggle-nav",
        "/api/tenants/hide-demo",
        "/api/dap/mock-login",
    ];
    if DENIED.contains(&route) {
        return true;
    }
    // Creating a tenant is fine to list remotely, not to perform.
    route == "/api/tenants" && *method != Method::GET
}

/// State for [`guard`].
#[derive(Debug)]
pub struct RemotePolicy {
    pub peer: Ipv4Addr,
    pub token: String,
}

/// Middleware on every remote route: peer allowlist → bearer → deny-list.
/// Installed with `route_layer`, so unmatched paths 404 without reaching a
/// handler and `MatchedPath` is always present here.
pub async fn guard(
    State(policy): State<Arc<RemotePolicy>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if peer.ip() != std::net::IpAddr::V4(policy.peer) {
        tracing::warn!(%peer, expected = %policy.peer, "remote listener: refused connection from unexpected peer");
        return forbidden("peer not allowed");
    }
    if let Some(rejection) = crate::serve::check_bearer_rejection(req.headers(), &policy.token) {
        tracing::warn!(%peer, "remote listener: missing or invalid bearer");
        return rejection;
    }
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    if remote_route_denied(req.method(), &route) {
        tracing::warn!(%peer, route = %route, "remote listener: route is local-only");
        return forbidden("this action is only available on the machine that holds the database");
    }
    next.run(req).await
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(bind: &str, peer: &str) -> RemoteListenerConfig {
        parse_config(Some(bind), None, Some(peer))
            .expect("valid config")
            .expect("config present")
    }

    #[test]
    fn unset_bind_means_no_remote_listener() {
        assert_eq!(parse_config(None, None, None).unwrap(), None);
        assert_eq!(parse_config(Some("  "), None, None).unwrap(), None);
    }

    #[test]
    fn peer_without_bind_is_refused() {
        assert!(parse_config(None, None, Some("10.69.0.2")).is_err());
    }

    #[test]
    fn valid_config_defaults_to_bridge0() {
        let cfg = ok("10.69.0.1:8443", "10.69.0.2");
        assert_eq!(cfg.bind, "10.69.0.1:8443".parse().unwrap());
        assert_eq!(cfg.iface, "bridge0");
        assert_eq!(cfg.peer, Ipv4Addr::new(10, 69, 0, 2));
        let ll = ok("169.254.78.150:8443", "169.254.131.139");
        assert_eq!(ll.peer, Ipv4Addr::new(169, 254, 131, 139));
    }

    #[test]
    fn wide_or_public_binds_are_refused() {
        for bind in [
            "0.0.0.0:8443",
            "127.0.0.1:8443",
            "8.8.8.8:8443",
            "255.255.255.255:8443",
            "224.0.0.1:8443",
        ] {
            assert!(
                parse_config(Some(bind), None, Some("10.69.0.2")).is_err(),
                "{bind} must be refused"
            );
        }
    }

    #[test]
    fn port_zero_and_garbage_are_refused() {
        assert!(parse_config(Some("10.69.0.1:0"), None, Some("10.69.0.2")).is_err());
        assert!(parse_config(Some("10.69.0.1"), None, Some("10.69.0.2")).is_err());
        assert!(parse_config(Some("[::1]:8443"), None, Some("10.69.0.2")).is_err());
    }

    #[test]
    fn peer_is_mandatory_lan_and_distinct() {
        assert!(parse_config(Some("10.69.0.1:8443"), None, None).is_err());
        assert!(parse_config(Some("10.69.0.1:8443"), None, Some("1.1.1.1")).is_err());
        assert!(parse_config(Some("10.69.0.1:8443"), None, Some("10.69.0.1")).is_err());
    }

    #[test]
    fn loopback_iface_is_refused() {
        assert!(parse_config(Some("10.69.0.1:8443"), Some("lo0"), Some("10.69.0.2")).is_err());
    }

    #[test]
    fn deny_list_covers_local_only_routes() {
        for route in [
            "/api/setup-nav-credentials",
            "/api/rotate-nav-credential",
            "/api/snapshots/restore",
            "/api/restore-from-nav-outgoing",
            "/api/tenants/:slug/switch",
            "/health/acknowledge-first-prod-launch",
        ] {
            assert!(
                remote_route_denied(&Method::POST, route),
                "{route} must be local-only"
            );
        }
        assert!(remote_route_denied(&Method::POST, "/api/tenants"));
        assert!(!remote_route_denied(&Method::GET, "/api/tenants"));
    }

    #[test]
    fn invoicing_routes_stay_reachable() {
        for route in [
            "/health",
            "/invoices",
            "/invoices/issue",
            "/invoices/:id/submit",
            "/invoices/:id/poll-ack",
            "/api/invoices/:id/storno",
            "/api/invoice-drafts",
            "/api/partners",
            "/api/products",
            "/api/snapshots/now",
        ] {
            assert!(
                !remote_route_denied(&Method::POST, route),
                "{route} must stay reachable"
            );
        }
    }
}
