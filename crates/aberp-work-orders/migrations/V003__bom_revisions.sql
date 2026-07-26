-- ADR-0105 — BOM revision history.
--
-- V001 already retains superseded BOM lines (`boms.retired_at`, never
-- DELETEd per ADR-0062 §6), but the retained rows carry no revision
-- IDENTITY: no rev number, no author, no reason, and the only grouping
-- key is `created_at` — which collides when two replaces land in the
-- same RFC3339 second. A retained-but-unidentifiable prior BOM is not
-- traceability.
--
-- This migration adds the identity layer. Additive, forward-only, no
-- backfill, no CHECK — same idempotent posture as V001/V002 (re-running
-- against a migrated tenant is a no-op).
--
-- 1) bom_revisions — one header row per authored BOM revision.
-- 2) boms.bom_rev_id — which revision each line belongs to.
-- 3) work_orders.bom_rev_id — WHICH REVISION A WO WAS RELEASED
--    AGAINST. This is the load-bearing traceability link: it answers
--    "what BOM was this batch built to" from the WO row alone.
--
-- NOT backfilled on purpose (ADR-0105 §2.4): pre-migration `boms` rows
-- have no recorded author or reason, and minting a synthetic revision
-- for them would fabricate attribution the system never had. They stay
-- `bom_rev_id IS NULL` ("legacy / unrevisioned") and the Release path
-- warns loudly rather than pinning a lie.

-- 1. bom_revisions — the revision header per ADR-0105 §2.2.
--
-- `rev_number` is monotonic per (tenant_id, product_id), 1-based,
-- allocated by an in-tx MAX(rev_number)+1 probe — the same
-- allocator-style posture `work_orders.wo_number` uses (V001), and the
-- same [[no-sql-specific]] reason for no DB-level UNIQUE: the
-- application-layer probe inside the write tx is the authoritative
-- gate, the index below only makes it fast.
--
-- `content_hash` is FNV-1a over the canonical sorted
-- `component_id=qty;` set — identical construction to the S429
-- `CalibrationTable::set_hash` / ADR-0104 price-snapshot pin. It makes
-- "is revision N materially the same as revision M" a byte comparison
-- rather than a set walk.
--
-- There is deliberately NO `superseded_at` column: the current
-- revision is MAX(rev_number) for the product. A second record of the
-- same fact is a second thing that can go stale (CLAUDE.md rule 12).
CREATE TABLE IF NOT EXISTS bom_revisions (
    bom_rev_id      VARCHAR       NOT NULL PRIMARY KEY,
    tenant_id       VARCHAR       NOT NULL,
    product_id      VARCHAR       NOT NULL,
    rev_number      INTEGER       NOT NULL,
    created_at      VARCHAR       NOT NULL,
    author          VARCHAR       NOT NULL,
    reason          VARCHAR,
    line_count      INTEGER       NOT NULL,
    content_hash    VARCHAR       NOT NULL
);

-- Revision list for a product (newest first) is the SPA read; the
-- in-tx MAX(rev_number) allocator probe uses the same prefix.
CREATE INDEX IF NOT EXISTS bom_revisions_tenant_product_rev_idx
    ON bom_revisions (tenant_id, product_id, rev_number);

-- 2. boms.bom_rev_id — the line → revision link. NULL only for
-- pre-ADR-0105 legacy rows; every line written by
-- `replace_bom_for_product` after this migration carries one.
ALTER TABLE boms ADD COLUMN IF NOT EXISTS bom_rev_id VARCHAR;

-- Lines-of-a-revision is the "show me revision 3" read.
CREATE INDEX IF NOT EXISTS boms_tenant_rev_idx
    ON boms (tenant_id, bom_rev_id);

-- 3. work_orders.bom_rev_id — the pin, stamped once at Release
-- (ADR-0105 §2.5). NULL means either "not yet released" or "released
-- against a legacy unrevisioned BOM"; the two are distinguished by
-- `released_at`.
ALTER TABLE work_orders ADD COLUMN IF NOT EXISTS bom_rev_id VARCHAR;
