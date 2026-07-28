//! D5 regression pin — the EMAIL ATTACHMENT render must ride the shared
//! Handle (2026-07-28, ADR-0099 H3 / CHECK N).
//!
//! ## The defect this pins
//!
//! PR #40 moved `serve::get_invoice_pdf` onto the shared `aberp_db::Handle`
//! and its finding doc recorded that "the PDF route, the auto-email compose
//! path and the manual resend handler all funnel through" it. The auto-email
//! compose path does NOT. `serve::send_invoice_email_route` — reached by BOTH
//! auto-send-on-issue (`SendTrigger::AutoOnIssue`) and the manual
//! `POST /api/invoices/:id/email` resend — called
//! `email_invoice::send_invoice_email`, which rendered the attachment ITSELF
//! from a `db_path` through the PATH-TAKING `print_invoice::render_to_bytes`.
//!
//! Post-#40 that function opens its own `aberp_db::Handle` — a SECOND DuckDB
//! instance co-resident with serve's. Under H3 runtime checkpointing is
//! disabled, so an invoice issued in THIS serve process is WAL-resident and
//! the second instance reads the last-checkpointed SUBSET. That is precisely
//! the first row of the 2026-07-27 DEV symptom table:
//!
//! ```text
//! auto-email on issue | compose failure: `render printed PDF for SMTP email
//!                       attachment: no InvoiceDraftCreated audit entry found
//!                       for invoice id inv_01KYJB52…`
//! ```
//!
//! `render printed PDF for SMTP email attachment` is `email_invoice`'s OWN
//! context string — the operator's auto-email failure came through the render
//! call PR #40 did not touch, not through `get_invoice_pdf`.
//!
//! ## Why the pin is shaped this way
//!
//! The stale read itself is NONDETERMINISTIC — a co-resident fresh open
//! sometimes replays the Handle's WAL (the reasoning `serve_modification_base_
//! read_coherence.rs` sets out). A test asserting "the attachment rendered"
//! could therefore pass while the fork is present (CLAUDE.md rule 9). So the
//! pin is on the CAUSE, made deterministic: `Handle::open` now calls
//! `assert_no_serve_handle`, so ANY second Handle on a serve-live tenant DB
//! panics in debug/test regardless of what the stale read happens to return.
//! With the tripwire armed, the pre-fix route panics inside
//! `render_to_bytes`; the fixed route renders through `state.db.read()` and
//! reaches the SMTP transport.
//!
//! The fix removes the capability rather than re-routing it (rule 12):
//! `SendInvoiceEmailInput` carries `pdf_bytes`, not a `db_path`, so
//! `email_invoice` can no longer open the DB at all.

use std::path::PathBuf;
use std::sync::Arc;

use aberp_audit_ledger::{Actor, BinaryHash, TenantId};
use aberp_billing::Currency;
use aberp_mnb_rates::{MnbError, MnbRate};
use time::Date;
use ulid::Ulid;

use aberp::email_invoice::SendTrigger;
use aberp::issue_invoice::{AddressJson, CustomerJson, LineJson, SupplierJson};
use aberp::mnb_rates_provider::MnbRatesProvider;
use aberp::nav_xml::CustomerVatStatus;
use aberp::serve::{self, AppState, IssueInvoiceRequest};

const TEST_TENANT: &str = "serve_email_attachment_test";

// ──────────────────────────────────────────────────────────────────────
// Fixtures — mirror `tests/serve_invoice_journey_handle_coherence.rs`
// (duplicated per CLAUDE.md rule 3).
// ──────────────────────────────────────────────────────────────────────

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-email-attachment")
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

fn fixture_request() -> IssueInvoiceRequest {
    IssueInvoiceRequest {
        customer: CustomerJson {
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
        },
        // Carries a per-line note + an invoice-level note so the render
        // exercises `load_invoice_notes` as well as the ledger lookup.
        lines: vec![LineJson {
            description: "Widget A".to_string(),
            quantity: rust_decimal::Decimal::from(2),
            unit_price: 1000,
            vat_rate_percent: 27,
            vat_rate_kind: aberp_billing::VatRateKind::Percent,
            note: Some("soronkénti megjegyzés".to_string()),
            unit: None,
        }],
        currency: Currency::Huf,
        series: None,
        bank_account_id: None,
        invoice_note: Some("számla szintű megjegyzés".to_string()),
        payment_deadline: None,
        delivery_date: None,
        delivery_date_override: None,
        // The auto-send tail is driven EXPLICITLY below so the assertion sits
        // on the route's own return value rather than on a fire-and-forget.
        email_buyer_on_issue: Some(false),
        submit_to_nav_on_issue: Some(false),
        payment_method: aberp_billing::PaymentMethod::default(),
        // LOAD-BEARING. `resolve_recipient_email`'s first rung is the
        // per-invoice override, and there is no `partners` row for this buyer.
        // Without it the route refuses at the wrong-recipient guard BEFORE the
        // render, and the pin below would pass vacuously (rule 9) — which is
        // what `a_missing_recipient_short_circuits_before_the_render` bounds.
        email_recipient_override: Some("buyer@example.invalid".to_string()),
    }
}

/// seller.toml WITH a `[seller.smtp]` section — the send must get PAST compose
/// and reach the transport. The host is a closed local port so the transport
/// fails immediately without touching the network.
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

[seller.smtp]
host = "127.0.0.1"
port = 1
username = "invoices@example.invalid"
from_address = "invoices@example.invalid"
security = "StartTls"
attach_xml = false
"#;
    let path = tenant_dir.join("seller.toml");
    std::fs::write(&path, body).expect("write seller.toml fixture");
    path
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

/// Issue an invoice through the shared Handle, then run the AUTO-SEND-ON-ISSUE
/// email route against it with the `SERVE_HANDLE_LIVE` tripwire armed.
///
/// The attachment render must ride the shared Handle. Pre-fix the route
/// reached `print_invoice::render_to_bytes`, whose own `Handle::open_default`
/// trips the tripwire; the send also could not have found the WAL-resident
/// invoice. Post-fix the render happens in `send_invoice_email_route` off
/// `state.db.read()` and the send fails at the SMTP TRANSPORT instead — which
/// is what this asserts, because a `compose` failure is exactly the DEV
/// symptom.
#[tokio::test(flavor = "current_thread")]
async fn auto_email_attachment_renders_off_the_shared_handle() {
    let dir = test_dir("auto-send");
    std::env::set_var("HOME", &dir);
    let _seller_toml = write_fixture_seller_toml(&dir);
    let db_path = dir.join("aberp.duckdb");
    let state = build_state(db_path.clone());

    // Seed the SMTP password so `load_smtp_credentials` succeeds and the route
    // actually reaches the render + transport (an unset password would short-
    // circuit before either, and the pin would pass vacuously).
    state
        .secrets_cache
        .refresh_smtp_password_after_write(zeroize::Zeroizing::new("s3cret".to_string()));

    // ARM the fork tripwire for the whole route call.
    let _serve_live = aberp_audit_ledger::serve_tripwire::register_serve_handle(&db_path);
    assert!(
        aberp_audit_ledger::serve_tripwire::is_serve_handle_live(&db_path),
        "the tripwire must be armed — otherwise this test cannot see the fork it \
         exists to catch"
    );

    let actor = Actor::from_local_cli("email-session".to_string(), "test-user");
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

    // The auto-send tail, verbatim: this is what `auto_send_after_issue` calls.
    let body = serve::send_invoice_email_route(
        &state,
        &summary.invoice_id,
        &summary.invoice_number,
        None,
        "87654321-2-13",
        "test-operator",
        SendTrigger::AutoOnIssue,
    )
    .await;

    // The send cannot SUCCEED (there is no SMTP server on 127.0.0.1:1) — but it
    // must get all the way to the TRANSPORT, which means the attachment
    // rendered. Asserted POSITIVELY: `!= "compose"` alone would also hold for
    // every early return above the render (`recipient_rejected`, `auth`), so it
    // could pass while the render never ran at all.
    assert_eq!(
        body.error_class.as_deref(),
        Some("transport"),
        "the auto-send must reach the SMTP transport for an invoice issued in this \
         very serve process. `compose` is the 2026-07-27 DEV symptom — the render \
         forking the DB instead of riding the shared Handle. `recipient_rejected` / \
         `auth` mean the route returned BEFORE the render and this pin proved \
         nothing. detail: {:?}",
        body.error_detail
    );
    assert_eq!(
        body.outcome, "failed",
        "there is no SMTP server on 127.0.0.1:1 — the send must fail at transport, \
         not report success. detail: {:?}",
        body.error_detail
    );

    // ── BOUND on the assertion above. The route refuses BEFORE the render when
    //    no buyer email resolves, so a fixture that silently lost its recipient
    //    would make the pin prove nothing about the render. Establish here, in
    //    THIS process (the `HOME` env var is process-global, so this cannot be a
    //    second `#[test]` — the two would race on it), that the early-return
    //    path is real and carries a DIFFERENT class than the one asserted.
    let no_recipient = serve::send_invoice_email_route(
        &state,
        "inv_does_not_exist",
        "TEST-INV/00001",
        None,
        "00000000-0-00",
        "test-operator",
        SendTrigger::Manual,
    )
    .await;
    assert_eq!(
        no_recipient.error_class.as_deref(),
        Some("recipient_rejected"),
        "an invoice with no resolvable buyer email must be refused BEFORE the \
         render (ADR-0047 §3 wrong-recipient guard) — that is the vacuous-pass \
         path the `transport` assertion above excludes; got {:?} / {:?}",
        no_recipient.error_class,
        no_recipient.error_detail
    );

    let _keep = &dir;
}
