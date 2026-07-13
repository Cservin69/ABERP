//! ADR-0099 H3 STEP 4c — the fused work-order cluster on the shared Handle.
//!
//! These pins exercise the six-family cluster
//! `{work_orders + products + inventory + qa + dispatch + invoice_draft}` end to
//! end through the serve request functions, driving them on ONE shared
//! `aberp_db::Handle` with the `SERVE_HANDLE_LIVE` tripwire ARMED (mirrors serve
//! boot: Handle open, then tripwire arm). What they prove:
//!
//!   A. **Re-entrancy** — a single `decide_qa_inspection_request` Pass runs the
//!      whole `decide_qa → try_auto_complete_wo → transition_work_order Complete →
//!      WoCompletion record_movement` chain in ONE tx on ONE write guard, with NO
//!      nested `db.write()`/`db.read()`. The non-reentrant writer-mutex tripwire
//!      would PANIC on a nested acquire; reaching the assertions is the proof. The
//!      WoCompletion inventory movement co-committed on the SAME guard is read back
//!      through the Handle.
//!
//!   B. **mark_shipped sees the Handle WO + invoice_draft chain unchanged** — the
//!      WO is driven to Completed by the Handle-based auto-complete above (its
//!      Completed state is WAL-resident on the Handle). `create_dispatch_request`
//!      then reads that WO in-tx and finds it eligible (a fresh `Connection::open`
//!      would miss the WAL-resident Complete and refuse), and
//!      `mark_dispatch_shipped_request`'s `BillingInvoiceSpawner` writes the
//!      `invoice_draft` row + one `InvoiceStaged` audit entry in the ship tx. The
//!      draft is read back, `InvoiceStaged` is present, and the whole audit chain
//!      `verify_chain`s — the invoice_draft / NAV-staging output is structurally
//!      unchanged by the migration.
//!
//!   C. **The full shipment gate blocks with no fresh conn** — a defense dispatch
//!      created THROUGH the Handle (WAL-resident) is read by the part-UID shipment
//!      gate through `state.db.read()`; the gate BLOCKS the unmarked units. A gate
//!      still using a fresh `Connection::open` would not see the WAL-resident
//!      dispatch and would fail OPEN (ship). Blocking on a Handle-only dispatch is
//!      the coherence proof; the armed tripwire additionally proves the block-audit
//!      append rides the shared writer (no forked `Ledger::open`).

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use duckdb::Connection;
use rust_decimal::Decimal;
use ulid::Ulid;

use aberp_audit_ledger::serve_tripwire::{is_serve_handle_live, register_serve_handle};
use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, EventKind, Ledger, LedgerMeta,
    TenantId,
};
use aberp_inventory::{
    ensure_schema as ensure_inventory_schema, record_movement, ActorKind, MovementReason,
    MovementRefKind, RecordMovementContext, RecordMovementInputs,
};
use aberp_qa::ensure_schema as ensure_qa_schema;
use aberp_work_orders::{
    create_work_order, ensure_schema as ensure_wo_schema, replace_bom_for_product,
    transition_routing_op, transition_work_order, BomLineInput, CreateWorkOrderInputs,
    RoutingOpAction, RoutingOpInput, RoutingOpTransitionInputs, TransitionInputs, WoAction,
    WoWriteContext,
};

use aberp::partners::{create_partner, CustomerType, PartnerInputs, PartnerKind};
use aberp::serve::{
    self, AppState, CreateDispatchBody, DecideQaInspectionBody, MarkDispatchShippedBody,
};

const TEST_TENANT: &str = "ten_step4c_cluster";
const TEST_LOGIN: &str = "ervin";

const PRODUCTS_SCHEMA_FOR_TESTS: &str = "
CREATE TABLE IF NOT EXISTS products (
    id               VARCHAR NOT NULL PRIMARY KEY,
    tenant_id        VARCHAR NOT NULL,
    name             VARCHAR NOT NULL,
    unit_kind        VARCHAR NOT NULL,
    unit_value       VARCHAR NOT NULL,
    currency         VARCHAR NOT NULL,
    unit_price_minor BIGINT  NOT NULL,
    created_at       VARCHAR NOT NULL,
    updated_at       VARCHAR NOT NULL,
    deleted_at       VARCHAR
);
";

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-step4c-cluster")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn meta() -> LedgerMeta {
    LedgerMeta::new(
        TenantId::new(TEST_TENANT).unwrap(),
        BinaryHash::from_bytes([0u8; 32]),
    )
}

/// Seed every cluster schema on a fresh connection BEFORE the shared Handle
/// opens (mirrors serve boot: fresh-open ensure passes, then the Handle re-opens
/// the same file and sees the tables under Q1).
fn ensure_all_schemas(db_path: &PathBuf) {
    let conn = Connection::open(db_path).expect("open test DB");
    conn.execute_batch(PRODUCTS_SCHEMA_FOR_TESTS)
        .expect("create products test schema");
    ensure_inventory_schema(&conn).expect("inventory schema");
    ensure_audit_schema(&conn).expect("audit schema");
    ensure_wo_schema(&conn).expect("wo schema");
    ensure_qa_schema(&conn).expect("qa schema");
    aberp_dispatch::ensure_schema(&conn).expect("dispatch schema");
    aberp::invoice_draft::ensure_schema(&conn).expect("invoice_draft schema");
    aberp::partners::ensure_schema(&conn).expect("partners schema");
}

fn insert_product(conn: &Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO products (id, tenant_id, name, unit_kind, unit_value, currency,
                               unit_price_minor, created_at, updated_at, deleted_at,
                               stock_qty, min_stock)
         VALUES (?, ?, ?, 'Nav', 'PIECE', 'HUF', 0, '2026-01-01T00:00:00Z',
                 '2026-01-01T00:00:00Z', NULL, 0, 0);",
        duckdb::params![id, TEST_TENANT, name],
    )
    .expect("insert product");
}

fn wo_ctx<'a>(m: &'a LedgerMeta, login: &str) -> WoWriteContext<'a> {
    WoWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: login.to_string(),
        },
        ledger_meta: m,
        ledger_actor: Actor::from_local_cli("seed-session".to_string(), login),
    }
}

fn seed_component_stock(conn: &mut Connection, m: &LedgerMeta, product_id: &str, qty: &str) {
    let tx = conn.transaction().unwrap();
    let ctx = RecordMovementContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: "seed".to_string(),
        },
        ledger_meta: m,
        ledger_actor: Actor::from_local_cli("seed-session".to_string(), "seed"),
    };
    record_movement(
        &tx,
        &ctx,
        RecordMovementInputs {
            product_id: product_id.to_string(),
            qty_delta: Decimal::from_str(qty).unwrap(),
            reason: MovementReason::Receipt,
            ref_kind: MovementRefKind::Manual,
            ref_id: None,
            notes: None,
            idempotency_key: format!("seed-{product_id}"),
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

/// Build a single-op WO, release it (consumes component stock), start it,
/// complete the op so a Pending QA inspection exists. Returns `(qa_id, wo_id)`.
/// All on FRESH connections before the Handle opens — the WO lands at
/// InProgress on disk (Q1-visible to the Handle).
fn seed_one_op_wo_with_pending_qa(db_path: &PathBuf) -> (String, String) {
    let mut conn = Connection::open(db_path).expect("reopen test DB");
    let m = meta();

    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Raw bar");
    seed_component_stock(&mut conn, &m, "prd_bar", "10");

    let tx = conn.transaction().unwrap();
    replace_bom_for_product(
        &tx,
        TEST_TENANT,
        "prd_widget",
        &[BomLineInput {
            component_id: "prd_bar".to_string(),
            qty_per_unit: Decimal::from_str("1").unwrap(),
        }],
    )
    .unwrap();
    tx.commit().unwrap();

    let tx = conn.transaction().unwrap();
    let (wo, ops) = create_work_order(
        &tx,
        &wo_ctx(&m, TEST_LOGIN),
        CreateWorkOrderInputs {
            wo_number: "WO-4C-001".to_string(),
            product_id: "prd_widget".to_string(),
            qty_target: Decimal::from_str("2").unwrap(),
            notes: None,
            routing_ops: vec![RoutingOpInput {
                op_name: "Polish".to_string(),
                est_time_min: None,
                est_cost_huf: None,
            }],
            idempotency_key: "create-4c-1".to_string(),
            source_quote_id: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    for (action, key) in [
        (WoAction::Release, "release-4c-1"),
        (WoAction::Start, "start-4c-1"),
    ] {
        let tx = conn.transaction().unwrap();
        transition_work_order(
            &tx,
            &wo_ctx(&m, TEST_LOGIN),
            &wo.wo_id,
            TransitionInputs {
                action,
                reason: None,
                source_event_id: None,
                idempotency_key: key.to_string(),
                actual_machining_minutes: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx(&m, TEST_LOGIN),
        &ops[0].routing_op_id,
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op-complete-4c-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    (outcome.qa_inspection_id, wo.wo_id)
}

/// Seed a partner of the given segment on a fresh connection before the Handle.
/// Returns the minted `prt_*` id.
fn seed_partner(db_path: &PathBuf, name: &str, ct: CustomerType) -> String {
    let conn = Connection::open(db_path).expect("reopen for partner seed");
    let inputs = PartnerInputs {
        display_name: name.to_string(),
        legal_name: name.to_string(),
        kind: PartnerKind::Customer,
        customer_vat_status: Default::default(),
        customer_type: ct,
        tax_number: None,
        eu_vat_number: None,
        address_street: None,
        address_postal_code: None,
        address_city: None,
        address_country: None,
        bank_account: None,
        contact_email: None,
        contact_phone: None,
    };
    create_partner(&conn, TEST_TENANT, &inputs)
        .expect("seed partner")
        .id
}

/// Seed a Completed WO row directly (no routing flow) for the gate test.
fn seed_completed_wo(db_path: &PathBuf, wo_id: &str, qty: &str) {
    let conn = Connection::open(db_path).expect("reopen for WO seed");
    insert_product(&conn, "prd_gate", "Gate widget");
    conn.execute(
        "INSERT INTO work_orders (
            wo_id, tenant_id, wo_number, product_id, qty_target, state,
            created_at, source_quote_id
         ) VALUES (?1, ?2, ?3, 'prd_gate', ?4, 'completed', '2026-06-06T00:00:00Z', NULL)",
        duckdb::params![wo_id, TEST_TENANT, wo_id, qty],
    )
    .expect("seed completed WO");
}

fn build_state(db_path: PathBuf) -> AppState {
    let tenant = TenantId::new(TEST_TENANT.to_string()).expect("tenant id");
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    AppState {
        db: serve::open_tenant_handle(&db_path, tenant.clone())
            .expect("test: open shared aberp-db Handle"),
        db_path: Arc::new(db_path),
        tenant,
        nav_enabled: true,
        binary_hash: aberp::binary_hash::BinaryHashHandle::from_ready(binary_hash),
        session_token: Arc::new("test-token".to_string()),
        secrets_cache: aberp::secrets_cache::SecretsCache::empty(),
        nav_poll_semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
            serve::NAV_POLL_DAEMON_CONCURRENCY,
        )),
        boot_state: Arc::new(std::sync::RwLock::new(serve::ServeBootState::Ready {
            operator_login: TEST_LOGIN.to_string(),
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
        email_relay_rate_limiter: std::sync::Arc::new(aberp::email_relay::RateLimiter::new()),
        pipeline_python_resolution: aberp::quote_pricing_pipeline::PythonResolutionHandle::dormant(
        ),
        storefront_credential: aberp::storefront_credential::StorefrontCredentialHandle::dormant(),
        email_outbox_daemon: aberp::email_outbox_poll_daemon::EmailOutboxDaemonHandle::dormant(),
        quote_pdf_rerender_queue: aberp::quote_pdf_rerender_queue::QuotePdfRerenderQueue::new(),
        digital_id: std::sync::Arc::new(aberp_digital_id::MockProvider::new()),
    }
}

/// A — re-entrancy: the whole decide→auto-complete→inventory chain on ONE guard,
/// tripwire armed, and the WoCompletion movement co-committed on that guard.
#[test]
fn qa_decide_auto_completes_wo_and_records_inventory_on_one_guard() {
    let dir = test_dir("reentrancy");
    let db_path = dir.join("test.duckdb");
    ensure_all_schemas(&db_path);
    let (qa_id, wo_id) = seed_one_op_wo_with_pending_qa(&db_path);

    let state = build_state(db_path.clone());
    // ARM both tripwires (mirrors serve boot). A nested writer acquire panics via
    // the re-entrancy tripwire; an independent `Ledger::open` panics via
    // SERVE_HANDLE_LIVE. Reaching the asserts proves neither fired.
    let _tripwire = register_serve_handle(&db_path);
    assert!(is_serve_handle_live(&db_path));

    let resp = serve::decide_qa_inspection_request(
        &state,
        &qa_id,
        TEST_LOGIN,
        DecideQaInspectionBody {
            decision: "pass".to_string(),
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "qa-decide-4c-1".to_string(),
        },
    )
    .expect("decide_qa must NOT panic (re-entrancy) and must succeed");
    assert_eq!(
        resp.wo_auto_completed.as_deref(),
        Some(wo_id.as_str()),
        "a one-op Pass must auto-complete the WO in the SAME tx (the re-entrant chain ran)"
    );

    // The WoCompletion inventory movement co-committed on the same guard — read it
    // back through the Handle (a fresh open would miss the WAL-resident movement).
    let conn = state.db.read().expect("shared reader");
    let movements =
        aberp_inventory::list_movements_for_product(&conn, TEST_TENANT, "prd_widget", 100, 0)
            .expect("list movements for finished good");
    assert!(
        movements
            .iter()
            .any(|mvt| mvt.reason == MovementReason::WoCompletion),
        "the auto-complete must have recorded a WoCompletion movement in the same guard, got {:?}",
        movements.iter().map(|m| m.reason).collect::<Vec<_>>()
    );
    assert!(is_serve_handle_live(&db_path));
}

/// B — mark_shipped reads the Handle-completed WO in-tx and spawns the invoice
/// draft + InvoiceStaged audit; the audit chain verifies (structurally unchanged).
#[test]
fn handle_completed_wo_dispatches_ships_and_spawns_invoice_draft_chain() {
    let dir = test_dir("ship-invoice-draft");
    let db_path = dir.join("test.duckdb");
    ensure_all_schemas(&db_path);
    let (qa_id, wo_id) = seed_one_op_wo_with_pending_qa(&db_path);
    let partner_id = seed_partner(&db_path, "Acme Industrial", CustomerType::Industrial);

    let state = build_state(db_path.clone());
    let _tripwire = register_serve_handle(&db_path);

    // Auto-complete the WO on the Handle (its Completed state is WAL-resident).
    let decided = serve::decide_qa_inspection_request(
        &state,
        &qa_id,
        TEST_LOGIN,
        DecideQaInspectionBody {
            decision: "pass".to_string(),
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "qa-decide-4c-2".to_string(),
        },
    )
    .expect("decide_qa");
    assert_eq!(decided.wo_auto_completed.as_deref(), Some(wo_id.as_str()));

    // create_dispatch reads the WO in-tx and must find it eligible (Completed).
    // A fresh Connection::open would miss the WAL-resident Complete → refuse.
    let dispatch = serve::create_dispatch_request(
        &state,
        TEST_LOGIN,
        CreateDispatchBody {
            wo_id: wo_id.clone(),
            partner_id: partner_id.clone(),
            notes: None,
            idempotency_key: "dsp-4c-1".to_string(),
        },
    )
    .expect("create_dispatch must see the Handle-completed WO as eligible");

    // mark_shipped reads the WO in-tx (product_id + qty) and the injected
    // BillingInvoiceSpawner writes the invoice_draft + InvoiceStaged in the tx.
    let shipped = serve::mark_dispatch_shipped_request(
        &state,
        &dispatch.dsp_id,
        TEST_LOGIN,
        MarkDispatchShippedBody {
            carrier_kind: "gls".to_string(),
            tracking_number: Some("TRK-1".to_string()),
            shipped_at: None,
            idempotency_key: "ship-4c-1".to_string(),
        },
    )
    .expect("mark_shipped on a Handle WO must succeed (non-defense, no gate)");
    let drf_id = shipped
        .spawned_invoice_id
        .expect("BillingInvoiceSpawner must have staged an invoice_draft");
    assert!(
        drf_id.starts_with("drf_"),
        "spawner returns a drf_* id: {drf_id}"
    );

    // The draft is readable through the Handle (invoice_draft is now a Handle family).
    let draft = serve::get_invoice_draft_request(&state, &drf_id)
        .expect("read draft")
        .expect("the just-spawned draft must exist");
    assert_eq!(
        draft.source_dispatch_id.as_deref(),
        Some(dispatch.dsp_id.as_str())
    );

    // The invoice-staging audit chain is intact and verifies. Read it through the
    // SHARED Handle (`Ledger::from_connection` on a `db.read()` try_clone) — a
    // fresh `Ledger::open` here would (correctly) trip SERVE_HANDLE_LIVE, since a
    // serve Handle is registered. `InvoiceStaged` from the spawner is present (the
    // NAV-staging output is structurally unchanged by the migration).
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    let tenant = TenantId::new(TEST_TENANT.to_string()).unwrap();
    let ledger = Ledger::from_connection(
        state.db.read().expect("shared reader for verify_chain"),
        tenant.clone(),
        binary_hash,
    );
    ledger
        .verify_chain()
        .expect("audit chain must verify after the ship tx");
    let saw_staged = Ledger::from_connection(
        state.db.read().expect("shared reader for entries"),
        tenant,
        binary_hash,
    )
    .entries()
    .expect("entries")
    .iter()
    .any(|e| matches!(e.kind, EventKind::InvoiceStaged));
    assert!(saw_staged, "the ship tx must have emitted InvoiceStaged");
    assert!(is_serve_handle_live(&db_path));
}

/// C — the full part-UID shipment gate blocks a defense dispatch created THROUGH
/// the Handle. A fresh-conn gate would miss the WAL-resident dispatch and ship;
/// blocking is the coherence proof. The armed tripwire proves the block-audit
/// append does not fork a Ledger.
#[test]
fn defense_shipment_gate_blocks_handle_dispatch_with_no_ledger_fork() {
    let dir = test_dir("gate-block");
    let db_path = dir.join("test.duckdb");
    ensure_all_schemas(&db_path);
    seed_completed_wo(&db_path, "wo_gate_1", "2");
    let partner_id = seed_partner(&db_path, "Aegis Defense", CustomerType::Defense);

    let state = build_state(db_path.clone());
    let _tripwire = register_serve_handle(&db_path);

    // The dispatch is created THROUGH the Handle → WAL-resident, visible only via
    // the Handle. The gate's read_dispatch_for_gate MUST read it via state.db.read().
    let dispatch = serve::create_dispatch_request(
        &state,
        TEST_LOGIN,
        CreateDispatchBody {
            wo_id: "wo_gate_1".to_string(),
            partner_id,
            notes: None,
            idempotency_key: "dsp-gate-1".to_string(),
        },
    )
    .expect("create defense dispatch");

    // Ship it: qty_target 2, zero part marks → the part-UID gate must BLOCK.
    let err = serve::mark_dispatch_shipped_request(
        &state,
        &dispatch.dsp_id,
        TEST_LOGIN,
        MarkDispatchShippedBody {
            carrier_kind: "gls".to_string(),
            tracking_number: None,
            shipped_at: None,
            idempotency_key: "ship-gate-1".to_string(),
        },
    )
    .expect_err("a defense dispatch with unmarked units must be BLOCKED at the gate");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("UID") || msg.contains("Conflict"),
        "the block must be the part-UID gate 409, got: {msg}"
    );
    // Reaching here without a SERVE_HANDLE_LIVE panic proves the block audit rode
    // the shared writer, and the block itself proves the gate read the WAL-resident
    // dispatch through the Handle (a fresh conn would have failed open → shipped).
    assert!(is_serve_handle_live(&db_path));
}
