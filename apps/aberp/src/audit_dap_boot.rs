//! S441 / ADR-0087 + ADR-0088 — boot wiring for the per-tenant DÁP/QES
//! timestamp-anchored audit chain.
//!
//! STRUCTURAL FLOOR. When a tenant has `dap_enabled = true`, the boot path:
//!   1. loads/mints the per-tenant service signing key from the OS keychain
//!      (`aberp.audit_service.<tenant>`, mirroring the NAV/CAD key pattern),
//!   2. opens a long-lived **service session** (`ServiceSessionOpened` +
//!      `ServiceOpen` anchor) BEFORE any daemon can fire (ADR-0088),
//!   3. runs **crash recovery** for sessions left open by a prior run
//!      (ADR-0087 §"Crash recovery"),
//!   4. spawns the **heartbeat actor** that takes a `Heartbeat` anchor every
//!      `audit_anchor_heartbeat_seconds`.
//!
//! Anchoring uses [`MockTimestampAuthority`] until NETLOCK onboarding lands
//! (the real [`aberp_audit_ledger::session::tsa::NetlockTsa`] is `todo!`).
//! The orchestration in [`open_service_session_and_recover`] is keychain-/
//! network-free and unit-tested; the keychain load and the Tokio actor are
//! thin shells around it.

use std::path::PathBuf;
use std::time::Duration;

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::OsRng;
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use aberp_audit_ledger::session::crypto::SessionKey;
use aberp_audit_ledger::session::tsa::{MockTimestampAuthority, TimestampAuthority};
use aberp_audit_ledger::session::{
    heartbeat, open_service_session, recover_crashed_sessions, SessionContext,
};
use aberp_audit_ledger::{Actor, BinaryHash, Ledger, TenantId};

const SERVICE_KEY_LEN: usize = 32;
/// Boot delay before the first heartbeat — mirrors the snapshot daemon so a
/// crash loop never hammers the chain at startup.
const HEARTBEAT_BOOT_DELAY_SECS: u64 = 60;

/// Keychain `service` field for a tenant's audit-service key. Mirrors
/// `aberp.nav.<tenant>` (nav-transport) and `aberp.cad.<tenant>` (cad_blob).
pub fn service_name(tenant_id: &str) -> String {
    format!("aberp.audit_service.{tenant_id}")
}

/// Keychain item name for the service signing key.
pub const ITEM_AUDIT_SERVICE_KEY: &str = "audit_service_signing_key";

/// Load the per-tenant service signing key from the keychain, or mint +
/// store one on first boot. A malformed stored key is a LOUD error, never a
/// silent re-mint — re-minting would orphan the `ServiceSessionEndorsed`
/// record linking the key to its operator (ADR-0088, same posture as the
/// CAD key in ADR-0083).
pub fn load_or_provision_service_key(tenant_id: &str) -> Result<SessionKey> {
    // ADR-0100 Phase 1 — through the shared `SecretStore` seam. Like the
    // CAD key, this symmetric signing seed is one of the 8 categories
    // Phase 3's self-hosted backend must carry.
    use aberp_secret_store::SecretStore as _;
    let service = service_name(tenant_id);
    let store = aberp_secret_store::keychain_store();
    match store.get(&service, ITEM_AUDIT_SERVICE_KEY).map_err(|e| {
        anyhow!("audit-service-key keychain backend error for tenant {tenant_id}: {e}")
    })? {
        Some(b64) => {
            let raw = Zeroizing::new(
                base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .context("decode stored audit-service key (base64)")?,
            );
            let seed: [u8; SERVICE_KEY_LEN] = raw.as_slice().try_into().map_err(|_| {
                anyhow!(
                    "stored audit-service key for tenant {tenant_id} is {} bytes, expected \
                     {SERVICE_KEY_LEN} — refusing to re-mint (would orphan the service-session \
                     endorsement)",
                    raw.len()
                )
            })?;
            Ok(SessionKey::from_seed(&seed))
        }
        None => {
            let mut seed = Zeroizing::new([0u8; SERVICE_KEY_LEN]);
            OsRng.fill_bytes(seed.as_mut());
            let b64 = base64::engine::general_purpose::STANDARD.encode(seed.as_ref());
            store
                .set(&service, ITEM_AUDIT_SERVICE_KEY, &b64)
                .context("store freshly-minted audit-service key in keychain")?;
            Ok(SessionKey::from_seed(&seed))
        }
    }
}

/// Open the service session and run crash recovery against `ledger`.
/// Returns the service [`SessionContext`] (which owns the service key) so
/// the caller can hand it to the heartbeat actor. Keychain-/network-free —
/// the unit-testable core of the boot path.
pub fn open_service_session_and_recover(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    service_key: SessionKey,
) -> Result<SessionContext> {
    let (svc, _anchor) = open_service_session(ledger, tsa, actor.clone(), service_key)
        .map_err(|e| anyhow!("open service session: {e}"))?;
    let recovered = recover_crashed_sessions(ledger, tsa, actor, &svc)
        .map_err(|e| anyhow!("crash recovery: {e}"))?;
    if !recovered.is_empty() {
        tracing::warn!(
            count = recovered.len(),
            "crash recovery: closed {} orphan audit session(s) from a prior run (S441/ADR-0087)",
            recovered.len()
        );
    }
    Ok(svc)
}

/// Dependencies for the heartbeat actor.
pub struct HeartbeatDeps {
    pub db_path: PathBuf,
    pub tenant: TenantId,
    pub binary_hash: BinaryHash,
    pub actor: Actor,
    pub service: SessionContext,
    pub interval: Duration,
}

/// Supervised heartbeat loop (ADR-0087): every `interval`, reopen the
/// ledger and take a `Heartbeat` anchor under the service session. Mirrors
/// the snapshot daemon's log-but-survive posture — a TSA outage queues a
/// `pending` anchor and never blocks (handled inside `heartbeat`); a panic
/// stops the actor (the service ctx is unrecoverable).
pub async fn run_heartbeat_supervised(deps: HeartbeatDeps, cancel: CancellationToken) {
    tracing::info!(
        interval_secs = deps.interval.as_secs(),
        "spawned audit heartbeat actor (S441 / ADR-0087)"
    );
    tokio::select! {
        _ = cancel.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_secs(HEARTBEAT_BOOT_DELAY_SECS)) => {}
    }
    let HeartbeatDeps {
        db_path,
        tenant,
        binary_hash,
        actor,
        mut service,
        interval,
    } = deps;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let db = db_path.clone();
        let tn = tenant.clone();
        let act = actor.clone();
        // Move the service ctx into the blocking task and ALWAYS return it
        // (even on error) so the loop keeps its signing key across cycles.
        let outcome = tokio::task::spawn_blocking(move || {
            let res = (|| -> Result<()> {
                let mut ledger = Ledger::open(&db, tn, binary_hash)
                    .map_err(|e| anyhow!("reopen ledger for heartbeat: {e}"))?;
                let tsa = MockTimestampAuthority::new();
                heartbeat(&mut ledger, &tsa, act, &service)
                    .map_err(|e| anyhow!("take heartbeat anchor: {e}"))?;
                Ok(())
            })();
            (service, res)
        })
        .await;
        match outcome {
            Ok((s, Ok(()))) => service = s,
            Ok((s, Err(e))) => {
                service = s;
                tracing::error!(error = %e, "heartbeat cycle failed; actor continues");
            }
            Err(join) => {
                tracing::error!(error = %join, "heartbeat task panicked; actor stops");
                return;
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_audit_ledger::verify_chain_signed;

    fn ledger() -> Ledger {
        Ledger::open_in_memory(
            TenantId::new("dap-boot-test").unwrap(),
            BinaryHash::from_bytes([2u8; 32]),
        )
        .unwrap()
    }
    fn actor() -> Actor {
        Actor::from_local_cli("proc".to_string(), "svc")
    }

    #[test]
    fn service_name_mirrors_keychain_convention() {
        assert_eq!(service_name("prod"), "aberp.audit_service.prod");
        assert_eq!(ITEM_AUDIT_SERVICE_KEY, "audit_service_signing_key");
    }

    #[test]
    fn boot_opens_service_session_and_chain_verifies() {
        let mut l = ledger();
        let tsa = MockTimestampAuthority::new();
        let key = SessionKey::from_seed(&[5u8; 32]);
        let svc = open_service_session_and_recover(&mut l, &tsa, actor(), key).unwrap();
        assert!(svc.session_id.starts_with("svc_"));

        let kinds: Vec<&str> = l
            .entries()
            .unwrap()
            .iter()
            .map(|e| e.kind.as_str())
            .collect();
        assert!(kinds.contains(&"auth.service_session_opened"));

        // Whole chain (base + signatures + anchors) verifies green.
        let entries = l.entries().unwrap();
        let anchors = l.anchors().unwrap();
        let verify_tsa = MockTimestampAuthority::new();
        let verdict = verify_chain_signed(
            l.tenant_id(),
            &entries,
            &anchors,
            |_e| None,
            |id| {
                if id == verify_tsa.identifier() {
                    Some(&verify_tsa as &dyn TimestampAuthority)
                } else {
                    None
                }
            },
        )
        .unwrap();
        assert!(verdict.fully_anchored);
    }
}
