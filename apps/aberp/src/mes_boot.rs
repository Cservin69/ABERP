//! Stage 3 manufacturing-adapter framework boot wiring
//! (S229 / PR-225 / ADR-0060 Phase β).
//!
//! Parses per-adapter env-var config, constructs the adapter, calls
//! `start()`, spawns the per-adapter ledger-writer task, and spawns a
//! cancellation-watcher that calls `adapter.stop()` when the shutdown
//! coordinator's root token fires.
//!
//! ## Why env vars and not seller.toml
//!
//! Per ADR-0060 §"Open questions → Operator-configurable adapter
//! registration", a `[mes]` section in `seller.toml` is the documented
//! long-term home. Landing it requires updating the four
//! [[seller-toml-write-invariant]] preservation paths (identity /
//! banks / smtp / numbering) AND the snapshot tool AND the runbook —
//! a substantial PR in its own right, deliberately separated from
//! Phase β per [[pushback-as-method]].
//!
//! Phase β uses env vars to gate adapter presence. The pattern mirrors
//! `ABERP_QUOTE_INTAKE_ENABLED=true` (S210). Default-off; production
//! runs that don't set the env var see no adapter, no port bound, no
//! ledger writer. Per [[trust-code-not-operator]] the DoS bounds
//! (`max_payload_len`, `max_concurrent_connections`) are NOT exposed
//! as env vars — only `scanner_id` / `host` / `port`.

use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use aberp_audit_ledger::{BinaryHash, TenantId};
use aberp_mes::{
    spawn_ledger_writer, Adapter, BarcodeScannerAdapter, BarcodeScannerConfig, LedgerWriterActor,
    LedgerWriterDeps, DEFAULT_LISTEN_PORT,
};

const ENV_BARCODE_ENABLED: &str = "ABERP_BARCODE_SCANNER_ENABLED";
const ENV_BARCODE_ID: &str = "ABERP_BARCODE_SCANNER_ID";
const ENV_BARCODE_HOST: &str = "ABERP_BARCODE_SCANNER_HOST";
const ENV_BARCODE_PORT: &str = "ABERP_BARCODE_SCANNER_PORT";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_SCANNER_ID: &str = "barcode-scanner-default";

/// Shared dependencies the MES boot path threads into each spawned
/// ledger-writer task. Built from the existing `recovery_state` at
/// the boot call site (`db_path` / `tenant` / `binary_hash`) +
/// operator session info.
#[derive(Debug, Clone)]
pub struct MesBootDeps {
    pub db_path: PathBuf,
    pub tenant: TenantId,
    pub binary_hash: BinaryHash,
    pub operator_login: String,
    pub session_id: String,
}

/// Outcome of booting the MES adapter set: the spawned task handles
/// the caller must register with the shutdown coordinator. Labels are
/// `&'static str` to match the coordinator's `register` signature; the
/// per-scanner identity is logged separately at spawn time and is not
/// needed inside the shutdown summary line.
#[derive(Debug)]
pub struct SpawnedMesTasks {
    pub handles: Vec<(&'static str, JoinHandle<()>)>,
}

/// Boot the MES adapter set as configured by env vars. Returns
/// `Ok(None)` when no adapter is enabled — boot proceeds silently.
///
/// On success the registered tasks fan out from `cancel`: every
/// spawned task respects `cancel.cancelled()` so a Tauri-window close
/// or a Ctrl-C exits within ms.
pub async fn boot_mes_adapters(
    deps: MesBootDeps,
    cancel: CancellationToken,
) -> Result<Option<SpawnedMesTasks>> {
    if !barcode_scanner_enabled() {
        tracing::info!(
            "MES barcode-scanner adapter disabled ({ENV_BARCODE_ENABLED} != true); skipping"
        );
        return Ok(None);
    }

    let cfg =
        read_barcode_scanner_config_from_env().context("read barcode-scanner config from env")?;
    let scanner_id = cfg.scanner_id.clone();
    let listen_addr = cfg.listen_addr;
    let listen_port = cfg.listen_port;
    tracing::info!(
        scanner_id = %scanner_id,
        listen_addr = %listen_addr,
        listen_port,
        "spawning MES barcode-scanner adapter (S229 / PR-225)"
    );

    let adapter: Arc<BarcodeScannerAdapter> = Arc::new(BarcodeScannerAdapter::new(cfg));
    adapter
        .start()
        .await
        .with_context(|| format!("barcode scanner adapter '{scanner_id}' start failed"))?;

    let adapter_for_writer: Arc<dyn Adapter> = adapter.clone();
    let writer_deps = LedgerWriterDeps {
        db_path: deps.db_path,
        tenant: deps.tenant,
        binary_hash: deps.binary_hash,
        actor: LedgerWriterActor {
            session_id: deps.session_id,
            operator_login: deps.operator_login,
        },
    };
    let writer_handle = spawn_ledger_writer(adapter_for_writer, writer_deps, cancel.clone());

    // Stopper: watches the same root cancel token; when it fires,
    // calls `adapter.stop()` so the listener task drains and the TCP
    // port releases. Registered separately so the shutdown summary
    // log line lists both halves.
    let stopper_adapter = adapter.clone();
    let stopper_cancel = cancel.clone();
    let stopper_handle = tokio::spawn(async move {
        stopper_cancel.cancelled().await;
        if let Err(e) = stopper_adapter.stop().await {
            tracing::warn!(
                scanner_id = %stopper_adapter.name(),
                error = %e,
                "MES barcode-scanner adapter stop failed during shutdown"
            );
        }
    });

    Ok(Some(SpawnedMesTasks {
        handles: vec![
            ("mes-barcode-scanner-writer", writer_handle),
            ("mes-barcode-scanner-stopper", stopper_handle),
        ],
    }))
}

fn barcode_scanner_enabled() -> bool {
    std::env::var(ENV_BARCODE_ENABLED)
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// One adapter's intent-snapshot for the operator dashboard tile
/// (PR-231 / S235). Reflects env-var configuration: what the boot
/// path INTENDED to start, not a live registry probe. Live health
/// is a separate refactor (the [`AdapterRegistry`] currently lives
/// in [`boot_mes_adapters`]'s scope, not on `AppState`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterStatusSnapshot {
    /// Stable identifier (matches the registry key when the adapter
    /// boots — `barcode_scanner_id` env-var or its default).
    pub name: String,
    /// `"enabled"` when the adapter's `..._ENABLED=true` env var is set,
    /// otherwise `"disabled"`. Closed vocab.
    pub status: &'static str,
    /// Kind label for the SPA — `"barcode-scanner"` etc. Lets the
    /// dashboard group / icon by family if more adapters land.
    pub kind: &'static str,
    /// Listen host (configured via env). Surfaced so the operator can
    /// confirm the bind address from the dashboard without opening a
    /// terminal.
    pub host: String,
    /// Listen port (configured via env).
    pub port: u16,
}

/// Snapshot every configurable MES adapter's env-var posture for the
/// operator dashboard. Always returns at least one row per
/// adapter-family we ship (currently barcode-scanner — S229); families
/// the operator hasn't enabled show `status: "disabled"` so the tile
/// surfaces them as "configured but off" rather than hiding them.
pub fn snapshot_mes_adapter_config() -> Vec<AdapterStatusSnapshot> {
    let scanner_id =
        std::env::var(ENV_BARCODE_ID).unwrap_or_else(|_| DEFAULT_SCANNER_ID.to_string());
    let host = std::env::var(ENV_BARCODE_HOST).unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port = std::env::var(ENV_BARCODE_PORT)
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(aberp_mes::DEFAULT_LISTEN_PORT);
    let status = if barcode_scanner_enabled() {
        "enabled"
    } else {
        "disabled"
    };
    vec![AdapterStatusSnapshot {
        name: scanner_id,
        status,
        kind: "barcode-scanner",
        host,
        port,
    }]
}

fn read_barcode_scanner_config_from_env() -> Result<BarcodeScannerConfig> {
    let scanner_id =
        std::env::var(ENV_BARCODE_ID).unwrap_or_else(|_| DEFAULT_SCANNER_ID.to_string());
    if scanner_id.trim().is_empty() {
        return Err(anyhow!(
            "{ENV_BARCODE_ID} is empty; refusing to start scanner with anonymous name"
        ));
    }

    let host_str = std::env::var(ENV_BARCODE_HOST).unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let listen_addr = IpAddr::from_str(&host_str)
        .with_context(|| format!("parse {ENV_BARCODE_HOST}={host_str}"))?;

    let listen_port = match std::env::var(ENV_BARCODE_PORT) {
        Ok(s) => s
            .parse::<u16>()
            .with_context(|| format!("parse {ENV_BARCODE_PORT}={s}"))?,
        Err(_) => DEFAULT_LISTEN_PORT,
    };

    let mut cfg = BarcodeScannerConfig::new(scanner_id);
    cfg.listen_addr = listen_addr;
    cfg.listen_port = listen_port;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: env vars are process-global; these tests serialize on a
    // shared mutex to avoid cross-test cross-talk. The set of tests is
    // small enough that the serialisation overhead is negligible.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for k in [
            ENV_BARCODE_ENABLED,
            ENV_BARCODE_ID,
            ENV_BARCODE_HOST,
            ENV_BARCODE_PORT,
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn enabled_defaults_to_false() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        assert!(!barcode_scanner_enabled());
    }

    #[test]
    fn enabled_is_case_insensitive_true() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(ENV_BARCODE_ENABLED, "TRUE");
        assert!(barcode_scanner_enabled());
        std::env::set_var(ENV_BARCODE_ENABLED, "True");
        assert!(barcode_scanner_enabled());
        std::env::set_var(ENV_BARCODE_ENABLED, "true");
        assert!(barcode_scanner_enabled());
        std::env::set_var(ENV_BARCODE_ENABLED, "false");
        assert!(!barcode_scanner_enabled());
        clear_env();
    }

    #[test]
    fn config_from_env_uses_documented_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let cfg = read_barcode_scanner_config_from_env().unwrap();
        assert_eq!(cfg.scanner_id, DEFAULT_SCANNER_ID);
        assert_eq!(cfg.listen_port, DEFAULT_LISTEN_PORT);
        assert_eq!(cfg.listen_addr, IpAddr::from_str(DEFAULT_HOST).unwrap());
    }

    #[test]
    fn config_from_env_picks_up_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(ENV_BARCODE_ID, "barcode-scanner-receiving-dock");
        std::env::set_var(ENV_BARCODE_HOST, "0.0.0.0");
        std::env::set_var(ENV_BARCODE_PORT, "9100");
        let cfg = read_barcode_scanner_config_from_env().unwrap();
        assert_eq!(cfg.scanner_id, "barcode-scanner-receiving-dock");
        assert_eq!(cfg.listen_port, 9100);
        assert_eq!(cfg.listen_addr, IpAddr::from_str("0.0.0.0").unwrap());
        clear_env();
    }

    #[test]
    fn config_from_env_rejects_blank_scanner_id() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(ENV_BARCODE_ID, "   ");
        let err = read_barcode_scanner_config_from_env().unwrap_err();
        assert!(err.to_string().contains(ENV_BARCODE_ID));
        clear_env();
    }

    #[test]
    fn config_from_env_rejects_malformed_port() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(ENV_BARCODE_PORT, "not-a-number");
        let err = read_barcode_scanner_config_from_env().unwrap_err();
        assert!(err.to_string().contains(ENV_BARCODE_PORT));
        clear_env();
    }

    #[test]
    fn config_from_env_rejects_malformed_host() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(ENV_BARCODE_HOST, "not::an::ip::!!");
        let err = read_barcode_scanner_config_from_env().unwrap_err();
        assert!(err.to_string().contains(ENV_BARCODE_HOST));
        clear_env();
    }
}
