//! Integration tests for `/api/partners` CRUD (PR-48α / session-68).
//!
//! Six pin tests on the library-helper boundary (mirrors the WORKING
//! `serve_issue_route.rs` posture per A159 / A162 / A163):
//!
//! 1. **create happy path** — valid `PartnerInputs` rounds-trips through
//!    `create_partner_request` and returns a Partner with a
//!    server-minted `prt_<ULID>` id + Rfc3339 timestamps.
//! 2. **create validation failure** — empty `display_name` + bad
//!    `tax_number` surfaces as `PartnerRouteError::Validation` with
//!    structured per-field errors. The route handler maps this to 400
//!    with the `validation_failed` body shape.
//! 3. **list** — two creates + a list returns both partners ordered
//!    by `display_name` ASC.
//! 4. **get-by-id** — fetch the created partner; all fields round-trip.
//! 5. **update** — mutate one field; re-fetch sees the new value and a
//!    bumped `updated_at`.
//! 6. **soft-delete + 404-after-delete** — delete returns Ok; a
//!    subsequent `get_partner_request` surfaces `NotFound`; a list
//!    omits the soft-deleted row.
//!
//! All tests run against an in-process DuckDB file under a per-test
//! scratch directory; the HTTPS listener is not spun. The full HTTP
//! status-code mapping (400 / 404 / 200 / 204) is structural — axum's
//! `(Status, Json(...)).into_response()` builds the response from the
//! typed value; pinning the response bytes themselves would couple the
//! test to axum's private response shape per CLAUDE.md rule 2.

use std::path::PathBuf;
use std::sync::Arc;

use aberp_audit_ledger::{BinaryHash, TenantId};
use ulid::Ulid;

use aberp::nav_xml::CustomerVatStatus;
use aberp::partners::{CustomerType, PartnerInputs, PartnerKind};
use aberp::serve::{self, AppState, PartnerRouteError};

const TEST_TENANT: &str = "serve_partners_route_test";

// ──────────────────────────────────────────────────────────────────────
// Fixtures
// ──────────────────────────────────────────────────────────────────────

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-serve-partners")
        .join(format!("{}-{}", label, Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn build_state(db_path: PathBuf) -> AppState {
    let tenant = TenantId::new(TEST_TENANT.to_string()).expect("tenant id");
    let binary_hash = BinaryHash::from_bytes([0u8; 32]);
    AppState {
        db: aberp::serve::open_tenant_handle(&db_path, tenant.clone())
            .expect("test: open shared aberp-db Handle"),
        db_path: Arc::new(db_path),
        tenant,
        nav_enabled: true,
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

fn minimal_valid_inputs(display: &str) -> PartnerInputs {
    PartnerInputs {
        display_name: display.to_string(),
        legal_name: format!("{} Kft.", display),
        kind: PartnerKind::Customer,
        // PR-97 / ADR-0048 — preserve pre-PR-97 implicit Domestic
        customer_vat_status: CustomerVatStatus::Domestic,
        customer_type: CustomerType::Unset,
        tax_number: Some("12345678-1-42".to_string()),
        eu_vat_number: Some("HU12345678".to_string()),
        address_street: Some("Fő utca 1.".to_string()),
        address_postal_code: Some("1011".to_string()),
        address_city: Some("Budapest".to_string()),
        address_country: Some("Magyarország".to_string()),
        bank_account: None,
        contact_email: Some("ops@example.hu".to_string()),
        contact_phone: None,
    }
}

// ──────────────────────────────────────────────────────────────────────
// Pin tests
// ──────────────────────────────────────────────────────────────────────

/// Pin #1 — create happy path. The route's library helper returns a
/// fully-populated Partner with server-minted `id` (prefixed `prt_`),
/// matching `display_name`/`legal_name`/`kind`/`tax_number`, and
/// Rfc3339 timestamps where `created_at == updated_at` and
/// `deleted_at IS None`.
#[test]
fn partners_create_happy_path_returns_populated_partner() {
    let dir = test_dir("create-happy");
    let state = build_state(dir.join("aberp.duckdb"));
    let inputs = minimal_valid_inputs("BSCE");

    let partner =
        serve::create_partner_request(&state, &inputs).expect("create happy path must succeed");

    assert!(
        partner.id.starts_with("prt_"),
        "partner id `{}` must be prefixed-ULID",
        partner.id
    );
    assert_eq!(partner.id.len(), 30, "prefixed PartnerId must be 30 chars");
    assert_eq!(partner.display_name, "BSCE");
    assert_eq!(partner.legal_name, "BSCE Kft.");
    assert_eq!(partner.kind, PartnerKind::Customer);
    assert_eq!(partner.tax_number.as_deref(), Some("12345678-1-42"));
    assert_eq!(partner.eu_vat_number.as_deref(), Some("HU12345678"));
    assert_eq!(partner.address_city.as_deref(), Some("Budapest"));
    assert_eq!(
        partner.created_at, partner.updated_at,
        "on create, created_at must equal updated_at"
    );
    assert!(
        partner.deleted_at.is_none(),
        "freshly-created partner must have NULL deleted_at"
    );

    let _keep = &dir;
}

/// Pin #2 — create validation failure. An empty `display_name` and a
/// malformed `tax_number` surface as `PartnerRouteError::Validation`
/// with structured per-field errors. The HTTP handler maps this to
/// 400 with the `validation_failed` envelope; the library boundary is
/// the load-bearing pin.
#[test]
fn partners_create_rejects_invalid_inputs_with_structured_errors() {
    let dir = test_dir("create-invalid");
    let state = build_state(dir.join("aberp.duckdb"));
    let inputs = PartnerInputs {
        display_name: "   ".to_string(),
        legal_name: "Valid Legal Kft.".to_string(),
        kind: PartnerKind::Both,
        // PR-97 / ADR-0048 — preserve pre-PR-97 implicit Domestic
        customer_vat_status: CustomerVatStatus::Domestic,
        customer_type: CustomerType::Unset,
        tax_number: Some("not-a-tax-number".to_string()),
        eu_vat_number: None,
        address_street: None,
        address_postal_code: None,
        address_city: None,
        address_country: None,
        bank_account: None,
        contact_email: None,
        contact_phone: None,
    };

    let err =
        serve::create_partner_request(&state, &inputs).expect_err("invalid inputs must reject");
    let errors = match err {
        PartnerRouteError::Validation(v) => v,
        other => panic!("expected Validation, got {other:?}"),
    };
    let flagged_fields: Vec<&str> = errors.iter().map(|e| e.field).collect();
    assert!(
        flagged_fields.contains(&"display_name"),
        "must flag display_name; got {:?}",
        flagged_fields
    );
    assert!(
        flagged_fields.contains(&"tax_number"),
        "must flag tax_number; got {:?}",
        flagged_fields
    );

    let _keep = &dir;
}

/// Pin #3 — list returns every active partner ordered by
/// `display_name` ASC. Two creates + one list call must return both
/// in alphabetical order.
#[test]
fn partners_list_returns_active_rows_ordered_by_display_name() {
    let dir = test_dir("list");
    let state = build_state(dir.join("aberp.duckdb"));

    serve::create_partner_request(&state, &minimal_valid_inputs("Zeta")).expect("create Zeta");
    serve::create_partner_request(&state, &minimal_valid_inputs("Alpha")).expect("create Alpha");

    let listed = serve::list_partners_request(&state, None).expect("list must succeed");
    assert_eq!(listed.len(), 2, "list must return both created partners");
    assert_eq!(
        listed[0].display_name, "Alpha",
        "list must order by display_name ASC"
    );
    assert_eq!(listed[1].display_name, "Zeta");

    // ?search=al filters case-insensitive prefix on display_name OR
    // legal_name. "Alpha" matches; "Zeta" does not.
    let filtered = serve::list_partners_request(&state, Some("al")).expect("search must succeed");
    assert_eq!(filtered.len(), 1, "search=al must match Alpha only");
    assert_eq!(filtered[0].display_name, "Alpha");

    let _keep = &dir;
}

/// Pin #4 — get-by-id round-trip. Every field set at create time
/// survives the SELECT path; missing optional fields stay `None`.
#[test]
fn partners_get_by_id_round_trips_every_field() {
    let dir = test_dir("get");
    let state = build_state(dir.join("aberp.duckdb"));
    let created = serve::create_partner_request(&state, &minimal_valid_inputs("Test"))
        .expect("create must succeed");

    let fetched = serve::get_partner_request(&state, &created.id).expect("get must succeed");
    assert_eq!(
        fetched, created,
        "get must return the exact Partner stored at create"
    );

    // Unknown id surfaces as NotFound (404 at the HTTP layer).
    let unknown_id = format!("prt_{}", Ulid::new());
    match serve::get_partner_request(&state, &unknown_id) {
        Err(PartnerRouteError::NotFound) => {}
        other => panic!("expected NotFound for unknown id, got {other:?}"),
    }

    let _keep = &dir;
}

/// Pin #5 — update bumps `updated_at` and persists the mutated field.
/// The original `created_at` must stay unchanged across the update
/// (only `updated_at` advances).
#[test]
fn partners_update_persists_mutated_field_and_bumps_updated_at() {
    let dir = test_dir("update");
    let state = build_state(dir.join("aberp.duckdb"));
    let created =
        serve::create_partner_request(&state, &minimal_valid_inputs("Original")).expect("create");

    // Sleep a millisecond so the formatted Rfc3339 string definitely
    // advances. Without this the test can race the same-instant case
    // and `assert_ne!` on the updated_at strings would flake.
    std::thread::sleep(std::time::Duration::from_millis(2));

    let mutated_inputs = PartnerInputs {
        display_name: "Renamed".to_string(),
        ..minimal_valid_inputs("Original")
    };
    let updated = serve::update_partner_request(
        &state,
        &created.id,
        &mutated_inputs,
        "test-operator",
        BinaryHash::from_bytes([0u8; 32]),
    )
    .expect("update must succeed");

    assert_eq!(updated.id, created.id, "id must stay stable across update");
    assert_eq!(updated.display_name, "Renamed", "mutation must persist");
    assert_eq!(
        updated.created_at, created.created_at,
        "created_at must stay stable across update"
    );
    assert_ne!(
        updated.updated_at, created.updated_at,
        "updated_at must advance"
    );

    // Update on unknown id surfaces as NotFound (404 at HTTP).
    let unknown_id = format!("prt_{}", Ulid::new());
    match serve::update_partner_request(
        &state,
        &unknown_id,
        &mutated_inputs,
        "test-operator",
        BinaryHash::from_bytes([0u8; 32]),
    ) {
        Err(PartnerRouteError::NotFound) => {}
        other => panic!("expected NotFound for unknown id, got {other:?}"),
    }

    let _keep = &dir;
}

/// Pin #6 — soft-delete + 404-after-delete. The row stays in the DB
/// (historical-invoice lookups can still resolve it), but the API
/// surface treats it as gone: get returns 404, list omits it.
#[test]
fn partners_soft_delete_makes_partner_invisible_to_api() {
    let dir = test_dir("delete");
    let state = build_state(dir.join("aberp.duckdb"));
    let created =
        serve::create_partner_request(&state, &minimal_valid_inputs("ToDelete")).expect("create");

    serve::delete_partner_request(&state, &created.id).expect("delete must succeed");

    // Get surfaces NotFound now (HTTP 404).
    match serve::get_partner_request(&state, &created.id) {
        Err(PartnerRouteError::NotFound) => {}
        other => panic!("expected NotFound after soft-delete, got {other:?}"),
    }

    // List omits the soft-deleted row.
    let listed = serve::list_partners_request(&state, None).expect("list");
    assert!(
        listed.is_empty(),
        "soft-deleted partner must not appear in list; got {:?}",
        listed
    );

    // Re-deleting the same id surfaces NotFound — defence against
    // a double-click DELETE re-issuing the request and the SPA
    // misreading a second 204 as "another partner deleted." Pinning
    // the second-call surface so an ill-considered refactor that
    // makes the soft-delete idempotent (returning Ok(()) the second
    // time) trips this test.
    match serve::delete_partner_request(&state, &created.id) {
        Err(PartnerRouteError::NotFound) => {}
        other => panic!("expected NotFound on re-delete, got {other:?}"),
    }

    let _keep = &dir;
}

// ──────────────────────────────────────────────────────────────────────
// Pin #7 — foreign-partner save regression (operator-reported, DEV,
// 2026-07-27). Route-level twin of
// `partners::tests::validate_partner_inputs_other_saves_without_eu_vat`:
// the unit pin covers the gate, this one covers the surface the operator
// actually hits (`POST /api/partners` → `create_partner_request`) and
// asserts the row is READABLE BACK, not merely accepted.
// ──────────────────────────────────────────────────────────────────────

fn foreign_inputs(display: &str, foreign_tax_id: Option<&str>, country: &str) -> PartnerInputs {
    PartnerInputs {
        display_name: display.to_string(),
        legal_name: format!("{display} GmbH"),
        kind: PartnerKind::Customer,
        customer_vat_status: CustomerVatStatus::Other,
        customer_type: CustomerType::Unset,
        // A foreign partner never carries a Hungarian ADÓSZÁM.
        tax_number: None,
        eu_vat_number: foreign_tax_id.map(str::to_string),
        address_street: Some("Hauptstr. 1".to_string()),
        address_postal_code: Some("10115".to_string()),
        address_city: Some("Berlin".to_string()),
        address_country: Some(country.to_string()),
        bank_account: None,
        contact_email: Some("ops@example.de".to_string()),
        contact_phone: None,
    }
}

/// Pin #7 — every legitimate foreign-partner shape saves through the
/// route helper and round-trips on read. Was: an `Other` partner
/// without an EU-shaped community VAT number was rejected 400
/// `validation_failed` at partner save, so a Swiss / US / tax-id-less
/// foreign partner (including a foreign SUPPLIER we never invoice)
/// could not be created at all.
///
/// Restoring the required-and-EU-shape-gated `Other` arm in
/// `validate_partner_inputs` turns cases (a) and (c) red
/// (mutation-verified).
#[test]
fn partners_create_accepts_foreign_partner_without_eu_vat_number() {
    let dir = test_dir("create-foreign");
    let state = build_state(dir.join("aberp.duckdb"));

    // (a) Foreign partner with NO tax identifier at all.
    let no_id = serve::create_partner_request(&state, &foreign_inputs("Kein Ust", None, "DE"))
        .expect("a foreign partner with no tax id must save");
    assert!(no_id.eu_vat_number.is_none());

    // (b) EU business with an EU community VAT number — unchanged arm.
    let eu =
        serve::create_partner_request(&state, &foreign_inputs("Muster", Some("DE123456789"), "DE"))
            .expect("an EU partner with an EU VAT number must save");
    assert_eq!(eu.eu_vat_number.as_deref(), Some("DE123456789"));

    // (c) Third-state (non-EU) partner carrying a non-EU tax id. `CHE-…`
    //     has no EU country prefix and is not VIES-shaped — the reused
    //     column is polymorphic for `Other`.
    let ch = serve::create_partner_request(
        &state,
        &foreign_inputs("Helvetia", Some("CHE-123.456.789"), "CH"),
    )
    .expect("a third-state partner with a non-EU tax id must save");
    assert_eq!(ch.eu_vat_number.as_deref(), Some("CHE-123.456.789"));

    // All three are readable back — the save persisted, it did not just
    // return a populated struct (CLAUDE.md rule 11: no silent no-op).
    for created in [&no_id, &eu, &ch] {
        let fetched = serve::get_partner_request(&state, &created.id).expect("re-read");
        assert_eq!(&fetched, created, "partner must round-trip on read");
        assert_eq!(fetched.customer_vat_status, CustomerVatStatus::Other);
    }

    // The relaxation is BOUNDED — a foreign partner carrying a Hungarian
    // ADÓSZÁM is still a 400 at this surface.
    let mut hu_tax = foreign_inputs("Falsch", Some("DE123456789"), "DE");
    hu_tax.tax_number = Some("24904362-2-41".to_string());
    match serve::create_partner_request(&state, &hu_tax) {
        Err(PartnerRouteError::Validation(errors)) => assert!(
            errors.iter().any(|e| e.field == "tax_number"),
            "must flag tax_number; got {errors:?}"
        ),
        other => {
            panic!("expected Validation on a foreign partner with a HU ADÓSZÁM, got {other:?}")
        }
    }

    let _keep = &dir;
}

// ──────────────────────────────────────────────────────────────────────
// Pin #8 — SPA→`/api/partners` WIRE contract (operator-reported, DEV,
// 2026-07-28).
//
// Why a JSON-body pin rather than another `PartnerInputs` struct
// literal: pins #1–#7 all construct `PartnerInputs` in Rust, so they
// exercise the business path while BYPASSING serde entirely. That is
// precisely how the `customer_type` casing fork reached DEV — the enum
// derived serde with no `rename_all`, so the wire wanted `"Industrial"`
// while every other representation of the same value (`as_db_str`, the
// `margin_profiles` wire contract, the SPA's `CustomerType` union) is
// snake_case. EVERY SPA partner save 422'd before the body ever reached
// a validator, and 438 lines of green route tests could not see it.
//
// These pins deserialize the EXACT bytes `composePartnerInputs`
// (`apps/aberp-ui/ui/src/lib/partners.ts`) emits, for all eight
// customer types × domestic/foreign, and assert the SERIALIZED response
// carries the same literal back (the edit-mode `<select>` matches on
// it — a PascalCase response leaves the control with no selected option
// and the operator with no recoverable state).
// ──────────────────────────────────────────────────────────────────────

/// The `value` column of `CUSTOMER_TYPE_OPTIONS` — the literals the
/// PartnerForm `<select>` actually puts on the wire — paired with the
/// variant each MUST deserialize to. Mirrored by
/// `customer_type_options_are_the_wire_contract` on the vitest side.
const SPA_CUSTOMER_TYPE_OPTIONS: [(&str, CustomerType); 8] = [
    ("industrial", CustomerType::Industrial),
    ("defense", CustomerType::Defense),
    ("aerospace", CustomerType::Aerospace),
    ("research", CustomerType::Research),
    ("prototype_shop", CustomerType::PrototypeShop),
    ("oem", CustomerType::Oem),
    ("consumer", CustomerType::Consumer),
    ("unset", CustomerType::Unset),
];

/// Compile-time exhaustiveness guard: a ninth `CustomerType` variant
/// breaks this match, which forces its SPA option into the table above
/// rather than letting a new segment ship unreachable from the form.
fn customer_type_index(ct: CustomerType) -> usize {
    match ct {
        CustomerType::Industrial => 0,
        CustomerType::Defense => 1,
        CustomerType::Aerospace => 2,
        CustomerType::Research => 3,
        CustomerType::PrototypeShop => 4,
        CustomerType::Oem => 5,
        CustomerType::Consumer => 6,
        CustomerType::Unset => 7,
    }
}

/// The exact JSON body `composePartnerInputs` emits: all fourteen keys
/// in composer order, empty optionals collapsed to `null` (not omitted).
/// `foreign` switches to the operator's reported Slovak shape — `Other`
/// vat status, no Hungarian ADÓSZÁM, an EU community VAT number.
fn spa_partner_body(display: &str, customer_type: &str, foreign: bool) -> String {
    let (vat_status, tax_number, eu_vat, street, postal, city, country) = if foreign {
        (
            "Other",
            "null",
            "\"SK123456789\"",
            "Dunaj",
            "82109",
            "Bratislava",
            "Slovakia",
        )
    } else {
        (
            "Domestic",
            "\"12345678-1-42\"",
            "null",
            "Fő utca 1.",
            "1011",
            "Budapest",
            "Magyarország",
        )
    };
    format!(
        r#"{{"display_name":"{display}","legal_name":"{display} Kft.","kind":"Customer","customer_vat_status":"{vat_status}","customer_type":"{customer_type}","tax_number":{tax_number},"eu_vat_number":{eu_vat},"address_street":"{street}","address_postal_code":"{postal}","address_city":"{city}","address_country":"{country}","bank_account":null,"contact_email":"ops@example.com","contact_phone":null}}"#
    )
}

/// The `customer_type` string on a serialized `Partner` — i.e. what the
/// SPA's `formFromPartner` reads back into the `<select>`.
fn wire_customer_type(partner: &aberp::partners::Partner) -> String {
    serde_json::to_value(partner).expect("Partner serializes")["customer_type"]
        .as_str()
        .expect("customer_type is a JSON string")
        .to_string()
}

/// Pin #8a — the option table covers the closed vocab exactly once, and
/// each literal is the variant's db-string. Keeps the wire, the DB
/// column, and `margin_profiles.customer_type` a single string per
/// value; a `rename_all` drift on the enum turns this red.
#[test]
fn spa_customer_type_options_cover_the_closed_vocab() {
    let mut seen = [false; 8];
    for (literal, variant) in SPA_CUSTOMER_TYPE_OPTIONS {
        let idx = customer_type_index(variant);
        assert!(!seen[idx], "variant listed twice at literal `{literal}`");
        seen[idx] = true;
        assert_eq!(
            variant.as_db_str(),
            literal,
            "SPA literal must equal the db-string so wire == column == margin_profiles"
        );
    }
    assert!(
        seen.iter().all(|s| *s),
        "a CustomerType variant has no SPA option: {seen:?}"
    );
}

/// Pin #8b — all eight customer types save for a DOMESTIC (HU) partner
/// through the real serialized request body, and the response hands the
/// same literal back.
#[test]
fn spa_body_saves_every_customer_type_domestic() {
    let dir = test_dir("spa-wire-domestic");
    let state = build_state(dir.join("aberp.duckdb"));

    for (literal, variant) in SPA_CUSTOMER_TYPE_OPTIONS {
        let body = spa_partner_body(&format!("HU {literal}"), literal, false);
        let inputs: PartnerInputs = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("SPA body for `{literal}` must deserialize: {e}"));
        assert_eq!(inputs.customer_type, variant);

        let created = serve::create_partner_request(&state, &inputs)
            .unwrap_or_else(|e| panic!("domestic `{literal}` must save: {e:?}"));
        assert_eq!(created.customer_type, variant);
        assert_eq!(
            wire_customer_type(&created),
            literal,
            "response literal must match the `<select>` option value"
        );

        // Persisted, not merely returned (CLAUDE.md rule 11).
        let fetched = serve::get_partner_request(&state, &created.id).expect("re-read");
        assert_eq!(fetched.customer_type, variant);
    }

    let _keep = &dir;
}

/// Pin #8c — the same eight, for the FOREIGN (EU / Slovak) partner the
/// operator was actually creating. `Other` vat status is a different
/// validator arm than Domestic, so the matrix is 8 × 2, not 8.
#[test]
fn spa_body_saves_every_customer_type_foreign_eu() {
    let dir = test_dir("spa-wire-foreign");
    let state = build_state(dir.join("aberp.duckdb"));

    for (literal, variant) in SPA_CUSTOMER_TYPE_OPTIONS {
        let body = spa_partner_body(&format!("SK {literal}"), literal, true);
        let inputs: PartnerInputs = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("SPA body for `{literal}` must deserialize: {e}"));

        let created = serve::create_partner_request(&state, &inputs)
            .unwrap_or_else(|e| panic!("foreign `{literal}` must save: {e:?}"));
        assert_eq!(created.customer_type, variant);
        assert_eq!(created.customer_vat_status, CustomerVatStatus::Other);
        assert_eq!(created.eu_vat_number.as_deref(), Some("SK123456789"));
        assert_eq!(wire_customer_type(&created), literal);
    }

    let _keep = &dir;
}

/// Pin #8d — no unrecoverable form state. The operator's sequence was:
/// open the form (default `unset`) → save → error → change the type →
/// save again. Both the default AND the post-change value must go
/// through, and the value the response feeds back into `formFromPartner`
/// must itself be re-submittable (a PascalCase response would leave the
/// `<select>` with no matching option and every retry stuck).
#[test]
fn spa_default_unset_saves_and_a_changed_type_re_saves() {
    let dir = test_dir("spa-wire-recovery");
    let state = build_state(dir.join("aberp.duckdb"));

    // 1. Fresh form, untouched dropdown: `unset` is the SPA default.
    let created: PartnerInputs =
        serde_json::from_str(&spa_partner_body("Recovery", "unset", true)).expect("default body");
    let partner = serve::create_partner_request(&state, &created)
        .expect("the form's DEFAULT customer_type must be saveable");
    assert_eq!(partner.customer_type, CustomerType::Unset);

    // 2. Re-open for edit: `formFromPartner` seeds the `<select>` from
    //    the response literal, so it must be one the form knows.
    let seeded = wire_customer_type(&partner);
    assert!(
        SPA_CUSTOMER_TYPE_OPTIONS.iter().any(|(v, _)| *v == seeded),
        "response `{seeded}` has no matching <select> option — the edit form would open blank"
    );

    // 3. Operator changes the type and re-submits.
    let edited: PartnerInputs =
        serde_json::from_str(&spa_partner_body("Recovery", "industrial", true))
            .expect("edited body");
    let updated = serve::update_partner_request(
        &state,
        &partner.id,
        &edited,
        "test-operator",
        BinaryHash::from_bytes([0u8; 32]),
    )
    .expect("a changed customer_type must re-save");
    assert_eq!(updated.customer_type, CustomerType::Industrial);
    assert_eq!(wire_customer_type(&updated), "industrial");

    let _keep = &dir;
}
