-- S232 / PR-228 / ADR-0062 — Work Orders v1 schema.
--
-- Three tables, all forward-only additive per ADR-0062 §1 + the
-- [[no-sql-specific]] posture (no CHECK constraints on closed-vocab
-- state columns — the transition table lives in
-- `aberp_work_orders::state::next_state`, not in the storage engine,
-- per ADR-0062 §"Cross-cutting decisions" #2 and the
-- [[duckdb-alter-column-type-check-constraint]] memory pin).
--
-- 1) work_orders   — the regulated entity. Closed-vocab `state`
--    column; transitions stamp the matching `*_at` timestamp.
-- 2) boms          — 1-level bill of materials per finished-good
--    product. Soft-retired via `retired_at` (ADR-0062 §6); never
--    DELETEd.
-- 3) routings      — per-WO linear routing operations. Each WO gets
--    its own set; ordered by `sequence`.
--
-- Posture: `CREATE TABLE IF NOT EXISTS` so re-running this migration
-- against a tenant that already has work-order rows is a no-op —
-- same idempotent posture every other ABERP boot migration uses
-- (products / partners / ap_invoice / restored_invoice / stock_movements).

-- 1. work_orders — the regulated entity per ADR-0062 §1.
--
-- `state` is plain VARCHAR (no CHECK) per [[no-sql-specific]] +
-- ADR-0062 §"Cross-cutting decisions" #2. The closed-vocab
-- `WorkOrderState` lives in `aberp_work_orders::state` and the
-- transition handler refuses illegal transitions with 400.
CREATE TABLE IF NOT EXISTS work_orders (
    wo_id           VARCHAR       NOT NULL PRIMARY KEY,
    tenant_id       VARCHAR       NOT NULL,
    wo_number       VARCHAR       NOT NULL,
    product_id      VARCHAR       NOT NULL,
    qty_target      DECIMAL(18,6) NOT NULL,
    state           VARCHAR       NOT NULL,
    created_at      VARCHAR       NOT NULL,
    released_at     VARCHAR,
    started_at      VARCHAR,
    completed_at    VARCHAR,
    cancelled_at    VARCHAR,
    hold_reason     VARCHAR,
    notes           VARCHAR
);

-- Lists by tenant + state are the SPA's primary read pattern (the
-- WorkOrderList state-facet chips); ordering by created_at is the
-- default. The (tenant_id, state, created_at) compound supports both
-- the chip filter and the default order without a fanout.
CREATE INDEX IF NOT EXISTS work_orders_tenant_state_created_idx
    ON work_orders (tenant_id, state, created_at);

-- Operator-visible WO numbers must be unique per tenant so the SPA's
-- search-by-WO-number stays unambiguous. Enforced by the application
-- layer's allocator; the index supports a fast lookup but the column
-- is not declared UNIQUE here (no DB-level constraint per the
-- [[no-sql-specific]] posture — the allocator's uniqueness probe
-- inside the same tx is the authoritative gate).
CREATE INDEX IF NOT EXISTS work_orders_tenant_wo_number_idx
    ON work_orders (tenant_id, wo_number);

-- 2. boms — 1-level bill of materials per ADR-0062 §1.
--
-- `product_id` is the FINISHED GOOD (the parent); `component_id` is
-- the child (a stock-tracked product). `qty_per_unit` is the quantity
-- of the component consumed per unit of finished good. ADR-0062 §6 —
-- BOM rows are SOFT-RETIRED via `retired_at`, NEVER DELETEd, so
-- historical WO releases can be re-traced against the BOM-as-of the
-- release timestamp.
CREATE TABLE IF NOT EXISTS boms (
    bom_line_id     VARCHAR       NOT NULL PRIMARY KEY,
    tenant_id       VARCHAR       NOT NULL,
    product_id      VARCHAR       NOT NULL,
    component_id    VARCHAR       NOT NULL,
    qty_per_unit    DECIMAL(18,6) NOT NULL,
    created_at      VARCHAR       NOT NULL,
    retired_at      VARCHAR
);

-- Active-BOM-rows-for-a-product is the load-bearing read (the Release
-- handler snapshots the active rows per ADR-0062 §5). Per-tenant +
-- per-product index supports it; the `retired_at IS NULL` filter is
-- applied at query time.
CREATE INDEX IF NOT EXISTS boms_tenant_product_idx
    ON boms (tenant_id, product_id);

-- 3. routings — per-WO linear routing operations per ADR-0062 §1.
--
-- One row per operation. `wo_id` ties it to the parent WO; `sequence`
-- is the linear order (1, 2, 3...). `state` is closed-vocab
-- `RoutingOpState` (Pending → Active → Completed | Skipped) — same
-- no-CHECK posture as `work_orders.state`. The application-layer
-- next-op-auto-Active cascade lives in
-- `aberp_work_orders::handlers::complete_routing_op`.
CREATE TABLE IF NOT EXISTS routings (
    routing_op_id   VARCHAR       NOT NULL PRIMARY KEY,
    tenant_id       VARCHAR       NOT NULL,
    wo_id           VARCHAR       NOT NULL,
    sequence        INTEGER       NOT NULL,
    op_name         VARCHAR       NOT NULL,
    est_time_min    INTEGER,
    est_cost_huf    DECIMAL(18,2),
    state           VARCHAR       NOT NULL,
    started_at      VARCHAR,
    completed_at    VARCHAR
);

-- Per-WO routing read (the WorkOrderDetail page lists the rows
-- ordered by sequence). Tenant + WO + sequence covers both the read
-- and the next-pending-sequence lookup.
CREATE INDEX IF NOT EXISTS routings_tenant_wo_sequence_idx
    ON routings (tenant_id, wo_id, sequence);
