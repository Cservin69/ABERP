//! ADR-0099 H3 STEP 4e — margin-floor DEAL coherence (closes the HIGH fail-open).
//!
//! The 4c/4d adversarial found a HIGH hole: STEP 4d moved the DEAL saga's
//! `quote_pricing_jobs.margin_below_floor` READ onto the shared Handle
//! (`quote_deal.rs`), but the operator-override WRITERS (the serve re-price
//! routes) still wrote that flag on a fresh `Connection::open` write-fork. Under
//! the H3 checkpoint-disabled model the Handle's persistent connection is BLIND
//! to a co-resident fork's WAL-resident write, so a below-floor override could be
//! DEALT — the S428 hard block ([[trust-code-not-operator]]) failing OPEN.
//!
//! STEP 4e migrated those writers onto the ONE shared Handle. This pin proves the
//! fix AND settles the codebase's internally-contradictory instance model with a
//! direct observation: with a live Handle held (as `state.db` is at runtime), a
//! below-floor flag written through the Handle's writer (`set_margin_result` — the
//! exact primitive the migrated re-price routes call) is OBSERVED by the REAL DEAL
//! read path (`run_deal_saga`) reading through the SAME Handle, so the saga BLOCKS
//! with `BelowMarginFloor`. Revert the writer to a fresh `Connection::open` fork
//! and this test fails: the Handle read no longer sees the write and the saga
//! proceeds — the fail-open regression.

use std::path::PathBuf;

use duckdb::Connection;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};
use aberp_quote_intake::log_table;

use aberp::quote_deal::{expected_deal_token, run_deal_saga, DealSagaError, DealSagaInputs};
use aberp::quote_pricing_jobs;
use aberp::serve::open_tenant_handle;

const T: &str = "step4e_margin_coherence";

fn test_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir()
            .join("aberp-step4e")
            .join(format!("{}-{}", label, ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// A below-floor flag written through the shared Handle is OBSERVED by the DEAL
/// saga reading through the SAME Handle, so the hard S428 block fires. This is
/// the whole point of the single-instance model: one Handle is coherent with
/// itself, which the fresh-`Connection::open` writer fork was NOT.
#[test]
fn deal_saga_handle_read_observes_below_floor_write_and_blocks() {
    let dir = test_dir("below-floor");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);
    // Storefront-shape id; `expected_deal_token` is its first 8 chars.
    let quote_id = "0226e154-9e6c-4c0a-9001-f3a8a0c0a000";

    // Schemas on a fresh conn BEFORE the Handle opens (the coherent Q1 direction:
    // a fresh writer that closes before the Handle opens IS visible to it).
    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        log_table::ensure_schema(&conn).unwrap();
        quote_pricing_jobs::ensure_schema(&conn).unwrap();
    }

    // The live shared Handle — the runtime `state.db`.
    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();

    let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();

    // Seed the DEAL precondition (a STAGED intake row) + a priced job row, THROUGH
    // the Handle, in one guard tx.
    {
        let mut guard = db.write().unwrap();
        let tx = guard.transaction().unwrap();
        log_table::insert_intake(
            &tx,
            T,
            quote_id,
            "inv_mc_1",
            "2026-06-05T08:00:00Z",
            now,
            "{}",
            "{}",
        )
        .expect("stage intake row");
        quote_pricing_jobs::insert_fetched_job(
            &tx,
            quote_id,
            T,
            "c@x.com",
            "Cust",
            "Comp",
            "AL_6061_T6",
            1,
            "part.stl",
            "/tmp/part.stl",
            now,
        )
        .expect("insert priced job row");
        tx.commit().unwrap();
    }

    // Flag the quote below its floor via the REAL writer primitive
    // (`set_margin_result` — the exact fn the migrated re-price routes call),
    // through the shared Handle's writer. This is NOT a hand-rolled UPDATE — it is
    // the production write path.
    {
        let guard = db.write().unwrap();
        let applied = quote_pricing_jobs::set_margin_result(
            &guard,
            quote_id,
            T,
            "{}",
            1000.0,
            /* margin_below_floor = */ true,
            Some(0.30),
            now,
        )
        .expect("set_margin_result on the shared writer");
        assert!(applied, "set_margin_result must update the seeded row");
    }

    // Run the REAL DEAL read path on the SAME shared Handle. `run_deal_saga` reads
    // `quote_pricing_jobs::margin_below_floor` on the guard connection; it MUST
    // observe the write above and return `BelowMarginFloor` (a hard, code-enforced
    // block, regardless of any operator confirmation).
    let meta = LedgerMeta::new(tenant.clone(), hash);
    let actor = Actor::from_local_cli(ulid::Ulid::new().to_string(), "operator");
    let err = {
        let mut guard = db.write().unwrap();
        run_deal_saga(
            &mut guard,
            &meta,
            actor,
            DealSagaInputs {
                tenant: T.to_string(),
                quote_id: quote_id.to_string(),
                actor: "operator".to_string(),
                deal_token: expected_deal_token(quote_id),
                refresh_ack: None,
            },
        )
        .expect_err("a below-floor quote must NOT be dealt")
    };

    let deal_err = err
        .downcast::<DealSagaError>()
        .expect("the saga error is a DealSagaError");
    assert!(
        matches!(deal_err, DealSagaError::BelowMarginFloor { .. }),
        "the Handle read OBSERVED the Handle write → the S428 hard block fires \
         (got {deal_err:?}). This settles the instance model: one shared Handle is \
         self-coherent, so the below-floor DEAL block cannot fail open."
    );
    // The route-facing machine code the SPA/gate dispatches on.
    assert_eq!(deal_err.machine_code(), "below_margin_floor");
}
