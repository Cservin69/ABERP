//! Subprocess lifecycle for the embedded `aberp serve` instance.
//!
//! The Tauri shell owns the backend process the way a unit-tested
//! supervisor would: spawn it on startup, parse the handshake line
//! off stdout (`handshake::parse`), then keep the `Child` alive for
//! the lifetime of the shell. Stderr is pumped to our own
//! `tracing::info!` lines verbatim — the backend's `tracing-subscriber`
//! formats human-readable logs that operators expect to see in the
//! same window.
//!
//! # Why not Tauri's shell plugin
//!
//! Tauri 2's `tauri-plugin-shell` exposes `shell::execute` to the
//! webview. ADR-0007 §"Tauri allow-list" explicitly forbids
//! `shell::all`, and the SPA has no need to launch arbitrary
//! programs. Spawning via `tokio::process::Command` from the Rust
//! setup hook keeps the subprocess off the SPA-reachable surface.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::handshake::{self, Handshake};

/// Wait at most this long for `aberp serve` to print its handshake
/// line. Cold-start usually completes in under 200ms; a 10s ceiling
/// is generous for the slowest first-launch (cert generation, DuckDB
/// schema migration). Beyond that we loud-fail per rule 12.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of a successful subprocess launch.
pub struct StartedBackend {
    /// The base URL (`https://127.0.0.1:<port>`) for every subsequent
    /// HTTPS call.
    pub url: String,
    /// Hex SHA-256 fingerprint of the loopback cert DER.
    pub fingerprint_hex: String,
    /// Kept around for kill-on-drop. We never need to read from it
    /// again after the handshake; the stderr drain owns its `tokio`
    /// task.
    pub child: Arc<Mutex<Child>>,
}

/// The state-bearing handle the Tauri command surface holds onto.
pub struct BackendHandle {
    pub url: String,
    pub session_token: String,
    pub client: reqwest::Client,
    pub tenant: String,
    #[allow(dead_code)]
    // Kept alive for the lifetime of the Tauri shell so the child is
    // killed when the shell exits. `tokio::process::Child::drop`
    // already kills by default; the Arc<Mutex<_>> shape lets a
    // future PR add a graceful-shutdown command without re-plumbing.
    child: Arc<Mutex<Child>>,
}

impl BackendHandle {
    pub fn new(
        started: StartedBackend,
        session_token: String,
        client: reqwest::Client,
        tenant: String,
    ) -> Self {
        BackendHandle {
            url: started.url,
            session_token,
            client,
            tenant,
            child: started.child,
        }
    }
}

/// Spawn `aberp serve --tenant <tenant> --db <db> --port 0` and parse
/// the handshake.
pub async fn spawn(aberp_bin: &Path, tenant: &str, db_path: &str) -> Result<StartedBackend> {
    tracing::info!(
        bin = %aberp_bin.display(),
        tenant = %tenant,
        db = %db_path,
        "spawning aberp serve subprocess"
    );

    let mut cmd = Command::new(aberp_bin);
    cmd.arg("serve")
        .arg("--tenant")
        .arg(tenant)
        .arg("--db")
        .arg(db_path)
        .arg("--port")
        .arg("0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn `{} serve --tenant {} --db {} --port 0`",
            aberp_bin.display(),
            tenant,
            db_path
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("subprocess stdout pipe not opened — Stdio::piped did not stick"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("subprocess stderr pipe not opened — Stdio::piped did not stick"))?;

    // Pump stderr verbatim to our own tracing. Backend logs are useful
    // to surface in the same window the operator runs the Tauri shell
    // in — debugging a "the SPA shows no invoices" report without the
    // backend's log stream is the kind of failure rule 12 names.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(target: "aberp.serve", "{line}");
        }
    });

    // Parse the handshake off stdout, time-bounded.
    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, wait_for_handshake_line(stdout))
        .await
        .map_err(|_| {
            anyhow!(
                "aberp serve did not print its handshake within {:?}",
                HANDSHAKE_TIMEOUT
            )
        })?
        .context("read handshake line from aberp serve stdout")?;

    Ok(StartedBackend {
        url: handshake.url,
        fingerprint_hex: handshake.fingerprint_hex,
        child: Arc::new(Mutex::new(child)),
    })
}

/// Read stdout line-by-line until we see a parseable handshake line
/// or stdout closes. Any non-handshake lines are forwarded to
/// `tracing::info!` so an operator running the shell with
/// `RUST_LOG=debug` can see the backend's startup output verbatim.
async fn wait_for_handshake_line<R>(stdout: R) -> Result<Handshake>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .context("read line from aberp serve stdout")?
    {
        if let Ok(handshake) = handshake::parse(&line) {
            return Ok(handshake);
        }
        // Non-handshake stdout lines are passed through; the backend
        // may print other things in the future (e.g. tenant DB
        // migration notes) and silencing them would hide real
        // signal.
        tracing::info!(target: "aberp.serve.stdout", "{line}");
    }
    Err(anyhow!(
        "aberp serve stdout closed before the handshake line appeared"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Conformance: the handshake-line reader must reject a stdout
    /// that closes without ever emitting a handshake. This is the
    /// "backend started, exited immediately" failure mode — typically
    /// a NAV credential gap or a DuckDB schema error. Loud per rule 12.
    #[tokio::test]
    async fn empty_stdout_loud_fails() {
        let empty: &[u8] = b"";
        let r = wait_for_handshake_line(empty).await;
        assert!(r.is_err(), "empty stdout must loud-fail");
    }

    /// Conformance: a stream that prints noise then closes also
    /// fails. The Tauri shell does NOT silently fall through to a
    /// default URL — it depends on the printed fingerprint for TLS
    /// trust.
    #[tokio::test]
    async fn noise_then_eof_loud_fails() {
        let noise: &[u8] = b"warming up the duckdb...\nready\n";
        let r = wait_for_handshake_line(noise).await;
        assert!(r.is_err(), "stdout without handshake must loud-fail");
    }

    /// Conformance: a handshake line wins even with chatter ahead
    /// of it.
    #[tokio::test]
    async fn handshake_preceded_by_noise_is_accepted() {
        let fp = hex::encode([0xa5u8; 32]);
        let mut buf = b"some startup chatter\nmore chatter\n".to_vec();
        buf.extend_from_slice(
            format!("aberp serve: https://127.0.0.1:12345/ (fingerprint sha256:{fp})\n").as_bytes(),
        );
        let buf_slice: &[u8] = &buf;
        let parsed = wait_for_handshake_line(buf_slice)
            .await
            .expect("handshake after chatter must parse");
        assert_eq!(parsed.port, 12345);
        assert_eq!(parsed.fingerprint_hex, fp);
    }
}
