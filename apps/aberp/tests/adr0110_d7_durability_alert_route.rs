//! **ADR-0110 D7 — the operator channel: does the WAL fence reach the SPA?**
//!
//! `crates/aberp-db/tests/adr0110_d7_wal_fence.rs` pins that the fence FIRES.
//! This pins the other end of the wire: that a fired fence turns into a
//! `durability_alert` on `GET /health`, which is what the SPA polls every 10 s
//! and turns into the full-width red banner.
//!
//! Ervin's decision (2026-08-12) is that the backend KEEPS SERVING when the
//! fence fires rather than hard-stopping. That is only defensible if the
//! operator cannot miss the signal, so the signal itself is load-bearing and
//! gets a gate of its own. A silent detection is the same class of defect as
//! no detection — arguably worse, because the code looks like it is watching.
//!
//! The banner state lives on the BACKEND deliberately (on the shared
//! `aberp_db::Handle`, surfaced here), not in the browser: it has to survive a
//! reload, and it has to be the same truth for every window.
//!
//! Scope: `$TMPDIR` only. The route is exercised in-process over the real
//! router, the same posture as `portable_demo_boot_e2e.rs` pin 1 — no TLS, no
//! keychain, so it runs in every `cargo test` gate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aberp_audit_ledger::{
    append_in_tx, ensure_schema, Actor, BinaryHash, EventKind, LedgerMeta, TenantId,
};
use aberp_db::{Handle, HandleConfig};
use duckdb::Connection;
use ulid::Ulid;

use aberp::serve::{self, AppState};

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aberp-adr0110-d7-route-{label}-{}", Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// A demo-tenant, NAV-off, Ready `AppState` — the `portable_demo_boot_e2e.rs`
/// pin-1 posture, which is the tree's established way to drive the real route
/// table without TLS or a keychain.
fn demo_state(db_path: PathBuf) -> AppState {
    let tenant = TenantId::new("demo".to_string()).expect("demo tenant id");
    // B1 ships the fence DISARMED, so these tests must arm it explicitly or
    // they would pass vacuously (no fence, no alert, no banner to check). The
    // boot re-derivation below is deliberately NOT gated on the flag — a
    // historical loss recorded while the fence was armed must still resurface.
    let db = Handle::open(
        &db_path,
        tenant.clone(),
        HandleConfig {
            wal_fence_enabled: true,
            ..Default::default()
        },
    )
    .expect("open shared Handle (fence ARMED)");
    // No explicit `restore_durability_alert_from_mirror` here: `Handle::open`
    // does it as part of construction, which is exactly the property
    // `the_production_handle_constructor_re_derives_the_alert` pins.
    AppState {
        db,
        db_path: Arc::new(db_path),
        tenant,
        nav_enabled: false,
        binary_hash: aberp::binary_hash::BinaryHashHandle::from_ready(BinaryHash::from_bytes(
            [0u8; 32],
        )),
        session_token: Arc::new("test-token".to_string()),
        secrets_cache: aberp::secrets_cache::SecretsCache::empty(),
        nav_poll_semaphore: Arc::new(tokio::sync::Semaphore::new(
            serve::NAV_POLL_DAEMON_CONCURRENCY,
        )),
        boot_state: Arc::new(std::sync::RwLock::new(serve::ServeBootState::Ready {
            operator_login: serve::NAV_DISABLED_LOGIN.to_string(),
        })),
        shutdown_token: tokio_util::sync::CancellationToken::new(),
        adapter_registry: Arc::new(std::sync::RwLock::new(aberp_mes::AdapterRegistry::new())),
        adapter_manager: Arc::new(aberp::mes_manager::AdapterManager::new(
            Arc::new(std::sync::RwLock::new(aberp_mes::AdapterRegistry::new())),
            tokio_util::sync::CancellationToken::new(),
        )),
        adapter_health_baseline: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        restore_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        catalogue_push: aberp::catalogue_push::CataloguePushHandle::dormant(),
        email_relay_rate_limiter: Arc::new(aberp::email_relay::RateLimiter::new()),
        pipeline_python_resolution: aberp::quote_pricing_pipeline::PythonResolutionHandle::dormant(
        ),
        storefront_credential: aberp::storefront_credential::StorefrontCredentialHandle::dormant(),
        email_outbox_daemon: aberp::email_outbox_poll_daemon::EmailOutboxDaemonHandle::dormant(),
        quote_pdf_rerender_queue: aberp::quote_pdf_rerender_queue::QuotePdfRerenderQueue::new(),
        digital_id: Arc::new(aberp_digital_id::MockProvider::new()),
    }
}

fn commit_one(h: &Handle, label: &str) {
    let tenant = TenantId::new("demo".to_string()).expect("demo tenant id");
    let meta = LedgerMeta::new(tenant, BinaryHash::from_bytes([7u8; 32]));
    let mut guard = h.write().expect("shared writer");
    let tx = guard.conn().transaction().expect("begin");
    append_in_tx(
        &tx,
        &meta,
        EventKind::Test,
        format!("{{\"probe\":\"{label}\"}}").into_bytes(),
        Actor::from_local_cli(format!("ulid-{label}"), "tester"),
        None,
    )
    .expect("append");
    tx.commit().expect("commit");
    drop(guard);
}

/// The GROUP-A defect primitive: a foreign DuckDB instance with DEFAULT
/// pragmas, whose close folds and truncates the live Handle's WAL.
fn foreign_open_and_close(db: &Path) {
    let c = Connection::open(db).expect("foreign open");
    c.execute_batch("SELECT 1;").expect("foreign read");
}

async fn health_json(state: AppState) -> (reqwest::StatusCode, serde_json::Value) {
    let app = serve::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral loopback port");
    let addr = listener.local_addr().expect("resolve bound addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .expect("GET /health");
    let status = resp.status();
    let body = resp.json().await.expect("/health returns JSON");
    server.abort();
    (status, body)
}

/// A healthy tenant reports `durability_alert: null` — present in the payload
/// and explicitly null, not omitted.
///
/// The distinction matters: an omitted key makes "the fence is watching and
/// quiet" indistinguishable from "this backend predates the fence", and a
/// durability signal that cannot tell those apart is not a signal.
#[tokio::test]
async fn a_healthy_tenant_reports_an_explicit_null_alert() {
    let dir = test_dir("healthy");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }
    let state = demo_state(db_path);
    commit_one(&state.db, "healthy-row");
    state.db.durable_ack().expect("healthy ack");

    let (status, body) = health_json(state).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        body.get("durability_alert").is_some(),
        "/health must always CARRY the key, so a fence-less backend is \
         distinguishable from a quiet one; got {body}"
    );
    assert!(
        body["durability_alert"].is_null(),
        "a healthy tenant must report a null durability_alert; got {}",
        body["durability_alert"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The gate.** Fire the fence with the real prod shape and require the alert
/// on the wire, with everything the banner needs to render.
///
/// Mutation-verified: drop the `durability_alert:` line from `handle_health`
/// and this goes RED while every other route test stays green — the fence
/// would still fire, still log, still audit, and the operator would still see
/// nothing.
#[tokio::test]
async fn a_fired_fence_surfaces_on_the_health_route() {
    let dir = test_dir("fired");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }
    let state = demo_state(db_path.clone());

    // (1) A committed, WAL-resident write, acked clean.
    commit_one(&state.db, "before");
    state.db.durable_ack().expect("precondition: healthy ack");

    // (2) THE DEFECT — a foreign opener folds and truncates the live WAL.
    foreign_open_and_close(&db_path);

    // (3) Another commit, then the ack that must refuse.
    commit_one(&state.db, "after");
    let err = state
        .db
        .durable_ack()
        .expect_err("precondition: the fence must fire on the GROUP-A shape");
    assert!(
        matches!(err, aberp_db::DbError::WalTruncatedUnderWriter { .. }),
        "precondition: expected the D7 fence error, got {err:?}"
    );

    // (4) KEEP SERVING. The route still answers 200 with `ok: true` — the
    //     backend is not bricked, which is the whole design (Ervin,
    //     2026-08-12). The alert is what carries the bad news.
    let (status, body) = health_json(state).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "KEEP-SERVING REGRESSION: a fired fence must not take the backend down; \
         /health must still answer 200"
    );
    assert_eq!(
        body["ok"],
        serde_json::json!(true),
        "the backend is still serving; `ok` reports the SERVICE, and the durability \
         alert reports the DURABILITY. Conflating them would make the banner \
         unreachable exactly when it matters."
    );

    // (5) THE SIGNAL.
    let alert = &body["durability_alert"];
    assert!(
        !alert.is_null(),
        "ADR-0110 D7 REGRESSION: the WAL fence fired and /health reported no \
         durability_alert. The operator's only channel is silent, and the \
         keep-serving decision rests entirely on that channel working. Body: {body}"
    );
    let message = alert["message"]
        .as_str()
        .expect("the alert must carry an operator-facing message string");
    assert!(
        message.contains("Durability loss detected"),
        "the message is rendered VERBATIM in the banner and must read as an alarm \
         an operator can act on; got {message:?}"
    );
    let breach = alert["breach"]
        .as_str()
        .expect("the alert must carry a machine breach code");
    assert!(
        matches!(
            breach,
            "wal_vanished" | "wal_shrank" | "wal_replaced" | "main_db_file_replaced"
        ),
        "breach must be one of the four fixed codes; got {breach:?}"
    );
    let detected_at = alert["detected_at"]
        .as_str()
        .expect("the alert must carry a detection timestamp");
    assert!(
        detected_at.contains('T') && detected_at.len() >= 20,
        "detected_at must be RFC3339 so the banner can say SINCE WHEN; got {detected_at:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The alert is STICKY across polls and across a later healthy ack.
///
/// The SPA re-probes every 10 s and re-mounts on every reload, so a banner that
/// depended on catching one response would blink out on the next poll. And a
/// recovery must not silently retract it: the writes that went missing do not
/// come back because the tenant is behaving again.
#[tokio::test]
async fn the_alert_survives_later_healthy_acks_and_repeated_polls() {
    let dir = test_dir("sticky");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }
    let state = demo_state(db_path.clone());

    commit_one(&state.db, "before");
    state.db.durable_ack().expect("healthy ack");
    foreign_open_and_close(&db_path);
    commit_one(&state.db, "after");
    let _ = state.db.durable_ack().expect_err("the fence fires");

    // The tenant behaves again — several clean acks.
    for i in 0..3 {
        commit_one(&state.db, &format!("recovered-{i}"));
        state
            .db
            .durable_ack()
            .expect("KEEP-SERVING: acks after a detected breach must succeed again");
    }

    // Three separate polls, as the SPA would make them (a fresh router each
    // time also stands in for a reload — the state is on the backend).
    for poll in 0..3 {
        let (status, body) = health_json(state.clone()).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert!(
            !body["durability_alert"].is_null(),
            "the durability alert must be STICKY: it vanished on poll {poll}, so the \
             operator's banner would blink out on the next 10 s tick (or on a reload) \
             even though writes were lost. Only an explicit clear takes it down."
        );
    }

    // ...and an explicit clear is what takes it down.
    state.db.clear_durability_alert();
    let (_, body) = health_json(state).await;
    assert!(
        body["durability_alert"].is_null(),
        "clear_durability_alert must be the one thing that retracts the banner"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── B2 — keep-serving must not degrade to keep-serving-and-FORGET ───────────

/// **The restart the banner asks for must not be the mute button.**
///
/// The banner says *stop and recover*. Before B2, doing exactly that cleared
/// it: the alert lived only in process memory, and the
/// `db.durability_loss_detected` row had been written into the very database
/// whose WAL was just truncated — so the DB copy is the copy most likely to be
/// gone. The operator restarted, saw a clean screen, and carried on invoicing.
///
/// This drives the real sequence: fire the fence, then DROP the Handle and
/// build a fresh one on the same tenant directory (a process restart, minus
/// the process), and require the alert back.
///
/// Mutation-verified: delete the `handle.restore_durability_alert_from_mirror()`
/// call from `serve::open_tenant_handle` and this goes RED while every other
/// test in this file stays green.
#[tokio::test]
async fn a_restart_preserves_an_unacknowledged_durability_alert() {
    let dir = test_dir("restart");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    // ── Boot 1: the loss happens and is detected ──────────────────────────
    {
        let state = demo_state(db_path.clone());
        commit_one(&state.db, "before");
        state.db.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db_path);
        commit_one(&state.db, "after");
        let _ = state
            .db
            .durable_ack()
            .expect_err("precondition: the fence must fire");
        assert!(
            state.db.durability_alert().is_some(),
            "precondition: boot 1 must be showing the banner"
        );
        // The Handle drops here — the process is gone.
    }

    // ── Boot 2: a brand-new Handle on the same tenant ─────────────────────
    let state = demo_state(db_path.clone());
    let (status, body) = health_json(state.clone()).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        !body["durability_alert"].is_null(),
        "ADR-0110 D7 / B2 REGRESSION: the durability alert did NOT survive a restart. The \
         banner tells the operator to stop and recover — if the restart it asks for is also \
         what silences it, keep-serving has degraded to keep-serving-and-FORGET, and the \
         operator resumes invoicing on a tenant that may not be persisting. The alert must be \
         re-derived at boot from the fsync'd audit mirror, which is the one copy a WAL \
         truncation cannot take. Body: {body}"
    );
    let message = body["durability_alert"]["message"]
        .as_str()
        .expect("the re-derived alert carries a message");
    assert!(
        message.contains("Durability loss detected"),
        "the re-derived alert must still read as an alarm; got {message:?}"
    );
    assert!(
        message.contains("survived a restart"),
        "the re-derived alert should say so — an operator who just restarted needs to know \
         this is the SAME loss, not a new one; got {message:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The operator acknowledges: the banner comes down, the acknowledgement is
/// recorded, and it is DURABLE — a further restart does not bring the banner
/// back.
///
/// That last clause is what makes the button worth pressing. An acknowledgement
/// that evaporated on restart would leave the operator with an alarm they can
/// never actually clear, and an alarm that cannot be cleared is one people
/// learn to route around.
///
/// Mutation-verified: make `acknowledge_durability_alert` clear the flag
/// WITHOUT appending the audit row and the final restart assertion goes RED —
/// the next boot re-derives the unacknowledged loss and the banner returns.
#[tokio::test]
async fn acknowledging_clears_the_banner_records_it_and_survives_a_restart() {
    let dir = test_dir("ack");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    let state = demo_state(db_path.clone());
    commit_one(&state.db, "before");
    state.db.durable_ack().expect("healthy ack");
    foreign_open_and_close(&db_path);
    commit_one(&state.db, "after");
    let _ = state.db.durable_ack().expect_err("the fence fires");
    assert!(
        state.db.durability_alert().is_some(),
        "precondition: banner up"
    );

    // ── Acknowledge over the REAL route ───────────────────────────────────
    let app = serve::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/health/acknowledge-durability-alert"))
        .bearer_auth(state.session_token.as_str())
        .send()
        .await
        .expect("POST the acknowledgement");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the acknowledge route must answer 200 on a Ready tenant"
    );
    let ack: serde_json::Value = resp.json().await.expect("JSON");
    assert!(
        ack["acknowledged_at"]
            .as_str()
            .is_some_and(|s| s.contains('T')),
        "the acknowledgement must report WHEN it happened (RFC3339); got {ack}"
    );
    server.abort();

    // The banner is down...
    let (_, body) = health_json(state.clone()).await;
    assert!(
        body["durability_alert"].is_null(),
        "acknowledging must take the banner down; got {}",
        body["durability_alert"]
    );

    // ...and it is RECORDED, in the mirror, where a truncation cannot reach it.
    let mirror = aberp_audit_ledger::mirror_path_for(&db_path);
    let entries = aberp_audit_ledger::read_mirror_entries(&mirror).expect("read mirror");
    assert!(
        entries
            .iter()
            .any(|e| e.kind == "db.durability_alert_acknowledged"),
        "B2 REGRESSION: the banner came down with NO durable record of who cleared it or when. \
         Clearing a durability alert must be an attributable, hash-chained act — an unrecorded \
         clear is indistinguishable from amnesia. Kinds present: {:?}",
        entries.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(
        entries
            .iter()
            .any(|e| e.kind == "db.durability_loss_detected"),
        "the LOSS itself must remain permanently recorded — acknowledging clears the banner, \
         not the history"
    );

    // ...and the acknowledgement is DURABLE: restart, banner stays down.
    drop(state);
    let restarted = demo_state(db_path.clone());
    let (_, body) = health_json(restarted).await;
    assert!(
        body["durability_alert"].is_null(),
        "B2 REGRESSION: the banner came BACK after an acknowledged loss was restarted. The \
         acknowledgement must out-rank the loss in the mirror, otherwise the operator has an \
         alarm they can never clear — and an uncleavable alarm is one people learn to ignore. \
         Body: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **`serve::open_tenant_handle` — the production constructor — must be the
/// thing that re-derives.**
///
/// [`a_restart_preserves_an_unacknowledged_durability_alert`] proves the
/// MECHANISM works, but it builds its Handle directly (it has to: B1 ships the
/// fence disarmed and that test needs it armed), so it calls
/// `restore_durability_alert_from_mirror` itself and cannot see whether serve
/// ever does. Mutation-verified and caught exactly that: deleting the call from
/// `open_tenant_handle` left it green. This closes the gap by going through the
/// real constructor.
///
/// It also pins a second property worth stating out loud: **re-derivation is
/// NOT gated on `wal_fence_enabled`.** A loss recorded while the fence was
/// armed must still resurface on a boot where it is disarmed — otherwise
/// turning the flag off would erase an outstanding alarm, which is a
/// remarkably bad thing for a safety flag to do.
#[tokio::test]
async fn the_production_handle_constructor_re_derives_the_alert() {
    let dir = test_dir("wiring");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    // Produce a real, mirrored `db.durability_loss_detected` row by firing the
    // fence on an ARMED Handle, then let that Handle go.
    {
        let armed = Handle::open(
            &db_path,
            TenantId::new("demo".to_string()).expect("tenant"),
            HandleConfig {
                wal_fence_enabled: true,
                ..Default::default()
            },
        )
        .expect("open armed Handle");
        commit_one(&armed, "before");
        armed.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db_path);
        commit_one(&armed, "after");
        let _ = armed
            .durable_ack()
            .expect_err("precondition: the fence fires");
    }

    // The production path, verbatim — `HandleConfig::default()`, fence DISARMED.
    let handle =
        serve::open_tenant_handle(&db_path, TenantId::new("demo".to_string()).expect("tenant"))
            .expect("open the tenant Handle the way serve boot does");

    assert!(
        handle.durability_alert().is_some(),
        "ADR-0110 D7 / B2 REGRESSION: `serve::open_tenant_handle` did not re-derive the \
         outstanding durability alert from the audit mirror. Every serve boot path reaches the \
         Handle through this function, so if the call is missing here the alert dies with the \
         process no matter how well the mechanism itself works — and the restart the banner \
         asks for silently clears it."
    );

    let _ = std::fs::remove_dir_all(&dir);
}
