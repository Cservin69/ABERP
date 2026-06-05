//! [`ZebraAdapter`] — Zebra-protocol thermal label printer adapter
//! (S245 / PR-238 / ADR-0060 Phase δ — first hardware-output adapter).
//!
//! ## Why a Zebra adapter
//!
//! Stage 3 Dispatch (S234 / PR-230) ships shipping labels alongside work
//! orders; Inventory (S231 / PR-227) emits bin/lot/product labels. Both
//! code paths need a vendor-pluggable way to push raw label-print bytes
//! to a thermal printer without taking on a vendor SDK.
//!
//! ZPL II is an open ASCII control language — every Zebra-protocol
//! thermal printer (Zebra ZD/ZT/GK series + the dozens of ZPL-compatible
//! clones from Honeywell / Citizen / TSC) accepts raw ZPL over a TCP
//! socket on port 9100. No vendor lock-in per the
//! [[spacex-vertical-integration]] memo; the adapter trades on the open
//! protocol, not on a proprietary toolkit.
//!
//! ## Wire shape
//!
//! - Connect: TCP to `host:port` (Zebra's well-known port is 9100).
//! - Write: raw ZPL II bytes as supplied by the caller — this adapter
//!   does NOT compose ZPL. A typed builder is queued for v2 (see
//!   "What this adapter does NOT do" below).
//! - Close: graceful FIN — Zebra firmware acks the print job on EOF.
//!
//! ## Health model
//!
//! - Health probe = TCP connect (with timeout) to the printer's port.
//!   No bytes are written; the probe answers "is the printer listening?".
//! - Probe runs once on `start()` (sets the initial health) and then
//!   every `probe_interval` (default 30s) while the adapter is running.
//! - Connect succeeds within `slow_threshold_ms` → `Healthy`.
//! - Connect succeeds but takes longer → `Degraded { reason: "slow probe NNNms" }`.
//! - Connect refused / errored / timed out → `Unhealthy { reason: … }`.
//!
//! Per [[trust-code-not-operator]] the operator does not need to notice
//! that the printer went down — the next probe flips health and the
//! Workshop dashboard tile (S240 / PR-234) lights red.
//!
//! ## Print path
//!
//! `print_zpl` opens its own TCP connection (separate from the probe
//! socket), writes the ZPL, and closes. On a transient mid-write error
//! the call retries ONCE with a brief backoff per the
//! [[trust-code-not-operator]] auto-reconnect posture. Persistent failure
//! returns [`AdapterError::OperationFailed`] and flips health to
//! `Unhealthy` so subsequent probes don't have to wait the full 30s
//! interval to notice.
//!
//! ## What this adapter does NOT do (v1 — queued for follow-ups)
//!
//! Tracked as PR-238 TODOs:
//!
//! - **ZPL template engine** — v1 callers pass raw ZPL strings; a typed
//!   ZPL builder (with field-merge guards against injection from
//!   operator-typed strings) is a separate PR.
//! - **SBPL / EPL fallback dialects** — Zebra-only. Per
//!   [[spacex-vertical-integration]] one protocol beats a polymorphic
//!   facade that pretends to abstract over four incompatible wire
//!   shapes.
//! - **Print queue persistence across restarts** — v1 is
//!   fire-and-forget; the Dispatch caller can re-issue print_zpl on
//!   next boot if it observes a missing label-printed audit entry.
//! - **SGD status-back queries** (out-of-labels, head-open, etc.) —
//!   Zebra exposes via the `~HS` / `! U1 getvar` family; deferred until
//!   a real production cell asks for it.
//!
//! ## DoS bounds (per [[trust-code-not-operator]])
//!
//! Hard limits enforced in code, not via operator config:
//!
//! - `max_payload_len` (default 65_536) — caps the ZPL string a single
//!   `print_zpl` invocation may carry. Above this the call returns
//!   `AdapterError::OperationFailed` and no bytes go on the wire.
//!   Realistic ZPL labels are a few hundred bytes; 64 KiB is generous
//!   even when a small graphic is embedded.
//! - `connect_timeout` (default 2s) — cap on connect-call blocking; the
//!   tokio `timeout` future races the connect attempt.
//!
//! ## Lifecycle
//!
//! `start()` runs the initial probe synchronously (so the first
//! `health()` call after start observes the real result) and then spawns
//! the periodic probe task. `stop()` cancels via [`CancellationToken`]
//! and waits for the probe task to drain. Both methods are idempotent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::adapter::{Adapter, AdapterHealth};
use crate::adapters::common::AdapterLifecycle;
use crate::error::AdapterError;
use crate::events::CanonicalEvent;

/// Default TCP port — Zebra's well-known raw-print port (the "RAW"
/// protocol every ZPL-compatible printer exposes by default). Same port
/// IPP printers use for raw mode.
pub const DEFAULT_LISTEN_PORT: u16 = 9100;

/// Default cap on a single ZPL payload. 64 KiB is generous for the
/// largest realistic label (a few hundred bytes of pure-text ZPL, or a
/// few KiB when a graphic is embedded). Above this the print call fails
/// fast — the caller is misusing the adapter or feeding it operator
/// input it never validated.
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 65_536;

/// Default cap on a connect attempt. 2s is long enough for a healthy
/// printer on the same LAN and short enough that the operator notices a
/// missing printer quickly. The tokio runtime races the connect future
/// against this timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(2000);

/// Default threshold above which a successful probe is reported as
/// `Degraded` rather than `Healthy`. 500ms catches printers reachable
/// but congested on a saturated shop-floor switch.
pub const DEFAULT_SLOW_THRESHOLD: Duration = Duration::from_millis(500);

/// Default interval between periodic health probes. Long enough that the
/// adapter is not chatty in the absence of print traffic, short enough
/// that the Workshop dashboard's 30s refresh tick (S240 / PR-234) lines
/// up with at most one probe-lag worth of staleness.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Default broadcast channel capacity. Carried for API parity with
/// [`BarcodeScannerConfig`](crate::BarcodeScannerConfig); the channel
/// stays empty in v1 because this adapter does not emit
/// [`CanonicalEvent`]s yet (label-printed events are queued for v2 alongside
/// the template engine).
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// Default backoff between the initial print attempt and the single
/// retry attempt on a transient mid-write error. Short — the retry is a
/// best-effort "did the printer just blink?" check, not a robust
/// reconnect-with-exponential-backoff loop.
pub const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Construction-time configuration for a [`ZebraAdapter`].
///
/// DoS bounds (`max_payload_len`, `connect_timeout`) are exposed only
/// so tests can shrink them; production paths use the `DEFAULT_*`
/// constants per [[trust-code-not-operator]].
#[derive(Debug, Clone)]
pub struct ZebraAdapterConfig {
    /// Stable identifier; becomes the adapter's [`Adapter::name`]. Used
    /// as the registry key + the `adapter_name` field on every
    /// audit-ledger entry the adapter produces. MUST be unique across
    /// registered adapters. Typical shape: `"label-printer-{station}"`
    /// (e.g. `"label-printer-dispatch-A"`).
    pub printer_id: String,
    /// Operator-readable display name surfaced on the Workshop
    /// dashboard tile. Distinct from `printer_id` so the operator can
    /// rename ("Dispatch — left bench") without disturbing the stable
    /// registry key.
    pub friendly_name: String,
    /// Printer host — IP address or DNS name. Resolved on each
    /// connect; no caching, so a DHCP lease move is picked up on the
    /// next probe.
    pub host: String,
    /// Printer TCP port. Production default is 9100; tests pass
    /// dynamically picked ephemeral ports.
    pub port: u16,
    pub max_payload_len: usize,
    pub connect_timeout: Duration,
    pub slow_threshold: Duration,
    pub probe_interval: Duration,
    pub retry_backoff: Duration,
    pub channel_capacity: usize,
}

impl ZebraAdapterConfig {
    /// Construct a config with default DoS bounds + probe timings; only
    /// `printer_id`, `friendly_name`, `host`, `port` are operator-
    /// meaningful. The default port (9100) covers every Zebra-protocol
    /// printer out of the box.
    pub fn new(
        printer_id: impl Into<String>,
        friendly_name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            printer_id: printer_id.into(),
            friendly_name: friendly_name.into(),
            host: host.into(),
            port,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            slow_threshold: DEFAULT_SLOW_THRESHOLD,
            probe_interval: DEFAULT_PROBE_INTERVAL,
            retry_backoff: DEFAULT_RETRY_BACKOFF,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// The Zebra-protocol thermal label printer [`Adapter`] implementation.
///
/// Clone-cheap via `Arc<ZebraAdapter>`. The internal state (lifecycle,
/// cached health, broadcast sender, probe-task handle) is interior-
/// mutable.
#[derive(Debug)]
pub struct ZebraAdapter {
    config: ZebraAdapterConfig,
    /// Cancel token, probe-task handle, and cached health — the shared
    /// task-lifecycle scaffolding (S259 / PR-248). The probe loop holds a
    /// clone of the health cell via [`AdapterLifecycle::health_slot`].
    lifecycle: AdapterLifecycle,
    sender: broadcast::Sender<CanonicalEvent>,
}

impl ZebraAdapter {
    /// Construct a stopped adapter ready for `start()`.
    pub fn new(config: ZebraAdapterConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity);
        Self {
            config,
            lifecycle: AdapterLifecycle::new(),
            sender,
        }
    }

    /// Operator-readable friendly name. Surfaces on the Workshop
    /// dashboard alongside the stable `printer_id`.
    pub fn friendly_name(&self) -> &str {
        &self.config.friendly_name
    }

    /// Send a raw ZPL payload to the printer. Opens a fresh TCP
    /// connection, writes every byte, closes — Zebra firmware
    /// interprets the FIN as the print-job boundary.
    ///
    /// On a transient mid-write error the call retries ONCE with
    /// `retry_backoff` of delay. Persistent failure returns
    /// [`AdapterError::OperationFailed`] AND flips the cached health to
    /// `Unhealthy` (so the dashboard tile reflects the failure without
    /// waiting for the next 30s periodic probe).
    ///
    /// Per the v1 doc-comment: the caller passes ZPL — the adapter does
    /// not compose, validate, or escape it.
    pub async fn print_zpl(&self, zpl: &str) -> Result<(), AdapterError> {
        if zpl.len() > self.config.max_payload_len {
            return Err(AdapterError::OperationFailed(format!(
                "ZPL payload size {} exceeds max_payload_len {} for printer '{}'",
                zpl.len(),
                self.config.max_payload_len,
                self.config.printer_id
            )));
        }

        let endpoint = self.config.endpoint();
        let bytes = zpl.as_bytes();

        // First attempt.
        match attempt_print(&endpoint, bytes, self.config.connect_timeout).await {
            Ok(()) => Ok(()),
            Err(first_err) => {
                tracing::warn!(
                    printer_id = %self.config.printer_id,
                    endpoint = %endpoint,
                    error = %first_err,
                    "print attempt 1/2 failed; retrying after backoff"
                );
                tokio::time::sleep(self.config.retry_backoff).await;

                match attempt_print(&endpoint, bytes, self.config.connect_timeout).await {
                    Ok(()) => Ok(()),
                    Err(retry_err) => {
                        let reason =
                            format!("print failed after retry: {first_err} (retry: {retry_err})");
                        // Flip cached health so the dashboard sees the
                        // failure before the next periodic probe runs.
                        self.lifecycle.set_health(AdapterHealth::Unhealthy {
                            reason: format!("print failed: {retry_err}"),
                        });
                        tracing::error!(
                            printer_id = %self.config.printer_id,
                            endpoint = %endpoint,
                            "print failed after 1 retry: {reason}"
                        );
                        Err(AdapterError::OperationFailed(reason))
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Adapter for ZebraAdapter {
    fn name(&self) -> &str {
        &self.config.printer_id
    }

    fn kind(&self) -> &'static str {
        "label-printer"
    }

    fn endpoint_host(&self) -> Option<String> {
        Some(self.config.host.clone())
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(self.config.port)
    }

    async fn start(&self) -> Result<(), AdapterError> {
        // Idempotent start guard: None means already running → no-op.
        let Some(cancel) = self.lifecycle.begin_start() else {
            return Ok(());
        };

        let endpoint = self.config.endpoint();
        let connect_timeout = self.config.connect_timeout;
        let slow_threshold = self.config.slow_threshold;

        // Initial probe runs synchronously so the caller's first
        // health() read sees the real result, not a transient Starting.
        let initial = probe_once(&endpoint, connect_timeout, slow_threshold).await;
        self.lifecycle.set_health(initial);

        let health_slot = self.lifecycle.health_slot();
        let printer_id = self.config.printer_id.clone();
        let probe_interval = self.config.probe_interval;

        let handle = tokio::spawn(async move {
            run_probe_loop(
                endpoint,
                cancel,
                probe_interval,
                connect_timeout,
                slow_threshold,
                health_slot,
                printer_id,
            )
            .await;
        });

        self.lifecycle.attach(handle);
        Ok(())
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        self.lifecycle.stop(&self.config.printer_id).await;
        Ok(())
    }

    fn health(&self) -> AdapterHealth {
        self.lifecycle.health()
    }

    fn subscribe(&self) -> broadcast::Receiver<CanonicalEvent> {
        self.sender.subscribe()
    }
}

async fn run_probe_loop(
    endpoint: String,
    cancel: CancellationToken,
    interval: Duration,
    connect_timeout: Duration,
    slow_threshold: Duration,
    health_slot: Arc<Mutex<AdapterHealth>>,
    printer_id: String,
) {
    let mut tick = tokio::time::interval(interval);
    // The first interval tick fires immediately; skip it (the initial
    // probe already ran synchronously in `start()`).
    tick.tick().await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!(printer_id = %printer_id, "probe loop cancelled");
                return;
            }
            _ = tick.tick() => {
                let next = probe_once(&endpoint, connect_timeout, slow_threshold).await;
                tracing::debug!(printer_id = %printer_id, endpoint = %endpoint, "probe result: {next:?}");
                *health_slot.lock().expect("health mutex poisoned") = next;
            }
        }
    }
}

/// Run a single TCP connect probe and translate the outcome into an
/// [`AdapterHealth`] snapshot. Bytes are never written; this is purely
/// a "is the printer listening" liveness check.
async fn probe_once(
    endpoint: &str,
    connect_timeout: Duration,
    slow_threshold: Duration,
) -> AdapterHealth {
    let start = std::time::Instant::now();
    let connect_fut = TcpStream::connect(endpoint);
    match tokio::time::timeout(connect_timeout, connect_fut).await {
        Ok(Ok(stream)) => {
            let elapsed = start.elapsed();
            // Drop the probe socket immediately — the printer treats
            // an empty stream + FIN as a no-op.
            drop(stream);
            if elapsed > slow_threshold {
                AdapterHealth::Degraded {
                    reason: format!("slow probe {}ms", elapsed.as_millis()),
                }
            } else {
                AdapterHealth::Healthy
            }
        }
        Ok(Err(e)) => AdapterHealth::Unhealthy {
            reason: classify_io_error(&e),
        },
        Err(_elapsed) => AdapterHealth::Unhealthy {
            reason: format!("connect timeout after {}ms", connect_timeout.as_millis()),
        },
    }
}

/// Single print attempt: connect, write_all, shutdown. No retry inside —
/// caller orchestrates the retry-once-with-backoff dance.
async fn attempt_print(
    endpoint: &str,
    bytes: &[u8],
    connect_timeout: Duration,
) -> Result<(), std::io::Error> {
    let connect_fut = TcpStream::connect(endpoint);
    let mut stream = match tokio::time::timeout(connect_timeout, connect_fut).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connect timeout after {}ms", connect_timeout.as_millis()),
            ));
        }
    };
    stream.write_all(bytes).await?;
    // Graceful FIN — Zebra firmware uses the close as the print-job
    // boundary, so an explicit shutdown matters.
    stream.shutdown().await?;
    Ok(())
}

fn classify_io_error(e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::ConnectionRefused => "connection refused".to_string(),
        ErrorKind::TimedOut => "connection timed out".to_string(),
        ErrorKind::HostUnreachable => "host unreachable".to_string(),
        ErrorKind::NetworkUnreachable => "network unreachable".to_string(),
        ErrorKind::ConnectionReset => "connection reset".to_string(),
        ErrorKind::ConnectionAborted => "connection aborted".to_string(),
        ErrorKind::AddrNotAvailable => "address not available".to_string(),
        ErrorKind::NotFound => "host not found".to_string(),
        other => format!("io error: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// Pick an ephemeral port by binding 0 and dropping the listener —
    /// same TOCTOU-tolerant pattern as `barcode_scanner`'s helper.
    async fn pick_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    /// Spawn a mock printer that accepts one connection, reads bytes
    /// until EOF, and returns the bytes via a oneshot channel.
    async fn spawn_mock_printer(
        port: u16,
    ) -> (
        tokio::sync::oneshot::Receiver<Vec<u8>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _peer) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            socket.read_to_end(&mut buf).await.unwrap();
            let _ = tx.send(buf);
        });
        (rx, handle)
    }

    /// Mock printer that accepts a connection, reads N bytes, then
    /// abruptly closes (simulating a mid-write break). Subsequent
    /// `connect`s succeed normally.
    async fn spawn_mock_printer_break_after(
        port: u16,
        break_after: usize,
    ) -> tokio::task::JoinHandle<()> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move {
            // Round 1: accept, read N bytes, close abruptly.
            if let Ok((mut socket, _peer)) = listener.accept().await {
                let mut buf = vec![0u8; break_after];
                let _ = socket.read_exact(&mut buf).await;
                drop(socket);
            }
            // Round 2 (retry): accept, drain to EOF, close.
            if let Ok((mut socket, _peer)) = listener.accept().await {
                let mut buf = Vec::new();
                let _ = socket.read_to_end(&mut buf).await;
                drop(socket);
            }
        })
    }

    fn cfg_for_test(printer_id: &str, port: u16) -> ZebraAdapterConfig {
        ZebraAdapterConfig {
            printer_id: printer_id.to_string(),
            friendly_name: format!("Test {printer_id}"),
            host: "127.0.0.1".to_string(),
            port,
            // Tight bounds for tests.
            max_payload_len: 4096,
            connect_timeout: Duration::from_millis(500),
            slow_threshold: Duration::from_millis(250),
            probe_interval: Duration::from_millis(150),
            retry_backoff: Duration::from_millis(20),
            channel_capacity: 16,
        }
    }

    // ====== Unit: classify_io_error ======

    #[test]
    fn classify_io_error_pins_canonical_kinds() {
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "x");
        assert_eq!(classify_io_error(&refused), "connection refused");

        let timed = std::io::Error::new(std::io::ErrorKind::TimedOut, "x");
        assert_eq!(classify_io_error(&timed), "connection timed out");

        let reset = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "x");
        assert_eq!(classify_io_error(&reset), "connection reset");
    }

    // ====== Defaults ======

    #[test]
    fn config_defaults_match_documented_constants() {
        let cfg = ZebraAdapterConfig::new("p1", "Dispatch A", "10.0.0.5", 9100);
        assert_eq!(cfg.port, 9100);
        assert_eq!(cfg.max_payload_len, DEFAULT_MAX_PAYLOAD_LEN);
        assert_eq!(cfg.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(cfg.slow_threshold, DEFAULT_SLOW_THRESHOLD);
        assert_eq!(cfg.probe_interval, DEFAULT_PROBE_INTERVAL);
        assert_eq!(cfg.retry_backoff, DEFAULT_RETRY_BACKOFF);
        assert_eq!(cfg.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
    }

    // ====== Health: probe_once direct ======

    #[tokio::test]
    async fn probe_once_healthy_on_open_port() {
        let port = pick_free_port().await;
        // Bind a listener but never accept — connect still succeeds
        // because the kernel completes the SYN-ACK before user-space
        // accept().
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let h = probe_once(
            &format!("127.0.0.1:{port}"),
            Duration::from_millis(500),
            Duration::from_millis(250),
        )
        .await;
        assert_eq!(h, AdapterHealth::Healthy);
    }

    #[tokio::test]
    async fn probe_once_unhealthy_on_closed_port() {
        let port = pick_free_port().await;
        // No listener — connect should be refused immediately on
        // loopback.
        let h = probe_once(
            &format!("127.0.0.1:{port}"),
            Duration::from_millis(500),
            Duration::from_millis(250),
        )
        .await;
        match h {
            AdapterHealth::Unhealthy { reason } => {
                assert!(
                    reason.contains("refused")
                        || reason.contains("aborted")
                        || reason.contains("reset"),
                    "expected refused/aborted/reset, got: {reason}"
                );
            }
            other => panic!("expected Unhealthy, got {other:?}"),
        }
    }

    // ====== print_zpl: happy path ======

    #[tokio::test]
    async fn print_zpl_delivers_bytes_to_printer() {
        let port = pick_free_port().await;
        let (rx, _mock_handle) = spawn_mock_printer(port).await;

        let adapter = ZebraAdapter::new(cfg_for_test("dispatch-a", port));
        let zpl = "^XA^FO50,50^A0N,40,40^FDHello^FS^XZ";
        adapter.print_zpl(zpl).await.unwrap();

        // Mock printer should have received exactly the bytes we sent.
        let received = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("mock printer receives bytes in time")
            .expect("mock printer channel open");
        assert_eq!(received, zpl.as_bytes());
    }

    // ====== print_zpl: connection refused ======

    #[tokio::test]
    async fn print_zpl_returns_error_on_connection_refused() {
        let port = pick_free_port().await;
        // No listener. Adapter has not started, but print_zpl is a
        // direct call — it should fail-loud and flip health to
        // Unhealthy.
        let adapter = ZebraAdapter::new(cfg_for_test("ghost", port));

        let err = adapter
            .print_zpl("^XA^FDhi^FS^XZ")
            .await
            .expect_err("connect refused should surface");
        match err {
            AdapterError::OperationFailed(reason) => {
                assert!(reason.contains("retry"), "{reason}");
            }
            other => panic!("expected OperationFailed, got {other:?}"),
        }

        // Health should have flipped to Unhealthy as a side effect.
        match adapter.health() {
            AdapterHealth::Unhealthy { .. } => {}
            other => panic!("expected Unhealthy after failed print, got {other:?}"),
        }
    }

    // ====== print_zpl: oversize payload ======

    #[tokio::test]
    async fn print_zpl_rejects_oversized_payload() {
        let port = pick_free_port().await;
        // Provide a listener so the failure isn't a connect-error
        // false positive.
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let adapter = ZebraAdapter::new(cfg_for_test("oversize", port));
        let oversized = "A".repeat(4097); // cfg max_payload_len is 4096

        let err = adapter
            .print_zpl(&oversized)
            .await
            .expect_err("oversize should fail");
        match err {
            AdapterError::OperationFailed(reason) => {
                assert!(reason.contains("max_payload_len"), "{reason}");
            }
            other => panic!("expected OperationFailed, got {other:?}"),
        }
    }

    // ====== print_zpl: retry-once on mid-write break ======

    #[tokio::test]
    async fn print_zpl_retries_once_after_mid_write_break() {
        let port = pick_free_port().await;
        let _mock_handle = spawn_mock_printer_break_after(port, 4).await;

        let adapter = ZebraAdapter::new(cfg_for_test("flaky", port));
        let zpl = "^XA^FDPAYLOAD^FS^XZ";
        // First attempt should encounter a broken pipe / connection
        // reset mid-write; second attempt should succeed against the
        // re-accepted connection. Adapter must surface Ok.
        adapter.print_zpl(zpl).await.unwrap();
    }

    // ====== Lifecycle ======

    #[tokio::test]
    async fn lifecycle_stopped_starting_healthy_unhealthy_stopped() {
        // No listener — start() probes and observes Unhealthy.
        let port = pick_free_port().await;
        let adapter = ZebraAdapter::new(cfg_for_test("lifecycle-unhealthy", port));
        assert_eq!(adapter.health(), AdapterHealth::Stopped);

        adapter.start().await.unwrap();
        // After start: initial probe ran synchronously → Unhealthy.
        match adapter.health() {
            AdapterHealth::Unhealthy { .. } => {}
            other => panic!("expected Unhealthy after start vs closed port, got {other:?}"),
        }

        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
    }

    #[tokio::test]
    async fn lifecycle_start_against_open_port_flips_to_healthy() {
        let port = pick_free_port().await;
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();

        let adapter = ZebraAdapter::new(cfg_for_test("lifecycle-healthy", port));
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
    }

    #[tokio::test]
    async fn periodic_probe_flips_unhealthy_to_healthy_when_printer_appears() {
        let port = pick_free_port().await;

        // Start adapter against closed port. Initial probe → Unhealthy.
        let adapter = ZebraAdapter::new(cfg_for_test("appearing", port));
        adapter.start().await.unwrap();
        match adapter.health() {
            AdapterHealth::Unhealthy { .. } => {}
            other => panic!("expected Unhealthy, got {other:?}"),
        }

        // Now open the listener — within one probe_interval (150ms in
        // test cfg) the periodic probe should flip health to Healthy.
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut flipped = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if matches!(adapter.health(), AdapterHealth::Healthy) {
                flipped = true;
                break;
            }
        }
        assert!(flipped, "periodic probe should have flipped to Healthy");

        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn periodic_probe_flips_healthy_to_unhealthy_when_printer_disappears() {
        let port = pick_free_port().await;
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();

        let adapter = ZebraAdapter::new(cfg_for_test("disappearing", port));
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);

        // Drop the listener — the next probe should fail.
        drop(listener);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut flipped = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if matches!(adapter.health(), AdapterHealth::Unhealthy { .. }) {
                flipped = true;
                break;
            }
        }
        assert!(flipped, "periodic probe should have flipped to Unhealthy");

        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn start_is_idempotent() {
        let port = pick_free_port().await;
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();

        let adapter = ZebraAdapter::new(cfg_for_test("idem-start", port));
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let port = pick_free_port().await;
        let _l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();

        let adapter = ZebraAdapter::new(cfg_for_test("idem-stop", port));
        adapter.stop().await.unwrap();
        adapter.start().await.unwrap();
        adapter.stop().await.unwrap();
        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
    }

    // ====== Adapter trait surface ======

    #[tokio::test]
    async fn adapter_trait_metadata_fields_match_config() {
        let port = pick_free_port().await;
        let adapter = ZebraAdapter::new(cfg_for_test("meta", port));
        assert_eq!(adapter.name(), "meta");
        assert_eq!(adapter.kind(), "label-printer");
        assert_eq!(adapter.endpoint_host(), Some("127.0.0.1".to_string()));
        assert_eq!(adapter.endpoint_port(), Some(port));
        assert_eq!(adapter.friendly_name(), "Test meta");
    }

    #[tokio::test]
    async fn zebra_adapter_is_dyn_safe() {
        let port = pick_free_port().await;
        let adapter: Arc<dyn Adapter> = Arc::new(ZebraAdapter::new(cfg_for_test("dyn", port)));
        assert_eq!(adapter.name(), "dyn");
        assert_eq!(adapter.kind(), "label-printer");
    }
}
