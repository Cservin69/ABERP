-- S443 / ADR-0092 — QC dimensional-inspection schema.
--
-- Two tables added alongside `qa_inspections` (V001). Per ADR-0092
-- §"Reconciliation", `qc_inspections` is a DIFFERENT altitude from
-- `qa_inspections`: the latter is a per-routing-op Pass/Fail/Rework/
-- Dispose DECISION queue with a state machine + cross-actor supersede;
-- the former is a per-feature dimensional MEASUREMENT record (nominal/
-- actual/deviation + a computed verdict tier). The ADR decided a
-- separate table so the qa state machine, its NOT-NULL `routing_op_id`,
-- and the `all_live_inspections_passed_for_wo` WO-completion gate are
-- NOT polluted by dimensional rows. We extend the aberp-qa CRATE (the
-- S443 brief's intent), not the qa_inspections TABLE.
--
-- Posture (matches V001): natural-key PKs, `CREATE TABLE IF NOT EXISTS`
-- (idempotent), no CHECK / no DEFAULT / no UNIQUE — every invariant
-- (verdict tier, unique (product, feature) plan, units match) lives in
-- code per [[no-sql-specific]]. A non-probing tenant simply has zero rows.

-- The nominal/tolerance source of truth — what makes the verdict ABERP's
-- code, not the machine's. Unique (tenant, product_id, feature_name)
-- among non-archived plans is enforced in `qc::plans` (no SQL UNIQUE).
CREATE TABLE IF NOT EXISTS qc_inspection_plans (
    plan_id                 VARCHAR NOT NULL PRIMARY KEY,
    tenant_id               VARCHAR NOT NULL,
    product_id              VARCHAR NOT NULL,
    feature_name            VARCHAR NOT NULL,
    nominal_value           DOUBLE  NOT NULL,
    upper_tol               DOUBLE  NOT NULL,
    lower_tol               DOUBLE  NOT NULL,
    units                   VARCHAR NOT NULL,
    optional_probe_cycle_id VARCHAR,
    enabled                 BOOLEAN NOT NULL,
    created_at              VARCHAR NOT NULL,
    archived_at             VARCHAR
);

CREATE INDEX IF NOT EXISTS qc_inspection_plans_tenant_product_idx
    ON qc_inspection_plans (tenant_id, product_id);

-- One row per inspected feature per measurement (manual entry today;
-- probe-sourced when a real `ProbeIngestionSource` lands). Plan
-- nominal/tol/feature/units are DENORMALISED (snapshot) so the row
-- records what it was actually measured against even if the plan later
-- changes — an audit/traceability requirement. `verdict` is the
-- code-computed tier; `auto_ncr_id` is set when an out-of-tolerance
-- verdict auto-spawned an S439 NCR.
CREATE TABLE IF NOT EXISTS qc_inspections (
    qci_id                     VARCHAR NOT NULL PRIMARY KEY,
    tenant_id                  VARCHAR NOT NULL,
    measured_at_utc            VARCHAR NOT NULL,
    source                     VARCHAR NOT NULL,
    source_event_id            VARCHAR,
    inspection_plan_id         VARCHAR NOT NULL,
    feature_name               VARCHAR NOT NULL,
    nominal_value              DOUBLE  NOT NULL,
    upper_tol                  DOUBLE  NOT NULL,
    lower_tol                  DOUBLE  NOT NULL,
    units                      VARCHAR NOT NULL,
    actual_value               DOUBLE  NOT NULL,
    deviation                  DOUBLE  NOT NULL,
    verdict                    VARCHAR NOT NULL,
    probe_serial               VARCHAR,
    last_calibration_at_utc    VARCHAR,
    calibration_stale_at_event BOOLEAN NOT NULL,
    auto_ncr_id                VARCHAR,
    linked_part_uid            VARCHAR,
    linked_heat_lot            VARCHAR,
    linked_wo_id               VARCHAR,
    recorded_by                VARCHAR NOT NULL,
    created_at                 VARCHAR NOT NULL
);

CREATE INDEX IF NOT EXISTS qc_inspections_tenant_wo_idx
    ON qc_inspections (tenant_id, linked_wo_id);

CREATE INDEX IF NOT EXISTS qc_inspections_tenant_part_idx
    ON qc_inspections (tenant_id, linked_part_uid);
