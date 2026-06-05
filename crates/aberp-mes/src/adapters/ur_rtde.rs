//! [`UrRtdeAdapter`] — Universal Robots RTDE adapter for cobot telemetry
//! (S248 / PR-241 / ADR-0060 Phase δ — third hardware-input adapter,
//! first robot).
//!
//! ## Why an RTDE adapter
//!
//! Universal Robots' RTDE (Real-Time Data Exchange) is the open binary
//! TCP protocol every UR cobot speaks on port 30004 — UR3, UR5, UR10,
//! UR16, e-Series. It's documented (UR support article 99726) and ships
//! enabled by default on every controller. Picking RTDE over PolyScope
//! SDK / Ethernet-IP wrappers is the same [[spacex-vertical-integration]]
//! call as Zebra/ZPL (PR-238) and MTConnect (PR-240): one open protocol
//! covers the entire cobot population without taking on a vendor SDK.
//!
//! ## Wire shape
//!
//! All RTDE packets share the framing:
//!
//! ```text
//! +----------------+--------+-----------+
//! | u16 length (BE)| u8 cmd | payload   |
//! +----------------+--------+-----------+
//! ```
//!
//! `length` covers the whole frame (header + payload).
//!
//! Command bytes used here (closed set per the v1 implementation —
//! every other cmd in the spec is queued for v2 in the TODO list below):
//!
//! | byte | name                                  | direction |
//! |------|---------------------------------------|-----------|
//! | 0x56 | RTDE_REQUEST_PROTOCOL_VERSION (`V`)   | client→   |
//! | 0x76 | RTDE_GET_URCONTROL_VERSION (`v`)      | client→   |
//! | 0x4F | RTDE_CONTROL_PACKAGE_SETUP_OUTPUTS    | client→   |
//! | 0x53 | RTDE_CONTROL_PACKAGE_START (`S`)      | client→   |
//! | 0x50 | RTDE_CONTROL_PACKAGE_PAUSE (`P`)      | client→   |
//! | 0x55 | RTDE_DATA_PACKAGE (`U`)               | server→   |
//!
//! ## Variables subscribed (v1 recipe)
//!
//! Fixed order — the wire payload layout follows this exact sequence:
//!
//! | variable                | type     | bytes |
//! |-------------------------|----------|-------|
//! | `timestamp`             | DOUBLE   | 8     |
//! | `robot_mode`            | INT32    | 4     |
//! | `safety_mode`           | INT32    | 4     |
//! | `runtime_state`         | UINT32   | 4     |
//! | `actual_TCP_pose`       | VECTOR6D | 48    |
//! | `actual_q`              | VECTOR6D | 48    |
//! | `output_int_register_0` | INT32    | 4     |
//!
//! Total data payload per frame: 120 bytes. Subscribed at 8 Hz — plenty
//! for shop-floor dashboard cadence; 500 Hz is the protocol max but
//! pointless at our consumer's refresh rate.
//!
//! ## Health model
//!
//! - `start()` initiates the connect-and-handshake task and returns
//!   immediately. The task transitions cached health as it progresses.
//! - TCP connect + RTDE handshake (request protocol version, get
//!   controller version, setup outputs, start) complete within
//!   `handshake_timeout` (default 5s) and the FIRST data frame arrives
//!   within `stall_threshold` after that → `Healthy`.
//! - TCP connect refused / handshake reject / unsupported variable in
//!   the SETUP_OUTPUTS response → `Unhealthy { reason }`.
//! - Connected + last frame older than `stall_threshold` (default 2s)
//!   → `Degraded { reason: "stream idle Nms" }`.
//! - Socket drop after Healthy → adapter enters reconnect-with-backoff,
//!   reports `Unhealthy { reason: "reconnecting (Nms backoff)" }`
//!   between attempts. Backoff doubles 500ms → 1s → 2s → 4s → 8s → 16s
//!   → capped at 30s. Resets to 500ms on the next Healthy.
//!
//! Per [[trust-code-not-operator]] the operator never has to notice a
//! transient e-stop / hand-jog disconnect — the next reconnect attempt
//! recovers and the Workshop dashboard tile (S240 / PR-234) re-greens
//! within the backoff window.
//!
//! ## Event emission
//!
//! On every received data frame the adapter compares the parsed
//! `robot_mode` and `safety_mode` against the previous frame's values.
//! If EITHER changed (or both), the adapter broadcasts ONE
//! [`CanonicalEvent::RobotStateChanged`] carrying both prev+new pairs.
//! Single-event-per-tick keeps the audit-ledger row count linear in
//! physical transitions and lets a single SPA / projection match
//! coincident transitions (e.g. `Running → Idle` mode coincident with
//! `Normal → ProtectiveStop` safety is a SINGLE operator-meaningful
//! event, not two).
//!
//! First-observed frame always emits an `Unknown → <mode>` /
//! `Unknown → <safety>` baseline so subscribers have a known starting
//! point regardless of when they attached.
//!
//! ## What this adapter does NOT do (v1 — queued for follow-ups)
//!
//! Tracked as PR-241 TODOs:
//!
//! - **RTDE inputs** (cmd `I` 0x49) — sending data TO the robot.
//!   Read-only adapter today.
//! - **Dashboard server commands** (port 29999) — different protocol;
//!   useful for load-program / play / pause / e-stop but out of scope
//!   for a telemetry adapter.
//! - **Primary client interface** (port 30001) — older protocol; RTDE
//!   subsumes its read-side use cases.
//! - **Operator-overridable recipes** — v1 ships the fixed 7-variable
//!   recipe above. A typed builder is queued for v2.
//! - **Tool flange + force-torque variables** — `actual_TCP_force`,
//!   `tcp_force_scalar`, etc. Pose + joints are enough for the
//!   dashboard's "what's the robot doing right now" question.
//! - **PolyScope program metadata** — program name / state is exposed
//!   via Dashboard server, not RTDE. Skipping until a real cell asks.
//!
//! ## DoS bounds (per [[trust-code-not-operator]])
//!
//! Hard limits enforced in code, not via operator config:
//!
//! - `max_frame_bytes` (default 64 KiB) — caps any single RTDE frame
//!   the adapter will accept. Realistic RTDE frames are <2 KiB; 64 KiB
//!   is generous even for a future op subscribing to ~100 variables.
//! - `handshake_timeout` (default 5s) — cap on the connect + handshake
//!   phase. Hits abort the attempt and trigger backoff.
//! - `stall_threshold` (default 2s) — drives the Healthy → Degraded
//!   transition. Tight enough that an operator notices a stuck arm
//!   from the dashboard tile, not by hand-walking to the cell.
//! - `backoff_cap` (default 30s) — caps the exponential reconnect
//!   backoff. A truly-down robot doesn't pile up reconnect noise.
//!
//! ## Lifecycle
//!
//! `start()` spawns the connect-and-stream task and returns. The task
//! handles connect → handshake → stream → (on drop) backoff →
//! reconnect indefinitely until `stop()` cancels via
//! [`CancellationToken`]. `stop()` cooperates with the task: if it's
//! currently streaming, the task sends `RTDE_CONTROL_PACKAGE_PAUSE` to
//! the controller, then closes the socket. Both methods are
//! idempotent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapter::{Adapter, AdapterHealth};
use crate::error::AdapterError;
use crate::events::{CanonicalEvent, RobotMode, SafetyMode};

// ===== Protocol constants =====

/// RTDE command bytes — closed set used by this adapter (see module
/// docs for the full byte table). Every other RTDE cmd in the spec is
/// out of scope for v1 and queued for v2 in the TODO list.
const CMD_REQUEST_PROTOCOL_VERSION: u8 = 0x56; // 'V'
const CMD_GET_URCONTROL_VERSION: u8 = 0x76; // 'v'
const CMD_CONTROL_PACKAGE_SETUP_OUTPUTS: u8 = 0x4F; // 'O'
const CMD_CONTROL_PACKAGE_START: u8 = 0x53; // 'S'
const CMD_CONTROL_PACKAGE_PAUSE: u8 = 0x50; // 'P'
const CMD_DATA_PACKAGE: u8 = 0x55; // 'U'

/// Default RTDE TCP port. Hardcoded on every UR controller; not
/// operator-configurable on the cobot side, so a custom port here is
/// only useful for tests / port-forwarders.
pub const DEFAULT_RTDE_PORT: u16 = 30004;

/// Protocol version this adapter negotiates. v2 is the modern
/// shape (frequency-carrying SETUP_OUTPUTS payload, recipe-ID-prefixed
/// data frames). v1 would also work but lacks the in-band frequency
/// negotiation we want.
const RTDE_PROTOCOL_VERSION: u16 = 2;

/// Output stream frequency requested at SETUP_OUTPUTS time, Hz. 8 Hz
/// matches the brief — 125ms between frames is well below the
/// Workshop dashboard's 30s refresh tick and well above the human
/// perception threshold for state changes.
const OUTPUT_FREQUENCY_HZ: f64 = 8.0;

/// The v1 output recipe — fixed-order list of variable names. Wire
/// payload layout follows this sequence (see module docs for the
/// type/byte table).
const OUTPUT_RECIPE: &[&str] = &[
    "timestamp",
    "robot_mode",
    "safety_mode",
    "runtime_state",
    "actual_TCP_pose",
    "actual_q",
    "output_int_register_0",
];

/// Expected RTDE variable-type strings the controller MUST return in
/// the SETUP_OUTPUTS response, in the same order as [`OUTPUT_RECIPE`].
/// If any returned type is `"NOT_FOUND"` or doesn't match here, the
/// handshake fails loud (the controller is a UR model we don't
/// understand, or a stale firmware that renamed a variable).
const EXPECTED_OUTPUT_TYPES: &[&str] = &[
    "DOUBLE", "INT32", "INT32", "UINT32", "VECTOR6D", "VECTOR6D", "INT32",
];

/// Total wire bytes per data-frame payload (after the recipe-ID byte).
/// Sum of variable sizes: 8 + 4 + 4 + 4 + 48 + 48 + 4 = 120.
const DATA_PAYLOAD_BYTES: usize = 120;

// ===== Default DoS bounds + timings =====

/// Default cap on a single RTDE frame's total wire bytes. 64 KiB is
/// generous — realistic frames are <2 KiB even with a wide recipe.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;

/// Default cap on a single connect-and-handshake attempt. Above this
/// the attempt aborts and the connect loop backs off.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default threshold above which a connected adapter with no recent
/// frame is reported as `Degraded`. Brief calls out 2s.
pub const DEFAULT_STALL_THRESHOLD: Duration = Duration::from_secs(2);

/// Default initial reconnect backoff. Doubles each attempt up to
/// [`DEFAULT_BACKOFF_CAP`].
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Default cap on the exponential reconnect backoff. 30s keeps a
/// truly-down robot from piling up reconnect noise.
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Default broadcast channel capacity. Matches the Zebra / MTConnect
/// adapters; 1024 is plenty for any realistic event-rate.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// Default cap on the best-effort `PAUSE` write the stop() path makes
/// before closing the socket. Short — we don't want stop() to block
/// for seconds waiting on a hung controller.
pub const DEFAULT_PAUSE_TIMEOUT: Duration = Duration::from_millis(500);

// ===== Config =====

/// Construction-time configuration for a [`UrRtdeAdapter`].
///
/// DoS bounds (`max_frame_bytes`, `handshake_timeout`,
/// `stall_threshold`, `initial_backoff`, `backoff_cap`,
/// `pause_timeout`) are exposed only so tests can shrink them;
/// production paths use the `DEFAULT_*` constants per
/// [[trust-code-not-operator]].
#[derive(Debug, Clone)]
pub struct UrRtdeAdapterConfig {
    /// Stable identifier; becomes the adapter's [`Adapter::name`].
    /// Used as the registry key + the `adapter_name` field on every
    /// audit-ledger entry. MUST be unique across registered adapters.
    /// Typical shape: `"{cell}-robot"` (e.g. `"cnc-cell-a-robot"`).
    pub robot_id: String,
    /// Operator-readable display name surfaced on the Workshop
    /// dashboard tile. Distinct from `robot_id` so the operator can
    /// rename ("Cell A — UR10e") without disturbing the stable
    /// registry key.
    pub friendly_name: String,
    /// Robot host — IP address or DNS name of the UR controller.
    /// Resolved on each connect; no caching, so a DHCP lease move is
    /// picked up on the next reconnect.
    pub host: String,
    /// Controller RTDE TCP port. Production default is 30004 (the only
    /// port any UR cobot's RTDE server uses); tests pass ephemeral
    /// ports.
    pub port: u16,
    /// Robot model label — informational only (display + logs).
    /// Examples: `"UR3"`, `"UR5e"`, `"UR10e"`, `"UR16"`. Adapter does
    /// not branch on this — RTDE is identical across the UR family.
    pub model: String,
    pub max_frame_bytes: usize,
    pub handshake_timeout: Duration,
    pub stall_threshold: Duration,
    pub initial_backoff: Duration,
    pub backoff_cap: Duration,
    pub pause_timeout: Duration,
    pub channel_capacity: usize,
}

impl UrRtdeAdapterConfig {
    /// Construct a config with default DoS bounds + timings; only the
    /// five operator-meaningful fields are exposed.
    pub fn new(
        robot_id: impl Into<String>,
        friendly_name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        model: impl Into<String>,
    ) -> Self {
        Self {
            robot_id: robot_id.into(),
            friendly_name: friendly_name.into(),
            host: host.into(),
            port,
            model: model.into(),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            stall_threshold: DEFAULT_STALL_THRESHOLD,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            backoff_cap: DEFAULT_BACKOFF_CAP,
            pause_timeout: DEFAULT_PAUSE_TIMEOUT,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ===== Adapter struct =====

/// The Universal Robots RTDE [`Adapter`] implementation.
///
/// Clone-cheap via `Arc<UrRtdeAdapter>`. Internal state (lifecycle,
/// cached health, broadcast sender, last observed mode/safety, connect-
/// task handle) is interior-mutable.
#[derive(Debug)]
pub struct UrRtdeAdapter {
    config: UrRtdeAdapterConfig,
    health: Arc<Mutex<AdapterHealth>>,
    sender: broadcast::Sender<CanonicalEvent>,
    cancel: Mutex<Option<CancellationToken>>,
    connect_handle: Mutex<Option<JoinHandle<()>>>,
    /// Most recently observed `robot_mode` / `safety_mode` pair.
    /// `None` until the first successful data frame establishes a
    /// baseline; the first emitted event always carries
    /// `previous_mode/previous_safety: Unknown` for that reason.
    last_state: Arc<Mutex<Option<(RobotMode, SafetyMode)>>>,
}

impl UrRtdeAdapter {
    /// Construct a stopped adapter ready for `start()`.
    pub fn new(config: UrRtdeAdapterConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity);
        Self {
            config,
            health: Arc::new(Mutex::new(AdapterHealth::Stopped)),
            sender,
            cancel: Mutex::new(None),
            connect_handle: Mutex::new(None),
            last_state: Arc::new(Mutex::new(None)),
        }
    }

    /// Operator-readable friendly name. Surfaces on the Workshop
    /// dashboard alongside the stable `robot_id`.
    pub fn friendly_name(&self) -> &str {
        &self.config.friendly_name
    }

    /// Robot model label (informational). Surfaces on the Workshop
    /// dashboard alongside the friendly name.
    pub fn model(&self) -> &str {
        &self.config.model
    }
}

#[async_trait]
impl Adapter for UrRtdeAdapter {
    fn name(&self) -> &str {
        &self.config.robot_id
    }

    fn kind(&self) -> &'static str {
        "robot"
    }

    fn endpoint_host(&self) -> Option<String> {
        Some(self.config.host.clone())
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(self.config.port)
    }

    async fn start(&self) -> Result<(), AdapterError> {
        // Idempotent: if already running, no-op.
        {
            let current = self.health.lock().expect("health mutex poisoned").clone();
            if !matches!(current, AdapterHealth::Stopped) {
                return Ok(());
            }
            *self.health.lock().expect("health mutex poisoned") = AdapterHealth::Starting;
        }

        let cancel = CancellationToken::new();
        *self.cancel.lock().expect("cancel mutex poisoned") = Some(cancel.clone());

        let config = self.config.clone();
        let health_slot = self.health.clone();
        let last_state_slot = self.last_state.clone();
        let sender = self.sender.clone();

        let handle = tokio::spawn(async move {
            run_connect_loop(config, cancel, health_slot, last_state_slot, sender).await;
        });

        *self
            .connect_handle
            .lock()
            .expect("connect_handle mutex poisoned") = Some(handle);
        Ok(())
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        let cancel_opt = self.cancel.lock().expect("cancel mutex poisoned").take();
        let handle_opt = self
            .connect_handle
            .lock()
            .expect("connect_handle mutex poisoned")
            .take();

        if let Some(token) = cancel_opt {
            token.cancel();
        }
        if let Some(handle) = handle_opt {
            if let Err(e) = handle.await {
                if e.is_panic() {
                    tracing::error!(
                        robot_id = %self.config.robot_id,
                        "RTDE connect task panicked during stop: {e}"
                    );
                }
            }
        }

        *self.health.lock().expect("health mutex poisoned") = AdapterHealth::Stopped;
        *self.last_state.lock().expect("last_state mutex poisoned") = None;
        Ok(())
    }

    fn health(&self) -> AdapterHealth {
        self.health.lock().expect("health mutex poisoned").clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<CanonicalEvent> {
        self.sender.subscribe()
    }
}

// ===== Connect loop =====

/// Outer connect-and-stream loop. Runs until the cancel token fires.
/// On each iteration: connect → handshake → stream. On stream drop
/// (socket error / handshake failure), backs off and retries.
async fn run_connect_loop(
    config: UrRtdeAdapterConfig,
    cancel: CancellationToken,
    health_slot: Arc<Mutex<AdapterHealth>>,
    last_state_slot: Arc<Mutex<Option<(RobotMode, SafetyMode)>>>,
    sender: broadcast::Sender<CanonicalEvent>,
) {
    let mut backoff = config.initial_backoff;
    let endpoint = config.endpoint();

    loop {
        if cancel.is_cancelled() {
            return;
        }

        // Connect + handshake under the handshake_timeout cap.
        let connect_attempt =
            tokio::time::timeout(config.handshake_timeout, connect_and_handshake(&endpoint));

        let stream = tokio::select! {
            _ = cancel.cancelled() => {
                return;
            }
            result = connect_attempt => {
                match result {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(reason)) => {
                        tracing::warn!(
                            robot_id = %config.robot_id,
                            endpoint = %endpoint,
                            "RTDE handshake failed: {reason}; backoff {}ms",
                            backoff.as_millis()
                        );
                        set_health(
                            &health_slot,
                            AdapterHealth::Unhealthy {
                                reason: format!("{reason}; reconnecting"),
                            },
                        );
                        if !sleep_with_cancel(backoff, &cancel).await {
                            return;
                        }
                        backoff = next_backoff(backoff, config.backoff_cap);
                        continue;
                    }
                    Err(_elapsed) => {
                        let reason = format!(
                            "handshake timed out after {}ms",
                            config.handshake_timeout.as_millis()
                        );
                        tracing::warn!(
                            robot_id = %config.robot_id,
                            endpoint = %endpoint,
                            "{reason}; backoff {}ms",
                            backoff.as_millis()
                        );
                        set_health(
                            &health_slot,
                            AdapterHealth::Unhealthy {
                                reason: format!("{reason}; reconnecting"),
                            },
                        );
                        if !sleep_with_cancel(backoff, &cancel).await {
                            return;
                        }
                        backoff = next_backoff(backoff, config.backoff_cap);
                        continue;
                    }
                }
            }
        };

        // Handshake succeeded; reset backoff and enter stream loop.
        backoff = config.initial_backoff;
        run_stream_loop(
            stream,
            &config,
            &cancel,
            &health_slot,
            &last_state_slot,
            &sender,
        )
        .await;

        // Either cancel fired (the stream loop returns) or the stream
        // ended. If cancel fired, loop top will return.
        if cancel.is_cancelled() {
            return;
        }

        // Stream ended without cancel — controller dropped us, reset
        // last-state baseline (operator will see Unknown→ resync) and
        // back off.
        *last_state_slot.lock().expect("last_state mutex poisoned") = None;
        set_health(
            &health_slot,
            AdapterHealth::Unhealthy {
                reason: format!("stream dropped; reconnecting in {}ms", backoff.as_millis()),
            },
        );
        if !sleep_with_cancel(backoff, &cancel).await {
            return;
        }
        backoff = next_backoff(backoff, config.backoff_cap);
    }
}

/// Connect TCP, run the RTDE handshake (request protocol version, get
/// controller version, setup outputs, start). Returns the stream ready
/// for the data-frame loop, or an operator-readable failure reason.
async fn connect_and_handshake(endpoint: &str) -> Result<TcpStream, String> {
    let mut stream = TcpStream::connect(endpoint)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    // 1. Request protocol version 2.
    let mut payload = Vec::with_capacity(2);
    payload.extend_from_slice(&RTDE_PROTOCOL_VERSION.to_be_bytes());
    write_frame(&mut stream, CMD_REQUEST_PROTOCOL_VERSION, &payload)
        .await
        .map_err(|e| format!("send request-protocol-version: {e}"))?;
    let (cmd, body) = read_frame(&mut stream, DEFAULT_MAX_FRAME_BYTES)
        .await
        .map_err(|e| format!("recv request-protocol-version ack: {e}"))?;
    if cmd != CMD_REQUEST_PROTOCOL_VERSION {
        return Err(format!(
            "unexpected ack cmd 0x{cmd:02X} for request-protocol-version"
        ));
    }
    if body.is_empty() || body[0] != 1 {
        return Err(format!(
            "controller rejected RTDE protocol version {RTDE_PROTOCOL_VERSION}"
        ));
    }

    // 2. Get URControl version (informational — we accept whatever).
    write_frame(&mut stream, CMD_GET_URCONTROL_VERSION, &[])
        .await
        .map_err(|e| format!("send get-urcontrol-version: {e}"))?;
    let (cmd, _body) = read_frame(&mut stream, DEFAULT_MAX_FRAME_BYTES)
        .await
        .map_err(|e| format!("recv urcontrol-version: {e}"))?;
    if cmd != CMD_GET_URCONTROL_VERSION {
        return Err(format!(
            "unexpected ack cmd 0x{cmd:02X} for get-urcontrol-version"
        ));
    }

    // 3. Setup outputs (v2 payload = DOUBLE frequency BE + ASCII
    //    comma-separated variable names).
    let mut payload = Vec::with_capacity(8 + 128);
    payload.extend_from_slice(&OUTPUT_FREQUENCY_HZ.to_be_bytes());
    payload.extend_from_slice(OUTPUT_RECIPE.join(",").as_bytes());
    write_frame(&mut stream, CMD_CONTROL_PACKAGE_SETUP_OUTPUTS, &payload)
        .await
        .map_err(|e| format!("send setup-outputs: {e}"))?;
    let (cmd, body) = read_frame(&mut stream, DEFAULT_MAX_FRAME_BYTES)
        .await
        .map_err(|e| format!("recv setup-outputs ack: {e}"))?;
    if cmd != CMD_CONTROL_PACKAGE_SETUP_OUTPUTS {
        return Err(format!("unexpected ack cmd 0x{cmd:02X} for setup-outputs"));
    }
    if body.is_empty() {
        return Err("setup-outputs ack has no recipe ID".to_string());
    }
    let recipe_id = body[0];
    let types_str = std::str::from_utf8(&body[1..])
        .map_err(|e| format!("setup-outputs types not UTF-8: {e}"))?;
    validate_output_types(types_str)?;
    if recipe_id == 0 {
        return Err("controller assigned recipe ID 0 (invalid)".to_string());
    }

    // 4. Start streaming.
    write_frame(&mut stream, CMD_CONTROL_PACKAGE_START, &[])
        .await
        .map_err(|e| format!("send start: {e}"))?;
    let (cmd, body) = read_frame(&mut stream, DEFAULT_MAX_FRAME_BYTES)
        .await
        .map_err(|e| format!("recv start ack: {e}"))?;
    if cmd != CMD_CONTROL_PACKAGE_START {
        return Err(format!("unexpected ack cmd 0x{cmd:02X} for start"));
    }
    if body.is_empty() || body[0] != 1 {
        return Err("controller rejected stream start".to_string());
    }

    Ok(stream)
}

/// Inner stream loop. Reads data frames, emits state-change events,
/// updates health (Healthy when fresh / Degraded when stale).
/// Returns when cancel fires (sends PAUSE first) OR when the read
/// errors (caller backs off + reconnects).
async fn run_stream_loop(
    mut stream: TcpStream,
    config: &UrRtdeAdapterConfig,
    cancel: &CancellationToken,
    health_slot: &Arc<Mutex<AdapterHealth>>,
    last_state_slot: &Arc<Mutex<Option<(RobotMode, SafetyMode)>>>,
    sender: &broadcast::Sender<CanonicalEvent>,
) {
    // S249-F11 — first-frame sanity flag. SETUP_OUTPUTS validates types
    // slot-by-slot but slots 1+2 are both INT32, so a controller (or
    // buggy responder) that transposed robot_mode and safety_mode
    // would pass validation while decode_data_payload reads bytes in
    // declared order, silently mis-classifying ProtectiveStop as a
    // benign mode change. On the first data frame in this connection
    // we assert both decoded modes land in their documented integer
    // ranges (not RobotMode::Unknown / SafetyMode::Unknown). On
    // failure we drop the connection — the outer connect loop
    // reconnects + re-handshakes per CLAUDE.md rule 12 (fail loud).
    let mut first_frame_validated = false;
    loop {
        let read_attempt = tokio::time::timeout(
            config.stall_threshold,
            read_frame(&mut stream, config.max_frame_bytes),
        );

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Best-effort PAUSE before close. Bounded so a hung
                // controller can't block stop().
                let _ = tokio::time::timeout(
                    config.pause_timeout,
                    write_frame(&mut stream, CMD_CONTROL_PACKAGE_PAUSE, &[]),
                ).await;
                // S249-F14 — bound shutdown() too. A controller with a
                // closed TCP window or full RX buffer can stall
                // `stream.shutdown()` indefinitely, defeating S213's
                // graceful-shutdown budget and producing user-visible
                // "it won't quit → SIGKILL becomes routine" behaviour.
                // Reuse the same pause_timeout — RST is acceptable on
                // expiry; PAUSE intent already arrived best-effort.
                let _ = tokio::time::timeout(
                    config.pause_timeout,
                    stream.shutdown(),
                ).await;
                return;
            }
            result = read_attempt => {
                match result {
                    Err(_elapsed) => {
                        set_health(
                            health_slot,
                            AdapterHealth::Degraded {
                                reason: format!(
                                    "stream idle > {}ms",
                                    config.stall_threshold.as_millis()
                                ),
                            },
                        );
                        // Keep looping; next read will either get a
                        // frame (back to Healthy) or error (reconnect).
                    }
                    Ok(Err(io_err)) => {
                        tracing::debug!(
                            robot_id = %config.robot_id,
                            "RTDE stream read error: {io_err}"
                        );
                        return;
                    }
                    Ok(Ok((cmd, payload))) => {
                        set_health(health_slot, AdapterHealth::Healthy);
                        if cmd == CMD_DATA_PACKAGE {
                            // S249-F11: validate the first decoded frame's
                            // mode codes are in-range. Mis-classification
                            // here would silently log ProtectiveStop as a
                            // benign mode change — drop + reconnect.
                            if !first_frame_validated {
                                match validate_first_frame(&payload) {
                                    Ok(()) => first_frame_validated = true,
                                    Err(reason) => {
                                        tracing::error!(
                                            robot_id = %config.robot_id,
                                            "RTDE first-frame sanity check failed: {reason}; \
                                             dropping connection for re-handshake"
                                        );
                                        return;
                                    }
                                }
                            }
                            handle_data_frame(
                                &payload,
                                &config.robot_id,
                                last_state_slot,
                                sender,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// S249-F11 — first-frame sanity check. Slot-swap defence: the
/// SETUP_OUTPUTS reply only carries variable TYPES (both robot_mode
/// and safety_mode are INT32), so a controller that returned the two
/// variables transposed would pass `validate_output_types` while
/// `decode_data_payload` reads bytes in declared order — silently
/// mis-classifying ProtectiveStop as a benign mode change. We check
/// the first decoded frame's modes land inside their documented
/// integer ranges (anything mapping to `Unknown` is rejected) and
/// drop the connection if either is Unknown so the outer connect
/// loop re-handshakes. Per CLAUDE.md rule 12 — fail loud.
pub(crate) fn validate_first_frame(payload: &[u8]) -> Result<(), String> {
    let snap = decode_data_payload(payload).map_err(|e| format!("decode failed: {e}"))?;
    if matches!(snap.robot_mode, RobotMode::Unknown) {
        return Err(
            "robot_mode decoded to Unknown on first frame — slot mis-order suspected".to_string(),
        );
    }
    if matches!(snap.safety_mode, SafetyMode::Unknown) {
        return Err(
            "safety_mode decoded to Unknown on first frame — slot mis-order suspected".to_string(),
        );
    }
    Ok(())
}

/// Decode a data-package payload (recipe-ID byte + 120 bytes of
/// variables), update `last_state`, and emit a `RobotStateChanged` if
/// either mode or safety transitioned.
fn handle_data_frame(
    payload: &[u8],
    robot_id: &str,
    last_state_slot: &Arc<Mutex<Option<(RobotMode, SafetyMode)>>>,
    sender: &broadcast::Sender<CanonicalEvent>,
) {
    let snapshot = match decode_data_payload(payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                robot_id = %robot_id,
                "RTDE data payload decode failed: {e}"
            );
            return;
        }
    };

    let new_mode = snapshot.robot_mode;
    let new_safety = snapshot.safety_mode;

    let mut last = last_state_slot.lock().expect("last_state mutex poisoned");
    let (prev_mode, prev_safety) = last.unwrap_or((RobotMode::Unknown, SafetyMode::Unknown));
    if prev_mode != new_mode || prev_safety != new_safety {
        let at_iso8601 = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        let event = CanonicalEvent::RobotStateChanged {
            robot_id: robot_id.to_string(),
            previous_mode: prev_mode,
            new_mode,
            previous_safety: prev_safety,
            new_safety,
            at_iso8601,
        };
        // Ignore SendError — broadcast::send returns Err only when no
        // receivers exist (legitimate state). Matches mtconnect pattern.
        let _ = sender.send(event);
        *last = Some((new_mode, new_safety));
    }
}

// ===== Pure helpers =====

/// Parsed snapshot of the leaf variables this adapter cares about from
/// a single RTDE data-package payload. Every field is populated on
/// success — DATA_PAYLOAD_BYTES is a fixed shape, not an envelope of
/// optional items.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RtdeSnapshot {
    pub timestamp: f64,
    pub robot_mode: RobotMode,
    pub safety_mode: SafetyMode,
    pub runtime_state: u32,
    pub actual_tcp_pose: [f64; 6],
    pub actual_q: [f64; 6],
    pub output_int_register_0: i32,
}

/// Decode the payload that follows the recipe-ID byte of an RTDE
/// data-package frame. The first byte of `payload` is the recipe ID
/// (validated by the controller, ignored here).
pub(crate) fn decode_data_payload(payload: &[u8]) -> Result<RtdeSnapshot, String> {
    if payload.len() != 1 + DATA_PAYLOAD_BYTES {
        return Err(format!(
            "data payload size {} != expected {}",
            payload.len(),
            1 + DATA_PAYLOAD_BYTES
        ));
    }
    // Skip recipe-ID byte; consume in declared order.
    let mut cursor = 1usize;
    let timestamp = read_double(payload, &mut cursor)?;
    let robot_mode_code = read_i32(payload, &mut cursor)?;
    let safety_mode_code = read_i32(payload, &mut cursor)?;
    let runtime_state = read_u32(payload, &mut cursor)?;
    let actual_tcp_pose = read_vector6d(payload, &mut cursor)?;
    let actual_q = read_vector6d(payload, &mut cursor)?;
    let output_int_register_0 = read_i32(payload, &mut cursor)?;

    Ok(RtdeSnapshot {
        timestamp,
        robot_mode: robot_mode_from_code(robot_mode_code),
        safety_mode: safety_mode_from_code(safety_mode_code),
        runtime_state,
        actual_tcp_pose,
        actual_q,
        output_int_register_0,
    })
}

/// Map a UR `robot_mode` integer code to the closed [`RobotMode`]
/// vocab. Out-of-range codes land on [`RobotMode::Unknown`].
pub(crate) fn robot_mode_from_code(code: i32) -> RobotMode {
    match code {
        -1 => RobotMode::NoController,
        0 => RobotMode::Disconnected,
        1 => RobotMode::ConfirmSafety,
        2 => RobotMode::Booting,
        3 => RobotMode::PowerOff,
        4 => RobotMode::PowerOn,
        5 => RobotMode::Idle,
        6 => RobotMode::Backdrive,
        7 => RobotMode::Running,
        8 => RobotMode::UpdatingFirmware,
        _ => RobotMode::Unknown,
    }
}

/// Map a UR `safety_mode` integer code to the closed [`SafetyMode`]
/// vocab. Out-of-range codes land on [`SafetyMode::Unknown`].
pub(crate) fn safety_mode_from_code(code: i32) -> SafetyMode {
    match code {
        1 => SafetyMode::Normal,
        2 => SafetyMode::Reduced,
        3 => SafetyMode::ProtectiveStop,
        4 => SafetyMode::Recovery,
        5 => SafetyMode::SafeguardStop,
        6 => SafetyMode::SystemEmergencyStop,
        7 => SafetyMode::RobotEmergencyStop,
        8 => SafetyMode::Violation,
        9 => SafetyMode::Fault,
        10 => SafetyMode::ValidateJointId,
        11 => SafetyMode::Undefined,
        _ => SafetyMode::Unknown,
    }
}

/// Validate that the SETUP_OUTPUTS response's comma-separated types
/// string matches [`EXPECTED_OUTPUT_TYPES`] exactly. Any deviation
/// (controller doesn't expose a variable, or returned a different
/// type) is fatal — we want loud-fail, not a silently wrong decode.
fn validate_output_types(types_str: &str) -> Result<(), String> {
    let actual: Vec<&str> = types_str.split(',').collect();
    if actual.len() != EXPECTED_OUTPUT_TYPES.len() {
        return Err(format!(
            "setup-outputs returned {} types, expected {}",
            actual.len(),
            EXPECTED_OUTPUT_TYPES.len()
        ));
    }
    for (i, (got, want)) in actual.iter().zip(EXPECTED_OUTPUT_TYPES.iter()).enumerate() {
        if got.trim() != *want {
            return Err(format!(
                "setup-outputs variable {} ('{}') typed '{}', expected '{}'",
                i, OUTPUT_RECIPE[i], got, want
            ));
        }
    }
    Ok(())
}

fn read_double(buf: &[u8], cursor: &mut usize) -> Result<f64, String> {
    let end = *cursor + 8;
    if end > buf.len() {
        return Err("short read: DOUBLE".to_string());
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[*cursor..end]);
    *cursor = end;
    Ok(f64::from_be_bytes(bytes))
}

fn read_i32(buf: &[u8], cursor: &mut usize) -> Result<i32, String> {
    let end = *cursor + 4;
    if end > buf.len() {
        return Err("short read: INT32".to_string());
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[*cursor..end]);
    *cursor = end;
    Ok(i32::from_be_bytes(bytes))
}

fn read_u32(buf: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let end = *cursor + 4;
    if end > buf.len() {
        return Err("short read: UINT32".to_string());
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[*cursor..end]);
    *cursor = end;
    Ok(u32::from_be_bytes(bytes))
}

fn read_vector6d(buf: &[u8], cursor: &mut usize) -> Result<[f64; 6], String> {
    let mut out = [0.0f64; 6];
    for slot in out.iter_mut() {
        *slot = read_double(buf, cursor)?;
    }
    Ok(out)
}

/// Read one RTDE frame off the wire: u16 length BE + u8 cmd +
/// (length - 3) bytes of payload. Caps total length at
/// `max_frame_bytes` for DoS-safety.
async fn read_frame(
    stream: &mut TcpStream,
    max_frame_bytes: usize,
) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 3];
    stream.read_exact(&mut header).await?;
    let total_len = u16::from_be_bytes([header[0], header[1]]) as usize;
    if total_len < 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {total_len} below header size"),
        ));
    }
    if total_len > max_frame_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {total_len} exceeds cap {max_frame_bytes}"),
        ));
    }
    let cmd = header[2];
    let payload_len = total_len - 3;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((cmd, payload))
}

/// Write one RTDE frame to the wire: u16 length BE + u8 cmd + payload.
async fn write_frame(stream: &mut TcpStream, cmd: u8, payload: &[u8]) -> std::io::Result<()> {
    let total_len_u = payload.len() + 3;
    let total_len: u16 = u16::try_from(total_len_u).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame length {total_len_u} exceeds u16"),
        )
    })?;
    let mut buf = Vec::with_capacity(total_len as usize);
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.push(cmd);
    buf.extend_from_slice(payload);
    stream.write_all(&buf).await?;
    Ok(())
}

/// Sleep but exit early on cancel. Returns `true` if the full duration
/// elapsed, `false` if cancel fired during the sleep.
async fn sleep_with_cancel(d: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        _ = cancel.cancelled() => false,
    }
}

fn next_backoff(current: Duration, cap: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > cap {
        cap
    } else {
        doubled
    }
}

fn set_health(slot: &Arc<Mutex<AdapterHealth>>, next: AdapterHealth) {
    *slot.lock().expect("health mutex poisoned") = next;
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc as StdArc;
    use tokio::net::TcpListener;
    use tokio::sync::{oneshot, Mutex as AsyncMutex};

    /// Pick an ephemeral port — same TOCTOU-tolerant pattern as the
    /// zebra/mtconnect adapter tests.
    async fn pick_free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn cfg_for_test(robot_id: &str, port: u16) -> UrRtdeAdapterConfig {
        UrRtdeAdapterConfig {
            robot_id: robot_id.to_string(),
            friendly_name: format!("Test {robot_id}"),
            host: "127.0.0.1".to_string(),
            port,
            model: "UR10e".to_string(),
            // Tight bounds for tests.
            max_frame_bytes: 4096,
            handshake_timeout: Duration::from_millis(800),
            stall_threshold: Duration::from_millis(300),
            initial_backoff: Duration::from_millis(80),
            backoff_cap: Duration::from_millis(400),
            pause_timeout: Duration::from_millis(100),
            channel_capacity: 32,
        }
    }

    /// Build a synthetic RTDE data-frame payload (recipe-ID byte +
    /// 120 bytes of declared-order variables). Used by every test
    /// that drives the data-frame path.
    fn build_data_frame_payload(
        timestamp: f64,
        robot_mode_code: i32,
        safety_mode_code: i32,
        runtime_state: u32,
        tcp_pose: [f64; 6],
        actual_q: [f64; 6],
        out_int_register_0: i32,
        recipe_id: u8,
    ) -> Vec<u8> {
        let mut p = Vec::with_capacity(1 + DATA_PAYLOAD_BYTES);
        p.push(recipe_id);
        p.extend_from_slice(&timestamp.to_be_bytes());
        p.extend_from_slice(&robot_mode_code.to_be_bytes());
        p.extend_from_slice(&safety_mode_code.to_be_bytes());
        p.extend_from_slice(&runtime_state.to_be_bytes());
        for v in tcp_pose {
            p.extend_from_slice(&v.to_be_bytes());
        }
        for v in actual_q {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.extend_from_slice(&out_int_register_0.to_be_bytes());
        p
    }

    /// Mock RTDE controller behaviour for a given test.
    enum MockBehaviour {
        /// Run the full handshake, then stream the given data frames
        /// with the given gap between them. After the sequence ends,
        /// idle on the socket (read replies forever).
        FullHandshakeThenStream {
            data_frames: Vec<Vec<u8>>, // raw payloads (recipe_id-prefixed)
            inter_frame_gap: Duration,
        },
        /// Reject the protocol-version request (respond accepted=0).
        RejectProtocolVersion,
        /// Run handshake successfully, stream ONE frame, then drop the
        /// socket. Lets us assert reconnect-attempted behaviour by
        /// having the listener accept twice.
        StreamOneFrameThenDrop { payload: Vec<u8> },
        /// Stream forever from a single repeated frame; records every
        /// frame received from the client into the captured-bytes vec
        /// so we can assert the final PAUSE on graceful shutdown.
        FullHandshakeThenStreamRecord {
            payload: Vec<u8>,
            inter_frame_gap: Duration,
            client_frames: StdArc<AsyncMutex<Vec<(u8, Vec<u8>)>>>,
        },
    }

    /// Spawn a listener that handles connections per `MockBehaviour`.
    /// Returns the listener task handle (drop to stop accepting) and
    /// an `accept_count` channel that fires once per accepted
    /// connection (only the connect-loop tests need this).
    async fn spawn_mock_controller(
        port: u16,
        behaviour: MockBehaviour,
    ) -> (tokio::task::JoinHandle<()>, oneshot::Receiver<u32>) {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let mut accept_count = 0u32;
            // We only emit on the channel once — when the first or
            // second accept happens (test-dependent). Send the running
            // count when the second accept happens for the reconnect
            // test; harmless for the others (rx is just dropped).
            let mut tx_slot = Some(tx);

            loop {
                let (mut sock, _peer) = match listener.accept().await {
                    Ok(t) => t,
                    Err(_) => return,
                };
                accept_count += 1;
                if accept_count == 2 {
                    if let Some(t) = tx_slot.take() {
                        let _ = t.send(accept_count);
                    }
                }

                let beh = clone_behaviour(&behaviour);
                tokio::spawn(async move {
                    handle_one_connection(&mut sock, beh).await;
                });
            }
        });
        (handle, rx)
    }

    fn clone_behaviour(b: &MockBehaviour) -> MockBehaviour {
        match b {
            MockBehaviour::FullHandshakeThenStream {
                data_frames,
                inter_frame_gap,
            } => MockBehaviour::FullHandshakeThenStream {
                data_frames: data_frames.clone(),
                inter_frame_gap: *inter_frame_gap,
            },
            MockBehaviour::RejectProtocolVersion => MockBehaviour::RejectProtocolVersion,
            MockBehaviour::StreamOneFrameThenDrop { payload } => {
                MockBehaviour::StreamOneFrameThenDrop {
                    payload: payload.clone(),
                }
            }
            MockBehaviour::FullHandshakeThenStreamRecord {
                payload,
                inter_frame_gap,
                client_frames,
            } => MockBehaviour::FullHandshakeThenStreamRecord {
                payload: payload.clone(),
                inter_frame_gap: *inter_frame_gap,
                client_frames: client_frames.clone(),
            },
        }
    }

    async fn handle_one_connection(sock: &mut tokio::net::TcpStream, behaviour: MockBehaviour) {
        // Read first frame: REQUEST_PROTOCOL_VERSION.
        let (cmd, _body) = match read_frame_mock(sock).await {
            Ok(v) => v,
            Err(_) => return,
        };
        if cmd != CMD_REQUEST_PROTOCOL_VERSION {
            return;
        }

        match behaviour {
            MockBehaviour::RejectProtocolVersion => {
                let _ = write_frame_mock(sock, CMD_REQUEST_PROTOCOL_VERSION, &[0u8]).await;
            }
            MockBehaviour::FullHandshakeThenStream {
                data_frames,
                inter_frame_gap,
            } => {
                if complete_handshake(sock).await.is_err() {
                    return;
                }
                stream_frames(sock, &data_frames, inter_frame_gap).await;
            }
            MockBehaviour::StreamOneFrameThenDrop { payload } => {
                if complete_handshake(sock).await.is_err() {
                    return;
                }
                let _ = write_frame_mock(sock, CMD_DATA_PACKAGE, &payload).await;
                // Drop the socket abruptly.
                let _ = sock.shutdown().await;
            }
            MockBehaviour::FullHandshakeThenStreamRecord {
                payload,
                inter_frame_gap,
                client_frames,
            } => {
                if complete_handshake(sock).await.is_err() {
                    return;
                }
                // Spawn a reader half that records every client-side
                // frame (so we can assert the PAUSE on stop()).
                let (mut read_half, mut write_half) = sock.split();
                let client_frames_for_reader = client_frames.clone();
                let read_task = async move {
                    loop {
                        let mut header = [0u8; 3];
                        if read_half.read_exact(&mut header).await.is_err() {
                            return;
                        }
                        let total = u16::from_be_bytes([header[0], header[1]]) as usize;
                        let cmd = header[2];
                        let payload_len = total.saturating_sub(3);
                        let mut payload = vec![0u8; payload_len];
                        if payload_len > 0 && read_half.read_exact(&mut payload).await.is_err() {
                            return;
                        }
                        client_frames_for_reader.lock().await.push((cmd, payload));
                    }
                };
                let write_task = async move {
                    loop {
                        if write_frame_via(&mut write_half, CMD_DATA_PACKAGE, &payload)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        tokio::time::sleep(inter_frame_gap).await;
                    }
                };
                let _ = tokio::join!(read_task, write_task);
            }
        }
    }

    /// After the protocol-version request has been read+accepted by
    /// the caller, read+respond to the remaining three handshake
    /// frames (get-urcontrol-version, setup-outputs, start).
    async fn complete_handshake(sock: &mut tokio::net::TcpStream) -> std::io::Result<()> {
        // Already read REQUEST_PROTOCOL_VERSION; respond accepted.
        write_frame_mock(sock, CMD_REQUEST_PROTOCOL_VERSION, &[1u8]).await?;

        // GET_URCONTROL_VERSION.
        let (cmd, _body) = read_frame_mock(sock).await?;
        if cmd != CMD_GET_URCONTROL_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected GET_URCONTROL_VERSION",
            ));
        }
        // Fake controller version: 5.11.0 build 12345.
        let mut version_body = Vec::with_capacity(16);
        version_body.extend_from_slice(&5u32.to_be_bytes());
        version_body.extend_from_slice(&11u32.to_be_bytes());
        version_body.extend_from_slice(&0u32.to_be_bytes());
        version_body.extend_from_slice(&12345u32.to_be_bytes());
        write_frame_mock(sock, CMD_GET_URCONTROL_VERSION, &version_body).await?;

        // CONTROL_PACKAGE_SETUP_OUTPUTS.
        let (cmd, _body) = read_frame_mock(sock).await?;
        if cmd != CMD_CONTROL_PACKAGE_SETUP_OUTPUTS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected SETUP_OUTPUTS",
            ));
        }
        // Respond recipe_id=1 + the expected types in order.
        let mut setup_body = Vec::new();
        setup_body.push(1u8);
        setup_body.extend_from_slice(EXPECTED_OUTPUT_TYPES.join(",").as_bytes());
        write_frame_mock(sock, CMD_CONTROL_PACKAGE_SETUP_OUTPUTS, &setup_body).await?;

        // CONTROL_PACKAGE_START.
        let (cmd, _body) = read_frame_mock(sock).await?;
        if cmd != CMD_CONTROL_PACKAGE_START {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected START",
            ));
        }
        write_frame_mock(sock, CMD_CONTROL_PACKAGE_START, &[1u8]).await?;

        Ok(())
    }

    async fn stream_frames(sock: &mut tokio::net::TcpStream, frames: &[Vec<u8>], gap: Duration) {
        for f in frames {
            if write_frame_mock(sock, CMD_DATA_PACKAGE, f).await.is_err() {
                return;
            }
            tokio::time::sleep(gap).await;
        }
        // After the sequence, sleep forever so the connection stays
        // open from the client's view; the client will pull cancel
        // first.
        tokio::time::sleep(Duration::from_secs(60)).await;
    }

    async fn read_frame_mock(sock: &mut tokio::net::TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
        let mut header = [0u8; 3];
        sock.read_exact(&mut header).await?;
        let total = u16::from_be_bytes([header[0], header[1]]) as usize;
        let cmd = header[2];
        let payload_len = total.saturating_sub(3);
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            sock.read_exact(&mut payload).await?;
        }
        Ok((cmd, payload))
    }

    async fn write_frame_mock(
        sock: &mut tokio::net::TcpStream,
        cmd: u8,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let total_len = (payload.len() + 3) as u16;
        let mut buf = Vec::with_capacity(total_len as usize);
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(cmd);
        buf.extend_from_slice(payload);
        sock.write_all(&buf).await
    }

    /// Variant of write_frame_mock that takes the split write half.
    async fn write_frame_via(
        w: &mut tokio::net::tcp::WriteHalf<'_>,
        cmd: u8,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let total_len = (payload.len() + 3) as u16;
        let mut buf = Vec::with_capacity(total_len as usize);
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(cmd);
        buf.extend_from_slice(payload);
        w.write_all(&buf).await
    }

    // ====== Defaults ======

    #[test]
    fn config_defaults_match_documented_constants() {
        let cfg = UrRtdeAdapterConfig::new(
            "cell-a-robot",
            "UR10e — Cell A",
            "10.0.2.71",
            30004,
            "UR10e",
        );
        assert_eq!(cfg.port, 30004);
        assert_eq!(cfg.model, "UR10e");
        assert_eq!(cfg.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(cfg.handshake_timeout, DEFAULT_HANDSHAKE_TIMEOUT);
        assert_eq!(cfg.stall_threshold, DEFAULT_STALL_THRESHOLD);
        assert_eq!(cfg.initial_backoff, DEFAULT_INITIAL_BACKOFF);
        assert_eq!(cfg.backoff_cap, DEFAULT_BACKOFF_CAP);
        assert_eq!(cfg.pause_timeout, DEFAULT_PAUSE_TIMEOUT);
        assert_eq!(cfg.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
    }

    // ====== Pure helpers ======

    #[test]
    fn robot_mode_mapping_pins_closed_vocab() {
        assert_eq!(robot_mode_from_code(-1), RobotMode::NoController);
        assert_eq!(robot_mode_from_code(0), RobotMode::Disconnected);
        assert_eq!(robot_mode_from_code(1), RobotMode::ConfirmSafety);
        assert_eq!(robot_mode_from_code(2), RobotMode::Booting);
        assert_eq!(robot_mode_from_code(3), RobotMode::PowerOff);
        assert_eq!(robot_mode_from_code(4), RobotMode::PowerOn);
        assert_eq!(robot_mode_from_code(5), RobotMode::Idle);
        assert_eq!(robot_mode_from_code(6), RobotMode::Backdrive);
        assert_eq!(robot_mode_from_code(7), RobotMode::Running);
        assert_eq!(robot_mode_from_code(8), RobotMode::UpdatingFirmware);
        // Out of range → Unknown (never silently maps to a known
        // variant — CLAUDE.md rule 12).
        assert_eq!(robot_mode_from_code(9), RobotMode::Unknown);
        assert_eq!(robot_mode_from_code(-2), RobotMode::Unknown);
        assert_eq!(robot_mode_from_code(i32::MIN), RobotMode::Unknown);
    }

    #[test]
    fn safety_mode_mapping_pins_closed_vocab() {
        assert_eq!(safety_mode_from_code(1), SafetyMode::Normal);
        assert_eq!(safety_mode_from_code(2), SafetyMode::Reduced);
        assert_eq!(safety_mode_from_code(3), SafetyMode::ProtectiveStop);
        assert_eq!(safety_mode_from_code(4), SafetyMode::Recovery);
        assert_eq!(safety_mode_from_code(5), SafetyMode::SafeguardStop);
        assert_eq!(safety_mode_from_code(6), SafetyMode::SystemEmergencyStop);
        assert_eq!(safety_mode_from_code(7), SafetyMode::RobotEmergencyStop);
        assert_eq!(safety_mode_from_code(8), SafetyMode::Violation);
        assert_eq!(safety_mode_from_code(9), SafetyMode::Fault);
        assert_eq!(safety_mode_from_code(10), SafetyMode::ValidateJointId);
        assert_eq!(safety_mode_from_code(11), SafetyMode::Undefined);
        assert_eq!(safety_mode_from_code(0), SafetyMode::Unknown);
        assert_eq!(safety_mode_from_code(12), SafetyMode::Unknown);
    }

    #[test]
    fn decode_data_payload_round_trips_known_variables() {
        let payload = build_data_frame_payload(
            1234567.5,
            7, // Running
            1, // Normal
            2, // PLAYING
            [0.1, 0.2, 0.3, -0.4, -0.5, -0.6],
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            42,
            1,
        );
        let s = decode_data_payload(&payload).unwrap();
        assert_eq!(s.timestamp, 1234567.5);
        assert_eq!(s.robot_mode, RobotMode::Running);
        assert_eq!(s.safety_mode, SafetyMode::Normal);
        assert_eq!(s.runtime_state, 2);
        assert_eq!(s.actual_tcp_pose, [0.1, 0.2, 0.3, -0.4, -0.5, -0.6]);
        assert_eq!(s.actual_q, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(s.output_int_register_0, 42);
    }

    #[test]
    fn decode_data_payload_rejects_wrong_size() {
        let too_short = vec![1u8; 50];
        let err = decode_data_payload(&too_short).expect_err("size mismatch must fail");
        assert!(err.contains("size"), "{err}");
    }

    #[test]
    fn validate_output_types_accepts_canonical_response() {
        let canonical = EXPECTED_OUTPUT_TYPES.join(",");
        validate_output_types(&canonical).unwrap();
    }

    #[test]
    fn validate_output_types_rejects_wrong_count() {
        let too_few = EXPECTED_OUTPUT_TYPES[..3].join(",");
        let err = validate_output_types(&too_few).expect_err("count mismatch must fail");
        assert!(err.contains("expected"), "{err}");
    }

    #[test]
    fn validate_output_types_rejects_wrong_type() {
        // Swap INT32 for the runtime_state slot (which is UINT32) —
        // the validator MUST fail loud, not silently accept a
        // misaligned wire format.
        let mut types: Vec<&str> = EXPECTED_OUTPUT_TYPES.to_vec();
        types[3] = "DOUBLE";
        let bad = types.join(",");
        let err = validate_output_types(&bad).expect_err("type mismatch must fail");
        assert!(err.contains("typed"), "{err}");
    }

    // ====== S249-F11: first-frame sanity ======

    #[test]
    fn validate_first_frame_accepts_in_range_modes() {
        // robot_mode_code=7 (Running), safety_mode_code=1 (Normal).
        let p = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        validate_first_frame(&p).expect("canonical values must pass");
    }

    #[test]
    fn validate_first_frame_rejects_unknown_robot_mode() {
        // 99 is out of -1..=8 → maps to RobotMode::Unknown.
        let p = build_data_frame_payload(0.0, 99, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let err = validate_first_frame(&p).expect_err("Unknown robot_mode must reject");
        assert!(err.contains("robot_mode"), "{err}");
    }

    #[test]
    fn validate_first_frame_rejects_unknown_safety_mode() {
        // 99 is out of 1..=11 → maps to SafetyMode::Unknown.
        let p = build_data_frame_payload(0.0, 7, 99, 2, [0.0; 6], [0.0; 6], 0, 1);
        let err = validate_first_frame(&p).expect_err("Unknown safety_mode must reject");
        assert!(err.contains("safety_mode"), "{err}");
    }

    #[test]
    fn validate_first_frame_rejects_short_payload() {
        let too_short = vec![1u8; 10];
        let err = validate_first_frame(&too_short).expect_err("short payload must reject");
        assert!(err.contains("decode"), "{err}");
    }

    #[test]
    fn next_backoff_doubles_until_cap() {
        assert_eq!(
            next_backoff(Duration::from_millis(500), Duration::from_secs(30)),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(1), Duration::from_secs(30)),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(16), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
        // Cap is sticky.
        assert_eq!(
            next_backoff(Duration::from_secs(30), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    // ====== Live HTTP via mock controller ======

    #[tokio::test]
    async fn start_against_valid_controller_reaches_healthy() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![frame],
                inter_frame_gap: Duration::from_millis(50),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-1", port));
        adapter.start().await.unwrap();

        // Wait up to 2s for the connect+handshake to complete and the
        // first data frame to flip health to Healthy.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got = adapter.health();
        while std::time::Instant::now() < deadline {
            if matches!(got, AdapterHealth::Healthy) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            got = adapter.health();
        }
        assert_eq!(got, AdapterHealth::Healthy, "expected Healthy");

        adapter.stop().await.unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Stopped);
    }

    #[tokio::test]
    async fn start_against_protocol_reject_reports_unhealthy() {
        let port = pick_free_port().await;
        let (_listener, _rx) =
            spawn_mock_controller(port, MockBehaviour::RejectProtocolVersion).await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-reject", port));
        adapter.start().await.unwrap();

        // Wait for handshake to fail.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got = adapter.health();
        while std::time::Instant::now() < deadline {
            if matches!(got, AdapterHealth::Unhealthy { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            got = adapter.health();
        }
        match got {
            AdapterHealth::Unhealthy { reason } => {
                assert!(
                    reason.contains("reject") || reason.contains("rejected"),
                    "{reason}"
                );
            }
            other => panic!("expected Unhealthy after protocol reject, got {other:?}"),
        }
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn start_against_closed_port_reports_unhealthy() {
        let port = pick_free_port().await;
        // No mock — port should refuse on loopback.
        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-closed", port));
        adapter.start().await.unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got = adapter.health();
        while std::time::Instant::now() < deadline {
            if matches!(got, AdapterHealth::Unhealthy { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            got = adapter.health();
        }
        match got {
            AdapterHealth::Unhealthy { reason } => {
                assert!(
                    reason.contains("connect")
                        || reason.contains("refused")
                        || reason.contains("error"),
                    "{reason}"
                );
            }
            other => panic!("expected Unhealthy after closed port, got {other:?}"),
        }
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn first_data_frame_emits_unknown_baseline_event() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![frame],
                inter_frame_gap: Duration::from_millis(50),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-baseline", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let evt = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        match evt {
            CanonicalEvent::RobotStateChanged {
                robot_id,
                previous_mode,
                new_mode,
                previous_safety,
                new_safety,
                ..
            } => {
                assert_eq!(robot_id, "ur-baseline");
                assert_eq!(previous_mode, RobotMode::Unknown);
                assert_eq!(new_mode, RobotMode::Running);
                assert_eq!(previous_safety, SafetyMode::Unknown);
                assert_eq!(new_safety, SafetyMode::Normal);
            }
            other => panic!("expected RobotStateChanged, got {other:?}"),
        }
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mode_transition_between_frames_emits_event() {
        let port = pick_free_port().await;
        // Two frames: Running/Normal → Idle/Normal.
        let f1 = build_data_frame_payload(1.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let f2 = build_data_frame_payload(2.0, 5, 1, 1, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![f1, f2],
                inter_frame_gap: Duration::from_millis(60),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-mode-trans", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        // First: Unknown → Running.
        let e1 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("first event")
            .expect("channel");
        match e1 {
            CanonicalEvent::RobotStateChanged {
                previous_mode,
                new_mode,
                ..
            } => {
                assert_eq!(previous_mode, RobotMode::Unknown);
                assert_eq!(new_mode, RobotMode::Running);
            }
            other => panic!("expected RobotStateChanged, got {other:?}"),
        }
        // Second: Running → Idle.
        let e2 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("second event")
            .expect("channel");
        match e2 {
            CanonicalEvent::RobotStateChanged {
                previous_mode,
                new_mode,
                previous_safety,
                new_safety,
                ..
            } => {
                assert_eq!(previous_mode, RobotMode::Running);
                assert_eq!(new_mode, RobotMode::Idle);
                assert_eq!(previous_safety, SafetyMode::Normal);
                assert_eq!(new_safety, SafetyMode::Normal);
            }
            other => panic!("expected RobotStateChanged, got {other:?}"),
        }
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn safety_transition_alone_emits_event() {
        let port = pick_free_port().await;
        // Two frames with SAME mode but different safety: Running/Normal
        // → Running/ProtectiveStop.
        let f1 = build_data_frame_payload(1.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let f2 = build_data_frame_payload(2.0, 7, 3, 2, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![f1, f2],
                inter_frame_gap: Duration::from_millis(60),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-safety", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        // First: Unknown → Running/Normal baseline.
        let _baseline = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("baseline")
            .expect("channel");
        // Second: mode unchanged, safety changed.
        let e2 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("safety event")
            .expect("channel");
        match e2 {
            CanonicalEvent::RobotStateChanged {
                previous_mode,
                new_mode,
                previous_safety,
                new_safety,
                ..
            } => {
                assert_eq!(previous_mode, RobotMode::Running);
                assert_eq!(new_mode, RobotMode::Running);
                assert_eq!(previous_safety, SafetyMode::Normal);
                assert_eq!(new_safety, SafetyMode::ProtectiveStop);
            }
            other => panic!("expected RobotStateChanged, got {other:?}"),
        }
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn unchanged_state_across_frames_emits_no_duplicate_event() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                // Same frame twice — should produce ONE event (the
                // Unknown→Running baseline), not two.
                data_frames: vec![frame.clone(), frame.clone(), frame],
                inter_frame_gap: Duration::from_millis(60),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-steady", port));
        let mut rx = adapter.subscribe();
        adapter.start().await.unwrap();

        let _baseline = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("baseline")
            .expect("channel");
        // No further events within a couple of frame gaps.
        let next = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            next.is_err(),
            "no event expected on unchanged state, got: {next:?}"
        );

        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mid_stream_socket_drop_triggers_reconnect() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, rx_accept) = spawn_mock_controller(
            port,
            MockBehaviour::StreamOneFrameThenDrop { payload: frame },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-reconnect", port));
        adapter.start().await.unwrap();

        // The mock drops after one frame; the connect loop should
        // reconnect, triggering a second accept. The receiver fires on
        // the second accept.
        let n = tokio::time::timeout(Duration::from_secs(3), rx_accept)
            .await
            .expect("second accept in time")
            .expect("oneshot channel open");
        assert_eq!(n, 2);

        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn graceful_shutdown_sends_pause_frame() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 7, 1, 2, [0.0; 6], [0.0; 6], 0, 1);
        let client_frames: StdArc<AsyncMutex<Vec<(u8, Vec<u8>)>>> =
            StdArc::new(AsyncMutex::new(Vec::new()));
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStreamRecord {
                payload: frame,
                inter_frame_gap: Duration::from_millis(60),
                client_frames: client_frames.clone(),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-pause", port));
        adapter.start().await.unwrap();

        // Wait for the adapter to reach Healthy (handshake done +
        // first data frame in).
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if matches!(adapter.health(), AdapterHealth::Healthy) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        adapter.stop().await.unwrap();

        // The mock records every client frame. The recorded sequence
        // should be: V (proto-version), v (urcontrol-version), O
        // (setup-outputs), S (start), then P (pause) on shutdown.
        // Give the mock a moment to drain the PAUSE write.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let frames = client_frames.lock().await;
        let cmds: Vec<u8> = frames.iter().map(|(c, _)| *c).collect();
        assert!(
            cmds.contains(&CMD_CONTROL_PACKAGE_PAUSE),
            "expected PAUSE in client frames, got: {cmds:?}"
        );
        // Pause must be the LAST recorded frame.
        assert_eq!(
            *cmds.last().unwrap(),
            CMD_CONTROL_PACKAGE_PAUSE,
            "PAUSE must be the final frame, got: {cmds:?}"
        );
    }

    // ====== Lifecycle ======

    #[tokio::test]
    async fn start_is_idempotent() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 5, 1, 1, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![frame],
                inter_frame_gap: Duration::from_millis(50),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-idem-start", port));
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        adapter.start().await.unwrap();
        adapter.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let port = pick_free_port().await;
        let frame = build_data_frame_payload(0.0, 5, 1, 1, [0.0; 6], [0.0; 6], 0, 1);
        let (_listener, _rx) = spawn_mock_controller(
            port,
            MockBehaviour::FullHandshakeThenStream {
                data_frames: vec![frame],
                inter_frame_gap: Duration::from_millis(50),
            },
        )
        .await;

        let adapter = UrRtdeAdapter::new(cfg_for_test("ur-idem-stop", port));
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
        let adapter = UrRtdeAdapter::new(cfg_for_test("meta", port));
        assert_eq!(adapter.name(), "meta");
        assert_eq!(adapter.kind(), "robot");
        assert_eq!(adapter.endpoint_host(), Some("127.0.0.1".to_string()));
        assert_eq!(adapter.endpoint_port(), Some(port));
        assert_eq!(adapter.friendly_name(), "Test meta");
        assert_eq!(adapter.model(), "UR10e");
    }

    #[tokio::test]
    async fn ur_rtde_adapter_is_dyn_safe() {
        let port = pick_free_port().await;
        let adapter: Arc<dyn Adapter> = Arc::new(UrRtdeAdapter::new(cfg_for_test("dyn", port)));
        assert_eq!(adapter.name(), "dyn");
        assert_eq!(adapter.kind(), "robot");
    }
}
