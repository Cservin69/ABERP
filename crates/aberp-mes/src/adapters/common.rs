//! Shared task-lifecycle scaffolding for client adapters (S259 / PR-248).
//!
//! ## What this module IS
//!
//! [`AdapterLifecycle`] owns the three pieces of interior-mutable state
//! that the Zebra (PR-238), MTConnect (PR-240), and UR-RTDE (PR-241)
//! adapters were each re-implementing byte-for-byte:
//!
//! 1. the cached [`AdapterHealth`] snapshot behind an `Arc<Mutex<…>>`
//!    (cloned into the spawned background loop so it can publish health),
//! 2. the [`CancellationToken`] that `stop()` fires to unwind the loop,
//! 3. the [`JoinHandle`] the loop runs on, awaited during `stop()`.
//!
//! It centralises the **idempotent start/stop handshake** and the
//! **health state machine** (`Stopped → Starting → … → Stopped`). That
//! state machine is load-bearing: S258 (`AdapterHealthTransitioned`)
//! audits every transition, so having ONE place that can write
//! `Starting`/`Stopped` is a correctness win, not just a LOC win.
//!
//! ## What this module is deliberately NOT (see PR-248 design note)
//!
//! - **No socket/transport abstraction.** The three adapters' wire loops
//!   are genuinely different shapes — Zebra is an interval TCP *probe*,
//!   MTConnect is an interval HTTP *poll*, UR-RTDE is a persistent TCP
//!   *stream with exponential reconnect-backoff*. Forcing those under one
//!   trait would hide more than it saves. Each adapter keeps its own
//!   concrete loop function; this module only owns the lifecycle *around*
//!   the loop.
//! - **No backoff policy.** Only UR-RTDE reconnects with backoff (1 of 3
//!   consumers). Promoting `next_backoff` here would be designing for a
//!   hypothetical adapter #4 off a single example — deferred to the
//!   rule-of-three.
//! - **No `classify_io_error`.** Lives in `zebra` (its only consumer);
//!   UR-RTDE formats its connect errors differently and unifying the two
//!   would change operator-visible reason strings.
//! - **`barcode_scanner` is not a consumer.** It is a TCP *server*
//!   (binds + accepts), tracks health in a lock-free `AtomicU8`, and its
//!   `start()` can fail (bind error). Its lifecycle shape is different
//!   enough that adopting `AdapterLifecycle` would be a behaviour change,
//!   not a dedup — left as-is.

use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapter::AdapterHealth;

/// Owns the cancel token, background task handle, and cached health for
/// one client adapter, and encodes the idempotent start/stop handshake
/// the Zebra / MTConnect / UR-RTDE adapters share.
///
/// Construct one in the adapter's `new()`; drive it from the adapter's
/// `Adapter::{start,stop,health}` impls. The adapter still owns and
/// spawns its own protocol loop — this type only brokers the lifecycle
/// state around it.
#[derive(Debug)]
pub struct AdapterLifecycle {
    /// Cached health snapshot. `Arc` so the spawned loop can hold a clone
    /// (via [`AdapterLifecycle::health_slot`]) and publish into the same
    /// cell the adapter's `health()` reads from.
    health: Arc<Mutex<AdapterHealth>>,
    /// Set by [`begin_start`](AdapterLifecycle::begin_start); taken +
    /// fired by [`stop`](AdapterLifecycle::stop).
    cancel: Mutex<Option<CancellationToken>>,
    /// Handle to the spawned loop; awaited during `stop()`.
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Default for AdapterLifecycle {
    fn default() -> Self {
        Self {
            health: Arc::new(Mutex::new(AdapterHealth::Stopped)),
            cancel: Mutex::new(None),
            task: Mutex::new(None),
        }
    }
}

impl AdapterLifecycle {
    /// A fresh, stopped lifecycle (health = [`AdapterHealth::Stopped`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared health cell, for the adapter to clone into its spawned
    /// loop so the loop can publish health updates that the adapter's
    /// `health()` observes.
    pub fn health_slot(&self) -> Arc<Mutex<AdapterHealth>> {
        self.health.clone()
    }

    /// Current cached health snapshot. Backs `Adapter::health`.
    pub fn health(&self) -> AdapterHealth {
        self.health.lock().expect("health mutex poisoned").clone()
    }

    /// Overwrite the cached health. Used by the adapter for out-of-band
    /// flips (e.g. Zebra's `print_zpl` marking `Unhealthy` after a failed
    /// write) and, via the cloned [`health_slot`](Self::health_slot), by
    /// the spawned loop.
    pub fn set_health(&self, next: AdapterHealth) {
        *self.health.lock().expect("health mutex poisoned") = next;
    }

    /// Idempotent start guard.
    ///
    /// If the adapter is already running (health is not `Stopped`),
    /// returns `None` — the caller should treat `start()` as a no-op.
    /// Otherwise transitions `Stopped → Starting`, mints + stores a fresh
    /// [`CancellationToken`], and returns a clone for the caller to hand
    /// to the loop it is about to spawn. After spawning, the caller calls
    /// [`attach`](Self::attach) with the resulting handle.
    pub fn begin_start(&self) -> Option<CancellationToken> {
        {
            let mut health = self.health.lock().expect("health mutex poisoned");
            if !matches!(*health, AdapterHealth::Stopped) {
                return None;
            }
            *health = AdapterHealth::Starting;
        }
        let token = CancellationToken::new();
        *self.cancel.lock().expect("cancel mutex poisoned") = Some(token.clone());
        Some(token)
    }

    /// Store the spawned loop's handle so [`stop`](Self::stop) can await
    /// it. Call exactly once after a `Some(_)` from
    /// [`begin_start`](Self::begin_start).
    pub fn attach(&self, handle: JoinHandle<()>) {
        *self.task.lock().expect("task mutex poisoned") = Some(handle);
    }

    /// Idempotent stop: fire the cancel token, await the loop's join
    /// handle (logging a panic if the loop unwound abnormally), then
    /// reset health to `Stopped`. Safe to call when already stopped (the
    /// token + handle slots are simply empty).
    ///
    /// `label` names the adapter in the panic-path log line (e.g. the
    /// printer / machine / robot id).
    pub async fn stop(&self, label: &str) {
        // Take both slots under their locks, drop the locks, THEN await —
        // holding a std::sync::Mutex across an await point would deadlock
        // / panic on re-entry.
        let cancel_opt = self.cancel.lock().expect("cancel mutex poisoned").take();
        let handle_opt = self.task.lock().expect("task mutex poisoned").take();

        if let Some(token) = cancel_opt {
            token.cancel();
        }
        if let Some(handle) = handle_opt {
            if let Err(e) = handle.await {
                if e.is_panic() {
                    tracing::error!(
                        adapter = %label,
                        "adapter background task panicked during stop: {e}"
                    );
                }
            }
        }

        *self.health.lock().expect("health mutex poisoned") = AdapterHealth::Stopped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_starts_stopped() {
        let lc = AdapterLifecycle::new();
        assert_eq!(lc.health(), AdapterHealth::Stopped);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(AdapterLifecycle::default().health(), AdapterHealth::Stopped);
    }

    #[test]
    fn set_health_round_trips() {
        let lc = AdapterLifecycle::new();
        lc.set_health(AdapterHealth::Healthy);
        assert_eq!(lc.health(), AdapterHealth::Healthy);
        lc.set_health(AdapterHealth::Degraded {
            reason: "slow".to_string(),
        });
        assert!(matches!(lc.health(), AdapterHealth::Degraded { .. }));
    }

    #[test]
    fn begin_start_transitions_stopped_to_starting() {
        let lc = AdapterLifecycle::new();
        let token = lc.begin_start();
        assert!(token.is_some(), "first begin_start yields a token");
        assert_eq!(lc.health(), AdapterHealth::Starting);
    }

    #[test]
    fn begin_start_is_idempotent_while_running() {
        let lc = AdapterLifecycle::new();
        assert!(lc.begin_start().is_some());
        // Second call while not Stopped returns None and does not mint a
        // new token or change health.
        assert!(
            lc.begin_start().is_none(),
            "begin_start while Starting must no-op"
        );
        assert_eq!(lc.health(), AdapterHealth::Starting);

        // Also a no-op once Healthy.
        lc.set_health(AdapterHealth::Healthy);
        assert!(lc.begin_start().is_none());
        assert_eq!(lc.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn returned_token_is_the_stored_token() {
        let lc = AdapterLifecycle::new();
        let token = lc.begin_start().expect("token");
        assert!(!token.is_cancelled());
        // The token stored internally is the same one the caller holds:
        // a clone shares cancellation state.
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[tokio::test]
    async fn stop_resets_to_stopped_when_idle() {
        // stop() on a never-started lifecycle is a clean no-op that lands
        // in Stopped.
        let lc = AdapterLifecycle::new();
        lc.set_health(AdapterHealth::Healthy);
        lc.stop("idle").await;
        assert_eq!(lc.health(), AdapterHealth::Stopped);
    }

    #[tokio::test]
    async fn full_cycle_cancels_and_joins_the_task() {
        let lc = AdapterLifecycle::new();
        let token = lc.begin_start().expect("token");
        assert_eq!(lc.health(), AdapterHealth::Starting);

        // Spawn a loop that runs until the token fires, then mark Healthy
        // to prove the cloned health_slot reaches the same cell.
        let health_slot = lc.health_slot();
        let loop_token = token.clone();
        let handle = tokio::spawn(async move {
            *health_slot.lock().expect("poison") = AdapterHealth::Healthy;
            loop_token.cancelled().await;
        });
        lc.attach(handle);

        // Give the task a moment to publish Healthy.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if matches!(lc.health(), AdapterHealth::Healthy) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(lc.health(), AdapterHealth::Healthy);

        // stop() must fire the token (unblocking the task), join it, and
        // land in Stopped.
        lc.stop("cycle").await;
        assert_eq!(lc.health(), AdapterHealth::Stopped);

        // A second stop() is idempotent.
        lc.stop("cycle").await;
        assert_eq!(lc.health(), AdapterHealth::Stopped);

        // After a full stop, begin_start works again (restartable).
        assert!(lc.begin_start().is_some());
        assert_eq!(lc.health(), AdapterHealth::Starting);
    }

    #[tokio::test]
    async fn stop_logs_but_survives_a_panicking_task() {
        let lc = AdapterLifecycle::new();
        let _token = lc.begin_start().expect("token");
        // A task that panics immediately. stop() must still complete and
        // reset to Stopped (the panic is logged, not propagated).
        let handle = tokio::spawn(async move {
            panic!("loop blew up");
        });
        lc.attach(handle);
        lc.stop("panicker").await;
        assert_eq!(lc.health(), AdapterHealth::Stopped);
    }
}
