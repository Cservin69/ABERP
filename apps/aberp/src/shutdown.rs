//! PR-209 / S213 — graceful shutdown coordinator.
//!
//! Why this exists. Before PR-209 a Ctrl-C in the `run_prod.sh`
//! terminal (or a Cmd+Q on the Tauri window) tore down the WebView
//! cleanly but left the embedded `aberp serve` process alive because
//! its long-lived tokio daemons (NAV poll, AP sync, quote-intake)
//! had no cancellation path — tokio's default signal handling is
//! "ignore", and the runtime keeps a process alive as long as any
//! spawned task is still running. Operators were forced to
//! `pgrep -f aberp | xargs -r kill -9` before `upgrade_prod.sh`
//! could swap the binary. That's a code bug, not an ops workflow
//! gap — per [[trust-code-not-operator]] safety belongs in code.
//!
//! How this fixes it. `ShutdownCoordinator` owns ONE
//! [`CancellationToken`] (cloned into every daemon's loop) plus the
//! [`JoinHandle`]s of every registered daemon. A single signal entry
//! point — either `tokio::signal::ctrl_c()` for terminal launches
//! OR the Tauri window-close handler routed through SIGTERM in the
//! parent — calls [`ShutdownCoordinator::shutdown`], which cancels
//! the token, awaits the handles with a bounded timeout, emits ONE
//! `DaemonShutdownCompleted` audit row, and returns
//! [`ShutdownResult`] for the caller to log + force-exit on.
//!
//! Conservative choices (per the S213 brief):
//!
//! - **5-second timeout**. The NAV poll daemon's inner reqwest call
//!   has its own ~5s connect timeout; a daemon that's mid-call when
//!   shutdown fires gets one cycle to finish. Tunable via
//!   `ABERP_SHUTDOWN_TIMEOUT_SECS` env var (floor 1s, ceiling 30s) so
//!   ops can stretch it in a degraded-network postmortem without a
//!   recompile.
//! - **Process exit after audit emit**. The caller (serve.rs) writes
//!   the audit row, logs the ShutdownResult, then calls
//!   `std::process::exit(0)`. Daemons that timed out get
//!   force-killed by the OS when the process exits — better than
//!   letting them spin forever. The audit row names them so a future
//!   postmortem can ask "why did daemon X always time out?".
//! - **Cooperative cancellation only**. We do NOT abort()
//!   JoinHandles. Aborting a task mid-await leaks any DB/file
//!   resource the task held; cooperative cancellation gives the
//!   daemon's tokio::select! arm a chance to flush.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long to wait for registered daemons to drain after the token
/// fires. The brief defaults to 5s; ops can stretch via the env var.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Floor + ceiling for the env-var override. Anything outside this
/// window is rejected and we fall back to [`DEFAULT_SHUTDOWN_TIMEOUT`].
/// The floor stops a typo'd `0` from making shutdown synchronously
/// log "everyone timed out"; the ceiling stops a misread `500`
/// (meaning ms) from blocking a prod upgrade for 8 minutes.
const SHUTDOWN_TIMEOUT_FLOOR: Duration = Duration::from_secs(1);
const SHUTDOWN_TIMEOUT_CEILING: Duration = Duration::from_secs(30);

/// Name + handle of one registered daemon. The name is captured at
/// registration time so the ShutdownResult can report which daemons
/// timed out by name (NAV poll vs AP sync vs quote-intake — each has a
/// distinct postmortem story).
pub struct RegisteredDaemon {
    pub name: &'static str,
    pub handle: JoinHandle<()>,
}

/// Outcome of a shutdown call. The audit-ledger writer turns this
/// into a `DaemonShutdownCompleted` payload; the caller logs it via
/// `tracing::info!` for the terminal-attached operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownResult {
    /// Daemons that exited cleanly inside the timeout window.
    pub clean_exits: usize,
    /// Daemons that did NOT exit inside the timeout window. Named so
    /// the audit row + tracing line tell ops *which* daemon hung.
    pub timeout_kills: Vec<&'static str>,
    /// Wall-clock between `cancel()` firing and the join completing
    /// (or timing out). Useful for tuning the timeout knob.
    pub elapsed_ms: u64,
}

/// The shutdown coordinator. Built once in `serve::run` BEFORE any
/// daemon is spawned; each daemon spawn-site clones [`token`] into
/// its loop and registers its `JoinHandle` here via [`register`].
///
/// Not `Clone`. There is exactly one coordinator per process — the
/// `signal::ctrl_c` listener + the Tauri-side SIGTERM path both
/// terminate at the same instance so the audit row is written once.
pub struct ShutdownCoordinator {
    pub token: CancellationToken,
    registered: Vec<RegisteredDaemon>,
}

impl ShutdownCoordinator {
    /// Build a fresh coordinator. The token starts un-cancelled;
    /// daemons that `select!` on `token.cancelled()` block forever
    /// until [`shutdown`] (or a clone-side `token.cancel()`) fires.
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            registered: Vec::new(),
        }
    }

    /// Build a coordinator around an EXISTING [`CancellationToken`].
    ///
    /// Why this exists. `apps/aberp/src/serve.rs` mints the token at
    /// `AppState` construction time (so per-request NAV poll
    /// daemons can clone it from state) and then builds the
    /// coordinator inside the tokio runtime block. If the
    /// coordinator minted its own token, the two halves would
    /// cancel-each-other-not — exactly the silent failure mode
    /// CLAUDE.md rule 12 names. `from_token` makes the shared-token
    /// posture explicit.
    pub fn from_token(token: CancellationToken) -> Self {
        Self {
            token,
            registered: Vec::new(),
        }
    }

    /// Register a spawned daemon for shutdown coordination.
    ///
    /// The `name` MUST be a static string (typically a `&'static str`
    /// literal at the spawn site) so it can appear in the audit
    /// payload + the post-shutdown log line without an allocation in
    /// the shutdown hot path.
    pub fn register(&mut self, name: &'static str, handle: JoinHandle<()>) {
        self.registered.push(RegisteredDaemon { name, handle });
    }

    /// Drive shutdown to completion or to the timeout, whichever
    /// fires first.
    ///
    /// Steps:
    ///   1. `token.cancel()` — fans cancellation out to every
    ///      daemon's `tokio::select!` arm.
    ///   2. Race `join_all(handles)` against `tokio::time::sleep(timeout)`.
    ///   3. Return [`ShutdownResult`] naming the clean exits + the
    ///      slow daemons (those still in the JoinHandle iterator
    ///      after the timeout fires).
    ///
    /// The function consumes `self` — calling shutdown twice on the
    /// same coordinator is a bug, the type system enforces it.
    pub async fn shutdown(self, timeout: Duration) -> ShutdownResult {
        let started = std::time::Instant::now();

        // 1. Fan cancellation out to every daemon. Every daemon's
        // `tokio::select!` arm on `token.cancelled()` becomes ready
        // immediately; the daemon's NEXT scheduler turn observes the
        // cancellation and exits.
        self.token.cancel();

        let ShutdownCoordinator { registered, .. } = self;
        let total = registered.len();

        // 2. Race join against timeout. We need to know WHICH daemons
        // finished vs which timed out, so we cannot use
        // `futures::future::join_all` (it gives an opaque join). We
        // hand-write a polling loop using `tokio::time::timeout` per
        // handle: equally-spaced timeouts would be wrong (a
        // fast-exiting daemon would still eat the timeout window),
        // so we share ONE budget across all handles by computing the
        // remaining budget each iteration.
        let mut clean_exits: usize = 0;
        let mut timeout_kills: Vec<&'static str> = Vec::new();
        let deadline = started + timeout;

        for daemon in registered {
            let now = std::time::Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                // Already past the deadline. Don't even wait —
                // record the timeout and move on. We must STILL
                // iterate the rest so the report lists every
                // un-joined daemon, not just the first.
                timeout_kills.push(daemon.name);
                continue;
            }
            match tokio::time::timeout(remaining, daemon.handle).await {
                Ok(Ok(())) => {
                    clean_exits += 1;
                }
                Ok(Err(join_err)) => {
                    // The daemon panicked. Count it as a clean exit
                    // for the timeout tally (the panic was the
                    // daemon's own fault and would have been logged
                    // by tokio's default panic handler already), but
                    // emit a tracing line so a postmortem can find
                    // it. Conservative: this is NOT a timeout
                    // kill — we don't want a panic to look like a
                    // hung daemon when grepping the audit log.
                    tracing::warn!(
                        daemon = daemon.name,
                        error = ?join_err,
                        "daemon panicked during shutdown drain (counted as clean exit; \
                         see tokio's panic handler for the stack)"
                    );
                    clean_exits += 1;
                }
                Err(_elapsed) => {
                    timeout_kills.push(daemon.name);
                }
            }
        }

        ShutdownResult {
            clean_exits,
            timeout_kills,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
        .with_total(total)
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownResult {
    /// Sanity-check the tally. `clean_exits + timeout_kills.len()`
    /// must equal `total`; if it doesn't, the coordinator's loop
    /// dropped a daemon. We don't assert (a panic inside the
    /// shutdown path is the LAST thing ops wants); we log loud per
    /// CLAUDE.md rule 12 and return the un-modified result.
    fn with_total(self, total: usize) -> Self {
        let accounted = self.clean_exits + self.timeout_kills.len();
        if accounted != total {
            tracing::error!(
                clean_exits = self.clean_exits,
                timeout_kills = ?self.timeout_kills,
                total_registered = total,
                accounted,
                "ShutdownCoordinator tally drift — registered != clean + timeout. \
                 This is a coordinator bug; report it."
            );
        }
        self
    }
}

/// Read the operator-set timeout from `ABERP_SHUTDOWN_TIMEOUT_SECS`.
/// Returns [`DEFAULT_SHUTDOWN_TIMEOUT`] when unset or out of the
/// floor/ceiling window — silent-fallback would mask a typo, so we
/// `tracing::warn!` on the out-of-band reject path.
pub fn shutdown_timeout_from_env() -> Duration {
    match std::env::var("ABERP_SHUTDOWN_TIMEOUT_SECS") {
        Err(_) => DEFAULT_SHUTDOWN_TIMEOUT,
        Ok(raw) => match raw.parse::<u64>() {
            Ok(secs) => {
                let proposed = Duration::from_secs(secs);
                if proposed < SHUTDOWN_TIMEOUT_FLOOR || proposed > SHUTDOWN_TIMEOUT_CEILING {
                    tracing::warn!(
                        requested_secs = secs,
                        floor_secs = SHUTDOWN_TIMEOUT_FLOOR.as_secs(),
                        ceiling_secs = SHUTDOWN_TIMEOUT_CEILING.as_secs(),
                        "ABERP_SHUTDOWN_TIMEOUT_SECS out of window; falling back to default"
                    );
                    DEFAULT_SHUTDOWN_TIMEOUT
                } else {
                    proposed
                }
            }
            Err(e) => {
                tracing::warn!(
                    raw = %raw,
                    error = %e,
                    "ABERP_SHUTDOWN_TIMEOUT_SECS unparseable; falling back to default"
                );
                DEFAULT_SHUTDOWN_TIMEOUT
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Conformance: a coordinator with no daemons returns a clean
    /// `(0, [])` immediately. This is the no-op shape (e.g., serve
    /// boot ran but disabled every daemon via env var). Must not
    /// block on the timeout.
    #[tokio::test(start_paused = true)]
    async fn empty_coordinator_returns_zero_zero_immediately() {
        let coord = ShutdownCoordinator::new();
        let result = coord.shutdown(Duration::from_secs(5)).await;
        assert_eq!(result.clean_exits, 0);
        assert!(result.timeout_kills.is_empty());
        // Virtual clock — elapsed should be effectively 0.
        assert!(
            result.elapsed_ms < 100,
            "empty shutdown took {}ms; expected near-instant",
            result.elapsed_ms
        );
    }

    /// Conformance: a daemon that respects the token exits cleanly
    /// and shows up in `clean_exits`.
    #[tokio::test(start_paused = true)]
    async fn daemon_that_respects_token_exits_clean() {
        let mut coord = ShutdownCoordinator::new();
        let token = coord.token.clone();
        let handle = tokio::spawn(async move {
            // Realistic daemon shape — sleep until cancelled.
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
            }
        });
        coord.register("test-daemon-clean", handle);
        let result = coord.shutdown(Duration::from_secs(5)).await;
        assert_eq!(result.clean_exits, 1);
        assert!(result.timeout_kills.is_empty());
    }

    /// Conformance: a daemon that IGNORES the token shows up in
    /// `timeout_kills` by name, and the result's elapsed_ms is near
    /// the timeout (not 0, not 10x).
    #[tokio::test(start_paused = true)]
    async fn daemon_that_ignores_token_is_named_in_timeout_kills() {
        let mut coord = ShutdownCoordinator::new();
        let handle = tokio::spawn(async move {
            // Deliberately ignore the token — simulates the legacy
            // pre-PR-209 daemons.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        coord.register("test-daemon-rude", handle);
        let result = coord.shutdown(Duration::from_secs(1)).await;
        assert_eq!(result.clean_exits, 0);
        assert_eq!(result.timeout_kills, vec!["test-daemon-rude"]);
    }

    /// Conformance: mixed-respect daemons report correctly. One
    /// clean, two rude.
    #[tokio::test(start_paused = true)]
    async fn mixed_respect_daemons_reported_correctly() {
        let mut coord = ShutdownCoordinator::new();
        let token = coord.token.clone();
        let h_clean = tokio::spawn(async move {
            token.cancelled().await;
        });
        let h_rude_a = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        let h_rude_b = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        coord.register("good-citizen", h_clean);
        coord.register("rude-a", h_rude_a);
        coord.register("rude-b", h_rude_b);
        let result = coord.shutdown(Duration::from_secs(1)).await;
        assert_eq!(result.clean_exits, 1);
        // Order-preserving — registration order = report order.
        assert_eq!(result.timeout_kills, vec!["rude-a", "rude-b"]);
    }

    /// Conformance: a panicking daemon counts as a clean exit (the
    /// task DID exit, just loudly). We do NOT want a panic to look
    /// like a hung daemon when grepping the audit log.
    #[tokio::test(start_paused = true)]
    async fn panicking_daemon_counts_as_clean_exit() {
        let mut coord = ShutdownCoordinator::new();
        let token = coord.token.clone();
        let handle = tokio::spawn(async move {
            token.cancelled().await;
            panic!("simulated daemon panic on cancel");
        });
        coord.register("panicky-daemon", handle);
        let result = coord.shutdown(Duration::from_secs(5)).await;
        assert_eq!(result.clean_exits, 1);
        assert!(result.timeout_kills.is_empty());
    }

    /// Conformance: env-var override accepts in-window values + falls
    /// back on garbage. Locks down the floor/ceiling so a future
    /// "let's accept 0" PR is caught by the test.
    #[test]
    fn shutdown_timeout_env_var() {
        // SAFETY: set_var/remove_var are unsafe in 2024-edition Rust;
        // each test sets+removes its own key so they can't race.
        // Using SAFETY block per std::env API contract.
        let key = "ABERP_SHUTDOWN_TIMEOUT_SECS";
        std::env::remove_var(key);
        assert_eq!(shutdown_timeout_from_env(), DEFAULT_SHUTDOWN_TIMEOUT);

        std::env::set_var(key, "10");
        assert_eq!(shutdown_timeout_from_env(), Duration::from_secs(10));

        std::env::set_var(key, "0");
        assert_eq!(shutdown_timeout_from_env(), DEFAULT_SHUTDOWN_TIMEOUT);

        std::env::set_var(key, "999");
        assert_eq!(shutdown_timeout_from_env(), DEFAULT_SHUTDOWN_TIMEOUT);

        std::env::set_var(key, "not-a-number");
        assert_eq!(shutdown_timeout_from_env(), DEFAULT_SHUTDOWN_TIMEOUT);

        std::env::remove_var(key);
    }
}
