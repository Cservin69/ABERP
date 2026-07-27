//! CUSTOMER-JOURNEY e2e — issue a domestic invoice, then every follow-up
//! operation that RE-READS it must find it (2026-07-27, DEV invoice #62).
//!
//! ## The bug this is the regression pin for
//!
//! `POST /invoices/issue` writes `InvoiceSequenceReserved` +
//! `InvoiceDraftCreated` (and the `invoice` / `invoice_line` rows) through the
//! ONE shared `aberp_db::Handle`. Under H3 (ADR-0099) runtime checkpointing is
//! DISABLED, so those rows stay WAL-resident for the life of the serve process.
//!
//! `print_invoice::render_to_bytes` — reached IN-SERVE from
//! `serve::get_invoice_pdf`, which the PDF route, the email-compose path and
//! the manual `POST /api/invoices/:id/email` resend all funnel through — used
//! to `Ledger::open(state.db_path)` a SECOND DuckDB instance. A separate
//! instance does not replay the live writer's WAL, so it read back the
//! last-checkpointed SUBSET of the file and reported:
//!
//! ```text
//! no InvoiceDraftCreated audit entry found for invoice id inv_… —
//! verify --id, --db, --tenant; …
//! ```
//!
//! …for an invoice that had just finalized and been NAV-acked. Operator
//! symptom: auto-email failed at compose, Újraküldés 404'd, the PDF route
//! 404'd, all on a live invoice. The same fresh-open pattern also covered the
//! per-invoice notes and the PR-73 bank snapshot, so even a render that DID
//! find its draft could have silently emitted a note-less, bank-less PDF.
//!
//! ## Why the assertion is shaped this way
//!
//! A plain "issue then render" assertion does NOT reliably fail on the broken
//! code: a co-resident fresh `Connection::open` sometimes REPLAYS the Handle's
//! WAL (folding it into the main file) and therefore sometimes sees the rows —
//! which is exactly why the 2026-07-20 run on the same DEV DB emailed fine and
//! the 2026-07-27 run did not. Nondeterminism is the defect, so pinning on the
//! *observable* read would be a test that can pass while the bug is present
//! (CLAUDE.md rule 9).
//!
//! The deterministic discriminator is the `SERVE_HANDLE_LIVE` tripwire
//! (`aberp_audit_ledger::serve_tripwire`, ADR-0099 H3 Addendum 3): with a serve
//! Handle REGISTERED for the tenant DB, ANY independent live open of that same
//! file panics in debug/test. Every `cargo test` build sets `debug_assertions`,
//! so arming it here makes the whole journey below a fork trace: on the pre-fix
//! code `render_to_bytes`'s `Ledger::open` panics the test; on the fixed code
//! every read rides `state.db.read()` and the journey completes.
//!
//! SMTP transport is deliberately out of scope (the operator's own SMTP test
//! passed on 2026-07-27; the failure was compose-side). The journey pins the
//! step that actually broke — rendering the PDF the email attaches — for the
//! first send, the resend, and the post-storno re-render.

use std::path::PathBuf;
use std::sync::Arc;

use aberp_audit_ledger::{Actor, BinaryHash, TenantId};
use aberp_billing::Currency;
use aberp_mnb_rates::{MnbError, MnbRate};
use time::Date;
use ulid::Ulid;

use aberp::issue_invoice::{AddressJson, CustomerJson, LineJson, SupplierJson};
use aberp::mnb_rates_provider::MnbRatesProvider;
use aberp::nav_xml::CustomerVatStatus;
use aberp::serve::{self, AppState, IssueInvoiceRequest, StornoInvoiceRequest};

const TEST_TENANT: &str = "serve_invoice_journey_test";

// ──────────────────────────────────────────────────────────────────────
// Fixtures — mirror `tests/serve_issue_route.rs` (duplicated per CLAUDE.md
// rule 3; extracting a shared dev-dep helper would widen the surface).
// ──────────────────────────────────────────────────────────────────────

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-invoice-journey")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn build_state(db_path: PathBuf) -> AppState {
    let tenant = TenantId::new(TEST_TENANT.to_string()).expect("tenant id");
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    let db = serve::open_tenant_handle(&db_path, tenant.clone())
        .expect("test: open shared aberp-db Handle");
    {
        let guard = db.write().expect("write guard to ensure audit schema");
        aberp_audit_ledger::ensure_schema(&guard).expect("ensure audit-ledger schema (test boot)");
    }
    AppState {
        db,
        db_path: Arc::new(db_path),
        tenant,
        nav_enabled: false,
        binary_hash: aberp::binary_hash::BinaryHashHandle::from_ready(binary_hash),
        session_token: Arc::new("test-token".to_string()),
        secrets_cache: aberp::secrets_cache::SecretsCache::empty(),
        nav_poll_semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
            serve::NAV_POLL_DAEMON_CONCURRENCY,
        )),
        boot_state: Arc::new(std::sync::RwLock::new(serve::ServeBootState::Ready {
            operator_login: "test-operator".to_string(),
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

fn fixture_supplier() -> SupplierJson {
    SupplierJson {
        tax_number: "12345678-1-42".to_string(),
        name: "ABERP Supplier Kft.".to_string(),
        address: AddressJson {
            country_code: "HU".to_string(),
            postal_code: "1011".to_string(),
            city: "Budapest".to_string(),
            street: "Fő utca 1.".to_string(),
        },
    }
}

/// DOMESTIC Hungarian buyer — the exact shape of the 2026-07-27 DEV invoice
/// (a NORMAL domestic invoice, not an EU/foreign partner).
fn fixture_customer() -> CustomerJson {
    CustomerJson {
        community_vat_number: None,
        vat_status: CustomerVatStatus::Domestic,
        partner_id: None,
        tax_number: "87654321-2-13".to_string(),
        name: "Vevő Kft.".to_string(),
        address: Some(AddressJson {
            country_code: "HU".to_string(),
            postal_code: "1052".to_string(),
            city: "Budapest".to_string(),
            street: "Váci utca 19.".to_string(),
        }),
    }
}

/// Carries a per-line note so the render exercises `load_invoice_notes` too —
/// the second of the three fresh opens the fix removed. A note-less fixture
/// would let a still-forked notes read pass unnoticed.
fn fixture_lines() -> Vec<LineJson> {
    vec![LineJson {
        description: "Widget A".to_string(),
        quantity: rust_decimal::Decimal::from(2),
        unit_price: 1000,
        vat_rate_percent: 27,
        vat_rate_kind: aberp_billing::VatRateKind::Percent,
        note: Some("soronkénti megjegyzés".to_string()),
        unit: None,
    }]
}

fn fixture_request() -> IssueInvoiceRequest {
    IssueInvoiceRequest {
        customer: fixture_customer(),
        lines: fixture_lines(),
        currency: Currency::Huf,
        series: None,
        bank_account_id: None,
        invoice_note: Some("számla szintű megjegyzés".to_string()),
        payment_deadline: None,
        delivery_date: None,
        delivery_date_override: None,
        // SMTP-free: the compose step under test is driven directly below.
        email_buyer_on_issue: Some(false),
        submit_to_nav_on_issue: Some(false),
        payment_method: aberp_billing::PaymentMethod::default(),
        email_recipient_override: None,
    }
}

fn write_fixture_seller_toml(home_dir: &std::path::Path) -> PathBuf {
    let tenant_dir = home_dir.join(".aberp").join(TEST_TENANT);
    std::fs::create_dir_all(&tenant_dir).expect("create tenant dir for seller.toml fixture");
    let body = r#"[seller]
legal_name = "ABERP Supplier Kft."
tax_number = "12345678-1-42"

[seller.address]
country_code = "HU"
postal_code = "1011"
city = "Budapest"
street = "Fő utca 1."
"#;
    let path = tenant_dir.join("seller.toml");
    std::fs::write(&path, body).expect("write seller.toml fixture");
    path
}

/// Drive the invoice to `Finalized` the way the real journey does — a NAV
/// submission attempt + response + a `SAVED` ack — WITHOUT touching NAV.
///
/// Appended through the shared Handle in ONE transaction (CLAUDE.md rule 15),
/// never through a fresh `Ledger::open`: this fixture runs with the tripwire
/// armed and must not itself be the fork the test is hunting.
fn finalize_via_handle(state: &AppState, actor: &Actor, invoice_id: &str) {
    use aberp_audit_ledger::{append_in_tx, EventKind, LedgerMeta};
    use aberp_billing::IdempotencyKey;

    let meta = LedgerMeta::new(state.tenant.clone(), BinaryHash::from_bytes([0u8; 32]));
    let idem = IdempotencyKey::new();
    let txid = "TESTTXID00000001";

    let mut guard = state.db.write().expect("write guard for NAV-ack fixture");
    let tx = guard.transaction().expect("begin NAV-ack fixture tx");
    for (kind, payload) in [
        (
            EventKind::InvoiceSubmissionAttempt,
            aberp::audit_payloads::InvoiceSubmissionAttemptPayload::new(
                invoice_id,
                idem,
                "test",
                b"<req/>".to_vec(),
            )
            .to_bytes(),
        ),
        (
            EventKind::InvoiceSubmissionResponse,
            aberp::audit_payloads::InvoiceSubmissionResponsePayload::new(
                invoice_id,
                idem,
                txid,
                b"<res/>".to_vec(),
            )
            .to_bytes(),
        ),
        (
            EventKind::InvoiceAckStatus,
            aberp::audit_payloads::InvoiceAckStatusPayload::new(
                invoice_id,
                txid,
                // The terminal NAV ack that derives `Finalized` — the exact
                // state Ervin's invoice #62 was in when cancel 404'd.
                "SAVED",
                b"<ack/>".to_vec(),
            )
            .to_bytes(),
        ),
    ] {
        append_in_tx(&tx, &meta, kind, payload, actor.clone(), None)
            .expect("append NAV-ack fixture entry");
    }
    tx.commit().expect("commit NAV-ack fixture tx");
}

struct UnreachableProvider;

#[async_trait::async_trait]
impl MnbRatesProvider for UnreachableProvider {
    async fn fetch_official_rate(
        &self,
        _currency: Currency,
        _date: Date,
    ) -> Result<MnbRate, MnbError> {
        unreachable!("UnreachableProvider must not be consulted — HUF path is rate-free")
    }
}

// ──────────────────────────────────────────────────────────────────────
// The journey
// ──────────────────────────────────────────────────────────────────────

/// issue (domestic HUF) → render the email PDF attachment → resend (render
/// again) → cancel (storno) → render the storno's PDF. Every step runs with a
/// LIVE registered serve Handle, so any step that re-opens the tenant DB
/// independently trips `SERVE_HANDLE_LIVE` and fails the test.
#[tokio::test(flavor = "current_thread")]
async fn issued_invoice_stays_findable_through_email_resend_and_storno() {
    let dir = test_dir("journey");
    std::env::set_var("HOME", &dir);
    let seller_toml = write_fixture_seller_toml(&dir);
    let db_path = dir.join("aberp.duckdb");
    let state = build_state(db_path.clone());

    // ARM the fork tripwire for the whole journey. serve does this at boot
    // behind `ABERP_SERVE_HANDLE_TRIPWIRE`; tests arm it directly so the
    // mechanism is proven regardless of the production arm.
    let _serve_live = aberp_audit_ledger::serve_tripwire::register_serve_handle(&db_path);
    assert!(
        aberp_audit_ledger::serve_tripwire::is_serve_handle_live(&db_path),
        "the tripwire must be armed — otherwise this test cannot see the fork \
         it exists to catch"
    );

    // ── 1. Issue a NORMAL domestic invoice.
    let actor = Actor::from_local_cli("journey-session".to_string(), "test-user");
    let actor_for_ack = actor.clone();
    let summary = serve::issue_invoice_request(
        &state,
        fixture_request(),
        fixture_supplier(),
        &UnreachableProvider,
        actor,
        None,
    )
    .await
    .expect("domestic HUF issuance must succeed");
    let invoice_id = summary.invoice_id.clone();

    // ── 2. Render the PDF the email attaches. THIS is the step that failed on
    //       DEV: `get_invoice_pdf` → `render_to_bytes` → find the invoice's
    //       `InvoiceDraftCreated`. `Ok(None)` is the 404 the operator saw.
    let rendered = serve::get_invoice_pdf(&state, &invoice_id, Some(&seller_toml))
        .expect("email-attachment render must not error")
        .unwrap_or_else(|| {
            panic!(
                "invoice {invoice_id} was JUST issued through the shared Handle but the \
                 email-attachment render cannot find it in the audit ledger — this is the \
                 2026-07-27 read-fork (compose/resend/PDF 404 on a live invoice)"
            )
        });
    assert_eq!(
        rendered.invoice_number, summary.invoice_number,
        "the rendered PDF must be THIS invoice, not a stale neighbour"
    );
    assert!(
        rendered.pdf_bytes.starts_with(b"%PDF"),
        "render must produce real PDF bytes"
    );

    // ── 3. Resend (Újraküldés). The manual `POST /api/invoices/:id/email`
    //       handler re-derives the number by re-rendering; a resend must be
    //       byte-identical to the first send's attachment.
    let resent = serve::get_invoice_pdf(&state, &invoice_id, Some(&seller_toml))
        .expect("resend render must not error")
        .expect("resend must still find the invoice in the audit ledger");
    assert_eq!(
        resent.pdf_bytes, rendered.pdf_bytes,
        "resend must attach the same PDF — a differing render means one of the two \
         reads saw a different DB state"
    );

    // ── 4. Cancel (storno). The operator's Sztornó button. Ervin's invoice was
    //       Finalized (NAV ack SAVED) when cancel 404'd, so take it there first.
    finalize_via_handle(&state, &actor_for_ack, &invoice_id);
    let storno = serve::storno_invoice_request(
        &state,
        &invoice_id,
        StornoInvoiceRequest {
            storno_reason: Some("téves kiállítás".to_string()),
            email_buyer_on_storno: Some(false),
            submit_to_nav_on_storno: Some(false),
        },
    )
    .unwrap_or_else(|e| {
        panic!("cancel (storno) of a live invoice must succeed, got: {e:?}");
    });

    // ── 5. The storno's OWN PDF must render — the storno tail emails the buyer
    //       the corrected paper trail, which re-enters the same render path for
    //       an invoice written moments ago.
    let storno_pdf = serve::get_invoice_pdf(&state, &storno.invoice_id, Some(&seller_toml))
        .expect("storno render must not error")
        .expect("the storno must be findable in the audit ledger immediately after issue");
    assert_eq!(
        storno_pdf.invoice_number, storno.invoice_number,
        "the storno PDF must be the storno's own document"
    );
    assert_ne!(
        storno_pdf.invoice_number, rendered.invoice_number,
        "the storno must carry its own number, not the base invoice's"
    );

    let _keep = &dir;
}
