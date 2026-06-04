//! [`BarcodeScannerAdapter`] — the first real Stage 3 adapter
//! (S229 / PR-225 / ADR-0060 Phase β).
//!
//! ## Why barcode scanner first
//!
//! Per ADR-0060's open question and the Stage 3 research README's
//! standing recommendation: barcode scanners are (a) the simplest
//! concrete adapter to validate the trait against a real device
//! interface, (b) immediately useful on the shop floor before any CNC
//! arrives, (c) the triggering input for every other Stage 3 surface
//! (FMS robots, Renishaw QC, packaging dispatch).
//!
//! ## What this adapter does
//!
//! It listens on a TCP socket for line-delimited UTF-8 payloads. Most
//! industrial barcode / QR scanners (Cognex DataMan, Datalogic Matrix,
//! Honeywell HF810) emit decoded strings over plain TCP with `\r\n`
//! framing — the device is the client, ABERP is the server. Many
//! support multiple concurrent network connections; this adapter
//! accepts them up to a hard-coded DoS cap.
//!
//! ## What this adapter does NOT do (yet)
//!
//! Two future modes are flagged but out-of-scope for PR-225:
//!
//! - **HID wedge mode** (Phase β.2) — small handheld scanners that
//!   present as USB HID and "type" the barcode. Needs OS-specific HID
//!   plumbing.
//! - **MQTT / OPC-UA network scanners** (Phase β.3) — modern
//!   smart-camera scanners that publish to a broker. Needs an MQTT
//!   client crate or the OPC-UA protocol stack landing first.
//!
//! ## Wire shape (TCP source mode)
//!
//! - Each accepted TCP connection sends one or more line-delimited
//!   UTF-8 payloads. Lines are terminated by `\n` or `\r\n`.
//! - The AIM Symbology ID prefix (`]` + 2 chars) is the industry
//!   standard barcode symbology header (`]C0` = Code128, `]Q1` = QR,
//!   etc.). When present at the start of a line, the prefix is
//!   stripped from the emitted payload and the symbology surfaces on
//!   the [`CanonicalEvent::ScanReceived`] event's `symbology` field.
//!   See [`aim_id_to_symbology`].
//! - Lines exceeding `max_payload_len` cause the connection to drop
//!   (DoS guard). The adapter stays running; the scanner can reconnect.
//! - Non-UTF-8 lines are dropped silently with a `warn!` log; the
//!   connection stays open for the next line (some scanners send a
//!   stray binary message at session start).
//! - Empty lines (blank `\r\n`) are skipped.
//!
//! ## Lifecycle
//!
//! `start()` binds the TCP listener and spawns the accept loop. `stop()`
//! cancels the listener task via [`CancellationToken`], waits for it to
//! drain, and releases the port. Both methods are idempotent.
//!
//! ## DoS bounds (per [[trust-code-not-operator]])
//!
//! Hard limits enforced in code, not via operator config:
//!
//! - `max_payload_len` (default 4096) — caps per-line bytes a peer can
//!   force the adapter to allocate before the connection is dropped.
//! - `max_concurrent_connections` (default 8) — caps the number of
//!   simultaneously held peer connections via a [`Semaphore`]. Excess
//!   peers see TCP accept + immediate close.
//!
//! These are configurable on the [`BarcodeScannerConfig`] for tests but
//! the defaults stand in production — adapter authors should not
//! expose them as operator knobs.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapter::{Adapter, AdapterHealth};
use crate::error::AdapterError;
use crate::events::CanonicalEvent;

/// Default TCP port — Cognex DataMan's well-known telnet-style server
/// port. Other vendors are configurable per device; 5800 is the
/// reasonable default for a single-device shop floor.
pub const DEFAULT_LISTEN_PORT: u16 = 5800;

/// Default per-line payload cap. 4096 bytes covers the longest realistic
/// QR / DataMatrix payload (a few hundred bytes typical) with headroom.
/// Above this the connection is dropped — protects against a stuck
/// device blasting a never-newline-terminated stream.
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 4096;

/// Default per-adapter concurrent-connection cap. Industrial cells
/// rarely exceed one or two devices per scanner adapter; 8 is generous.
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 8;

/// Default broadcast channel capacity. Per ADR-0060 §"Open questions
/// → broadcast channel sizing", the framework standard is `bounded(1024)`
/// per-adapter — generous for shop-floor scan rates (a fast scanner
/// emits a few scans per second; 1024 = ~5 min buffering at 3/sec).
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

const STATE_STOPPED: u8 = 0;
const STATE_STARTING: u8 = 1;
const STATE_HEALTHY: u8 = 2;
const STATE_UNHEALTHY: u8 = 3;

/// Construction-time configuration for a [`BarcodeScannerAdapter`].
///
/// DoS bounds (`max_payload_len`, `max_concurrent_connections`) are
/// exposed only so tests can shrink them; production paths use the
/// `DEFAULT_*` constants per [[trust-code-not-operator]].
#[derive(Debug, Clone)]
pub struct BarcodeScannerConfig {
    /// Stable identifier; becomes the adapter's [`Adapter::name`]. Used
    /// as the registry key + the `adapter_name` field on every
    /// audit-ledger entry the adapter produces. MUST be unique across
    /// registered adapters. Typical shape:
    /// `"barcode-scanner-{station}"` (e.g.
    /// `"barcode-scanner-receiving-dock"`).
    pub scanner_id: String,
    pub listen_addr: IpAddr,
    pub listen_port: u16,
    pub max_payload_len: usize,
    pub max_concurrent_connections: usize,
    pub channel_capacity: usize,
}

impl BarcodeScannerConfig {
    /// Construct a config with default DoS bounds; only `scanner_id` +
    /// `listen_port` are operator-meaningful (production cells use the
    /// 127.0.0.1 default for in-process testing OR an explicit
    /// `0.0.0.0` when the scanner reaches across a real network).
    pub fn new(scanner_id: impl Into<String>) -> Self {
        Self {
            scanner_id: scanner_id.into(),
            listen_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listen_port: DEFAULT_LISTEN_PORT,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
            max_concurrent_connections: DEFAULT_MAX_CONCURRENT_CONNECTIONS,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.listen_addr, self.listen_port)
    }
}

/// The barcode scanner [`Adapter`] implementation.
///
/// Clone-cheap via `Arc<BarcodeScannerAdapter>`. The internal state
/// (lifecycle, broadcast sender, peer counter) is interior-mutable.
#[derive(Debug)]
pub struct BarcodeScannerAdapter {
    config: BarcodeScannerConfig,
    state: AtomicU8,
    sender: broadcast::Sender<CanonicalEvent>,
    cancel: Mutex<Option<CancellationToken>>,
    listener_handle: Mutex<Option<JoinHandle<()>>>,
    peer_count: Arc<AtomicUsize>,
}

impl BarcodeScannerAdapter {
    /// Construct a stopped adapter ready for `start()`.
    pub fn new(config: BarcodeScannerConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity);
        Self {
            config,
            state: AtomicU8::new(STATE_STOPPED),
            sender,
            cancel: Mutex::new(None),
            listener_handle: Mutex::new(None),
            peer_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Snapshot of currently held peer connections. Useful for the
    /// future operations dashboard surface.
    pub fn peer_count(&self) -> usize {
        self.peer_count.load(Ordering::SeqCst)
    }

    /// Resolved listen address (host + port). Useful for tests that
    /// pass `listen_port = 0` to bind a free port and then need to know
    /// the resolved port to connect to.
    pub fn configured_addr(&self) -> SocketAddr {
        self.config.socket_addr()
    }
}

#[async_trait]
impl Adapter for BarcodeScannerAdapter {
    fn name(&self) -> &str {
        &self.config.scanner_id
    }

    fn kind(&self) -> &'static str {
        "barcode-scanner"
    }

    fn endpoint_host(&self) -> Option<String> {
        Some(self.config.listen_addr.to_string())
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(self.config.listen_port)
    }

    async fn start(&self) -> Result<(), AdapterError> {
        // Idempotent: Stopped/Unhealthy → Starting → Healthy.
        // Healthy/Starting stays put.
        match self.state.compare_exchange(
            STATE_STOPPED,
            STATE_STARTING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => {}
            Err(STATE_HEALTHY) | Err(STATE_STARTING) => return Ok(()),
            Err(_) => {
                // Recover from a prior Unhealthy via fresh CAS attempt.
                self.state.store(STATE_STARTING, Ordering::SeqCst);
            }
        }

        let addr = self.config.socket_addr();
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                self.state.store(STATE_STOPPED, Ordering::SeqCst);
                return Err(AdapterError::StartFailed(format!("bind {addr}: {e}")));
            }
        };

        let cancel = CancellationToken::new();
        *self.cancel.lock().expect("cancel mutex poisoned") = Some(cancel.clone());

        let sender = self.sender.clone();
        let scanner_id = self.config.scanner_id.clone();
        let max_payload_len = self.config.max_payload_len;
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_connections));
        let peer_count = self.peer_count.clone();

        let handle = tokio::spawn(async move {
            run_accept_loop(
                listener,
                cancel,
                sender,
                scanner_id,
                max_payload_len,
                semaphore,
                peer_count,
            )
            .await;
        });

        *self
            .listener_handle
            .lock()
            .expect("listener_handle mutex poisoned") = Some(handle);
        self.state.store(STATE_HEALTHY, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        // Take the cancel + handle under the lock, then drop the lock
        // before awaiting (the await would otherwise hold the lock
        // across a suspension point — `std::sync::Mutex` panics on
        // re-entry but more importantly would block subscribe()
        // callers).
        let cancel_opt = self.cancel.lock().expect("cancel mutex poisoned").take();
        let handle_opt = self
            .listener_handle
            .lock()
            .expect("listener_handle mutex poisoned")
            .take();

        if let Some(token) = cancel_opt {
            token.cancel();
        }
        if let Some(handle) = handle_opt {
            // A panicked accept loop or one that already exited returns
            // an error; either way the listener is gone — proceed to
            // mark Stopped. Log loud per CLAUDE.md rule 12 if the join
            // returned a panic.
            if let Err(e) = handle.await {
                if e.is_panic() {
                    tracing::error!(
                        scanner_id = %self.config.scanner_id,
                        "listener task panicked during stop: {e}"
                    );
                }
            }
        }

        self.state.store(STATE_STOPPED, Ordering::SeqCst);
        Ok(())
    }

    fn health(&self) -> AdapterHealth {
        match self.state.load(Ordering::SeqCst) {
            STATE_STOPPED => AdapterHealth::Stopped,
            STATE_STARTING => AdapterHealth::Starting,
            STATE_HEALTHY => AdapterHealth::Healthy,
            STATE_UNHEALTHY => AdapterHealth::Unhealthy {
                reason: "listener task exited unexpectedly".to_string(),
            },
            other => AdapterHealth::Unhealthy {
                reason: format!("barcode scanner adapter in invalid state {other}"),
            },
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<CanonicalEvent> {
        self.sender.subscribe()
    }
}

async fn run_accept_loop(
    listener: TcpListener,
    cancel: CancellationToken,
    sender: broadcast::Sender<CanonicalEvent>,
    scanner_id: String,
    max_payload_len: usize,
    semaphore: Arc<Semaphore>,
    peer_count: Arc<AtomicUsize>,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!(scanner_id = %scanner_id, "accept loop cancelled; draining");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((socket, peer_addr)) => {
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                tracing::warn!(
                                    scanner_id = %scanner_id,
                                    peer = %peer_addr,
                                    "rejecting connection: max_concurrent_connections reached"
                                );
                                // Drop the socket — peer sees an
                                // immediate close. Loud log per
                                // CLAUDE.md rule 12 so the operator
                                // knows the cap fired.
                                drop(socket);
                                continue;
                            }
                        };
                        let sender = sender.clone();
                        let scanner_id = scanner_id.clone();
                        let peer_count = peer_count.clone();
                        let conn_cancel = cancel.child_token();
                        tokio::spawn(async move {
                            peer_count.fetch_add(1, Ordering::SeqCst);
                            handle_connection(
                                socket,
                                peer_addr,
                                scanner_id,
                                max_payload_len,
                                sender,
                                conn_cancel,
                            )
                            .await;
                            peer_count.fetch_sub(1, Ordering::SeqCst);
                            drop(permit);
                        });
                    }
                    Err(e) => {
                        // Non-fatal accept failure (peer reset before
                        // accept, FD exhaustion, etc.) — log loud, stay
                        // running. The next accept attempt may succeed.
                        tracing::warn!(
                            scanner_id = %scanner_id,
                            error = %e,
                            "accept failure; retrying"
                        );
                    }
                }
            }
        }
    }
}

async fn handle_connection(
    socket: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    scanner_id: String,
    max_payload_len: usize,
    sender: broadcast::Sender<CanonicalEvent>,
    cancel: CancellationToken,
) {
    let (read_half, _write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    loop {
        buf.clear();
        let outcome = tokio::select! {
            _ = cancel.cancelled() => return,
            outcome = read_line_bounded(&mut reader, &mut buf, max_payload_len) => outcome,
        };
        match outcome {
            LineRead::Ok => {
                let trimmed = strip_line_terminator(&buf);
                let line = match std::str::from_utf8(trimmed) {
                    Ok(s) => s,
                    Err(_) => {
                        tracing::warn!(
                            scanner_id = %scanner_id,
                            peer = %peer_addr,
                            "non-utf8 scan payload; dropping line"
                        );
                        continue;
                    }
                };
                if line.is_empty() {
                    continue;
                }
                let (symbology, payload) = split_aim_prefix(line);
                let at_iso8601 = OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
                let event = CanonicalEvent::ScanReceived {
                    scanner_id: scanner_id.clone(),
                    payload: payload.to_string(),
                    symbology: symbology.map(|s| s.to_string()),
                    source_addr: Some(peer_addr.to_string()),
                    at_iso8601,
                };
                // `send` returns `Err(SendError(event))` only when there
                // are no live receivers. That's a normal startup window
                // before the ledger writer subscribes — don't crash
                // the adapter; the scan is lost-by-design (broadcast
                // semantics per ADR-0060 §"Why broadcast over mpsc").
                let _ = sender.send(event);
            }
            LineRead::TooLong => {
                tracing::warn!(
                    scanner_id = %scanner_id,
                    peer = %peer_addr,
                    max = max_payload_len,
                    "payload exceeded max_payload_len; closing connection"
                );
                return;
            }
            LineRead::Eof => return,
            LineRead::Err(e) => {
                tracing::warn!(
                    scanner_id = %scanner_id,
                    peer = %peer_addr,
                    error = %e,
                    "read error; closing connection"
                );
                return;
            }
        }
    }
}

enum LineRead {
    Ok,
    Eof,
    TooLong,
    Err(std::io::Error),
}

/// Read a single line from `reader` into `buf`, bounded by `max`
/// bytes. The newline byte (and any leading `\r`) are included in
/// `buf` and stripped by [`strip_line_terminator`] downstream.
///
/// Per-byte read through the [`BufReader`] — slow in raw IOPS but
/// the [`BufReader`] internally batches the system-call layer, so
/// throughput stays acceptable. Barcode scanners emit one short
/// payload per scan event; throughput is never the bottleneck.
async fn read_line_bounded(
    reader: &mut BufReader<OwnedReadHalf>,
    buf: &mut Vec<u8>,
    max: usize,
) -> LineRead {
    loop {
        if buf.len() >= max {
            return LineRead::TooLong;
        }
        let mut byte = [0u8; 1];
        match reader.read(&mut byte).await {
            Ok(0) => {
                return if buf.is_empty() {
                    LineRead::Eof
                } else {
                    // Connection closed mid-line — treat the partial
                    // line as the last record (some scanners don't
                    // newline-terminate their final emission).
                    LineRead::Ok
                };
            }
            Ok(_) => {}
            Err(e) => return LineRead::Err(e),
        }
        if byte[0] == b'\n' {
            return LineRead::Ok;
        }
        buf.push(byte[0]);
    }
}

fn strip_line_terminator(buf: &[u8]) -> &[u8] {
    match buf.strip_suffix(b"\n") {
        Some(s) => s.strip_suffix(b"\r").unwrap_or(s),
        None => buf.strip_suffix(b"\r").unwrap_or(buf),
    }
}

/// Map an AIM Symbology Identifier prefix code to its human-readable
/// symbology name.
///
/// AIM ID prefixes are the industry standard for self-describing
/// barcode wire output — the device prepends `]` + 2 chars to the
/// decoded payload, e.g. `]C0123456789012` for a Code128 payload of
/// `123456789012`. This adapter's contract is to strip the prefix and
/// surface the symbology on the [`CanonicalEvent::ScanReceived::symbology`]
/// field for downstream consumers that care.
///
/// Returns `None` for prefixes outside this adapter's recognized set
/// — the symbology field stays `None` and the full original payload
/// (prefix intact) appears on `payload`. The set covers the six
/// most-common shop-floor symbologies; extensions land mechanically
/// when a real device emits something unrecognized.
pub fn aim_id_to_symbology(prefix: &str) -> Option<&'static str> {
    match prefix {
        "]C0" | "]C1" | "]C2" | "]C4" => Some("Code128"),
        "]A0" | "]A1" | "]A3" | "]A4" | "]A5" | "]A7" => Some("Code39"),
        "]Q0" | "]Q1" | "]Q2" | "]Q3" | "]Q4" | "]Q5" | "]Q6" => Some("QR"),
        "]d0" | "]d1" | "]d2" | "]d3" | "]d4" | "]d5" | "]d6" => Some("DataMatrix"),
        "]I0" | "]I1" | "]I3" => Some("ITF"),
        "]E0" | "]E3" | "]E4" => Some("EAN"),
        _ => None,
    }
}

/// Strip the leading 3-character AIM ID prefix (if present and
/// recognized) and return `(symbology, remaining_payload)`.
///
/// Returns `(None, payload)` unchanged when the input doesn't start
/// with `]` OR the recognized 3-char prefix isn't in the
/// [`aim_id_to_symbology`] table. This is the loud-don't-silent
/// posture per the [`Adapter`] author rules — an unrecognized
/// `]Xx`-style prefix is preserved verbatim in the payload, the
/// symbology stays `None`, and downstream consumers can still see the
/// raw scanner output if they need to.
pub fn split_aim_prefix(payload: &str) -> (Option<&'static str>, &str) {
    if !payload.starts_with(']') {
        return (None, payload);
    }
    if payload.len() < 3 {
        return (None, payload);
    }
    // AIM ID is `]` + exactly 2 chars (modifier letter + modifier
    // digit). char-boundary safety: AIM ID chars are always ASCII per
    // the spec, so `is_char_boundary(3)` MUST hold for a well-formed
    // prefix. For non-ASCII bytes at positions 1-2 (malformed device
    // output) the boundary check fails — bail and treat the whole
    // payload as opaque.
    if !payload.is_char_boundary(3) {
        return (None, payload);
    }
    let (prefix, rest) = payload.split_at(3);
    match aim_id_to_symbology(prefix) {
        Some(symb) => (Some(symb), rest),
        None => (None, payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    /// Pick an ephemeral port by binding port 0 to localhost, capturing
    /// the resolved port, and dropping the listener. There's a TOCTOU
    /// race window but in practice tests rebind within microseconds —
    /// every other ABERP integration test follows this pattern.
    async fn pick_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn cfg_for_test(scanner_id: &str, port: u16) -> BarcodeScannerConfig {
        BarcodeScannerConfig {
            scanner_id: scanner_id.to_string(),
            listen_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listen_port: port,
            // Tight bounds for integration tests — keep them well below
            // the production defaults so the oversized-payload + max-
            // connections tests are cheap to drive.
            max_payload_len: 64,
            max_concurrent_connections: 4,
            channel_capacity: 64,
        }
    }

    // ====== Unit: aim_id_to_symbology + split_aim_prefix ======

    #[test]
    fn aim_id_table_pins_six_canonical_symbologies() {
        assert_eq!(aim_id_to_symbology("]C0"), Some("Code128"));
        assert_eq!(aim_id_to_symbology("]C1"), Some("Code128"));
        assert_eq!(aim_id_to_symbology("]A0"), Some("Code39"));
        assert_eq!(aim_id_to_symbology("]Q1"), Some("QR"));
        assert_eq!(aim_id_to_symbology("]d1"), Some("DataMatrix"));
        assert_eq!(aim_id_to_symbology("]I0"), Some("ITF"));
        assert_eq!(aim_id_to_symbology("]E0"), Some("EAN"));
    }

    #[test]
    fn aim_id_table_returns_none_for_unknown_prefix() {
        assert_eq!(aim_id_to_symbology("]X9"), None);
        assert_eq!(aim_id_to_symbology(""), None);
        assert_eq!(aim_id_to_symbology("Q1"), None);
        assert_eq!(aim_id_to_symbology("]Q"), None);
    }

    #[test]
    fn split_aim_prefix_recognizes_and_strips() {
        let (symb, rest) = split_aim_prefix("]C0HELLO");
        assert_eq!(symb, Some("Code128"));
        assert_eq!(rest, "HELLO");

        let (symb, rest) = split_aim_prefix("]Q1https://example.com/wo");
        assert_eq!(symb, Some("QR"));
        assert_eq!(rest, "https://example.com/wo");
    }

    #[test]
    fn split_aim_prefix_passes_through_unknown() {
        // Unknown 3-char prefix — payload preserved verbatim, symbology None.
        let (symb, rest) = split_aim_prefix("]X9unknown");
        assert_eq!(symb, None);
        assert_eq!(rest, "]X9unknown");
    }

    #[test]
    fn split_aim_prefix_passes_through_no_prefix() {
        // Bare payload, no AIM prefix at all.
        let (symb, rest) = split_aim_prefix("plain-scan-12345");
        assert_eq!(symb, None);
        assert_eq!(rest, "plain-scan-12345");
    }

    #[test]
    fn split_aim_prefix_handles_short_input() {
        // Shorter than 3 chars — can't have an AIM prefix.
        let (symb, rest) = split_aim_prefix("]C");
        assert_eq!(symb, None);
        assert_eq!(rest, "]C");

        let (symb, rest) = split_aim_prefix("]");
        assert_eq!(symb, None);
        assert_eq!(rest, "]");

        let (symb, rest) = split_aim_prefix("");
        assert_eq!(symb, None);
        assert_eq!(rest, "");
    }

    #[test]
    fn split_aim_prefix_safe_on_non_ascii_after_bracket() {
        // Pathological: bracket + multi-byte UTF-8 in the prefix
        // positions. The 3-byte split point would land mid-codepoint;
        // the guard kicks in and the whole payload passes through.
        let pathological = "]Ω0test";
        let (symb, rest) = split_aim_prefix(pathological);
        assert_eq!(symb, None);
        assert_eq!(rest, pathological);
    }

    // ====== Unit: strip_line_terminator ======

    #[test]
    fn strip_line_terminator_handles_crlf_lf_and_neither() {
        assert_eq!(strip_line_terminator(b"hello\r\n"), b"hello");
        assert_eq!(strip_line_terminator(b"hello\n"), b"hello");
        assert_eq!(strip_line_terminator(b"hello\r"), b"hello");
        assert_eq!(strip_line_terminator(b"hello"), b"hello");
        assert_eq!(strip_line_terminator(b""), b"");
    }

    // ====== Integration: single scan end-to-end ======

    #[tokio::test]
    async fn single_tcp_scan_emits_canonical_event() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-scan-1", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(b"ORDER-12345\r\n").await.unwrap();
        stream.flush().await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event arrives within 2s")
            .expect("event channel open");

        match event {
            CanonicalEvent::ScanReceived {
                scanner_id,
                payload,
                symbology,
                source_addr,
                at_iso8601,
            } => {
                assert_eq!(scanner_id, "test-scan-1");
                assert_eq!(payload, "ORDER-12345");
                assert_eq!(symbology, None);
                assert!(source_addr.is_some());
                assert!(!at_iso8601.is_empty());
            }
            other => panic!("expected ScanReceived, got {other:?}"),
        }

        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn aim_prefix_surfaces_as_symbology() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-symb", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(b"]C0Code128Payload\r\n").await.unwrap();
        stream.flush().await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event arrives")
            .expect("channel open");

        match event {
            CanonicalEvent::ScanReceived {
                payload, symbology, ..
            } => {
                assert_eq!(payload, "Code128Payload");
                assert_eq!(symbology.as_deref(), Some("Code128"));
            }
            other => panic!("expected ScanReceived, got {other:?}"),
        }

        adapter.stop().await.unwrap();
    }

    // ====== Integration: concurrent connections ======

    #[tokio::test]
    async fn concurrent_connections_each_emit_scans() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-concur", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        // Three concurrent peers, each sends two scans.
        let mut clients = Vec::new();
        for client_idx in 0..3 {
            let h = tokio::spawn(async move {
                let mut stream = TcpStream::connect(("127.0.0.1", port))
                    .await
                    .expect("connect");
                stream
                    .write_all(format!("PEER{client_idx}-SCAN1\r\n").as_bytes())
                    .await
                    .unwrap();
                stream
                    .write_all(format!("PEER{client_idx}-SCAN2\r\n").as_bytes())
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
                // Hold the connection open briefly so adapter can drain
                // both lines before EOF triggers connection close.
                tokio::time::sleep(Duration::from_millis(100)).await;
            });
            clients.push(h);
        }

        // Collect six events (3 peers × 2 lines).
        let mut received = Vec::new();
        for _ in 0..6 {
            let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .expect("event arrives")
                .expect("channel open");
            received.push(event);
        }

        for h in clients {
            h.await.unwrap();
        }

        // Every payload arrived; per-peer order preserved.
        let mut by_peer: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for ev in received {
            if let CanonicalEvent::ScanReceived { payload, .. } = ev {
                let peer = payload
                    .split('-')
                    .next()
                    .expect("payload has prefix")
                    .to_string();
                by_peer.entry(peer).or_default().push(payload);
            }
        }
        assert_eq!(by_peer.len(), 3, "all three peers produced events");
        for (peer, payloads) in by_peer {
            assert_eq!(
                payloads.len(),
                2,
                "peer {peer} produced 2 scans, got {payloads:?}"
            );
            assert!(
                payloads[0].ends_with("SCAN1"),
                "per-connection ordering: SCAN1 before SCAN2 for {peer}"
            );
            assert!(
                payloads[1].ends_with("SCAN2"),
                "per-connection ordering: SCAN2 after SCAN1 for {peer}"
            );
        }

        adapter.stop().await.unwrap();
    }

    // ====== Integration: oversized payload ======

    #[tokio::test]
    async fn oversized_payload_drops_connection_no_event() {
        let port = pick_free_port().await;
        // max_payload_len is 64 per cfg_for_test.
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-oversize", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // 200 bytes, no newline — adapter must drop the connection
        // before the line ever completes.
        let oversized = vec![b'A'; 200];
        // The write may succeed (kernel buffer) OR fail mid-flight when
        // the adapter shuts the socket — either is acceptable.
        let _ = stream.write_all(&oversized).await;
        let _ = stream.flush().await;

        // No event should arrive — give the adapter 500ms to confirm.
        let timed_out = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err();
        assert!(
            timed_out,
            "no ScanReceived should fire for oversized payload"
        );

        // Adapter MUST still be Healthy.
        assert_eq!(adapter.health(), AdapterHealth::Healthy);

        adapter.stop().await.unwrap();
    }

    // ====== Integration: malformed UTF-8 ======

    #[tokio::test]
    async fn non_utf8_line_dropped_adapter_stays_healthy() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-bad-utf8", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // Invalid UTF-8 (lone continuation byte) then a valid line.
        stream.write_all(&[0xC0, 0xC0, b'\n']).await.unwrap();
        stream.write_all(b"GOOD-SCAN\r\n").await.unwrap();
        stream.flush().await.unwrap();

        // The bad line is dropped; the good line emits one event.
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event arrives")
            .expect("channel open");
        match event {
            CanonicalEvent::ScanReceived { payload, .. } => assert_eq!(payload, "GOOD-SCAN"),
            other => panic!("expected ScanReceived, got {other:?}"),
        }
        assert_eq!(adapter.health(), AdapterHealth::Healthy);

        adapter.stop().await.unwrap();
    }

    // ====== Integration: stop releases port ======

    #[tokio::test]
    async fn stop_releases_port_for_rebind() {
        let port = pick_free_port().await;
        let cfg = cfg_for_test("test-stop", port);
        let adapter = BarcodeScannerAdapter::new(cfg.clone());
        adapter.start().await.unwrap();
        adapter.stop().await.unwrap();

        // Rebind same port — proves the listener task released the FD.
        let adapter2 = BarcodeScannerAdapter::new(cfg);
        adapter2
            .start()
            .await
            .expect("port should be free after stop");
        adapter2.stop().await.unwrap();
    }

    #[tokio::test]
    async fn start_stop_start_lifecycle_health_flips() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-lifecycle", port));
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn start_is_idempotent() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-idem-start", port));
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-idem-stop", port));
        adapter.stop().await.unwrap();
        adapter.start().await.unwrap();
        adapter.stop().await.unwrap();
        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
    }

    // ====== Integration: random bytes don't panic ======

    #[tokio::test]
    async fn random_bytes_do_not_panic_adapter() {
        let port = pick_free_port().await;
        let adapter = BarcodeScannerAdapter::new(cfg_for_test("test-random", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // Deterministic pseudo-random byte sequence (no Math.random
        // equivalents in tests — match-the-codebase posture). Includes
        // bytes that would be syntactically meaningful (0x5D = `]`,
        // 0x0A = `\n`, 0x0D = `\r`) plus invalid UTF-8.
        let payload: Vec<u8> = (0..200u16)
            .map(|i| (i.wrapping_mul(31) ^ 0xAA) as u8)
            .collect();
        let _ = stream.write_all(&payload).await;
        let _ = stream.flush().await;

        // Whatever the adapter decides to emit (zero or more events),
        // it MUST not panic — and the adapter MUST stay healthy.
        let _ = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert_eq!(adapter.health(), AdapterHealth::Healthy);

        adapter.stop().await.unwrap();
    }

    // ====== Type-level: dyn-safety ======

    #[tokio::test]
    async fn barcode_scanner_adapter_is_dyn_safe() {
        let port = pick_free_port().await;
        let cfg = cfg_for_test("test-dyn", port);
        let adapter: std::sync::Arc<dyn Adapter> =
            std::sync::Arc::new(BarcodeScannerAdapter::new(cfg));
        assert_eq!(adapter.name(), "test-dyn");
    }

    // ====== Config defaults pinned ======

    #[test]
    fn config_defaults_match_documented_constants() {
        let cfg = BarcodeScannerConfig::new("scanner-x");
        assert_eq!(cfg.listen_port, DEFAULT_LISTEN_PORT);
        assert_eq!(cfg.max_payload_len, DEFAULT_MAX_PAYLOAD_LEN);
        assert_eq!(
            cfg.max_concurrent_connections,
            DEFAULT_MAX_CONCURRENT_CONNECTIONS
        );
        assert_eq!(cfg.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
        assert_eq!(cfg.listen_addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
