//! D2a/D2b regression pins — the modification route's BASE-INVOICE reads
//! (2026-07-27, ADR-0099 H3 / CHECK N).
//!
//! ## The defect these pin
//!
//! `serve::modification_invoice_request` reached two fresh
//! `Connection::open(&state.db_path)` openers while serve held the shared
//! `aberp_db::Handle`:
//!
//! * `read_base_line_vat_kinds` (D2a) — the ADR-0101 S2 guard's input, and
//! * `read_base_currency` (D2b) — the ADR-0037 §4 C6 invariant's input.
//!
//! Under H3 runtime checkpointing is DISABLED, so a base invoice issued since
//! serve boot lives in the shared Handle's WAL. A second DuckDB instance does
//! not replay that WAL, so it reads the last-checkpointed SUBSET of the file.
//!
//! D2b fails LOUDLY on that (its `query_row` errors when the row is missing).
//! D2a did not. It returned `Ok(vec![])` via `.unwrap_or_default()`, and the
//! guard immediately downstream is
//!
//! ```text
//! if let Some(kind) = base_vat_kinds.iter().copied().find(|k| !k.is_percent()) { reject }
//! ```
//!
//! which over an EMPTY vector passes VACUOUSLY. Step 1's precondition
//! (`derive_state_for`) IS Handle-routed, so it finds the base and opens the
//! door; the forked read then failed to shut the gate. Net effect on real ÁFA:
//! modifying an AAM / domestic-reverse-charge / intra-Community base through
//! the SPA re-filed it to NAV as plain `<vatPercentage>0.00</…>`, silently
//! dropping the exemption / self-assessment (CLAUDE.md rule 11, worst class).
//!
//! ## Why the pins are shaped this way
//!
//! The existing `tests/serve_modification_route.rs` cannot see any of this: its
//! `issue_and_finalize_base` deliberately DROPS the seeding Handle before
//! `build_state` opens the AppState's own one, so nothing is ever WAL-resident
//! behind a live writer and every fresh open is coherent. Both tests here keep
//! ONE Handle live across issuance AND modification — the real serve shape —
//! and arm the `SERVE_HANDLE_LIVE` tripwire so any step that opens the tenant
//! DB through a hooked seam (`Ledger::open`, `DuckDbBillingStore::open`) fails
//! loudly instead of silently reading stale.
//!
//! The tripwire alone is NOT enough for D2a, and that shapes the second test.
//! It hooks those two named seams; the D2a fork was a BARE `Connection::open`
//! in serve.rs, which no seam covers. And the stale read itself is
//! nondeterministic — a co-resident fresh open SOMETIMES replays the Handle's
//! WAL and therefore sometimes sees the rows, which is exactly why this class
//! reaches production intermittently. Pinning on the observable stale read
//! would be a test that can pass while the bug is present (CLAUDE.md rule 9).
//!
//! So the deterministic pin is on the INVARIANT rather than on the cause:
//! `modification_must_block_when_the_base_vat_kinds_read_comes_back_empty`
//! reproduces the torn read's exact observable shape — base `invoice` row
//! present (so the currency check passes), `invoice_line` rows unreadable — by
//! deleting the lines through the shared Handle, and asserts the route BLOCKS.
//! Pre-fix that call returns `Ok(summary)`: the guard passes vacuously and a
//! modification is really issued. That is the mis-filing, reproduced.
//!
//! Routing the read through the Handle removes today's CAUSE; erroring on an
//! empty result removes the CLASS. The pins cover both halves.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aberp_audit_ledger::{Actor, BinaryHash, TenantId};
use aberp_billing::{Currency, VatRateKind};
use aberp_mnb_rates::{MnbError, MnbRate};
use time::Date;
use ulid::Ulid;

use aberp::issue_invoice::{AddressJson, CustomerJson, LineJson, SupplierJson};
use aberp::mnb_rates_provider::MnbRatesProvider;
use aberp::nav_xml::CustomerVatStatus;
use aberp::serve::{
    self, AppState, IssueInvoiceRequest, ModificationInvoiceRequest, ModificationRouteError,
};

const TEST_TENANT: &str = "serve_modification_base_read_test";

// ──────────────────────────────────────────────────────────────────────
// Fixtures — mirror `tests/serve_invoice_journey_handle_coherence.rs`
// (duplicated per CLAUDE.md rule 3; extracting a shared dev-dep helper would
// widen the surface beyond this fix).
// ──────────────────────────────────────────────────────────────────────

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-modification-base-read")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// ONE process-wide fixture HOME for the whole binary.
///
/// `supplier_from_seller_toml` (modification step 5) reads
/// `$HOME/.aberp/<tenant>/seller.toml`, and `set_var("HOME", …)` is
/// process-global — two tests in this binary racing on it would flake. The
/// `OnceLock` sets it exactly once, before either test can read it, and NEVER
/// points at the operator's real `~/.aberp`.
fn fixture_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = test_dir("home");
        let tenant_dir = home.join(".aberp").join(TEST_TENANT);
        std::fs::create_dir_all(&tenant_dir).expect("create tenant dir for seller.toml fixture");
        std::fs::write(
            tenant_dir.join("seller.toml"),
            r#"[seller]
legal_name = "ABERP Supplier Kft."
tax_number = "12345678-1-42"

[seller.address]
country_code = "HU"
postal_code = "1011"
city = "Budapest"
street = "Fő utca 1."
"#,
        )
        .expect("write seller.toml fixture");
        std::env::set_var("HOME", &home);
        home
    })
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
        // NAV stays OFF: these pins are about what the route DECIDES, and a
        // pre-fix run must not be able to reach the real filing surface.
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

/// HUF throughout — the vat-kind guard is currency-independent and HUF keeps
/// the MNB rate provider unreachable.
fn fixture_issue_request(kind: VatRateKind, rate_percent: u16) -> IssueInvoiceRequest {
    IssueInvoiceRequest {
        customer: fixture_customer(),
        lines: vec![LineJson {
            description: "Base line".to_string(),
            quantity: rust_decimal::Decimal::from(1),
            unit_price: 1000,
            vat_rate_percent: rate_percent,
            vat_rate_kind: kind,
            note: None,
            unit: None,
        }],
        currency: Currency::Huf,
        series: None,
        bank_account_id: None,
        invoice_note: None,
        payment_deadline: None,
        delivery_date: None,
        delivery_date_override: None,
        email_buyer_on_issue: Some(false),
        submit_to_nav_on_issue: Some(false),
        payment_method: aberp_billing::PaymentMethod::default(),
        email_recipient_override: None,
    }
}

/// The SPA's modification body: currency MATCHES the base (HUF) and every line
/// carries the `#[serde(default)]` `Percent` kind that `composeModificationBody`
/// leaves behind. This is the body that re-files a 0% wire.
fn fixture_modification_body() -> ModificationInvoiceRequest {
    ModificationInvoiceRequest {
        customer: fixture_customer(),
        lines: vec![LineJson {
            description: "Corrected line".to_string(),
            quantity: rust_decimal::Decimal::from(2),
            unit_price: 1500,
            vat_rate_percent: 0,
            vat_rate_kind: VatRateKind::Percent,
            note: None,
            unit: None,
        }],
        currency: Currency::Huf,
        modification_date: "2026-05-24".to_string(),
        series: None,
        email_buyer_on_modification: Some(false),
        submit_to_nav_on_modification: Some(false),
        email_recipient_override: None,
    }
}

/// Drive the base to `Finalized` (attempt / response / SAVED ack) so the
/// modification precondition passes.
///
/// Appended through the shared Handle in ONE transaction (CLAUDE.md rule 15),
/// never through a fresh `Ledger::open`: this fixture runs with the tripwire
/// armed and must not itself be the fork the test is hunting.
fn finalize_via_handle(state: &AppState, actor: &Actor, invoice_id: &str) {
    use aberp_audit_ledger::{append_in_tx, EventKind, LedgerMeta};
    use aberp_billing::IdempotencyKey;

    let meta = LedgerMeta::new(state.tenant.clone(), BinaryHash::from_bytes([0u8; 32]));
    let idem = IdempotencyKey::new();
    let txid = "TESTTXID00000002";

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

/// Issue `kind` at `rate_percent`% and drive it to `Finalized`, all on the
/// AppState's OWN live Handle. Returns the base invoice id.
async fn issue_and_finalize_on_live_handle(
    state: &AppState,
    kind: VatRateKind,
    rate_percent: u16,
) -> String {
    let actor = Actor::from_local_cli("modbase-session".to_string(), "test-user");
    let summary = serve::issue_invoice_request(
        state,
        fixture_issue_request(kind, rate_percent),
        fixture_supplier(),
        &UnreachableProvider,
        actor.clone(),
        None,
    )
    .await
    .expect("HUF base issuance must succeed");
    finalize_via_handle(state, &actor, &summary.invoice_id);
    summary.invoice_id
}

// ──────────────────────────────────────────────────────────────────────
// The pins
// ──────────────────────────────────────────────────────────────────────

/// D2a coherence journey — issue a DOMESTIC REVERSE-CHARGE base and modify it
/// WITHOUT ever dropping the writer's Handle, the real serve shape.
///
/// The ADR-0101 S2 guard must still see the base's non-`Percent` kind and
/// reject. Its input now rides `state.db.read()` (a `try_clone` of the ONE
/// instance), so it sees the WAL-resident `invoice_line` rows the pre-fix
/// second DuckDB instance could miss.
///
/// This is the CAUSE-side pin. It is not the deterministic discriminator — a
/// co-resident fresh open sometimes replays the WAL and passes anyway; that
/// nondeterminism IS the defect. The pin below is the deterministic one.
#[tokio::test(flavor = "current_thread")]
async fn reverse_charge_base_is_still_rejected_with_the_writer_handle_live() {
    let _home = fixture_home();
    let dir = test_dir("reverse-charge-live-handle");
    let db_path = dir.join("aberp.duckdb");
    let state = build_state(db_path.clone());

    let _serve_live = aberp_audit_ledger::serve_tripwire::register_serve_handle(&db_path);
    assert!(
        aberp_audit_ledger::serve_tripwire::is_serve_handle_live(&db_path),
        "the tripwire must be armed — otherwise this test cannot see a hooked fork"
    );

    // Wired non-`Percent` kinds require a 0% line (ADR-0101 §4 preflight).
    let base_invoice_id =
        issue_and_finalize_on_live_handle(&state, VatRateKind::DomesticReverseCharge, 0).await;

    let err =
        serve::modification_invoice_request(&state, &base_invoice_id, fixture_modification_body())
            .expect_err(
            "modifying a domestic-reverse-charge base must be rejected — proceeding would re-file \
         it to NAV as plain 0% VAT and drop the self-assessment",
        );
    match err {
        ModificationRouteError::BadRequest(message) => {
            assert!(
                message.contains(VatRateKind::DomesticReverseCharge.as_str()),
                "the guard must name the base kind it read off the live Handle, got: {message}"
            );
            assert!(
                message.contains("aberp issue-modification"),
                "the guard must steer to the CLI fallback, got: {message}"
            );
        }
        other => panic!(
            "expected the ADR-0101 S2 BadRequest; got {other:?}. An `Other` here means the base \
             read failed outright; an `Ok` means the guard passed over a reverse-charge base"
        ),
    }
    let _keep = &dir;
}

/// D2a CLASS pin, and the deterministic RED-then-green discriminator.
///
/// Reproduces the torn read's exact observable shape: the base `invoice` row is
/// present (so `read_base_currency`'s C6 check passes and cannot mask the
/// result) while its `invoice_line` rows are unreadable, so
/// `read_base_line_vat_kinds` sees NOTHING. The lines are deleted through the
/// shared Handle rather than left to WAL timing, because the stale read itself
/// is nondeterministic and a test that only sometimes fails is a test that can
/// pass while the bug is present (CLAUDE.md rule 9).
///
/// PRE-FIX this call returns `Ok(summary)`: `.unwrap_or_default()` hands the
/// guard an empty vector, `.find(|k| !k.is_percent())` passes vacuously over
/// it, and a modification is really issued off an all-`Percent` 0% body — the
/// mis-filing. POST-FIX an empty result is a hard error, so the route blocks.
///
/// This is a TRUST-THE-CODE invariant, deliberately independent of the Handle
/// routing that fixes today's cause: whatever future reason a base-kinds read
/// has for coming back empty, "empty" must never be read as "no non-percent
/// kinds → safe to re-file at 0%".
#[tokio::test(flavor = "current_thread")]
async fn modification_must_block_when_the_base_vat_kinds_read_comes_back_empty() {
    let _home = fixture_home();
    let dir = test_dir("empty-base-vat-kinds");
    let db_path = dir.join("aberp.duckdb");
    let state = build_state(db_path.clone());

    let _serve_live = aberp_audit_ledger::serve_tripwire::register_serve_handle(&db_path);

    // A plain 27% `Percent` base: the guard has NOTHING legitimate to reject,
    // so the only thing that can block the modification is the empty-read
    // check. Were the base non-`Percent`, a green result would not distinguish
    // "blocked because the kinds were unreadable" from "blocked because a kind
    // was read and it was exempt".
    let base_invoice_id = issue_and_finalize_on_live_handle(&state, VatRateKind::Percent, 27).await;

    // Make the base's kinds unreadable, leaving the `invoice` row intact.
    {
        let guard = state
            .db
            .write()
            .expect("write guard to blind the base vat_rate_kind read");
        guard
            .execute(
                "DELETE FROM invoice_line WHERE invoice_id = ?;",
                duckdb::params![&base_invoice_id],
            )
            .expect("delete base invoice_line rows");
    }

    let err = serve::modification_invoice_request(
        &state,
        &base_invoice_id,
        fixture_modification_body(),
    )
    .map(|summary| summary.invoice_number.clone())
    .expect_err(
        "an unreadable base-VAT-kinds result must BLOCK the modification. Returning Ok means the \
         ADR-0101 S2 guard passed VACUOUSLY over an empty vector and a modification was really \
         issued off an all-`Percent` 0% body — the silent NAV mis-filing this pin exists for",
    );
    match err {
        ModificationRouteError::Other(e) => {
            let message = format!("{e:#}");
            assert!(
                message.contains("cannot establish the VAT rate-kinds"),
                "the block must name WHY it refused, not fail incidentally further down the \
                 route; got: {message}"
            );
            assert!(
                message.contains(&base_invoice_id),
                "the block must name the base invoice so the operator can act, got: {message}"
            );
        }
        other => panic!(
            "expected the loud unreadable-base block; got {other:?}. A `BadRequest` here means \
             something else rejected first and this pin is no longer discriminating"
        ),
    }
    let _keep = &dir;
}
