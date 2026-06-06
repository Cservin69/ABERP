//! S266 / PR-255 — outbound storefront push of the material catalogue
//! (design doc §4 / §14-C).
//!
//! ABERP has **no public inbound surface** (ADR-0057: local Tauri app,
//! loopback HTTPS, no webhook). So the storefront's material dropdown is
//! not *pulled* from ABERP — it is **pushed** out: ABERP `PUT`s the public
//! projection of `quoting_materials` to `{storefront}/api/catalogue/materials`
//! on a cadence and on every operator write. The storefront caches it and
//! serves its `/quote` dropdown from that cache; the customer's browser
//! never reaches ABERP.
//!
//! ## Design choices (flagged in the PR report)
//!
//! - **Location: an app module, not a new crate.** The push reads
//!   [`crate::quoting_materials::list_public`] (which lives in the app) and
//!   needs the [`CataloguePushHandle`] in `AppState` for the on-write
//!   trigger. A crate (the quote-intake shape) would have to re-implement
//!   the table read and could not see `AppState`. When the full
//!   `crates/aberp-quoting` daemon lands (design doc §2, S271+), this
//!   module migrates into it.
//! - **Surface secret reuse (SPOC).** The brief names `ABERP_SITE_ADMIN_TOKEN`,
//!   but no such env var exists — the storefront surface's actual secret is
//!   the quote-intake bearer (`ABERP_QUOTE_INTAKE_TOKEN` / keychain
//!   `quote_intake_token` / `[quote_intake].base_url`). Per `[[aberp-smtp-spoc]]`
//!   ("one secret per surface"), the push REUSES the already-resolved
//!   quote-intake `base_url` + bearer rather than minting a second token for
//!   the same storefront. Consequence: catalogue-push is active iff
//!   quote-intake is configured (same storefront). If Ervin ever wants them
//!   decoupled, that is a follow-up introducing a shared `[storefront]`
//!   config slot — out of scope for one PR (surgical-change discipline).
//! - **Cadence** is a fixed 15 minutes ([`PUSH_CADENCE_SECS`]) per the brief;
//!   not operator-tunable in v1.
//!
//! Failure handling mirrors the S256 quote-intake daemon: exponential
//! backoff (5s → 15s → 60s → cadence) on transient errors; a **401 pauses**
//! the daemon (a rotated bearer) and the `quote.material_catalogue_pushed`
//! audit entry + the in-memory status drive the Settings "re-paste bearer"
//! prompt. Resumption is on the next `aberp serve` boot.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use duckdb::Connection;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;
use zeroize::Zeroizing;

use aberp_audit_ledger::{append_in_tx, Actor, BinaryHash, EventKind, LedgerMeta, TenantId};

/// 15 minutes (design doc §4). Not operator-tunable in v1.
pub const PUSH_CADENCE_SECS: u64 = 900;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const BOOT_DELAY_SECS: u64 = 30;
const CATALOGUE_PATH: &str = "/api/catalogue/materials";

// ── Shared handle (lives in AppState; the on-write trigger + status) ─────

/// The status snapshot the Settings → Material Catalogue page reads to
/// show "last push" and the paused/re-paste-bearer banner.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CataloguePushStatus {
    /// A push daemon is running this process (false = dormant, e.g. no
    /// storefront configured).
    pub running: bool,
    /// A 401 paused the daemon — the operator must re-paste the bearer and
    /// restart. Sticky until next boot.
    pub paused: bool,
    pub last_attempt_at: Option<String>,
    /// `ok` / `unauthorized` / `transport` / `unexpected_status`.
    pub last_outcome: Option<String>,
    pub last_pushed_count: Option<i64>,
    pub last_detail: Option<String>,
}

/// Created once at `AppState` construction (so the SPA always has a status
/// to read, even dormant). When the storefront is configured, the boot
/// block clones this into the daemon `CataloguePushService` and spawns it.
#[derive(Debug)]
pub struct CataloguePushHandle {
    notify: tokio::sync::Notify,
    running: AtomicBool,
    status: Mutex<CataloguePushStatus>,
}

impl CataloguePushHandle {
    /// A dormant handle — no daemon yet. Stored in `AppState`.
    pub fn dormant() -> Arc<Self> {
        Arc::new(Self {
            notify: tokio::sync::Notify::new(),
            running: AtomicBool::new(false),
            status: Mutex::new(CataloguePushStatus::default()),
        })
    }

    /// Wake the daemon for an immediate push (operator saved a row). A
    /// no-op if no daemon is running (dormant / paused).
    pub fn trigger(&self) {
        self.notify.notify_one();
    }

    fn mark_running(&self) {
        self.running.store(true, Ordering::SeqCst);
        if let Ok(mut s) = self.status.lock() {
            s.running = true;
        }
    }

    /// Current status, for the list route.
    pub fn snapshot(&self) -> CataloguePushStatus {
        let mut s = self.status.lock().map(|g| g.clone()).unwrap_or_default();
        s.running = self.running.load(Ordering::SeqCst);
        s
    }

    fn record(&self, attempt_at: String, outcome: &PushOutcome) {
        if let Ok(mut s) = self.status.lock() {
            s.last_attempt_at = Some(attempt_at);
            s.last_outcome = Some(outcome.label().to_string());
            s.last_pushed_count = outcome.pushed_count();
            s.last_detail = outcome.detail();
            if matches!(outcome, PushOutcome::Unauthorized) {
                s.paused = true;
            }
        }
    }
}

// ── Outcome ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Ok { count: i64 },
    Unauthorized,
    Transport(String),
    UnexpectedStatus(u16),
}

impl PushOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            PushOutcome::Ok { .. } => "ok",
            PushOutcome::Unauthorized => "unauthorized",
            PushOutcome::Transport(_) => "transport",
            PushOutcome::UnexpectedStatus(_) => "unexpected_status",
        }
    }
    fn is_ok(&self) -> bool {
        matches!(self, PushOutcome::Ok { .. })
    }
    fn pushed_count(&self) -> Option<i64> {
        match self {
            PushOutcome::Ok { count } => Some(*count),
            _ => None,
        }
    }
    fn detail(&self) -> Option<String> {
        match self {
            PushOutcome::Ok { .. } | PushOutcome::Unauthorized => None,
            PushOutcome::Transport(s) => Some(s.clone()),
            PushOutcome::UnexpectedStatus(c) => Some(format!("HTTP {c}")),
        }
    }
}

// ── The wire body ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct CatalogueBody {
    materials: Vec<crate::quoting_materials::PublicMaterial>,
}

// ── Service / daemon ─────────────────────────────────────────────────────

/// Dependencies for the audit write + table read (mirrors `QuoteIntakeDeps`).
pub struct CataloguePushDeps {
    pub db_path: PathBuf,
    pub tenant: TenantId,
    pub binary_hash: BinaryHash,
    pub operator_login: String,
}

pub struct CataloguePushService {
    handle: Arc<CataloguePushHandle>,
    base_url: String,
    bearer: Zeroizing<String>,
    cadence: Duration,
    client: reqwest::Client,
    deps: CataloguePushDeps,
}

impl CataloguePushService {
    /// `base_url` + `bearer` are the already-resolved quote-intake storefront
    /// credentials (SPOC). `base_url` has no trailing slash (the resolver
    /// strips it).
    pub fn new(
        handle: Arc<CataloguePushHandle>,
        base_url: String,
        bearer: Zeroizing<String>,
        deps: CataloguePushDeps,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("build catalogue-push reqwest client")?;
        Ok(Self {
            handle,
            base_url,
            bearer,
            cadence: Duration::from_secs(PUSH_CADENCE_SECS),
            client,
            deps,
        })
    }

    /// The boot-spawned loop. 30s settle, then push on cadence OR when the
    /// operator triggers a write. Backoff on transient failure; PAUSE on 401.
    pub async fn run_daemon_forever(self, cancel: CancellationToken) {
        self.handle.mark_running();
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(BOOT_DELAY_SECS)) => {}
        }

        let mut backoff_idx: usize = 0;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let outcome = self.push_once("daemon").await;

            if matches!(outcome, PushOutcome::Unauthorized) {
                tracing::error!(
                    "catalogue-push daemon PAUSED: storefront returned 401 \
                     (bearer rotated/invalid). Re-paste the bearer token in \
                     Settings → Quote Intake and restart ABERP to resume."
                );
                return;
            }

            let sleep_dur = if outcome.is_ok() {
                backoff_idx = 0;
                self.cadence
            } else {
                let d = backoff_duration(backoff_idx, self.cadence);
                backoff_idx = backoff_idx.saturating_add(1);
                tracing::warn!(
                    backoff_secs = d.as_secs(),
                    outcome = outcome.label(),
                    "catalogue push failed; backing off"
                );
                d
            };

            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = self.handle.notify.notified() => {
                    // operator write — push immediately
                }
                _ = tokio::time::sleep(sleep_dur) => {}
            }
        }
    }

    /// One push attempt: read the public projection, PUT it, classify,
    /// audit, and record the status. Used by the daemon and (via the
    /// trigger) on operator write.
    pub async fn push_once(&self, trigger: &str) -> PushOutcome {
        let attempt_at = now_rfc3339();

        // Read the public catalogue off the DB (sync duckdb on a blocking
        // thread).
        let db_path = self.deps.db_path.clone();
        let tenant_str = self.deps.tenant.as_str().to_string();
        let rows = match tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).with_context(|| {
                format!("open DuckDB at {} for catalogue push", db_path.display())
            })?;
            crate::quoting_materials::list_public(&conn, &tenant_str)
        })
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                let outcome = PushOutcome::Transport(format!("read catalogue: {e:#}"));
                self.finish(trigger, attempt_at, outcome.clone()).await;
                return outcome;
            }
            Err(join) => {
                let outcome = PushOutcome::Transport(format!("read task panicked: {join}"));
                self.finish(trigger, attempt_at, outcome.clone()).await;
                return outcome;
            }
        };

        let count = rows.len() as i64;
        let body = CatalogueBody { materials: rows };
        let url = format!("{}{CATALOGUE_PATH}", self.base_url);
        let auth = format!("Bearer {}", &*self.bearer);

        let outcome = match self
            .client
            .put(&url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    PushOutcome::Ok { count }
                } else if status.as_u16() == 401 {
                    PushOutcome::Unauthorized
                } else {
                    PushOutcome::UnexpectedStatus(status.as_u16())
                }
            }
            Err(e) => PushOutcome::Transport(scrub(&e.to_string())),
        };

        self.finish(trigger, attempt_at, outcome.clone()).await;
        outcome
    }

    async fn finish(&self, trigger: &str, attempt_at: String, outcome: PushOutcome) {
        self.handle.record(attempt_at.clone(), &outcome);
        self.write_audit(trigger, &outcome).await;
    }

    async fn write_audit(&self, trigger: &str, outcome: &PushOutcome) {
        let db_path = self.deps.db_path.clone();
        let tenant = self.deps.tenant.clone();
        let binary_hash = self.deps.binary_hash;
        let login = self.deps.operator_login.clone();
        let trigger = trigger.to_string();
        let outcome = outcome.clone();

        let res = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = Connection::open(&db_path)
                .context("open DuckDB for MaterialCataloguePushed audit")?;
            aberp_audit_ledger::ensure_schema(&conn).context("ensure audit schema")?;
            let payload = serde_json::json!({
                "trigger": trigger,
                "outcome": outcome.label(),
                "pushed_count": outcome.pushed_count(),
                "detail": outcome.detail(),
                "idempotency_key": Ulid::new().to_string(),
            });
            let bytes = serde_json::to_vec(&payload).context("serialize push payload")?;
            let tx = conn.transaction().context("begin push audit tx")?;
            let meta = LedgerMeta::new(tenant, binary_hash);
            let actor = Actor::from_local_cli(Ulid::new().to_string(), &login);
            append_in_tx(
                &tx,
                &meta,
                EventKind::MaterialCataloguePushed,
                bytes,
                actor,
                None,
            )
            .context("append MaterialCataloguePushed")?;
            tx.commit().context("commit push audit")?;
            Ok(())
        })
        .await;

        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = ?e, "catalogue-push audit write failed"),
            Err(join) => tracing::error!(%join, "catalogue-push audit task panicked"),
        }
    }
}

fn backoff_duration(idx: usize, cadence: Duration) -> Duration {
    match idx {
        0 => Duration::from_secs(5),
        1 => Duration::from_secs(15),
        2 => Duration::from_secs(60),
        _ => cadence,
    }
}

/// Strip any bearer token that might appear in a reqwest error string.
fn scrub(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(pos) = out.find("Bearer ") {
        out.replace_range(pos.., "Bearer <redacted>");
    }
    out
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule_matches_quote_intake() {
        let cad = Duration::from_secs(PUSH_CADENCE_SECS);
        assert_eq!(backoff_duration(0, cad), Duration::from_secs(5));
        assert_eq!(backoff_duration(1, cad), Duration::from_secs(15));
        assert_eq!(backoff_duration(2, cad), Duration::from_secs(60));
        assert_eq!(backoff_duration(3, cad), cad);
        assert_eq!(backoff_duration(99, cad), cad);
    }

    #[test]
    fn outcome_labels_and_pause_flag() {
        assert_eq!(PushOutcome::Ok { count: 3 }.label(), "ok");
        assert_eq!(PushOutcome::Ok { count: 3 }.pushed_count(), Some(3));
        assert_eq!(PushOutcome::Unauthorized.label(), "unauthorized");
        assert_eq!(PushOutcome::Transport("dns".into()).label(), "transport");
        assert_eq!(
            PushOutcome::UnexpectedStatus(503).label(),
            "unexpected_status"
        );
        assert_eq!(
            PushOutcome::UnexpectedStatus(503).detail(),
            Some("HTTP 503".to_string())
        );
    }

    #[test]
    fn handle_records_outcome_and_sets_paused_on_401() {
        let h = CataloguePushHandle::dormant();
        assert!(!h.snapshot().running);
        h.mark_running();
        assert!(h.snapshot().running);

        h.record(
            "2026-06-06T00:00:00Z".to_string(),
            &PushOutcome::Ok { count: 5 },
        );
        let s = h.snapshot();
        assert_eq!(s.last_outcome.as_deref(), Some("ok"));
        assert_eq!(s.last_pushed_count, Some(5));
        assert!(!s.paused);

        h.record(
            "2026-06-06T00:15:00Z".to_string(),
            &PushOutcome::Unauthorized,
        );
        assert!(h.snapshot().paused, "401 must set the sticky paused flag");
    }

    #[test]
    fn scrub_redacts_bearer() {
        let s = scrub("error sending request with Bearer abc.def.ghi");
        assert!(s.contains("Bearer <redacted>"));
        assert!(!s.contains("abc.def.ghi"));
    }
}
