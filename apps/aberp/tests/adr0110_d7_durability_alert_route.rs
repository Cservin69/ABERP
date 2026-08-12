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
    // R3-N2 — mirror serve's REAL boot order: reconcile the audit mirror with
    // the DB (serve.rs, `boot step: reconciling audit-ledger mirror with DB`)
    // BEFORE opening the shared Handle. Omitting it made these tests run
    // against a state production never reaches: a mirror-ahead condition that
    // stays frozen forever. In serve, `ensure_consistent_with_db` attempts the
    // gated auto-heal on every boot, which replays the DB up to the mirror head
    // and so UNFREEZES `sync_mirror`; when the heal refuses, serve does not
    // boot at all. Either way the "permanently frozen mirror" this file used to
    // assume is not a production state.
    {
        let conn = Connection::open(&db_path).expect("open for the boot mirror reconcile");
        aberp_audit_ledger::ensure_schema(&conn).expect("ensure schema at boot");
        match aberp_audit_ledger::ensure_consistent_with_db(
            &conn,
            &aberp_audit_ledger::mirror_path_for(&db_path),
        ) {
            Ok(_) => {}
            // Serve REFUSES to boot on these. The tests here are about what the
            // banner does on a tenant that DOES boot, so surface and continue —
            // a test that silently swallowed a refusal would be claiming to
            // exercise a boot that never happens.
            Err(e) => eprintln!("boot mirror reconcile surfaced (serve would refuse here): {e}"),
        }
    }
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
/// call from `Handle::open` — that is where it lives; `serve::open_tenant_handle`
/// deliberately does NOT carry it (see the constructor's docs) — and this goes
/// RED alongside `the_production_handle_constructor_re_derives_the_alert`.
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
         acknowledgement must out-rank the loss, otherwise the operator has an alarm they can \
         never clear — and an unclearable alarm is one people learn to ignore. Body: {body}"
    );

    // R2-N3 — the assertion above, alone, cannot tell "the acknowledgement was
    // honoured" from "re-derivation is dead and never raises anything". Run the
    // SAME code path against a tenant where the loss is UNacknowledged and
    // require the opposite verdict. Now "down" is a decision, not a silence.
    {
        let other = test_dir("ack-control");
        let other_db = other.join("aberp.duckdb");
        {
            let c = Connection::open(&other_db).expect("seed open");
            ensure_schema(&c).expect("seed schema");
            c.execute_batch("CHECKPOINT;").expect("seed fold");
        }
        {
            let ctl = demo_state(other_db.clone());
            commit_one(&ctl.db, "before");
            ctl.db.durable_ack().expect("healthy ack");
            foreign_open_and_close(&other_db);
            commit_one(&ctl.db, "after");
            let _ = ctl.db.durable_ack().expect_err("the fence fires");
        }
        let ctl = demo_state(other_db.clone());
        let (_, body) = health_json(ctl).await;
        assert!(
            !body["durability_alert"].is_null(),
            "R2-N3 REGRESSION: the identical re-derivation path raised NOTHING for a tenant \
             whose loss was never acknowledged. That means the 'stays down' assertion above is \
             vacuous — it was reading a dead code path, not an honoured acknowledgement. \
             Body: {body}"
        );
        let _ = std::fs::remove_dir_all(&other);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ── R2-B1 — the acknowledgement must work in the ONE case it is for ─────────

/// **A real loss FREEZES the mirror, and the acknowledgement was landing only
/// there.**
///
/// [`acknowledging_clears_the_banner_records_it_and_survives_a_restart`] acks
/// IN-PROCESS, before any restart, so the Handle's in-memory catalog still
/// holds the post-fold seq and `sync_mirror` sees no divergence. That is not
/// the scenario B2 exists for. B2's entire premise is that the banner survives
/// a restart — which means the operator acknowledges AFTER restarting.
///
/// What happens then, and why the ack was inert:
///
/// 1. The truncation drops WAL-resident rows, so on the next boot the **DB head
///    REGRESSES**. The mirror is append-only and `fsync`'d, so it keeps the
///    higher seq.
/// 2. The mirror is now AHEAD of the DB. `sync_mirror` (mirror.rs:653) returns
///    `MirrorDivergent` and appends **nothing**.
/// 3. `WriteGuard::drop` only `warn!`s on that, so the mirror stays frozen for
///    the rest of the process — every audit row after the incident is refused.
///    (R3-N2: "for good" was too strong. Serve's boot mirror-reconcile attempts
///    a gated auto-heal on the NEXT boot, which replays the DB up to the mirror
///    head and un-freezes it. The freeze is real within a process, not
///    permanent across restarts.)
/// 4. `record_durability_alert_ack_audit` nevertheless commits to the DB and
///    returns 200. The operator watches the banner drop and reasonably believes
///    they have acknowledged it.
/// 5. The next boot re-derives from the mirror alone, sees loss-with-no-ack,
///    and re-raises. Forever. The button does nothing that lasts.
///
/// This drives exactly that: loss → RESTART → acknowledge over the real route →
/// RESTART → the banner must stay down, through the same boot order serve uses
/// (reconcile, then open the Handle).
///
/// R3-N2 note on what this test is now worth: with the boot reconcile in the
/// path it no longer isolates the DB half — the heal un-freezes the mirror, so
/// the acknowledgement reaches it too, and deleting `db_audit_times` leaves this
/// green. It earns its place as the END-TO-END pin (real loss, real route, two
/// real restarts). The both-store rule itself is pinned directly by
/// `an_ack_that_reached_only_the_db_still_clears_a_loss_that_reached_only_the_mirror`
/// in `crates/aberp-db/tests/adr0110_d7_wal_fence.rs`.
#[tokio::test]
async fn after_a_real_loss_the_acknowledgement_survives_even_though_the_mirror_froze() {
    let dir = test_dir("frozen-mirror-ack");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    // ── Boot 1: stage a REAL loss (WAL-resident rows truncated away) ──────
    {
        let state = demo_state(db_path.clone());
        commit_one(&state.db, "pre-loss-1");
        state.db.durable_ack().expect("healthy ack");
        // Several WAL-resident commits, so the truncation really does cost the
        // DB rows the mirror already has.
        for i in 0..3 {
            commit_one(&state.db, &format!("wal-resident-{i}"));
        }
        foreign_open_and_close(&db_path);
        commit_one(&state.db, "post-truncation");
        let _ = state
            .db
            .durable_ack()
            .expect_err("precondition: the fence must fire");
        assert!(
            state.db.durability_alert().is_some(),
            "precondition: boot 1 raised the banner"
        );
    }

    // ── Boot 2: the operator sees the banner and ACKNOWLEDGES ─────────────
    let state = demo_state(db_path.clone());
    assert!(
        state.db.durability_alert().is_some(),
        "precondition: the alert survived the restart (B2)"
    );

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
        "the acknowledge route must succeed after a real loss too"
    );
    server.abort();

    let (_, body) = health_json(state.clone()).await;
    assert!(
        body["durability_alert"].is_null(),
        "the banner must come down in-process after acknowledging"
    );
    drop(state);

    // ── Boot 3: it must STAY down ─────────────────────────────────────────
    let restarted = demo_state(db_path.clone());
    let (_, body) = health_json(restarted).await;
    assert!(
        body["durability_alert"].is_null(),
        "ADR-0110 D7 / R2-B1 REGRESSION: the banner came BACK after the operator acknowledged \
         a REAL loss. The truncation regressed the DB head below the append-only mirror's, so \
         `sync_mirror` returns MirrorDivergent and appends NOTHING from then on — the mirror is \
         frozen for good and the acknowledgement never reaches it. The route still committed to \
         the DB and returned 200, so the operator watched the banner drop and believes they \
         acknowledged it. Re-derivation must therefore also consult the DB, which is \
         authoritative for anything written after the restart. Body: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A torn mirror tail must not silently swallow the alarm** (R2-B2).
///
/// `read_mirror_entries` is STRICT: a missing trailing newline, a JSON error or
/// a seq gap makes it return `Err`, and the re-derivation's error arm logs and
/// moves on — boot comes up clean, no banner.
///
/// An unterminated final line is the commonest crash artifact there is, and a
/// durability incident is *precisely* the condition most likely to produce one.
/// So the strict reader fails exactly when it is needed. The tree already owns
/// the right primitive — `read_mirror_under_tail_policy`, which the boot
/// reconciler uses — and it hands back the chain-reverified intact prefix.
#[tokio::test]
async fn a_torn_mirror_tail_does_not_lose_the_alarm() {
    let dir = test_dir("torn-tail");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    // Stage a loss so the mirror carries a `db.durability_loss_detected` row.
    {
        let state = demo_state(db_path.clone());
        commit_one(&state.db, "before");
        state.db.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db_path);
        commit_one(&state.db, "after");
        let _ = state.db.durable_ack().expect_err("the fence fires");
    }

    // Tear the tail: append an unterminated partial line, the shape a crash
    // mid-append leaves behind.
    let mirror = aberp_audit_ledger::mirror_path_for(&db_path);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&mirror)
            .expect("open mirror to tear it");
        f.write_all(br#"{"id":"partial","seq":9999,"prev_hash":"de"#)
            .expect("write a torn trailing line");
    }
    assert!(
        aberp_audit_ledger::read_mirror_entries(&mirror).is_err(),
        "precondition: the STRICT reader must reject this torn tail — otherwise this test is \
         not exercising the condition it names"
    );

    // Boot onto the torn mirror.
    let state = demo_state(db_path.clone());
    let (_, body) = health_json(state).await;
    assert!(
        !body["durability_alert"].is_null(),
        "ADR-0110 D7 / R2-B2 REGRESSION: a TORN MIRROR TAIL silently swallowed the durability \
         alarm — boot came up clean with the banner absent. An unterminated final line is the \
         commonest crash artifact, and a durability incident is exactly the condition most \
         likely to co-occur with one, so the strict reader fails precisely when it matters. \
         Use `read_mirror_under_tail_policy` (the boot reconciler's reader), which returns the \
         chain-reverified intact prefix. Body: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── R3-N2 — the boot reconcile is part of the story ────────────────────────

/// **A SECOND loss, after an earlier one was acknowledged, DOES re-raise across
/// a restart.**
///
/// This replaces a test that claimed the opposite. That one asserted a second
/// loss left no durable trace, and it passed — but only because this file's
/// `demo_state` skipped serve's boot mirror-reconcile, so it pinned a state
/// production never reaches.
///
/// What the reconcile changes: after a real truncation the mirror is ahead of
/// the DB, and `ensure_consistent_with_db` attempts a **gated auto-heal** on
/// every boot. On success it replays the DB up to the mirror head, which brings
/// the DB back level and **un-freezes `sync_mirror`** — so the acknowledgement
/// and every later loss row reach the mirror after all. The "permanently frozen
/// mirror" the old test staged is not a production steady state: either the
/// heal runs, or the heal refuses and serve does not boot at all (see the
/// ADR-0110 D7 note on refuse-to-boot).
///
/// So the real residual is much narrower than it was written up as, and it is
/// recorded that way in ADR-0110 D7.4d: it needs a process that never restarts
/// between the two incidents, and in that process the in-session banner is up
/// the whole time. That is not a gap worth re-prioritising D5 over — D5 is
/// still right, on its own merits.
///
/// Mutation-verified: remove the reconcile block from `demo_state` and this
/// goes RED, which is exactly how the old test was passing.
#[tokio::test]
async fn a_second_loss_after_an_acknowledged_one_re_raises_across_a_restart() {
    let dir = test_dir("second-loss-re-raises");
    let db_path = dir.join("aberp.duckdb");
    {
        let c = Connection::open(&db_path).expect("seed open");
        ensure_schema(&c).expect("seed schema");
        c.execute_batch("CHECKPOINT;").expect("seed fold");
    }

    // Loss #1.
    {
        let state = demo_state(db_path.clone());
        commit_one(&state.db, "before");
        state.db.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db_path);
        commit_one(&state.db, "after");
        let _ = state.db.durable_ack().expect_err("loss #1 fires");
    }

    // Restart, then acknowledge it over the real route.
    {
        let state = demo_state(db_path.clone());
        assert!(
            state.db.durability_alert().is_some(),
            "precondition: loss #1 survived the restart"
        );
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
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        server.abort();
    }

    // Loss #2, on the still-unrecovered tenant.
    {
        let state = demo_state(db_path.clone());
        assert!(
            state.db.durability_alert().is_none(),
            "precondition: loss #1 is acknowledged and quiet"
        );
        commit_one(&state.db, "post-ack");
        state.db.durable_ack().expect("healthy ack");
        foreign_open_and_close(&db_path);
        commit_one(&state.db, "second-loss");
        let _ = state.db.durable_ack().expect_err("loss #2 fires");
        assert!(
            state.db.durability_alert().is_some(),
            "loss #2 must raise the banner in-session"
        );
    }

    // ...and it must STILL be up after the restart.
    let restarted = demo_state(db_path.clone());
    let (_, body) = health_json(restarted).await;
    assert!(
        !body["durability_alert"].is_null(),
        "R3-N2 REGRESSION: a SECOND durability loss, occurring after the operator acknowledged \
         the first, did not survive a restart. An acknowledgement clears the loss it answered, \
         not all future ones — and the boot mirror-reconcile heals the mirror-ahead state on \
         every boot, so `sync_mirror` is working again and loss #2's audit row does reach the \
         mirror. If this is red, either the reconcile is no longer in the boot path or an ack \
         is being read as covering losses that post-date it. Body: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **R2-N2 — a FAILED audit append must leave the banner UP.**
///
/// `acknowledge_durability_alert` records first and clears second, and that
/// order is the whole safety property: an unrecorded clear is indistinguishable
/// from amnesia, and the next boot would re-raise the banner with no trace of
/// who took it down or why. Swapping to clear-first/audit-second left every
/// other test green, so the ordering was asserted only by a comment.
///
/// The append is made to fail deterministically by dropping `audit_ledger` out
/// from under it — a blunt instrument, but it exercises the exact branch
/// (`record_durability_alert_ack_audit` returning `Err`, the `?` in the library
/// core) that a real disk-full or chain-integrity failure would take.
#[tokio::test]
async fn a_failed_acknowledgement_audit_leaves_the_banner_up() {
    let dir = test_dir("ack-audit-fails");
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

    // Break the audit append.
    {
        let guard = state.db.write().expect("writer");
        guard
            .execute_batch("DROP TABLE audit_ledger;")
            .expect("drop the audit table so the ack's append must fail");
    }

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
    let status = resp.status();
    server.abort();

    assert_eq!(
        status,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "an acknowledgement whose audit row could not be written must FAIL LOUDLY, not report \
         success (ADR-0110 R3 / rule 11)"
    );
    assert!(
        state.db.durability_alert().is_some(),
        "R2-N2 REGRESSION: the banner came DOWN even though the acknowledgement could not be \
         recorded. Audit-then-clear is the safety order: clearing first leaves the operator \
         with no banner AND no trail, and the next boot re-raises it with nothing to explain \
         why. A failed append must leave the alert standing."
    );

    let _ = std::fs::remove_dir_all(&dir);
}
