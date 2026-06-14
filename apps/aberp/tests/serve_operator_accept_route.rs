//! S354 / PR-42 (U16) — integration tests for operator accept-on-behalf
//! (`POST /api/quote-pricing-jobs/:quote_id/accept`).
//!
//! Tests hit the `pub` library helpers (`accept_quote_precheck`,
//! `record_operator_accept_audit`, `post_operator_accept`) directly — the
//! WORKING serve-route posture per A159 (`serve_pricing_job_material_route.rs`).
//! The HTTP status mapping (200 / 400 / 404 / 409 / 502) is structural in
//! the handler and the 401 Bearer gate is the shared `check_bearer_rejection`
//! (pinned by the serve.rs unit tests every sibling route relies on).
//! Covered here:
//!
//! 1. **precheck — Posted row** → `Ready` (happy path is acceptable).
//! 2. **precheck — Fetched row** → `NotAcceptable{state:"fetched"}` (only
//!    a Posted/delivered quote can be accepted).
//! 3. **precheck — wrong tenant** → `NotFound` (404-not-403 convention).
//! 4. **precheck — already accepted** → a prior `success` audit blocks
//!    (409); a prior FAILED-writeback audit does NOT block (retry allowed).
//! 5. **audit — success payload** round-trips with `outcome:"success"`,
//!    `retry_available:false`, channel / note / operator / ts intact (F12).
//! 6. **audit — failure payload** captures the classified reason
//!    (`outcome`, `retry_available:true`, `writeback_http_status`,
//!    `writeback_body_excerpt`).
//! 7. **e2e POST** against a hand-rolled TCP mock: HTML 200 →
//!    `RoutingMisconfigured` (audit-worthy), 422 JSON → `AppRejected`,
//!    200 JSON `{status}` → `Success`; the request carries the Bearer +
//!    the `operator_accepted` signed body.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aberp_audit_ledger::{BinaryHash, EventKind, Ledger, TenantId};
use ulid::Ulid;

use aberp::quote_pricing_jobs::{self, JobState};
use aberp::quote_pricing_pipeline::WritebackOutcome;
use aberp::serve::{self, AcceptPrecheck, AppState};

const TEST_TENANT: &str = "serve_operator_accept_test";

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-serve-operator-accept")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn build_state(db_path: PathBuf) -> AppState {
    let tenant = TenantId::new(TEST_TENANT.to_string()).expect("tenant id");
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    AppState {
        db_path: Arc::new(db_path),
        tenant,
        binary_hash: aberp::binary_hash::BinaryHashHandle::from_ready(binary_hash),
        session_token: Arc::new("test-token".to_string()),
        secrets_cache: aberp::secrets_cache::SecretsCache::empty(),
        nav_poll_semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
            aberp::serve::NAV_POLL_DAEMON_CONCURRENCY,
        )),
        boot_state: Arc::new(std::sync::RwLock::new(
            aberp::serve::ServeBootState::Ready {
                operator_login: "test-operator".to_string(),
            },
        )),
        shutdown_token: tokio_util::sync::CancellationToken::new(),
        adapter_registry: Arc::new(std::sync::RwLock::new(aberp_mes::AdapterRegistry::new())),
        adapter_manager: Arc::new(aberp::mes_manager::AdapterManager::new(
            Arc::new(std::sync::RwLock::new(aberp_mes::AdapterRegistry::new())),
            tokio_util::sync::CancellationToken::new(),
        )),
        adapter_health_baseline: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        restore_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        catalogue_push: aberp::catalogue_push::CataloguePushHandle::dormant(),
        email_relay_rate_limiter: std::sync::Arc::new(aberp::email_relay::RateLimiter::new()),
        pipeline_python_resolution: aberp::quote_pricing_pipeline::PythonResolutionHandle::dormant(
        ),
        storefront_credential: aberp::storefront_credential::StorefrontCredentialHandle::dormant(),
        email_outbox_daemon: aberp::email_outbox_poll_daemon::EmailOutboxDaemonHandle::dormant(),
        quote_pdf_rerender_queue: aberp::quote_pdf_rerender_queue::QuotePdfRerenderQueue::new(),
        digital_id: std::sync::Arc::new(aberp_digital_id::MockProvider::new()),
    }
}

fn fixed_ts() -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap()
}

/// Insert a fresh Fetched row.
fn seed_fetched_row(db_path: &PathBuf, tenant: &str, quote_id: &str) {
    let conn = duckdb::Connection::open(db_path).expect("open db");
    quote_pricing_jobs::insert_fetched_job(
        &conn,
        quote_id,
        tenant,
        "cust@example.com",
        "Customer Kft.",
        "Acme Manufacturing Kft.",
        "6061-T6",
        4,
        "bracket.step",
        "/tmp/bracket.step",
        fixed_ts(),
    )
    .expect("insert job");
}

/// Drive a fresh row all the way to terminal `Posted` (priced + delivered).
fn seed_posted_row(db_path: &PathBuf, tenant: &str, quote_id: &str) {
    seed_fetched_row(db_path, tenant, quote_id);
    let mut conn = duckdb::Connection::open(db_path).expect("open db");
    quote_pricing_jobs::set_state(&conn, quote_id, tenant, JobState::Extracting, fixed_ts())
        .expect("ex");
    quote_pricing_jobs::set_extracted(&mut conn, quote_id, tenant, "blake3:x", "{}", fixed_ts())
        .expect("extract");
    quote_pricing_jobs::set_priced(&mut conn, quote_id, tenant, "{}", 10.0, fixed_ts())
        .expect("price");
    quote_pricing_jobs::set_rendered(
        &mut conn,
        quote_id,
        tenant,
        "/tmp/x.pdf",
        "2026-07-06",
        fixed_ts(),
    )
    .expect("render");
    quote_pricing_jobs::set_state(&conn, quote_id, tenant, JobState::Posted, fixed_ts())
        .expect("post");
}

fn accept_entries(db_path: &PathBuf) -> Vec<aberp_audit_ledger::Entry> {
    let ledger = Ledger::open(
        db_path,
        TenantId::new(TEST_TENANT.to_string()).unwrap(),
        BinaryHash::from_bytes([0u8; 32]),
    )
    .expect("open ledger");
    ledger
        .entries()
        .expect("read entries")
        .into_iter()
        .filter(|e| e.kind == EventKind::QuotePricingOperatorAccepted)
        .collect()
}

const QID: &str = "q-accept-00-0000-0000-000000000000";

// ── precheck ──────────────────────────────────────────────────────────

#[test]
fn precheck_ready_on_posted_row() {
    let dir = test_dir("ready");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    seed_posted_row(&db, TEST_TENANT, QID);

    let verdict = serve::accept_quote_precheck(&state, QID, TEST_TENANT).expect("precheck");
    assert!(
        matches!(verdict, AcceptPrecheck::Ready),
        "a Posted row is acceptable, got {verdict:?}"
    );
}

#[test]
fn precheck_not_acceptable_on_fetched_row() {
    let dir = test_dir("notacceptable");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    seed_fetched_row(&db, TEST_TENANT, QID);

    let verdict = serve::accept_quote_precheck(&state, QID, TEST_TENANT).expect("precheck");
    match verdict {
        AcceptPrecheck::NotAcceptable { state: s } => assert_eq!(s, "fetched"),
        other => panic!("expected NotAcceptable, got {other:?}"),
    }
}

#[test]
fn precheck_wrong_tenant_is_not_found() {
    let dir = test_dir("wrongtenant");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    // Posted row planted under a DIFFERENT tenant — invisible to the
    // operator's tenant → NotFound.
    seed_posted_row(&db, "some-other-tenant", QID);

    let verdict = serve::accept_quote_precheck(&state, QID, TEST_TENANT).expect("precheck");
    assert!(
        matches!(verdict, AcceptPrecheck::NotFound),
        "a foreign-tenant row is invisible, got {verdict:?}"
    );
}

#[test]
fn precheck_already_accepted_only_after_success_audit() {
    let dir = test_dir("already");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    seed_posted_row(&db, TEST_TENANT, QID);

    // A FAILED-writeback accept must NOT block a retry.
    serve::record_operator_accept_audit(
        &state,
        QID,
        "phone",
        "left voicemail",
        "operator-ada",
        1_780_000_000_000,
        None,
        &WritebackOutcome::RoutingMisconfigured {
            http_status: 200,
            content_type: "text/html".to_string(),
            body_excerpt: "<!doctype html>".to_string(),
        },
    )
    .expect("record failed attempt");
    let verdict = serve::accept_quote_precheck(&state, QID, TEST_TENANT).expect("precheck");
    assert!(
        matches!(verdict, AcceptPrecheck::Ready),
        "a failed accept leaves the quote retryable, got {verdict:?}"
    );

    // A SUCCESSFUL accept blocks any further attempt.
    serve::record_operator_accept_audit(
        &state,
        QID,
        "phone",
        "customer confirmed on call",
        "operator-ada",
        1_780_000_000_001,
        None,
        &WritebackOutcome::Success { idempotent: false },
    )
    .expect("record success");
    let verdict = serve::accept_quote_precheck(&state, QID, TEST_TENANT).expect("precheck");
    assert!(
        matches!(verdict, AcceptPrecheck::AlreadyAccepted),
        "a successful accept blocks re-accept, got {verdict:?}"
    );
}

// ── audit payload ─────────────────────────────────────────────────────

#[test]
fn audit_success_payload_round_trips() {
    let dir = test_dir("audit-ok");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    seed_posted_row(&db, TEST_TENANT, QID);

    serve::record_operator_accept_audit(
        &state,
        QID,
        "in_person",
        "signed PO handed over at the shop",
        "operator-bob",
        1_780_000_000_000,
        Some("/var/confirmations/q-accept.png"),
        &WritebackOutcome::Success { idempotent: false },
    )
    .expect("record");

    let entries = accept_entries(&db);
    assert_eq!(entries.len(), 1, "one quote.operator_accepted row");
    let p: serde_json::Value = serde_json::from_slice(&entries[0].payload).expect("decode payload");
    assert_eq!(p["quote_id"], QID);
    assert_eq!(p["tenant_id"], TEST_TENANT);
    assert_eq!(p["channel"], "in_person");
    assert_eq!(p["note"], "signed PO handed over at the shop");
    assert_eq!(p["operator_user_id"], "operator-bob");
    assert_eq!(p["accepted_at_ms"], 1_780_000_000_000i64);
    assert_eq!(
        p["customer_confirmation_path"],
        "/var/confirmations/q-accept.png"
    );
    assert_eq!(p["outcome"], "success");
    assert_eq!(p["retry_available"], false);
}

#[test]
fn audit_failure_payload_captures_reason() {
    let dir = test_dir("audit-fail");
    let db = dir.join("aberp.duckdb");
    let state = build_state(db.clone());
    seed_posted_row(&db, TEST_TENANT, QID);

    serve::record_operator_accept_audit(
        &state,
        QID,
        "email",
        "forwarded customer OK e-mail",
        "operator-ada",
        1_780_000_000_002,
        None,
        &WritebackOutcome::AppRejected {
            http_status: 422,
            body_excerpt: "{\"error\":\"hmac_invalid\"}".to_string(),
        },
    )
    .expect("record");

    let entries = accept_entries(&db);
    assert_eq!(entries.len(), 1);
    let p: serde_json::Value = serde_json::from_slice(&entries[0].payload).expect("decode payload");
    assert_eq!(p["outcome"], "app_rejected");
    assert_eq!(p["retry_available"], true);
    assert_eq!(p["writeback_http_status"], 422);
    assert_eq!(p["writeback_body_excerpt"], "{\"error\":\"hmac_invalid\"}");
    // The confirmation path was absent → JSON null.
    assert!(p["customer_confirmation_path"].is_null());
}

// ── e2e POST against a hand-rolled TCP mock ───────────────────────────

fn http_canned(status_line: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Spawn a one-shot mock that captures the inbound request bytes and
/// replies with `response`. Returns the bound addr + the capture handle.
async fn spawn_capture_mock(
    response: String,
) -> (std::net::SocketAddr, Arc<Mutex<Option<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_for_task = Arc::clone(&captured);
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16 * 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            *captured_for_task.lock().unwrap() =
                Some(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (addr, captured)
}

fn accept_body() -> serde_json::Value {
    serde_json::json!({
        "status": "operator_accepted",
        "channel": "phone",
        "note": "customer confirmed by phone",
        "operator_user_id": "operator-ada",
        "accepted_at_ms": 1_780_000_000_000i64,
        "hmac_signature": "deadbeef",
    })
}

#[tokio::test]
async fn e2e_html_200_is_routing_misconfigured_and_carries_bearer_and_body() {
    let (addr, captured) = spawn_capture_mock(http_canned(
        "200 OK",
        "text/html; charset=utf-8",
        "<!doctype html><html>spa shell</html>",
    ))
    .await;
    let outcome = serve::post_operator_accept(
        &format!("http://{addr}"),
        "00000000-0000-0000-0000-000000000001",
        "t0k3n",
        &accept_body(),
    )
    .await;
    assert!(
        matches!(outcome, WritebackOutcome::RoutingMisconfigured { .. }),
        "HTML 200 must classify as routing-misconfig, got {outcome:?}"
    );
    let req = captured.lock().unwrap().clone().expect("captured request");
    // hyper lowercases HTTP/1.1 header names on the wire — match case-insensitively.
    assert!(
        req.to_lowercase().contains("authorization: bearer t0k3n"),
        "bearer missing: {req}"
    );
    assert!(
        req.contains("operator_accepted"),
        "signed body missing: {req}"
    );
    assert!(
        req.contains("/api/quotes/00000000-0000-0000-0000-000000000001/status"),
        "wrong path: {req}"
    );
}

#[tokio::test]
async fn e2e_422_json_is_app_rejected() {
    let (addr, _cap) = spawn_capture_mock(http_canned(
        "422 Unprocessable Entity",
        "application/json",
        "{\"error\":\"hmac_invalid\"}",
    ))
    .await;
    let outcome = serve::post_operator_accept(
        &format!("http://{addr}"),
        "00000000-0000-0000-0000-000000000001",
        "t0k3n",
        &accept_body(),
    )
    .await;
    match outcome {
        WritebackOutcome::AppRejected { http_status, .. } => assert_eq!(http_status, 422),
        other => panic!("expected AppRejected, got {other:?}"),
    }
}

#[tokio::test]
async fn e2e_200_json_with_status_is_success() {
    let (addr, _cap) = spawn_capture_mock(http_canned(
        "200 OK",
        "application/json",
        "{\"status\":\"approved\",\"id\":\"00000000-0000-0000-0000-000000000001\"}",
    ))
    .await;
    let outcome = serve::post_operator_accept(
        &format!("http://{addr}"),
        "00000000-0000-0000-0000-000000000001",
        "t0k3n",
        &accept_body(),
    )
    .await;
    assert!(
        matches!(outcome, WritebackOutcome::Success { .. }),
        "200 JSON with a status field is a synced accept, got {outcome:?}"
    );
}
