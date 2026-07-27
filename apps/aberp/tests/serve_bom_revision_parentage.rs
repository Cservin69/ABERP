//! ADR-0105 — the BOM-revision READ routes must not assert a false
//! parentage.
//!
//! `bom_revisions` rows are addressed by `bom_rev_id` alone
//! (`read_bom_revision` / `list_bom_lines_for_revision` filter on
//! `tenant_id + bom_rev_id`, NOT on `product_id`), so the only thing
//! keeping `GET /api/products/:id/bom/revisions/:rev_id` honest is the
//! explicit product-parentage check in `get_bom_revision_request` — and
//! `diff_bom_revisions_request` leans on that same check to keep a diff
//! inside ONE product.
//!
//! Without it, `GET /api/products/prd_A/bom/revisions/<rev-of-B>`
//! answers 200 with product B's as-built lines under product A's URL,
//! and the diff route renders a B-vs-A component delta in A's revision
//! history panel. That is the exact failure the feature exists to
//! prevent: an as-built record that reads as truth while describing a
//! different part.
//!
//! Found by adversarial review of PR #34 (2026-07-27): deleting the
//! parentage check outright left the whole 2969-test suite GREEN. These
//! are the pins that red it.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use duckdb::Connection;
use rust_decimal::Decimal;
use ulid::Ulid;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};
use aberp_inventory::{ensure_schema as ensure_inventory_schema, ActorKind};
use aberp_work_orders::{
    ensure_schema as ensure_wo_schema, replace_bom_for_product, BomLineInput, WoWriteContext,
};

use aberp::serve::{self, AppState, WorkOrderRouteError};

const TEST_TENANT: &str = "ten_bomrev_parentage";
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
        .join("aberp-bomrev-parentage")
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

/// Author one revision for `product_id` and return its `bmr_*` id. All on
/// a fresh connection BEFORE the Handle opens, so the rows are Q1-visible.
fn author_revision(db_path: &PathBuf, product_id: &str, component_id: &str, qty: &str) -> String {
    let mut conn = Connection::open(db_path).expect("reopen test DB");
    let m = meta();
    let tx = conn.transaction().unwrap();
    let out = replace_bom_for_product(
        &tx,
        &wo_ctx(&m, "bom-author"),
        product_id,
        &[BomLineInput {
            component_id: component_id.to_string(),
            qty_per_unit: Decimal::from_str(qty).unwrap(),
        }],
        Some("seed"),
    )
    .expect("author BOM revision");
    tx.commit().unwrap();
    out.revision.bom_rev_id
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

/// Seed two products, each with its own single-line BOM revision.
/// Returns `(state, rev_a, rev_b)`.
fn seed_two_products() -> (AppState, String, String) {
    let dir = test_dir("parentage");
    let db_path = dir.join("test.duckdb");

    let conn = Connection::open(&db_path).expect("open test DB");
    conn.execute_batch(PRODUCTS_SCHEMA_FOR_TESTS)
        .expect("products schema");
    ensure_inventory_schema(&conn).expect("inventory schema");
    ensure_audit_schema(&conn).expect("audit schema");
    ensure_wo_schema(&conn).expect("wo schema");
    insert_product(&conn, "prd_a", "Product A");
    insert_product(&conn, "prd_b", "Product B");
    insert_product(&conn, "prd_comp_a", "Component A");
    insert_product(&conn, "prd_comp_b", "Component B");
    drop(conn);

    let rev_a = author_revision(&db_path, "prd_a", "prd_comp_a", "2");
    let rev_b = author_revision(&db_path, "prd_b", "prd_comp_b", "7");
    assert_ne!(rev_a, rev_b);

    (build_state(db_path), rev_a, rev_b)
}

/// A revision read under the WRONG product's URL is refused — the route
/// must not hand back another part's as-built lines.
#[test]
fn revision_read_refuses_a_revision_belonging_to_another_product() {
    let (state, rev_a, rev_b) = seed_two_products();

    // Positive control: each product resolves its OWN revision, with the
    // lines it was authored with. If this fails the pin below is vacuous.
    let own = serve::get_bom_revision_request(&state, "prd_a", &rev_a)
        .expect("product A must resolve its own revision");
    assert_eq!(own.revision.product_id, "prd_a");
    assert_eq!(own.lines.len(), 1);
    assert_eq!(own.lines[0].component_id, "prd_comp_a");

    // The attack: ask for product B's revision under product A's path.
    let cross = serve::get_bom_revision_request(&state, "prd_a", &rev_b);
    assert!(
        matches!(cross, Err(WorkOrderRouteError::NotFound)),
        "a revision of product B must NOT resolve under product A's URL — \
         the response would assert a false as-built parentage; got {:?}",
        cross.map(|d| d.revision.product_id)
    );
}

/// A diff must never straddle two products: both sides go through the
/// same parentage gate, so a cross-product pair is refused rather than
/// rendered as this product's revision-to-revision change.
#[test]
fn diff_refuses_a_cross_product_revision_pair() {
    let (state, rev_a, rev_b) = seed_two_products();

    // Positive control: a same-product diff of a revision against itself
    // is empty, and — crucially — REACHES the diff (no spurious refusal).
    let same = serve::diff_bom_revisions_request(&state, "prd_a", &rev_a, &rev_a)
        .expect("a same-product diff must be reachable");
    assert!(same.is_empty(), "a revision diffed against itself is empty");

    // The attack, both orderings — the gate must hold on the `from` side
    // and the `to` side, not just whichever is read first.
    for (from, to, side) in [(&rev_a, &rev_b, "to"), (&rev_b, &rev_a, "from")] {
        let cross = serve::diff_bom_revisions_request(&state, "prd_a", from, to);
        assert!(
            matches!(cross, Err(WorkOrderRouteError::NotFound)),
            "a diff whose {side} side belongs to another product must be refused, \
             not rendered as product A's BOM history"
        );
    }
}
