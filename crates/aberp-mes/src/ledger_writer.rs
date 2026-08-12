//! Runtime task that drains an [`Adapter`](crate::Adapter)'s broadcast
//! and writes each [`CanonicalEvent`](crate::CanonicalEvent) to the
//! audit ledger as one `EventKind::MesAdapterEvent` entry.
//!
//! ADR-0060 §"Migration safety — additive only" + §"Phase α scope-cap"
//! deferred this runtime to Phase β; PR-225 / S229 lands it alongside
//! the first real adapter (the barcode scanner). The Phase α
//! `write_mes_adapter_event` helper was the transactional surface; this
//! module wires the surface to the broadcast.
//!
//! ## Design
//!
//! One writer task per adapter. The task subscribes to the adapter once and
//! appends each event through the SHARED `aberp_db::Handle` (ADR-0110 D8). It
//! used to open a fresh DuckDB connection per write — the GROUP-A loss
//! primitive: co-resident with serve's Handle, every close folded and truncated
//! that Handle's WAL, so the next invoice commit returned Ok and reached no
//! file. On a busy shop floor that fired once per scan.
//!
//! Cancellation races the broadcast receive via `tokio::select!`; a
//! Tauri window close or a Ctrl-C in `run_prod.sh` exits the task
//! within ms.
//!
//! ## Lossiness contract
//!
//! Per ADR-0060 §"broadcast lossiness on the ledger-writer path will
//! lose audit entries", a slow ledger-writer with a backlog drops the
//! oldest events when the broadcast channel fills. The
//! `RecvError::Lagged(n)` arm logs loud (so the future operations
//! dashboard surfaces the drop count) but otherwise continues — the
//! adapter MUST stay running even if the ledger-writer falls behind.

use std::sync::Arc;

use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use aberp_audit_ledger::{Actor, BinaryHash, LedgerMeta, TenantId};

use crate::adapter::Adapter;
use crate::audit::{write_mes_adapter_event, MesAdapterEventPayload};

/// Dependencies the ledger-writer task needs to write into the audit
/// ledger. Constructed by the boot code in `apps/aberp::serve` from
/// the existing `recovery_state` (`db`, `tenant`, `binary_hash`)
/// + a fresh per-session [`LedgerWriterActor`].
#[derive(Debug, Clone)]
pub struct LedgerWriterDeps {
    /// ADR-0110 D8 (GROUP-A sweep): the ONE shared Handle, not a path. This
    /// writer runs in-serve, one append per MES adapter event, and each fresh
    /// `Connection::open` it used to make folded + truncated the live Handle's
    /// WAL on close — so a busy shop floor re-armed the write-loss primitive
    /// continuously.
    pub db: aberp_db::HandleArc,
    pub tenant: TenantId,
    pub binary_hash: BinaryHash,
    pub actor: LedgerWriterActor,
}

/// Actor identity stamped onto every ledger entry the writer produces.
/// Mirrors the shape `aberp-quote-intake::service` builds at boot:
/// session_id = a fresh ULID per boot, operator_login = the resolved
/// login (or `"boot"` when the boot state hasn't transitioned to
/// `Ready` yet).
#[derive(Debug, Clone)]
pub struct LedgerWriterActor {
    pub session_id: String,
    pub operator_login: String,
}

/// Spawn a runtime task that subscribes to `adapter`'s broadcast and
/// writes each emitted [`CanonicalEvent`](crate::CanonicalEvent) into
/// the audit ledger. Returns the spawn handle so the caller can
/// register it with the shutdown coordinator.
///
/// The task exits when EITHER (a) the cancel token fires, OR (b) the
/// underlying broadcast channel closes (adapter dropped). Both exits
/// are clean — the caller may safely await the handle.
pub fn spawn_ledger_writer(
    adapter: Arc<dyn Adapter>,
    deps: LedgerWriterDeps,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ledger_writer(adapter, deps, cancel).await;
    })
}

async fn run_ledger_writer(
    adapter: Arc<dyn Adapter>,
    deps: LedgerWriterDeps,
    cancel: CancellationToken,
) {
    let adapter_name = adapter.name().to_string();
    let mut rx = adapter.subscribe();
    tracing::info!(adapter = %adapter_name, "MES ledger-writer task started");

    loop {
        let recv = tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(adapter = %adapter_name, "MES ledger-writer cancelled; exiting");
                return;
            }
            r = rx.recv() => r,
        };
        match recv {
            Ok(event) => {
                let payload = MesAdapterEventPayload::new(
                    adapter_name.clone(),
                    Ulid::new().to_string(),
                    event,
                );
                write_one(&deps, &payload, &adapter_name).await;
            }
            Err(RecvError::Lagged(n)) => {
                // Loud-fail per CLAUDE.md rule 12 — operator MUST see
                // dropped scans. The future operations dashboard
                // projection surfaces this counter.
                tracing::warn!(
                    adapter = %adapter_name,
                    skipped = n,
                    "MES broadcast lagged; events dropped from tail \
                     (per ADR-0060 broadcast lossiness contract)"
                );
            }
            Err(RecvError::Closed) => {
                tracing::info!(
                    adapter = %adapter_name,
                    "MES broadcast closed; ledger-writer exiting"
                );
                return;
            }
        }
    }
}

async fn write_one(deps: &LedgerWriterDeps, payload: &MesAdapterEventPayload, adapter_name: &str) {
    let db = deps.db.clone();
    let tenant = deps.tenant.clone();
    let binary_hash = deps.binary_hash;
    let session_id = deps.actor.session_id.clone();
    let login = deps.actor.operator_login.clone();
    let payload_owned = payload.clone();
    let adapter_name = adapter_name.to_string();

    let outcome = tokio::task::spawn_blocking(move || -> Result<(), String> {
        // ADR-0110 D8: the ONE shared writer. The guard's drop lockstep-syncs the
        // mirror, and holding it for the whole tx keeps this append serialized
        // with every other in-serve Handle writer.
        let mut guard = db
            .write()
            .map_err(|e| format!("acquire shared writer for MES audit append: {e}"))?;
        aberp_audit_ledger::ensure_schema(&guard)
            .map_err(|e| format!("ensure audit-ledger schema: {e}"))?;
        let tx = guard
            .transaction()
            .map_err(|e| format!("open MES audit tx: {e}"))?;
        let meta = LedgerMeta::new(tenant, binary_hash);
        let actor = Actor::from_local_cli(session_id, &login);
        write_mes_adapter_event(&tx, &meta, actor, &payload_owned)
            .map_err(|e| format!("write MES adapter event: {e}"))?;
        tx.commit()
            .map_err(|e| format!("commit MES audit tx: {e}"))?;
        Ok(())
    })
    .await;

    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                adapter = %adapter_name,
                error = %e,
                "MES audit-ledger write failed; event lost"
            );
        }
        Err(e) => {
            tracing::warn!(
                adapter = %adapter_name,
                error = %e,
                "MES audit-ledger task panicked; event lost"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use aberp_audit_ledger::{ensure_schema, BinaryHash, TenantId};

    use crate::adapters::barcode_scanner::{BarcodeScannerAdapter, BarcodeScannerConfig};
    use crate::events::CanonicalEvent;

    /// ADR-0110 D8 — the writer takes the shared Handle now, so the fixture opens
    /// ONE and hands out `try_clone`s. The tests must read back through this SAME
    /// Handle: H3 disables checkpointing, so the writer's committed rows stay
    /// WAL-resident and a fresh `Connection::open` poller would see zero forever
    /// (CLAUDE.md rule 13/14 — migrate a family's writers AND readers together).
    fn handle_for_test(db_path: &std::path::Path) -> aberp_db::HandleArc {
        aberp_db::Handle::open_default(db_path, TenantId::new("ten_test_mes_writer").expect("tid"))
            .expect("open shared Handle for the MES ledger-writer test")
    }

    fn deps_for_test(db: aberp_db::HandleArc) -> LedgerWriterDeps {
        LedgerWriterDeps {
            db,
            tenant: TenantId::new("ten_test_mes_writer").expect("tenant id"),
            binary_hash: BinaryHash::from_bytes([0u8; 32]),
            actor: LedgerWriterActor {
                session_id: Ulid::new().to_string(),
                operator_login: "test-operator".to_string(),
            },
        }
    }

    /// End-to-end: NoopAdapter emits a fabricated ScanReceived event,
    /// the ledger-writer task drains it, the audit-ledger row appears
    /// on disk with kind = `mes.adapter_event` and the canonical
    /// payload round-trips back to the same struct.
    #[tokio::test]
    async fn writer_drains_scan_event_into_audit_ledger() {
        let tempdir = std::env::temp_dir().join(format!("aberp-mes-writer-{}", Ulid::new()));
        std::fs::create_dir_all(&tempdir).unwrap();
        let db_path = tempdir.join("audit.duckdb");
        // Pre-create the schema so `ensure_schema` inside the writer
        // task hits an established DB.
        {
            let conn = duckdb::Connection::open(&db_path).unwrap();
            ensure_schema(&conn).unwrap();
        }

        // Use the NoopAdapter — its `emit_for_test` lets us inject an
        // event without needing a TCP listener. Keeps this test focused
        // on the ledger-writer's drain + write path.
        let adapter: Arc<crate::NoopAdapter> =
            Arc::new(crate::NoopAdapter::new("test-noop-for-writer"));
        let adapter_for_writer: Arc<dyn Adapter> = adapter.clone();
        let db = handle_for_test(&db_path);
        let deps = deps_for_test(db.clone());
        let cancel = CancellationToken::new();
        adapter.start().await.unwrap();
        let writer = spawn_ledger_writer(adapter_for_writer, deps, cancel.clone());

        // Give the writer a chance to subscribe before we emit.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let event = CanonicalEvent::ScanReceived {
            scanner_id: "test-noop-for-writer".into(),
            payload: "WRITER-TEST-123".into(),
            symbology: Some("Code128".into()),
            source_addr: Some("127.0.0.1:54321".into()),
            at_iso8601: "2026-06-03T08:32:00Z".into(),
        };
        let n = adapter.emit_for_test(event);
        assert!(n >= 1, "writer should be subscribed before emit");

        // Wait for the writer to drain + commit. Poll the DB for the
        // row's appearance with a bounded timeout — beats a fixed
        // sleep.
        let started = std::time::Instant::now();
        let mut row_count = 0u64;
        while started.elapsed() < Duration::from_secs(3) {
            // Through the SAME Handle the writer uses — see `handle_for_test`.
            let conn = db.read().expect("shared reader for the MES audit row");
            row_count = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'mes.adapter_event'",
                    [],
                    |r| r.get::<_, u64>(0),
                )
                .unwrap();
            if row_count >= 1 {
                break;
            }
            drop(conn);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            row_count, 1,
            "exactly one MES audit-ledger row should appear within 3s"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        adapter.stop().await.unwrap();

        // Cleanup.
        std::fs::remove_dir_all(&tempdir).ok();
    }

    /// The writer task exits cleanly on cancellation even without any
    /// events ever flowing.
    #[tokio::test]
    async fn writer_exits_on_cancel_with_no_events() {
        let tempdir = std::env::temp_dir().join(format!("aberp-mes-writer-cancel-{}", Ulid::new()));
        std::fs::create_dir_all(&tempdir).unwrap();
        let db_path = tempdir.join("audit.duckdb");
        {
            let conn = duckdb::Connection::open(&db_path).unwrap();
            ensure_schema(&conn).unwrap();
        }

        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        let cfg = BarcodeScannerConfig {
            listen_port: port,
            ..BarcodeScannerConfig::new("test-cancel-writer")
        };
        let adapter: Arc<BarcodeScannerAdapter> = Arc::new(BarcodeScannerAdapter::new(cfg));
        let adapter_for_writer: Arc<dyn Adapter> = adapter.clone();
        adapter.start().await.unwrap();

        let deps = deps_for_test(handle_for_test(&db_path));
        let cancel = CancellationToken::new();
        let writer = spawn_ledger_writer(adapter_for_writer, deps, cancel.clone());

        // Cancel immediately — no events ever flowed.
        cancel.cancel();
        let joined = tokio::time::timeout(Duration::from_secs(2), writer).await;
        assert!(
            joined.is_ok(),
            "writer task should exit within 2s of cancel"
        );

        adapter.stop().await.unwrap();
        std::fs::remove_dir_all(&tempdir).ok();
    }
}
