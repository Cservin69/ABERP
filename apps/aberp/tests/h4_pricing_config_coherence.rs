//! ADR-0099 H4 — pricing-config Handle coherence (backlog item 2).
//!
//! The three pricing-config families — `margin_profiles`, `quoting_materials`,
//! `quoting_tunables` — were HALF-migrated: their audit rode the shared Handle
//! (`append_machine_event` / `append_in_tx`), but every serve CRUD handler wrote
//! and read the BUSINESS rows on a fresh `Connection::open`. Under the H3
//! checkpoint-disabled model the runtime pricing pipeline reads this config
//! THROUGH the Handle (`quote_pricing_pipeline::advance_price` on `db.write()`,
//! `reprice_quote` on a Handle tx, `catalogue_push` / `amend_pricing_job_material`
//! on `db.read()`). A persistent Handle is Q2-BLIND to a fresh connection's
//! post-open commit, so an operator's margin/material/tunable edit was invisible
//! to the pipeline — quotes priced on STALE config, the margin floor reading an
//! empty/old profile and silently passing ([[trust-code-not-operator]], the same
//! fail-open class STEP 4e closed for `quote_pricing_jobs`).
//!
//! Backlog item 2 routed every writer AND reader of these families onto the ONE
//! shared Handle. These pins prove the instance model per subsystem: a config
//! write committed through the Handle's writer is OBSERVED by a CO-RESIDENT
//! Handle reader (`db.read()` — a `try_clone` sharing the one instance, F-C). A
//! fresh-`Connection::open` writer fork would NOT be seen by the persistent
//! Handle reader — revert any migrated writer to a fresh open and the matching
//! assertion below fails (the fail-open regression).

use std::path::PathBuf;

use duckdb::Connection;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};

use aberp::partners::CustomerType;
use aberp::serve::open_tenant_handle;
use aberp::{margin_profiles, quoting_machines, quoting_materials, quoting_tunables};

const T: &str = "h4_pricing_config_coherence";

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("aberp-h4-pricing").join(format!(
        "{}-{}",
        label,
        ulid::Ulid::new()
    ));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// A margin profile written through the shared Handle's writer is OBSERVED by a
/// co-resident Handle reader — the exact read the pricing pipeline's margin-floor
/// resolution performs (`active_profile_for_customer_type`). Business row + audit
/// co-commit on ONE guard tx (the canonical `create_ncr` recipe).
#[test]
fn margin_profile_handle_write_is_seen_by_coresident_handle_read() {
    let dir = test_dir("margin");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    // Schemas on a fresh conn BEFORE the Handle opens (coherent Q1 direction).
    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        margin_profiles::ensure_schema(&conn).unwrap();
    }

    // The live shared Handle — the runtime `state.db`.
    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();

    // Write a margin profile via the REAL primitive on the Handle writer, fusing
    // the business INSERT and the audit append in ONE tx (rule 15), then DROP the
    // write guard so the reader below is not a re-entrant acquire.
    let inputs = margin_profiles::MarginProfileInputs {
        name: "OEM profile".to_string(),
        customer_type: "oem".to_string(),
        gross_margin_pct: 40.0,
        min_margin_pct: 20.0,
        notes: None,
        enabled: true,
    };
    {
        let mut guard = db.write().unwrap();
        let tx = guard.transaction().unwrap();
        let created = match margin_profiles::create_profile(&tx, T, &inputs).unwrap() {
            margin_profiles::CreateOutcome::Created(p) => p,
            other => panic!("expected Created, got {other:?}"),
        };
        let meta = LedgerMeta::new(tenant.clone(), hash);
        let actor = Actor::from_local_cli(ulid::Ulid::new().to_string(), "operator");
        aberp_audit_ledger::append_in_tx(
            &tx,
            &meta,
            aberp_audit_ledger::EventKind::MarginProfileCreated,
            serde_json::to_vec(&serde_json::json!({ "profile_id": created.id })).unwrap(),
            actor,
            None,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Read through a co-resident Handle reader (`db.read()` try_clone) — the
    // margin-floor read path. It MUST observe the just-committed profile.
    let conn = db.read().unwrap();
    let profile = margin_profiles::active_profile_for_customer_type(&conn, T, CustomerType::Oem)
        .expect("read margin profile through the Handle");
    let profile = profile.expect(
        "the co-resident Handle read OBSERVED the Handle write — a fresh-open writer fork \
         would leave this None (the stale margin-floor fail-open)",
    );
    assert_eq!(profile.customer_type, "oem");
    assert_eq!(profile.gross_margin_pct, 40.0);
    assert_eq!(profile.min_margin_pct, 20.0);
}

/// A material written through the Handle writer (business + audit fused inside
/// `create_material`) is OBSERVED by a co-resident Handle reader — the exact read
/// the pipeline (`list_materials`) and the storefront push (`list_public`) do.
#[test]
fn material_handle_write_is_seen_by_coresident_handle_read() {
    let dir = test_dir("material");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        quoting_materials::ensure_schema(&conn).unwrap();
    }

    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();

    let inputs = quoting_materials::MaterialInputs {
        grade: "TI_6AL4V".to_string(),
        display_name: "Titanium Ti-6Al-4V".to_string(),
        density_g_cm3: 4.43,
        cost_per_kg_eur: 35.0,
        machining_difficulty: 1.8,
        carbide_life_multiplier: 1.0,
        stock_status: "in_stock".to_string(),
        lead_time_default_days: 0,
        quote_multiplier: 1.0,
        notes: None,
    };
    {
        let mut guard = db.write().unwrap();
        let meta = LedgerMeta::new(tenant.clone(), hash);
        quoting_materials::create_material(&mut guard, &meta, "operator", T, &inputs)
            .expect("create material through the Handle");
    }

    let conn = db.read().unwrap();
    let materials = quoting_materials::list_materials(&conn, T).expect("list materials via Handle");
    assert!(
        materials.iter().any(|m| m.grade == "TI_6AL4V"),
        "the co-resident Handle read OBSERVED the Handle write — a fresh-open writer fork \
         would leave this catalogue stale (the pipeline/storefront reading old materials)",
    );
}

/// A complexity rule written through the Handle writer (business + audit fused
/// inside `create_complexity_rule`) is OBSERVED by a co-resident Handle reader —
/// the exact read the pipeline (`list_complexity_rules`) performs per pricing pass.
#[test]
fn tunable_handle_write_is_seen_by_coresident_handle_read() {
    let dir = test_dir("tunable");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    {
        let mut conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        quoting_tunables::ensure_schema(&mut conn, T).unwrap();
    }

    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();

    let inputs = quoting_tunables::ComplexityRuleInputs {
        feature_type: "pocket".to_string(),
        size_bucket: "L".to_string(),
        count_min: 1,
        count_max: Some(5),
        base_time_minutes: 2.5,
        multiplier: 1.3,
        setup_penalty_minutes: 4.0,
        notes: Some("H4 coherence pin".to_string()),
    };
    let created_id = {
        let mut guard = db.write().unwrap();
        let meta = LedgerMeta::new(tenant.clone(), hash);
        quoting_tunables::create_complexity_rule(&mut guard, &meta, "operator", T, &inputs)
            .expect("create complexity rule through the Handle")
            .id
    };

    let conn = db.read().unwrap();
    let rules = quoting_tunables::list_complexity_rules(&conn, T)
        .expect("list complexity rules via Handle");
    assert!(
        rules.iter().any(|r| r.id == created_id),
        "the co-resident Handle read OBSERVED the Handle write — a fresh-open writer fork \
         would leave the tunables stale (the pipeline pricing on old complexity rules)",
    );
}

/// A machine's capacity written through the Handle writer (business + audit fused
/// in ONE tx) is OBSERVED by a co-resident Handle reader via the EXACT primitive
/// the pricing pipeline uses for lead-time — `list_enabled_capacities`
/// (`quote_pricing_pipeline::advance_price:972`). Before the H4 migration the
/// machine CRUD writers forked a fresh `Connection::open`, so the pipeline's
/// persistent-Handle read was Q2-blind to a just-edited machine and lead-time was
/// computed on STALE capacity (the same fail-open class, do-not-defer per 4e).
#[test]
fn machine_capacity_handle_write_is_seen_by_pipeline_handle_read() {
    let dir = test_dir("machine");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        quoting_machines::ensure_schema(&conn).unwrap();
    }

    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();

    let inputs = quoting_machines::MachineInputs {
        name: "DMG MORI DMU 50".to_string(),
        family: "5-axis-mill".to_string(),
        max_envelope_xyz_mm: [500.0, 450.0, 400.0],
        daily_hours_avail: 16.0,
        buffer_pct: 20.0,
        enabled: true,
    };
    {
        let mut guard = db.write().unwrap();
        let tx = guard.transaction().unwrap();
        let machine = quoting_machines::create_machine(&tx, T, &inputs)
            .expect("create machine through the Handle");
        let meta = LedgerMeta::new(tenant.clone(), hash);
        let actor = Actor::from_local_cli(ulid::Ulid::new().to_string(), "operator");
        aberp_audit_ledger::append_in_tx(
            &tx,
            &meta,
            aberp_audit_ledger::EventKind::MachineCreated,
            serde_json::to_vec(&serde_json::json!({ "machine_id": machine.id })).unwrap(),
            actor,
            None,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Read capacity through the co-resident Handle reader — the exact call the
    // pricing pipeline makes to compute lead-time. It MUST observe the new machine
    // (an empty DB before the write ⇒ observing it is unambiguous).
    let conn = db.read().unwrap();
    let capacities =
        quoting_machines::list_enabled_capacities(&conn, T).expect("list capacities via Handle");
    assert!(
        capacities
            .iter()
            .any(|c| c.daily_hours_avail == 16.0 && c.buffer_pct == 20.0),
        "the pipeline-shaped Handle read OBSERVED the Handle capacity write — a fresh-open \
         writer fork would leave lead-time priced on stale machine capacity (got {capacities:?})",
    );
}
